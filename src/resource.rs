//! Cooperative indexing controls; never interrupts a file replacement transaction.
use crate::{embed::Embedder, store::Store};
use anyhow::Result;
use std::{
    fs::{self, File, OpenOptions},
    io::ErrorKind,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const SEARCH_TTL: Duration = Duration::from_secs(30);
const SEARCH_REFRESH: Duration = Duration::from_secs(5);
const SEARCH_UNLOAD_AFTER: Duration = Duration::from_secs(1);
static LEASE_ID: AtomicU64 = AtomicU64::new(0);

/// Independent filesystem lease: read-only searches never update SQLite.
/// Keep this guard alive across the entire interactive operation.
pub struct SearchActivity {
    lease: Option<(PathBuf, mpsc::Sender<()>, JoinHandle<()>)>,
}

impl Drop for SearchActivity {
    fn drop(&mut self) {
        if let Some((path, stop, worker)) = self.lease.take() {
            let _ = stop.send(());
            let _ = worker.join();
            let _ = fs::remove_file(path);
        }
    }
}

fn activity_dir(store: &Store) -> Option<PathBuf> {
    if store.vec_path.as_os_str().is_empty() {
        return None;
    }
    let mut path = store.vec_path.as_os_str().to_owned();
    path.push(".search-activity");
    Some(path.into())
}

/// Refresh a unique lease while alive; crashed processes expire within 30s.
/// In-memory stores need no coordination and return a no-op guard.
pub fn begin_search(store: &Store) -> Result<SearchActivity> {
    let Some(dir) = activity_dir(store) else {
        return Ok(SearchActivity { lease: None });
    };
    fs::create_dir_all(&dir)?;
    let (path, file) = loop {
        let id = LEASE_ID.fetch_add(1, Ordering::Relaxed);
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = dir.join(format!("{}-{stamp}-{id}.lease", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => break (path, file),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };
    // A stale scan may only clean up an orphan, never a live refresher.
    if let Err(error) = fs2::FileExt::lock_exclusive(&file) {
        let _ = fs::remove_file(&path);
        return Err(error.into());
    }
    let (stop, receiver) = mpsc::channel();
    let worker = match thread::Builder::new()
        .name("search-lease".into())
        .spawn(move || {
            refresh_lease(file, receiver, SEARCH_REFRESH);
        }) {
        Ok(worker) => worker,
        Err(error) => {
            let _ = fs::remove_file(&path);
            return Err(error.into());
        }
    };
    Ok(SearchActivity {
        lease: Some((path, stop, worker)),
    })
}

fn refresh_lease(file: File, stop: mpsc::Receiver<()>, interval: Duration) {
    while matches!(
        stop.recv_timeout(interval),
        Err(mpsc::RecvTimeoutError::Timeout)
    ) {
        if file.set_modified(SystemTime::now()).is_err() {
            break;
        }
    }
}

fn search_active(store: &Store) -> Result<bool> {
    let Some(dir) = activity_dir(store) else {
        return Ok(false);
    };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "lease") {
            continue;
        }
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        // Future timestamps are stale too, so clock skew cannot block forever.
        if SystemTime::now()
            .duration_since(metadata.modified()?)
            .is_ok_and(|age| age < SEARCH_TTL)
        {
            return Ok(true);
        }
        if fs2::FileExt::try_lock_exclusive(&file).is_ok() {
            let _ = fs::remove_file(path);
        }
    }
    Ok(false)
}

fn duty_cycle(store: &Store) -> Result<u8> {
    Ok(store
        .setting("index_duty_cycle")?
        .and_then(|s| s.parse::<u8>().ok())
        .filter(|d| (1..=100).contains(d))
        .unwrap_or(25))
}

pub fn index_batch_size(store: &Store) -> Result<usize> {
    Ok(match duty_cycle(store)? {
        1..=25 => 1,
        26..=50 => 2,
        _ => 16,
    })
}

pub fn status(store: &Store, state: &str) -> Result<()> {
    if let Some(scan) = store.setting("last_scan")? {
        let mut scan: serde_json::Value = serde_json::from_str(&scan)?;
        scan["status"] = state.into();
        store.set_setting("last_scan", &scan.to_string())?;
    }
    Ok(())
}

pub fn gate(store: &Store, engine: &mut Option<Embedder>) -> Result<bool> {
    gate_pair(store, engine, &mut None)
}

/// Release both native and optional full multimodal weights when paused.
pub fn gate_pair(
    store: &Store,
    engine: &mut Option<Embedder>,
    media_engine: &mut Option<Embedder>,
) -> Result<bool> {
    let mut paused = false;
    let mut search_wait = None;
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
                    *media_engine = None;
                    status(store, "paused")?;
                    paused = true;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            _ => {
                // Yield only between completed inference/transaction work.
                // Keep warm weights for brief searches, then release duplicate
                // indexing models during a sustained interactive session.
                // Activity is advisory: filesystem failures must not stop work.
                if search_active(store).unwrap_or(false) {
                    let started = search_wait.get_or_insert_with(Instant::now);
                    if started.elapsed() >= SEARCH_UNLOAD_AFTER {
                        *engine = None;
                        *media_engine = None;
                    }
                    thread::sleep(Duration::from_millis(200));
                    continue;
                }
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
    throttle_pair(store, engine, &mut None, compute)
}

pub fn throttle_pair(
    store: &Store,
    engine: &mut Option<Embedder>,
    media_engine: &mut Option<Embedder>,
    compute: Duration,
) -> Result<bool> {
    let duty = duty_cycle(store)?;
    let deadline = Instant::now() + rest(compute, duty);
    loop {
        if !gate_pair(store, engine, media_engine)? {
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

    struct TestDb(PathBuf);

    impl TestDb {
        fn new() -> Self {
            let id = LEASE_ID.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("weft-resource-{}-{id}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir.join("index.sqlite"))
        }
    }

    impl Drop for TestDb {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    #[test]
    fn index_batch_policy_defaults_and_boundaries() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        assert_eq!(duty_cycle(&store).unwrap(), 25);
        assert_eq!(index_batch_size(&store).unwrap(), 1);
        for (setting, expected) in [
            ("1", 1),
            ("25", 1),
            ("26", 2),
            ("50", 2),
            ("51", 16),
            ("100", 16),
            ("0", 1),
            ("101", 1),
            ("garbage", 1),
        ] {
            store.set_setting("index_duty_cycle", setting).unwrap();
            assert_eq!(index_batch_size(&store).unwrap(), expected, "{setting}");
        }
    }

    #[test]
    fn search_activity_read_only_concurrent_release_and_memory_noop() {
        let db = TestDb::new();
        drop(Store::open(&db.0).unwrap());
        let store = Store::open_read_only(&db.0).unwrap();
        assert!(store.set_setting("search_activity", "1").is_err());
        let before = fs::read(&db.0).unwrap();
        let first = begin_search(&store).unwrap();
        let second = begin_search(&store).unwrap();
        assert!(search_active(&store).unwrap());
        drop(first);
        assert!(search_active(&store).unwrap());
        drop(second);
        assert!(!search_active(&store).unwrap());
        assert_eq!(before, fs::read(&db.0).unwrap());
        assert_eq!(
            fs::read_dir(activity_dir(&store).unwrap()).unwrap().count(),
            0
        );

        let memory = Store::open(Path::new(":memory:")).unwrap();
        let guard = begin_search(&memory).unwrap();
        assert!(guard.lease.is_none());
        assert!(!search_active(&memory).unwrap());
    }

    #[test]
    fn search_activity_stale_crash_and_future_markers_do_not_block() {
        let db = TestDb::new();
        let store = Store::open(&db.0).unwrap();
        let dir = activity_dir(&store).unwrap();
        fs::create_dir_all(&dir).unwrap();
        for (name, timestamp) in [
            (
                "crashed.lease",
                SystemTime::now() - SEARCH_TTL - Duration::from_secs(1),
            ),
            (
                "future.lease",
                SystemTime::now() + Duration::from_secs(3600),
            ),
        ] {
            let file = File::create(dir.join(name)).unwrap();
            file.set_modified(timestamp).unwrap();
        }
        assert!(!search_active(&store).unwrap());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        assert!(gate(&store, &mut None).unwrap());
    }

    #[test]
    fn search_activity_refreshes_and_stale_scan_preserves_live_lease() {
        let db = TestDb::new();
        let store = Store::open(&db.0).unwrap();
        let guard = begin_search(&store).unwrap();
        let path = &guard.lease.as_ref().unwrap().0;
        let file = OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - SEARCH_TTL - Duration::from_secs(1))
            .unwrap();
        assert!(!search_active(&store).unwrap());
        assert!(
            path.exists(),
            "a stale scan must not remove a live refresher"
        );
        let (stop, receiver) = mpsc::channel();
        let worker =
            thread::spawn(move || refresh_lease(file, receiver, Duration::from_millis(10)));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !search_active(&store).unwrap() {
            assert!(Instant::now() < deadline, "lease refresh timed out");
            thread::sleep(Duration::from_millis(5));
        }
        stop.send(()).unwrap();
        worker.join().unwrap();
        drop(guard);
        assert!(!search_active(&store).unwrap());
    }

    #[test]
    fn search_activity_gates_indexing_until_all_guards_drop() {
        let db = TestDb::new();
        let store = Store::open(&db.0).unwrap();
        let first = begin_search(&store).unwrap();
        let second = begin_search(&store).unwrap();
        let path = db.0.clone();
        let (done, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let store = Store::open(&path).unwrap();
            let mut engine = Some(Embedder::Http {
                endpoint: String::new(),
                model: String::new(),
            });
            let mut media = Some(Embedder::Http {
                endpoint: String::new(),
                model: String::new(),
            });
            let resume = gate_pair(&store, &mut engine, &mut media).unwrap();
            done.send((resume, engine.is_some(), media.is_some()))
                .unwrap();
        });
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(first);
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(second);
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            (true, true, true)
        );
        worker.join().unwrap();
    }

    #[test]
    fn search_activity_long_wait_unloads_both_engines() {
        let db = TestDb::new();
        let store = Store::open(&db.0).unwrap();
        let guard = begin_search(&store).unwrap();
        let path = db.0.clone();
        let (started, ready) = mpsc::channel();
        let worker = thread::spawn(move || {
            let store = Store::open(&path).unwrap();
            let mut engine = Some(Embedder::Http {
                endpoint: String::new(),
                model: String::new(),
            });
            let mut media = Some(Embedder::Http {
                endpoint: String::new(),
                model: String::new(),
            });
            started.send(()).unwrap();
            let resume = gate_pair(&store, &mut engine, &mut media).unwrap();
            (resume, engine.is_none(), media.is_none())
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        thread::sleep(SEARCH_UNLOAD_AFTER + Duration::from_millis(400));
        drop(guard);
        assert_eq!(worker.join().unwrap(), (true, true, true));
    }

    #[test]
    fn search_activity_io_errors_are_nonfatal_and_stop_has_priority() {
        let db = TestDb::new();
        let store = Store::open(&db.0).unwrap();
        let dir = activity_dir(&store).unwrap();
        File::create(&dir).unwrap();
        assert!(search_active(&store).is_err());
        assert!(gate(&store, &mut None).unwrap());
        fs::remove_file(dir).unwrap();
        let _guard = begin_search(&store).unwrap();
        store.set_setting("index_action", "stop").unwrap();
        assert!(!gate(&store, &mut None).unwrap());
    }
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
