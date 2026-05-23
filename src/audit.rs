//! Append-only JSON Lines log for state-file mutations the bridge performs
//! silently. Today it records mappings cleared during SessionStart wipes
//! (`source=startup|clear`) so a user investigating "where did my task go?"
//! can trace every disappearance to a specific event. Each line is a
//! self-contained JSON object; readers can `jq` or `tail -f`. Timestamps
//! are Unix-epoch seconds (decode with `date -r <n>` or `strftime`) to
//! avoid pulling in a date-formatting dependency.

use anyhow::{Context, Result};
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One row in the cleared-mappings log. Captured per task_id at the moment
/// resume drops it. `reason` is the SessionStart `source` value
/// (`startup` / `clear`) so future causes can be added without breaking
/// existing readers.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ClearedEntry {
    pub epoch_secs: u64,
    pub reason: String,
    pub task_id: String,
    pub plan_path: String,
}

/// Return the audit log path that sits next to the state file. The state
/// file lives at `<dir>/.claude/plan-bridge-state.json`; the audit log
/// shares the same directory.
pub fn cleared_log_path_for(state_path: &Path) -> PathBuf {
    let dir = state_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join("plan-bridge-cleared.jsonl")
}

/// Append one line per entry to the cleared-mappings log. Creates the file
/// if missing; never truncates. Failures are surfaced via `Result` so the
/// caller can decide whether to swallow them (resume's wipe should keep
/// going even if the audit write fails — losing the log is worse than
/// rejecting a session restart).
pub fn append_cleared(state_path: &Path, entries: &[ClearedEntry]) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let log_path = cleared_log_path_for(state_path);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    for e in entries {
        let line = serde_json::to_string(e).context("serialize cleared entry")?;
        writeln!(file, "{line}").with_context(|| format!("write {}", log_path.display()))?;
    }
    Ok(())
}

/// Convenience constructor — stamps `epoch_secs` from the current wall
/// clock so callers don't have to thread a clock through.
pub fn entry_now(reason: &str, task_id: &str, plan_path: &str) -> ClearedEntry {
    let epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    ClearedEntry {
        epoch_secs,
        reason: reason.to_string(),
        task_id: task_id.to_string(),
        plan_path: plan_path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch_dir() -> PathBuf {
        crate::test_utils::scratch_dir("audit")
    }

    #[test]
    fn log_path_sits_next_to_state_file() {
        let p = cleared_log_path_for(Path::new("/x/.claude/plan-bridge-state.json"));
        assert_eq!(p, Path::new("/x/.claude/plan-bridge-cleared.jsonl"));
    }

    #[test]
    fn append_creates_file_and_writes_one_line_per_entry() {
        let dir = scratch_dir();
        let state_path = dir.join("plan-bridge-state.json");
        let entries = vec![
            ClearedEntry {
                epoch_secs: 100,
                reason: "startup".to_string(),
                task_id: "t-1".to_string(),
                plan_path: "1.1".to_string(),
            },
            ClearedEntry {
                epoch_secs: 100,
                reason: "startup".to_string(),
                task_id: "t-2".to_string(),
                plan_path: "1.2".to_string(),
            },
        ];
        append_cleared(&state_path, &entries).unwrap();

        let log = std::fs::read_to_string(cleared_log_path_for(&state_path)).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["task_id"], "t-1");
        assert_eq!(first["plan_path"], "1.1");
        assert_eq!(first["reason"], "startup");
        assert_eq!(first["epoch_secs"], 100);
    }

    #[test]
    fn append_is_additive_across_calls() {
        let dir = scratch_dir();
        let state_path = dir.join("plan-bridge-state.json");
        append_cleared(
            &state_path,
            &[ClearedEntry {
                epoch_secs: 100,
                reason: "startup".to_string(),
                task_id: "t-1".to_string(),
                plan_path: "1.1".to_string(),
            }],
        )
        .unwrap();
        append_cleared(
            &state_path,
            &[ClearedEntry {
                epoch_secs: 200,
                reason: "clear".to_string(),
                task_id: "t-2".to_string(),
                plan_path: "2.0".to_string(),
            }],
        )
        .unwrap();

        let log = std::fs::read_to_string(cleared_log_path_for(&state_path)).unwrap();
        assert_eq!(log.lines().count(), 2);
        assert!(log.contains("\"t-1\""));
        assert!(log.contains("\"t-2\""));
    }

    #[test]
    fn empty_entries_is_no_op() {
        let dir = scratch_dir();
        let state_path = dir.join("plan-bridge-state.json");
        append_cleared(&state_path, &[]).unwrap();
        assert!(!cleared_log_path_for(&state_path).exists());
    }

    #[test]
    fn entry_now_stamps_recent_epoch() {
        // entry_now should produce a timestamp within a few seconds of now.
        let e = entry_now("startup", "t-1", "1.1");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(
            e.epoch_secs <= now && now - e.epoch_secs < 5,
            "entry_now stamp drifted: entry={} now={}",
            e.epoch_secs,
            now
        );
    }
}
