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
    /// Phase CJ: `Some(id)` when baseline wrote/advanced the phase high-water
    /// marker in PLAN.md (the migration path for a pre-CJ plan). `None` when the
    /// marker was already correct (or the plan has no phases to pin).
    pub marker_seeded: Option<String>,
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
        let id = leaf.id();
        if id.is_empty() {
            report.skipped_no_id.push(leaf.title().to_string());
            continue;
        }
        if mapped_paths.contains(id) {
            report.already_mapped.push(id.to_string());
            continue;
        }
        let synthetic_id = format!("{BASELINE_PREFIX}{id}");
        state.record(
            &synthetic_id,
            Mapping {
                plan_path: id.to_string(),
                last_synced_title: leaf.title().to_string(),
                last_synced_state: leaf.state(),
                last_synced_annotations: annotations_to_strings(leaf.annotations()),
                ..Default::default()
            },
        );
        report.baselined.push(id.to_string());
    }

    if !report.baselined.is_empty() {
        state.save(&state_path)?;
    }

    // Phase CJ: seed / advance the phase high-water marker so an existing
    // (pre-CJ) plan migrates off the PLAN_ARCHIVE.md scrape. Recompute the true
    // high-water over live PLAN.md + PLAN_ARCHIVE.md + any existing marker, and
    // rewrite the marker only when it's actually missing or behind — a minimal
    // top-of-file edit that leaves the rest of the document untouched, so
    // baseline stays a non-reformatting resync.
    if let Some(id) = crate::phase_seq::seed_high_water_for_plan(plan_path)
        && crate::phase_seq::marker_of_text(&text).as_deref() != Some(id.as_str())
    {
        let new_text = crate::phase_seq::set_marker_in_text(&text, &id);
        std::fs::write(plan_path, new_text)
            .with_context(|| format!("write {}", plan_path.display()))?;
        report.marker_seeded = Some(id);
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

    use crate::test_utils::write_plan;

    fn scratch_dir() -> PathBuf {
        crate::test_utils::scratch_dir("baseline")
    }

    #[test]
    fn baselines_every_leaf_when_state_empty() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "## Phase 1 - Phase\n  - [ ] 1.1 A\n  - [x] 1.2 B\n- [-] 2.0 Skipped\n",
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
        let plan = write_plan(&dir, "## Phase 1 - Phase\n  - [ ] 1.1 A\n");
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
        let plan = write_plan(&dir, "## Phase 1 - Phase\n  - [x] 1.1 Already done\n");
        baseline(&plan).unwrap();
        let state = State::load(&default_state_path_for(&plan)).unwrap();
        let m = state.mappings.get("baseline:1.1").unwrap();
        assert_eq!(m.last_synced_title, "Already done");
        assert_eq!(m.last_synced_state, crate::ast::NodeState::Done);
    }

    #[test]
    fn idempotent() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "## Phase 1 - Phase\n  - [ ] 1.1 A\n");
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
            "## Phase 1 - Phase\n  - [ ] 1.1 Real id\n  - [ ] No id here\n  - [ ] Another no id\n",
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
    fn seeds_marker_from_live_and_archive_for_pre_cj_plan() {
        // Phase CJ migration: a markerless plan with a live phase LOWER than an
        // already-archived id. baseline must seed the marker to the true
        // high-water (the archived `CI`), so the next read skips the archive and
        // still can't re-hand-out a swept id.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "# PLAN\n## Phase B - live\n  - [ ] B.1 open\n");
        std::fs::write(
            crate::phase_seq::archive_path_for(&plan),
            "## Phase CA - swept\n## Phase CI - swept\n",
        )
        .unwrap();

        let report = baseline(&plan).unwrap();
        assert_eq!(report.marker_seeded.as_deref(), Some("CI"));
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(
            after.starts_with("<!-- plan-bridge:phase-high-water=CI -->\n"),
            "marker not seeded at top:\n{after}"
        );
        // Live content preserved verbatim below the marker.
        assert!(after.contains("## Phase B - live"));
        assert!(after.contains("- [ ] B.1 open"));
        // Second baseline is a no-op on the marker (already correct).
        let report2 = baseline(&plan).unwrap();
        assert_eq!(report2.marker_seeded, None);
    }

    #[test]
    fn baseline_no_marker_for_plan_without_phases() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "# PLAN\n\nJust prose, no phases.\n");
        let report = baseline(&plan).unwrap();
        assert_eq!(report.marker_seeded, None);
        assert!(
            !std::fs::read_to_string(&plan)
                .unwrap()
                .contains("phase-high-water")
        );
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
