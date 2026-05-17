//! `plan-bridge baseline` — seed the state file with the current PLAN.md so
//! the first reconcile after install isn't a wall of `LeafAdded`.
//!
//! For each leaf in PLAN.md that doesn't have a state mapping, we insert one
//! with `task_id = "baseline:<plan_path>"`. Reconcile treats these like normal
//! mappings — no `LeafAdded` — so the install is quiet.
//!
//! When Claude later runs a real `TaskCreate` against a baselined `plan_path`,
//! `writeback_create` evicts the baseline mapping and replaces it with the
//! real `task_id`. From the user's perspective, baseline is a one-shot
//! initialization that silently dissolves as real tasks come online.

use crate::parser::parse;
use crate::state::{Mapping, State, default_state_path_for};
use crate::writeback::annotations_to_strings;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::Path;

pub const BASELINE_PREFIX: &str = "baseline:";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct BaselineReport {
    pub baselined: Vec<String>,
    pub already_mapped: Vec<String>,
    /// Leaves with empty `id` (bare-checkbox bullets like `- [ ] no id here`).
    /// Untrackable — no stable plan_path to key state by. Reported so users
    /// can see how many leaves the bridge can't follow without explicit ids.
    pub skipped_no_id: Vec<String>,
}

pub fn baseline(plan_path: &Path) -> Result<BaselineReport> {
    let text = std::fs::read_to_string(plan_path)
        .with_context(|| format!("read {}", plan_path.display()))?;
    let plan = parse(&text)?;

    let state_path = default_state_path_for(plan_path);
    let mut state = State::load(&state_path)?;

    let mapped_paths: HashSet<String> = state
        .mappings
        .values()
        .map(|m| m.plan_path.clone())
        .collect();

    let mut report = BaselineReport::default();

    for leaf in plan.leaves() {
        // Phase 18: skip empty-id leaves (bare-checkbox bullets without a
        // dotted id). They have no stable plan_path so any state entry would
        // collide on key `baseline:` — last-write-wins drops siblings, and
        // every subsequent reconcile emits noisy LeafTitleChanged drift.
        // Untrackable by design: give the leaf an id if you want it baselined.
        if leaf.id.is_empty() {
            report.skipped_no_id.push(leaf.title.clone());
            continue;
        }
        if mapped_paths.contains(&leaf.id) {
            report.already_mapped.push(leaf.id.clone());
            continue;
        }
        let synthetic_id = format!("{BASELINE_PREFIX}{}", leaf.id);
        state.record(
            &synthetic_id,
            Mapping {
                plan_path: leaf.id.clone(),
                last_synced_title: leaf.title.clone(),
                last_synced_state: leaf.state,
                last_synced_annotations: annotations_to_strings(&leaf.annotations),
            },
        );
        report.baselined.push(leaf.id.clone());
    }

    if !report.baselined.is_empty() {
        state.save(&state_path)?;
    }
    Ok(report)
}

/// Remove any baseline mapping for `plan_path`. Called by `writeback_create`
/// when a real `TaskCreate` lands so we don't keep duplicate mappings.
pub fn evict_baseline_for(state: &mut State, plan_path: &str) -> bool {
    let synthetic_id = format!("{BASELINE_PREFIX}{plan_path}");
    state.remove(&synthetic_id).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "plan-bridge-baseline-{}-{}",
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

    fn write_plan(dir: &Path, contents: &str) -> PathBuf {
        let p = dir.join("PLAN.md");
        std::fs::write(&p, contents).unwrap();
        p
    }

    #[test]
    fn baselines_every_leaf_when_state_empty() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [ ] 1.1 A\n  - [x] 1.2 B\n- [-] 2.0 Skipped\n",
        );
        let report = baseline(&plan).unwrap();
        // Leaves: 1.1, 1.2, 2.0 (2.0 has no children → is a leaf).
        assert_eq!(report.baselined.len(), 3);
        assert!(report.baselined.contains(&"1.1".to_string()));
        assert!(report.baselined.contains(&"1.2".to_string()));
        assert!(report.baselined.contains(&"2.0".to_string()));

        let state = State::load(&default_state_path_for(&plan)).unwrap();
        assert_eq!(state.plan_path("baseline:1.1"), Some("1.1"));
        assert_eq!(state.plan_path("baseline:1.2"), Some("1.2"));
    }

    #[test]
    fn skips_already_mapped_leaves() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 A\n");
        let state_path = default_state_path_for(&plan);
        let mut state = State::default();
        state.insert("real-task", "1.1");
        state.save(&state_path).unwrap();

        let report = baseline(&plan).unwrap();
        assert!(report.baselined.is_empty());
        assert_eq!(report.already_mapped, vec!["1.1".to_string()]);

        let loaded = State::load(&state_path).unwrap();
        assert_eq!(loaded.plan_path("real-task"), Some("1.1"));
        assert!(loaded.plan_path("baseline:1.1").is_none());
    }

    #[test]
    fn captures_current_state_in_mapping() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [x] 1.0 Already done\n");
        baseline(&plan).unwrap();
        let state = State::load(&default_state_path_for(&plan)).unwrap();
        let m = state.mappings.get("baseline:1.0").unwrap();
        assert_eq!(m.last_synced_title, "Already done");
        assert_eq!(m.last_synced_state, crate::ast::NodeState::Done);
    }

    #[test]
    fn idempotent() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 A\n");
        baseline(&plan).unwrap();
        let report = baseline(&plan).unwrap();
        assert!(report.baselined.is_empty());
        assert_eq!(report.already_mapped, vec!["1.1".to_string()]);
    }

    #[test]
    fn skips_empty_id_leaves_to_avoid_collision() {
        // Phase 18.1 regression — quicksight shakeout. Bare-checkbox leaves
        // (no dotted id) used to all key on `baseline:` with plan_path="" —
        // last-write-wins collapsed N leaves into 1 state entry, then
        // reconcile spammed N-1 LeafTitleChanged deltas every prompt.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [ ] 1.1 Real id\n  - [ ] No id here\n  - [ ] Another no id\n",
        );
        let report = baseline(&plan).unwrap();
        assert_eq!(report.baselined, vec!["1.1".to_string()]);
        assert_eq!(report.skipped_no_id.len(), 2);
        assert!(report.skipped_no_id.iter().any(|t| t == "No id here"));

        let state = State::load(&default_state_path_for(&plan)).unwrap();
        // Only one baseline entry; no `baseline:` collision key.
        assert_eq!(state.plan_path("baseline:1.1"), Some("1.1"));
        assert_eq!(state.plan_path("baseline:"), None);
        assert_eq!(state.mappings.len(), 1);
    }

    #[test]
    fn evict_baseline_for_drops_synthetic_mapping() {
        let mut state = State::default();
        state.insert("baseline:1.1", "1.1");
        state.insert("real-task", "1.1");
        assert!(evict_baseline_for(&mut state, "1.1"));
        assert!(state.plan_path("baseline:1.1").is_none());
        assert_eq!(state.plan_path("real-task"), Some("1.1"));
    }
}
