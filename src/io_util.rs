//! Phase 41.4: small filesystem helpers shared by the modules that write
//! PLAN.md, PLAN_ARCHIVE.md, and the state file. Before 41.4, both
//! `archive.rs::atomic_write` and `state.rs::save` open-coded the same
//! tmp-file + rename dance with subtly different error messages.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Atomically write `bytes` to `path`: create parent dirs, write to a
/// sibling `<path>.tmp`, then rename onto `path`. A crash between the
/// write and the rename leaves either the previous file intact OR a
/// `.tmp` file the next save will overwrite — never a half-written
/// target. Same contract the bridge has relied on since Phase 8.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("no parent for {}", path.display()))?;
    if !parent.as_os_str().is_empty() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = tmp_path(path);
    std::fs::write(&tmp, bytes).with_context(|| format!("write tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// `<path>.tmp` — the sibling file [atomic_write] writes through before
/// renaming onto `path`. Exposed so tests can assert the tmp file does
/// not linger after a successful save.
pub fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}
