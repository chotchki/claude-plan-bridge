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
    /// Phase 40.1: the currently focused phase id, set by
    /// `plan_activate <PHASE>` and cleared by `plan_deactivate` (or when
    /// the active phase archives). When `Some`, resume's rehydration
    /// prompt scopes to leaves under that phase (40.3); reconcile
    /// foregrounds its drift (40.5); writeback emits a soft warning when
    /// a TaskCreate lands on a different phase (40.4). None = today's
    /// behavior — all open leaves load, no scoping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_phase: Option<String>,
    /// Phase BY.11: opt-in raw hook-payload capture. When `true`, the
    /// writeback hook appends each verbatim stdin payload to a sibling
    /// `plan-bridge-debug.jsonl` so an operator can confirm exactly what the
    /// harness forwarded (notably whether `metadata.plan_path` survived).
    /// Off by default and omitted from the serialized file when false, so
    /// existing state files and every non-opted-in project are untouched.
    /// Flip per-project with `claude-plan-bridge debug on`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub debug: bool,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            mappings: BTreeMap::new(),
            pending_rehydration: BTreeSet::new(),
            rehydration_announced: 0,
            active_phase: None,
            debug: false,
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
    /// half-written JSON blob on disk. Phase 41.4: delegates to the
    /// shared `crate::io_util::atomic_write`.
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self).context("serialize state")?;
        crate::io_util::atomic_write(path, &json)
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

    /// Set the active phase. `Some(id)` focuses subsequent resume/reconcile
    /// flows on the named phase; `None` clears focus (today's behavior).
    pub fn set_active_phase(&mut self, phase_id: Option<String>) {
        self.active_phase = phase_id;
    }

    /// Read the active phase id, if any.
    pub fn active_phase(&self) -> Option<&str> {
        self.active_phase.as_deref()
    }

    /// Phase-id check derived from a leaf's plan_path. A task with
    /// `plan_path` `"AS.1.2"` is in phase `"AS"`; `"1.1"` is in phase
    /// `"1"`. Returns `Some(true)` when the task is in the named active
    /// phase, `Some(false)` otherwise, and `None` when no active phase is
    /// set (callers treat `None` as "everything matches").
    pub fn is_in_active_phase(&self, plan_path: &str) -> Option<bool> {
        let active = self.active_phase.as_deref()?;
        Some(phase_id_of(plan_path) == active)
    }
}

/// Extract the phase id from a leaf's `plan_path` — the first
/// dot-separated segment. `"AS.1.2"` → `"AS"`, `"1.1"` → `"1"`,
/// `"AI"` → `"AI"` (already a bare phase id). Used by the activation
/// path to determine whether a task belongs to the active phase.
pub fn phase_id_of(plan_path: &str) -> &str {
    plan_path.split('.').next().unwrap_or(plan_path)
}

/// Default `.claude/plan-bridge-state.json` next to the plan file.
pub fn default_state_path_for(plan_path: &Path) -> PathBuf {
    plan_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".claude")
        .join("plan-bridge-state.json")
}

// Phase 41.4: tmp_path moved to crate::io_util. Re-exported here for the
// existing `save_is_atomic_via_tmp_rename` test which asserts the tmp
// file doesn't linger after a clean save.
#[cfg(test)]
use crate::io_util::tmp_path;

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir() -> PathBuf {
        crate::test_utils::scratch_dir("state-test")
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

    // -----------------------------------------------------------------
    // Phase 40.1: active_phase field + accessors
    // -----------------------------------------------------------------

    #[test]
    fn default_state_has_no_active_phase() {
        let s = State::default();
        assert_eq!(s.active_phase(), None);
    }

    #[test]
    fn set_active_phase_roundtrips_via_save_load() {
        let dir = scratch_dir();
        let path = dir.join("state.json");
        let mut s = State::default();
        s.set_active_phase(Some("AS".to_string()));
        s.save(&path).unwrap();
        let loaded = State::load(&path).unwrap();
        assert_eq!(loaded.active_phase(), Some("AS"));
    }

    #[test]
    fn set_active_phase_none_clears() {
        let mut s = State::default();
        s.set_active_phase(Some("AI".to_string()));
        assert_eq!(s.active_phase(), Some("AI"));
        s.set_active_phase(None);
        assert_eq!(s.active_phase(), None);
    }

    #[test]
    fn is_in_active_phase_matches_first_segment() {
        let mut s = State::default();
        s.set_active_phase(Some("AS".to_string()));
        // Direct task under AS.
        assert_eq!(s.is_in_active_phase("AS.1"), Some(true));
        // Deep subtask under AS.
        assert_eq!(s.is_in_active_phase("AS.1.2.3"), Some(true));
        // Same prefix as a string — but a different phase.
        assert_eq!(s.is_in_active_phase("AR.1"), Some(false));
        // Phase id itself.
        assert_eq!(s.is_in_active_phase("AS"), Some(true));
    }

    #[test]
    fn is_in_active_phase_with_no_focus_returns_none() {
        let s = State::default();
        assert_eq!(s.is_in_active_phase("AS.1"), None);
        assert_eq!(s.is_in_active_phase("anything"), None);
    }

    #[test]
    fn legacy_state_file_without_active_phase_field_loads_clean() {
        // Pre-40 state files don't have the active_phase field. They should
        // still load — serde default makes it None.
        let dir = scratch_dir();
        let path = dir.join("legacy_state.json");
        let legacy_json = r#"{
            "version": 1,
            "mappings": {
                "task-1": {
                    "plan_path": "1.2.3",
                    "last_synced_title": "Old task",
                    "last_synced_state": "pending",
                    "last_synced_annotations": []
                }
            }
        }"#;
        std::fs::write(&path, legacy_json).unwrap();
        let loaded = State::load(&path).unwrap();
        assert_eq!(loaded.active_phase(), None);
        assert_eq!(loaded.plan_path("task-1"), Some("1.2.3"));
    }

    #[test]
    fn default_state_has_debug_off() {
        assert!(!State::default().debug);
    }

    #[test]
    fn save_omits_debug_field_when_false() {
        // Off-by-default must not pollute state files for projects that never
        // opt in — the field is absent when false.
        let dir = scratch_dir();
        let path = dir.join("state.json");
        let mut s = State::default();
        s.insert("t1", "1.0");
        s.save(&path).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            !contents.contains("debug"),
            "debug shouldn't appear when false:\n{contents}"
        );
    }

    #[test]
    fn debug_true_roundtrips_via_save_load() {
        let dir = scratch_dir();
        let path = dir.join("state.json");
        let s = State {
            debug: true,
            ..Default::default()
        };
        s.save(&path).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"debug\": true"), "got:\n{contents}");
        assert!(State::load(&path).unwrap().debug);
    }

    #[test]
    fn legacy_state_file_without_debug_field_loads_with_debug_off() {
        let dir = scratch_dir();
        let path = dir.join("legacy.json");
        std::fs::write(&path, r#"{"version":1,"mappings":{}}"#).unwrap();
        assert!(!State::load(&path).unwrap().debug);
    }

    #[test]
    fn save_omits_active_phase_field_when_none() {
        // When active_phase is None, the field shouldn't appear in the
        // serialized JSON — keeps state files clean for projects that
        // never use activation.
        let dir = scratch_dir();
        let path = dir.join("state.json");
        let mut s = State::default();
        s.insert("t1", "1.0");
        s.save(&path).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            !contents.contains("active_phase"),
            "active_phase shouldn't appear when None:\n{contents}"
        );
    }
}
