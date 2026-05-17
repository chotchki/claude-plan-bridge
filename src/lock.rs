//! Cross-process advisory lock used to serialize writebacks against the same
//! PLAN.md / state file pair. The lock target is a sidecar file in the
//! state-file directory (e.g. `.claude/plan-bridge-state.json.lock`) — kept
//! separate from PLAN.md so other tooling reading the markdown isn't affected.
//!
//! Loud-failure contract (per Phase 8): if the lock can't be acquired within
//! the timeout, we return Err. The CLI surfaces that as a `decision: "block"`
//! hook output — never silent on busy, never stale-data success.

use anyhow::{Context, Result};
use fs4::fs_std::FileExt;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Run `f` while holding an exclusive advisory lock on the sidecar lock file
/// for `state_path`. The lock file is created lazily if missing. Lock is
/// released when this function returns (whether `f` succeeded or not).
pub fn with_state_lock<F, T>(state_path: &Path, timeout: Duration, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let lock_path = lock_path_for(state_path);
    if let Some(parent) = lock_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create lock dir {}", parent.display()))?;
        }
    }
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open lock {}", lock_path.display()))?;
    acquire_with_timeout(&lock_file, timeout)
        .with_context(|| format!("acquire lock {}", lock_path.display()))?;
    let result = f();
    // fs4 releases the advisory lock when the File handle is dropped.
    drop(lock_file);
    result
}

fn lock_path_for(state_path: &Path) -> PathBuf {
    let mut p = state_path.as_os_str().to_owned();
    p.push(".lock");
    PathBuf::from(p)
}

fn acquire_with_timeout(f: &std::fs::File, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        match f.try_lock_exclusive() {
            Ok(true) => return Ok(()),
            Ok(false) => {
                if start.elapsed() >= timeout {
                    anyhow::bail!(
                        "plan-bridge: writeback lock busy after {:?} — another writeback is in-flight",
                        timeout
                    );
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(e) => {
                return Err(anyhow::Error::from(e).context("try_lock_exclusive"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "plan-bridge-lock-{}-{}",
            std::process::id(),
            uniq()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn uniq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn lock_acquired_and_released_in_sequence() {
        let dir = scratch_dir();
        let state = dir.join("state.json");
        with_state_lock(&state, DEFAULT_TIMEOUT, || Ok(1u32)).unwrap();
        // Second call must succeed too — the lock from the first was released.
        let v = with_state_lock(&state, DEFAULT_TIMEOUT, || Ok(42u32)).unwrap();
        assert_eq!(v, 42);
    }

    #[test]
    fn forced_busy_lock_surfaces_loud_error() {
        // Regression for Phase 8.0 loud-failure contract: if another holder has
        // the lock, with_state_lock must Err — never silently succeed.
        let dir = scratch_dir();
        let state = dir.join("state.json");
        let lock_path = lock_path_for(&state);
        // Manually open and acquire to simulate a different process holding it.
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let holder = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        assert!(holder.try_lock_exclusive().unwrap(), "preconditions: holder grabs lock");

        // Short timeout so the test is fast.
        let err = with_state_lock(&state, Duration::from_millis(150), || Ok(()))
            .expect_err("expected lock-busy error");
        let msg = format!("{err:#}");
        assert!(msg.contains("busy"), "got: {msg}");

        // Release and confirm next acquire succeeds.
        FileExt::unlock(&holder).unwrap();
        drop(holder);
        with_state_lock(&state, DEFAULT_TIMEOUT, || Ok(())).unwrap();
    }

    #[test]
    fn concurrent_callers_serialize_through_the_lock() {
        // Spawn N threads that each call with_state_lock and increment a
        // shared counter inside the critical section. Without serialization,
        // a non-atomic read-modify-write on the counter would race; with the
        // lock, the final value must equal N.
        let dir = scratch_dir();
        let state = dir.join("state.json");
        let n = 16;
        let barrier = Arc::new(Barrier::new(n));
        let counter = Arc::new(std::sync::Mutex::new(0u32));
        // Mutex above is just to safely read the final value — the test of the
        // *file lock* is that the closures don't overlap. We assert overlap-free
        // by tracking concurrent entries via an atomic that should stay <= 1
        // throughout the critical section.
        let concurrent = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let max_seen = Arc::new(std::sync::atomic::AtomicU32::new(0));

        let mut handles = Vec::with_capacity(n);
        for _ in 0..n {
            let state = state.clone();
            let barrier = Arc::clone(&barrier);
            let counter = Arc::clone(&counter);
            let concurrent = Arc::clone(&concurrent);
            let max_seen = Arc::clone(&max_seen);
            handles.push(thread::spawn(move || {
                barrier.wait();
                with_state_lock(&state, Duration::from_secs(10), || {
                    let inside = concurrent.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    max_seen.fetch_max(inside, std::sync::atomic::Ordering::SeqCst);
                    // Small sleep to widen the window for a race to surface.
                    thread::sleep(Duration::from_millis(5));
                    *counter.lock().unwrap() += 1;
                    concurrent.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                })
                .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(*counter.lock().unwrap(), n as u32, "every caller ran exactly once");
        assert_eq!(
            max_seen.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "lock failed to serialize — saw {} concurrent entries",
            max_seen.load(std::sync::atomic::Ordering::SeqCst)
        );
    }
}
