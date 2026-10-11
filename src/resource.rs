//! Cooperative indexing controls; never interrupts a file replacement transaction.
use crate::{embed::Embedder, store::Store};
use anyhow::Result;
use std::time::{Duration, Instant};

pub fn status(store: &Store, state: &str) -> Result<()> {
    if let Some(scan) = store.setting("last_scan")? {
        let mut scan: serde_json::Value = serde_json::from_str(&scan)?;
        scan["status"] = state.into();
        store.set_setting("last_scan", &scan.to_string())?;
    }
    Ok(())
}

pub fn gate(store: &Store, engine: &mut Option<Embedder>) -> Result<bool> {
    let mut paused = false;
    loop {
        match store.setting("index_action")?.as_deref() {
            Some("stop") => {
                status(store, "stopped")?;
                return Ok(false);
            }
            Some("pause") => {
                if !paused {
                    // No GPU kernels run after the current inference completes.
                    // Releasing weights also avoids retaining RAM while paused.
                    *engine = None;
                    status(store, "paused")?;
                    paused = true;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            _ => {
                if paused {
                    status(store, "running")?;
                }
                return Ok(true);
            }
        }
    }
}

pub fn rest(compute: Duration, duty: u8) -> Duration {
    compute.mul_f64((100.0 / f64::from(duty)) - 1.0)
}

pub fn throttle(store: &Store, engine: &mut Option<Embedder>, compute: Duration) -> Result<bool> {
    let duty = store
        .setting("index_duty_cycle")?
        .and_then(|s| s.parse::<u8>().ok())
        .filter(|d| (1..=100).contains(d))
        .unwrap_or(100);
    let deadline = Instant::now() + rest(compute, duty);
    loop {
        if !gate(store, engine)? {
            return Ok(false);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(true);
        }
        std::thread::sleep(remaining.min(Duration::from_millis(200)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    #[test]
    fn duty_cycle_budget_and_stop_status() {
        assert_eq!(
            rest(Duration::from_millis(20), 25),
            Duration::from_millis(60)
        );
        assert_eq!(rest(Duration::from_millis(20), 100), Duration::ZERO);
        let store = Store::open(Path::new(":memory:")).unwrap();
        store
            .set_setting("last_scan", r#"{"status":"running","indexed":5}"#)
            .unwrap();
        store.set_setting("index_action", "stop").unwrap();
        assert!(!gate(&store, &mut None).unwrap());
        let scan: serde_json::Value =
            serde_json::from_str(&store.setting("last_scan").unwrap().unwrap()).unwrap();
        assert_eq!(scan["status"], "stopped");
        assert_eq!(scan["indexed"], 5);
    }
}
