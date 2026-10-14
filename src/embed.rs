use crate::native::NativeModel;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

const STDERR_TAIL_LIMIT: usize = 8 * 1024;

// Drain stderr continuously: an unconsumed pipe could block the embedding
// worker, and an inherited pipe cannot be inspected after a native crash.
fn collect_stderr(mut stderr: impl Read + Send + 'static) -> (Arc<Mutex<Vec<u8>>>, JoinHandle<()>) {
    let tail = Arc::new(Mutex::new(Vec::new()));
    let shared = Arc::clone(&tail);
    let reader = std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        while let Ok(len) = stderr.read(&mut buffer) {
            if len == 0 {
                break;
            }
            let _ = std::io::stderr().write_all(&buffer[..len]);
            if let Ok(mut recorded) = shared.lock() {
                recorded.extend_from_slice(&buffer[..len]);
                if recorded.len() > STDERR_TAIL_LIMIT {
                    let excess = recorded.len() - STDERR_TAIL_LIMIT;
                    recorded.drain(..excess);
                }
            }
        }
    });
    (tail, reader)
}

fn python_failure(
    child: &mut Child,
    tail: &Arc<Mutex<Vec<u8>>>,
    reader: &mut Option<JoinHandle<()>>,
    reason: &str,
) -> anyhow::Error {
    let status = if reason == "EOF before reply" {
        child.wait().ok()
    } else {
        child.try_wait().ok().flatten()
    };
    if status.is_some() {
        // When the process has exited, finish draining its diagnostics first.
        if let Some(reader) = reader.take() {
            let _ = reader.join();
        }
    }
    let excerpt = tail
        .lock()
        .map(|log| String::from_utf8_lossy(&log).trim().to_string())
        .unwrap_or_default();
    let status = status.map_or_else(|| "status unavailable".to_string(), |s| s.to_string());
    if excerpt.is_empty() {
        anyhow::anyhow!("Python embedding bridge failed ({status}): {reason}")
    } else {
        anyhow::anyhow!("Python embedding bridge failed ({status}): {reason}; stderr: {excerpt}")
    }
}

fn python_program() -> String {
    std::env::var("WEFT_PYTHON")
        .or_else(|_| std::env::var("MYVEC_PYTHON"))
        .unwrap_or_else(|_| {
            if let Some(home) = std::env::var_os("HOME") {
                let candidate = Path::new(&home).join(".local/share/weft/media-venv/bin/python");
                if candidate.is_file() {
                    return candidate.to_string_lossy().into_owned();
                }
            }
            "python3".to_string()
        })
}

#[derive(Clone)]
pub struct Input {
    pub kind: &'static str,
    pub value: String,
}

pub enum Embedder {
    Native(NativeModel),
    Http {
        endpoint: String,
        model: String,
    },
    Python {
        child: Child,
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
        stderr_tail: Arc<Mutex<Vec<u8>>>,
        stderr_reader: Option<JoinHandle<()>>,
    },
}

impl Embedder {
    pub fn new(
        backend: &str,
        endpoint: &str,
        model: &str,
        model_file: &Path,
        device: &str,
        bridge: Option<&Path>,
        text_only: bool,
    ) -> Result<Self> {
        match backend {
            "native" => Ok(Self::Native(NativeModel::load(model_file, device)?)),
            "http" => Ok(Self::Http {
                endpoint: endpoint.to_string(),
                model: model.to_string(),
            }),
            "python" => {
                // The default bridge travels inside the executable, so an installed
                // `weft` works from any working directory without the source tree.
                let mut command = Command::new(python_program());
                command.arg("-u");
                if let Some(path) = bridge {
                    command.arg(path);
                } else {
                    command
                        .arg("-c")
                        .arg(include_str!("../scripts/embed_bridge.py"));
                }
                let mut child = command
                    .arg("--model")
                    .arg(if model == "embeddinggemma-2" {
                        "google/embeddinggemma-2"
                    } else {
                        model
                    })
                    .args(if text_only { vec!["--text-only"] } else { vec![] })
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .context("starting Python embedding bridge (set WEFT_PYTHON to a Python with sentence-transformers installed)")?;
                let stdin = child
                    .stdin
                    .take()
                    .context("Python bridge stdin unavailable")?;
                let stdout = BufReader::new(
                    child
                        .stdout
                        .take()
                        .context("Python bridge stdout unavailable")?,
                );
                let (stderr_tail, stderr_reader) = collect_stderr(
                    child
                        .stderr
                        .take()
                        .context("Python bridge stderr unavailable")?,
                );
                Ok(Self::Python {
                    child,
                    stdin,
                    stdout,
                    stderr_tail,
                    stderr_reader: Some(stderr_reader),
                })
            }
            _ => bail!("backend must be native, http, or python"),
        }
    }

    pub fn embed(&mut self, inputs: &[Input], role: &str) -> Result<Vec<Vec<f32>>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let output: Vec<Vec<f32>> = match self {
            Self::Native(model) => {
                if inputs.iter().any(|x| x.kind != "text") {
                    bail!(
                        "image/audio/video require --backend python (see README for dependencies)"
                    );
                }
                inputs
                    .iter()
                    .map(|input| model.embed(&input.value, role))
                    .collect::<Result<_>>()?
            }
            Self::Http { endpoint, model } => {
                if inputs.iter().any(|x| x.kind != "text") {
                    bail!(
                        "image/audio/video require --backend python (see README for dependencies)"
                    );
                }
                let prefix = if role == "query" {
                    "task: search result | query: "
                } else {
                    "title: none | text: "
                };
                let text: Vec<String> = inputs
                    .iter()
                    .map(|x| format!("{prefix}{}", x.value))
                    .collect();
                let req = json!({"model":model,"input":text});
                let response: Value = ureq::post(endpoint).timeout(std::time::Duration::from_secs(180))
                    .send_json(req).with_context(|| format!("embedding request to {endpoint} failed; start an embedding server first"))?
                    .into_json().context("decoding embedding response")?;
                if let Some(data) = response.get("data").and_then(Value::as_array) {
                    let mut indexed: Vec<(usize, Vec<f32>)> = data
                        .iter()
                        .enumerate()
                        .map(|(i, item)| {
                            let idx = item
                                .get("index")
                                .and_then(Value::as_u64)
                                .unwrap_or(i as u64) as usize;
                            let embedding = item.get("embedding").context("missing embedding")?;
                            Ok((idx, parse_vec(embedding)?))
                        })
                        .collect::<Result<_>>()?;
                    indexed.sort_by_key(|(i, _)| *i);
                    indexed.into_iter().map(|(_, v)| v).collect()
                } else if let Some(data) = response.get("embeddings").and_then(Value::as_array) {
                    data.iter().map(parse_vec).collect::<Result<_>>()?
                } else {
                    bail!("embedding server response has neither data nor embeddings")
                }
            }
            Self::Python {
                child,
                stdin,
                stdout,
                stderr_tail,
                stderr_reader,
            } => {
                let arr: Vec<Value> = inputs
                    .iter()
                    .map(|x| json!({"kind":x.kind,"value":x.value}))
                    .collect();
                let send_result = (|| -> Result<()> {
                    serde_json::to_writer(&mut *stdin, &json!({"inputs":arr,"role":role}))?;
                    writeln!(stdin)?;
                    stdin.flush()?;
                    Ok(())
                })();
                if let Err(error) = send_result {
                    return Err(python_failure(
                        child,
                        stderr_tail,
                        stderr_reader,
                        &format!("writing request: {error}"),
                    ));
                }
                let mut line = String::new();
                match stdout.read_line(&mut line) {
                    Ok(0) => {
                        return Err(python_failure(
                            child,
                            stderr_tail,
                            stderr_reader,
                            "EOF before reply",
                        ));
                    }
                    Err(error) => {
                        return Err(python_failure(
                            child,
                            stderr_tail,
                            stderr_reader,
                            &format!("reading reply: {error}"),
                        ));
                    }
                    Ok(_) => {}
                }
                let response: Value =
                    serde_json::from_str(&line).context("invalid Python bridge response")?;
                if let Some(err) = response.get("error") {
                    bail!("Python embedding bridge: {err}");
                }
                response
                    .get("embeddings")
                    .and_then(Value::as_array)
                    .context("bridge missing embeddings")?
                    .iter()
                    .map(parse_vec)
                    .collect::<Result<_>>()?
            }
        };
        if output.len() != inputs.len() {
            bail!(
                "embedding server returned {} results for {} inputs",
                output.len(),
                inputs.len()
            );
        }
        if output
            .iter()
            .any(|v| v.is_empty() || v.iter().any(|n| !n.is_finite()))
        {
            bail!("embedding contained NaN/Inf or was empty");
        }
        Ok(output)
    }
}

impl Drop for Embedder {
    fn drop(&mut self) {
        if let Self::Python {
            child,
            stderr_reader,
            ..
        } = self
        {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(reader) = stderr_reader.take() {
                let _ = reader.join();
            }
        }
    }
}

fn parse_vec(v: &Value) -> Result<Vec<f32>> {
    v.as_array()
        .context("embedding must be an array")?
        .iter()
        .map(|v| {
            v.as_f64()
                .map(|x| x as f32)
                .context("invalid embedding coordinate")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_bridge_retains_stderr_and_exit_status_on_crash() {
        if Command::new(python_program())
            .arg("--version")
            .output()
            .is_err()
        {
            return; // Python is optional for native-only installations.
        }
        let path = std::env::temp_dir().join(format!(
            "weft-bridge-test-{}-{}.py",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &path,
            "import sys\nsys.stdin.readline()\nprint('media-decode-test-error', file=sys.stderr, flush=True)\nsys.exit(23)\n",
        )
        .unwrap();
        let result = (|| -> Result<String> {
            let mut bridge = Embedder::new(
                "python",
                "",
                "mock",
                Path::new(""),
                "cpu",
                Some(&path),
                false,
            )?;
            let error = bridge
                .embed(
                    &[Input {
                        kind: "image",
                        value: "fake.png".into(),
                    }],
                    "document",
                )
                .unwrap_err();
            Ok(error.to_string())
        })();
        let _ = std::fs::remove_file(path);
        let message = result.unwrap();
        assert!(message.contains("media-decode-test-error"), "{message}");
        assert!(message.contains("23"), "{message}");
    }
}
