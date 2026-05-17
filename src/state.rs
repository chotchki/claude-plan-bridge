use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 1;

/// Per-project bridge state. Maps Claude's `taskId` to a dotted `plan_path`
/// so writeback can locate the right PLAN.md line from a `TaskUpdate` payload
/// alone. `pending_rehydration` carries the set of plan_paths the SessionStart
/// hook just emitted to Claude for re-creation — reconcile consults it to
/// suppress LeafAdded drift in the prompt-window between rehydration emit and
/// the matching TaskCreates landing. Writeback evicts paths from the set as
/// each TaskCreate completes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    pub version: u32,
    pub mappings: BTreeMap<String, Mapping>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub pending_rehydration: BTreeSet<String>,
    /// Total plan_paths announced in the last SessionStart rehydration prompt.
    /// Lets writeback render "rehydration complete: N/N" when the matching
    /// TaskCreates have drained `pending_rehydration` to empty. Reset to 0
    /// once the confirmation fires.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rehydration_announced: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            mappings: BTreeMap::new(),
            pending_rehydration: BTreeSet::new(),
            rehydration_announced: 0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mapping {
    pub plan_path: String,
    /// Last title we wrote into PLAN.md for this task. Used by reconcile to
    /// detect external title edits.
    #[serde(default)]
    pub last_synced_title: String,
    /// Last checkbox state we wrote into PLAN.md for this task. Used by
    /// reconcile to detect external `[ ]` / `[x]` / `[-]` flips.
    #[serde(default)]
    pub last_synced_state: crate::ast::NodeState,
    /// Last annotations (text-form, one entry per annotation) under this leaf.
    /// Used by reconcile to surface user-added notes between turns.
    #[serde(default)]
    pub last_synced_annotations: Vec<String>,
    /// `session_id` (from the HookPayload) of the session that created this
    /// mapping. Used by writeback_create to refuse same-session duplicate
    /// TaskCreates for the same plan_path while still allowing cross-session
    /// re-mapping after restart. Empty string when unknown (pre-26.6 state
    /// files load as "").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_in_session: String,
}

impl State {
    /// Load state from disk, or return `Default::default()` if the file
    /// doesn't exist yet. Refuses to load a file with a version newer than
    /// this binary supports, so a forward-compat tool doesn't get downgraded
    /// silently.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes =
            std::fs::read(path).with_context(|| format!("read state file {}", path.display()))?;
        let state: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse state file {}", path.display()))?;
        if state.version > CURRENT_VERSION {
            bail!(
                "state file {} has version {}, but this binary supports up to {}",
                path.display(),
                state.version,
                CURRENT_VERSION
            );
        }
        Ok(state)
    }

    /// Save state via tmp-file + rename so a crash mid-write can't leave a
    /// half-written JSON blob on disk.
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .with_context(|| format!("state path {} has no parent", path.display()))?;
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        let tmp = tmp_path(path);
        let json = serde_json::to_vec_pretty(self).context("serialize state")?;
        std::fs::write(&tmp, json).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
        Ok(())
    }

    pub fn insert(&mut self, task_id: impl Into<String>, plan_path: impl Into<String>) {
        self.mappings.insert(
            task_id.into(),
            Mapping {
                plan_path: plan_path.into(),
                ..Default::default()
            },
        );
    }

    /// Replace the full mapping for a task (used when writeback wants to set
    /// `last_synced_*` fields alongside the path).
    pub fn record(&mut self, task_id: impl Into<String>, mapping: Mapping) {
        self.mappings.insert(task_id.into(), mapping);
    }

    pub fn remove(&mut self, task_id: &str) -> Option<Mapping> {
        self.mappings.remove(task_id)
    }

    pub fn plan_path(&self, task_id: &str) -> Option<&str> {
        self.mappings.get(task_id).map(|m| m.plan_path.as_str())
    }
}

/// Default `.claude/plan-bridge-state.json` next to the plan file.
pub fn default_state_path_for(plan_path: &Path) -> PathBuf {
    plan_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".claude")
        .join("plan-bridge-state.json")
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn scratch_dir() -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "plan-bridge-state-test-{}-{}",
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
    fn load_missing_file_returns_default() {
        let dir = scratch_dir();
        let s = State::load(&dir.join("nope.json")).unwrap();
        assert_eq!(s, State::default());
    }

    #[test]
    fn roundtrip_save_and_load() {
        let dir = scratch_dir();
        let path = dir.join("state.json");
        let mut s = State::default();
        s.insert("task-1", "1.2.3");
        s.insert("task-2", "1.2.4");
        s.save(&path).unwrap();
        let loaded = State::load(&path).unwrap();
        assert_eq!(s, loaded);
        assert_eq!(loaded.plan_path("task-1"), Some("1.2.3"));
    }

    #[test]
    fn save_creates_parent_directory() {
        let dir = scratch_dir();
        let path = dir.join("nested/deeper/state.json");
        State::default().save(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn save_is_atomic_via_tmp_rename() {
        let dir = scratch_dir();
        let path = dir.join("state.json");
        let mut s = State::default();
        s.insert("a", "1.0");
        s.save(&path).unwrap();
        // The tmp file should not linger after a successful save.
        let tmp = tmp_path(&path);
        assert!(!tmp.exists(), "tmp file leaked: {}", tmp.display());
    }

    #[test]
    fn refuses_future_version() {
        let dir = scratch_dir();
        let path = dir.join("state.json");
        let future = r#"{"version":99,"mappings":{}}"#;
        std::fs::write(&path, future).unwrap();
        let err = State::load(&path).unwrap_err();
        assert!(err.to_string().contains("version 99"), "got: {err}");
    }

    #[test]
    fn remove_and_lookup() {
        let mut s = State::default();
        s.insert("t1", "1.0");
        assert_eq!(s.plan_path("t1"), Some("1.0"));
        let m = s.remove("t1");
        assert!(m.is_some());
        assert_eq!(s.plan_path("t1"), None);
    }

    #[test]
    fn default_state_path_lives_next_to_plan() {
        let plan = Path::new("/project/PLAN.md");
        let state = default_state_path_for(plan);
        assert_eq!(state, Path::new("/project/.claude/plan-bridge-state.json"));
    }
}
