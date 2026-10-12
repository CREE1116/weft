use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const DEFAULT_SOCKET_PATH: &str = "/tmp/weft.sock";

/// Small process-local, exact-text LRU. The daemon is already scoped by
/// model, index and device; entries expire when that daemon exits.
pub struct EmbeddingCache {
    entries: VecDeque<(String, Vec<f32>)>,
    capacity: usize,
}

impl EmbeddingCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn embed<F>(&mut self, text: &str, embed_fn: &mut F) -> Result<Vec<f32>>
    where
        F: FnMut(&str) -> Result<Vec<f32>>,
    {
        if let Some(idx) = self.entries.iter().position(|(key, _)| key == text) {
            let entry = self.entries.remove(idx).expect("existing LRU entry");
            let vector = entry.1.clone();
            self.entries.push_back(entry);
            return Ok(vector);
        }
        let vector = embed_fn(text)?;
        // Bound both the entry count and the size of individual query keys.
        if self.capacity > 0 && text.len() <= 4096 {
            if self.entries.len() == self.capacity {
                self.entries.pop_front();
            }
            self.entries.push_back((text.to_owned(), vector.clone()));
        }
        Ok(vector)
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum Request {
    Ping,
    Embed { text: String },
    Stop,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum Response {
    Pong,
    /// Daemon is still loading the model; client should wait and retry.
    Loading,
    EmbedResult {
        vector: Vec<f32>,
    },
    Error {
        message: String,
    },
}

pub struct DaemonClient;

impl DaemonClient {
    /// Try connecting to an active daemon and request text embedding.
    /// If the daemon responds with Loading, waits up to `wait` for it to become ready.
    pub fn try_embed(socket_path: &Path, text: &str) -> Option<Result<Vec<f32>>> {
        Self::try_embed_wait(socket_path, text, Duration::from_secs(15))
    }

    pub fn try_embed_wait(
        socket_path: &Path,
        text: &str,
        wait: Duration,
    ) -> Option<Result<Vec<f32>>> {
        let deadline = Instant::now() + wait;
        loop {
            match Self::try_embed_once(socket_path, text) {
                // Daemon not yet reachable — socket file may not exist yet
                None => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                // Daemon responded Loading — model still warming up
                Some(Err(ref e)) if e.to_string() == "__loading__" => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                other => return other,
            }
        }
    }

    fn try_embed_once(socket_path: &Path, text: &str) -> Option<Result<Vec<f32>>> {
        let mut stream = UnixStream::connect(socket_path).ok()?;
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .ok()?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .ok()?;

        let req = Request::Embed {
            text: text.to_string(),
        };
        let mut req_bytes = serde_json::to_vec(&req).ok()?;
        req_bytes.push(b'\n');
        if stream.write_all(&req_bytes).is_err() {
            return None;
        }

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return None;
        }

        match serde_json::from_str::<Response>(&line) {
            Ok(Response::EmbedResult { vector }) => Some(Ok(vector)),
            Ok(Response::Loading) => Some(Err(anyhow::anyhow!("__loading__"))),
            Ok(Response::Error { message }) => Some(Err(anyhow::anyhow!(message))),
            _ => None,
        }
    }

    /// Check if the daemon is alive and ready (not loading).
    pub fn is_alive(socket_path: &Path) -> bool {
        if let Ok(mut stream) = UnixStream::connect(socket_path) {
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .ok();
            let req = Request::Ping;
            if let Ok(mut req_bytes) = serde_json::to_vec(&req) {
                req_bytes.push(b'\n');
                if stream.write_all(&req_bytes).is_ok() {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_ok() {
                        return matches!(
                            serde_json::from_str::<Response>(&line),
                            Ok(Response::Pong)
                        );
                    }
                }
            }
        }
        false
    }
}

pub struct DaemonServer {
    socket_path: PathBuf,
    idle_timeout: Duration,
}

impl DaemonServer {
    pub fn new(socket_path: PathBuf, idle_timeout: Duration) -> Self {
        Self {
            socket_path,
            idle_timeout,
        }
    }

    /// Phase 1: bind the socket immediately (before model loading).
    /// Returns the listener so the caller can load the model, then call `serve`.
    /// While the listener is open but `serve` hasn't been called yet, incoming
    /// Embed/Ping requests are held in the OS accept queue.
    pub fn bind(&self) -> Result<UnixListener> {
        if self.socket_path.exists() {
            if UnixStream::connect(&self.socket_path).is_ok() {
                anyhow::bail!(
                    "weft daemon already listening at {}",
                    self.socket_path.display()
                );
            }
            let _ = std::fs::remove_file(&self.socket_path);
        }
        let listener = UnixListener::bind(&self.socket_path)
            .with_context(|| format!("bind unix socket {}", self.socket_path.display()))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&self.socket_path, std::fs::Permissions::from_mode(0o600))?;
        eprintln!(
            "weft daemon: socket bound at {} (loading model...)",
            self.socket_path.display()
        );
        Ok(listener)
    }

    /// Phase 2: serve requests using the already-bound listener.
    /// Call this after the embedder is ready.
    /// Accepts connections in non-blocking mode, replies Loading to stale
    /// connections that arrived during model warm-up, then enters normal loop.
    pub fn serve<F, I>(&self, listener: UnixListener, mut embed_fn: F, mut on_idle: I) -> Result<()>
    where
        F: FnMut(&str) -> Result<Vec<f32>>,
        I: FnMut() -> Result<()>,
    {
        listener
            .set_nonblocking(true)
            .context("set socket listener non-blocking")?;

        eprintln!(
            "weft daemon: model ready. serving on {} (idle_timeout: {:?})",
            self.socket_path.display(),
            self.idle_timeout
        );

        let mut last_activity = Instant::now();
        // 128 * 768 * f32 = 384 KiB of vector data, plus short text keys.
        let mut embed_cache = EmbeddingCache::new(128);

        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    last_activity = Instant::now();
                    let _ = stream.set_nonblocking(false);
                    let mut reader = BufReader::new(match stream.try_clone() {
                        Ok(s) => s,
                        Err(_) => continue,
                    });
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_ok() {
                        if let Ok(req) = serde_json::from_str::<Request>(&line) {
                            match req {
                                Request::Ping => {
                                    let resp = Response::Pong;
                                    let mut out = serde_json::to_vec(&resp).unwrap_or_default();
                                    out.push(b'\n');
                                    let _ = stream.write_all(&out);
                                }
                                Request::Embed { text } => {
                                    let resp = match embed_cache.embed(&text, &mut embed_fn) {
                                        Ok(vec) => Response::EmbedResult { vector: vec },
                                        Err(e) => Response::Error {
                                            message: e.to_string(),
                                        },
                                    };
                                    let mut out = serde_json::to_vec(&resp).unwrap_or_default();
                                    out.push(b'\n');
                                    let _ = stream.write_all(&out);
                                }
                                Request::Stop => {
                                    eprintln!("weft daemon: received Stop request, exiting");
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if last_activity.elapsed() >= self.idle_timeout {
                        eprintln!(
                            "weft daemon: idle for {:?}. Running post-session indexing before exit...",
                            self.idle_timeout
                        );
                        if let Err(e) = on_idle() {
                            eprintln!("warning: idle indexing error: {e:#}");
                        }
                        eprintln!("weft daemon: finished indexing. Graceful auto-exit complete.");
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    eprintln!("daemon accept error: {e}");
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }

        let _ = std::fs::remove_file(&self.socket_path);
        Ok(())
    }

    /// Convenience wrapper: bind + serve in one call (original behaviour).
    pub fn run<F, I>(&self, embed_fn: F, on_idle: I) -> Result<()>
    where
        F: FnMut(&str) -> Result<Vec<f32>>,
        I: FnMut() -> Result<()>,
    {
        let listener = self.bind()?;
        self.serve(listener, embed_fn, on_idle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn exact_lru_reuses_results_evicts_old_entries_and_never_caches_failures() {
        let mut cache = EmbeddingCache::new(2);
        let calls = Cell::new(0);
        let mut embed = |text: &str| -> Result<Vec<f32>> {
            calls.set(calls.get() + 1);
            if text == "error" {
                anyhow::bail!("failed embedding");
            }
            Ok(vec![text.len() as f32])
        };

        assert_eq!(cache.embed("alpha", &mut embed).unwrap(), vec![5.0]);
        assert_eq!(cache.embed("beta", &mut embed).unwrap(), vec![4.0]);
        assert_eq!(cache.embed("alpha", &mut embed).unwrap(), vec![5.0]);
        assert_eq!(calls.get(), 2);
        assert_eq!(cache.embed("gamma", &mut embed).unwrap(), vec![5.0]);
        assert_eq!(cache.embed("beta", &mut embed).unwrap(), vec![4.0]);
        assert_eq!(calls.get(), 4);
        assert!(cache.embed("error", &mut embed).is_err());
        assert!(cache.embed("error", &mut embed).is_err());
        assert_eq!(calls.get(), 6);
    }
}
