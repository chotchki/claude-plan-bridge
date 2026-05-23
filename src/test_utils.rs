//! Phase 41.2: shared test-scaffolding helpers, consolidated from the
//! 13-copy spread across module test blocks and the 3 integration test
//! files. `pub` (not `#[cfg(test)]`) so integration tests under
//! `tests/` can use them via `plan_bridge::test_utils::*`; marked
//! `#[doc(hidden)]` to keep the public API surface clean.
//!
//! The "most paranoid" uniqueness recipe wins: process id + wall-clock
//! nanos + per-process atomic counter. Avoids any test collision under
//! `cargo test` parallelism (16 threads default), repeated runs in the
//! same wall-clock second, AND a cargo-watch loop firing before the OS
//! reclaims the previous run's temp dir.

#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[doc(hidden)]
pub fn scratch_dir(prefix: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "plan-bridge-{prefix}-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        N.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| {
        panic!("create scratch dir {}: {e}", dir.display());
    });
    dir
}

/// Write a `PLAN.md` file under `dir` with `contents`, returning the path.
#[doc(hidden)]
pub fn write_plan(dir: &Path, contents: &str) -> PathBuf {
    let p = dir.join("PLAN.md");
    std::fs::write(&p, contents).unwrap_or_else(|e| {
        panic!("write {}: {e}", p.display());
    });
    p
}
