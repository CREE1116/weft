use crate::native::NativeModel;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn python_program() -> String {
    std::env::var("WEFT_PYTHON")
        .or_else(|_| std::env::var("MYVEC_PYTHON"))
        .unwrap_or_else(|_| "python3".to_string())
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
    },
}

impl Embedder {
    pub fn new(
        backend: &str,
        endpoint: &str,
        model: &str,
        model_file: &Path,
        device: &str,
        bridge: &Path,
        text_only: bool,
    ) -> Result<Self> {
        match backend {
            "native" => Ok(Self::Native(NativeModel::load(model_file, device)?)),
            "http" => Ok(Self::Http {
                endpoint: endpoint.to_string(),
                model: model.to_string(),
            }),
            "python" => {
                let mut child = Command::new(python_program())
                    .arg("-u")
                    .arg(bridge)
                    .arg("--model")
                    .arg(model)
                    .args(if text_only {
                        vec!["--text-only"]
                    } else {
                        vec![]
                    })
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .with_context(|| format!("starting Python bridge {}", bridge.display()))?;
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
                Ok(Self::Python {
                    child,
                    stdin,
                    stdout,
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
            Self::Python { stdin, stdout, .. } => {
                let arr: Vec<Value> = inputs
                    .iter()
                    .map(|x| json!({"kind":x.kind,"value":x.value}))
                    .collect();
                serde_json::to_writer(&mut *stdin, &json!({"inputs":arr,"role":role}))?;
                writeln!(stdin)?;
                stdin.flush()?;
                let mut line = String::new();
                if stdout.read_line(&mut line)? == 0 {
                    bail!("Python embedding process exited; check its stderr");
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
        if let Self::Python { child, .. } = self {
            let _ = child.kill();
            let _ = child.wait();
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
