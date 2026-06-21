//! Phase BY.6: `drop-mapping` subcommand. Release a stale state mapping
//! without touching PLAN.md — the recovery path (feedback #6) for a mapping
//! whose target leaf was hand-archived or hand-deleted, so its synthetic or
//! harness task id no longer points at a live line. The normal `archive`
//! command already drops mappings for the leaves it moves; this verb is for
//! the cases where PLAN.md changed outside the bridge.
//!
//! Matches on EITHER the dotted leaf id (`m.plan_path`) OR the raw task id
//! (the map key), so a caller can pass whichever they have in front of them.
//! Idempotent: a target with no matching mapping is a clean no-op.
use crate::state::{State, default_state_path_for};
use anyhow::Result;
use std::path::Path;

/// Outcome of a `drop-mapping` run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DropMappingReport {
    /// The target string the caller queried (plan_path or task id).
    pub target: String,
    /// task_ids whose mapping was removed (empty when nothing matched).
    pub dropped: Vec<String>,
}

/// Remove every state mapping whose `plan_path` equals `target` OR whose task
/// id equals `target`. Does not read or write PLAN.md. Idempotent.
pub fn drop_mapping(plan_path: &Path, target: &str) -> Result<DropMappingReport> {
    let state_path = default_state_path_for(plan_path);
    crate::lock::with_state_lock(&state_path, crate::lock::DEFAULT_TIMEOUT, || {
        let mut state = State::load(&state_path)?;
        let mut to_drop: Vec<String> = state
            .mappings
            .iter()
            .filter(|(tid, m)| m.plan_path == target || tid.as_str() == target)
            .map(|(tid, _)| tid.clone())
            .collect();
        to_drop.sort();
        for tid in &to_drop {
            state.remove(tid);
        }
        if !to_drop.is_empty() {
            state.save(&state_path)?;
        }
        Ok(DropMappingReport {
            target: target.to_string(),
            dropped: to_drop,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Mapping, State, default_state_path_for};
    use crate::test_utils::write_plan;

    fn scratch_dir() -> std::path::PathBuf {
        crate::test_utils::scratch_dir("drop-mapping")
    }

    fn seed(plan: &Path, entries: &[(&str, &str)]) {
        let state_path = default_state_path_for(plan);
        let mut state = State::default();
        for (tid, path) in entries {
            state.record(
                *tid,
                Mapping {
                    plan_path: path.to_string(),
                    last_synced_title: format!("task at {path}"),
                    ..Default::default()
                },
            );
        }
        state.save(&state_path).unwrap();
    }

    #[test]
    fn drops_by_plan_path() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "## Phase 1 - Phase\n  - [ ] 1.1 Task\n");
        seed(&plan, &[("68", "BS.5"), ("70", "1.1")]);

        let report = drop_mapping(&plan, "BS.5").unwrap();
        assert_eq!(report.dropped, vec!["68".to_string()]);

        let state = State::load(&default_state_path_for(&plan)).unwrap();
        assert_eq!(state.plan_path("68"), None, "BS.5 mapping dropped");
        assert_eq!(state.plan_path("70"), Some("1.1"), "unrelated mapping kept");
    }

    #[test]
    fn drops_by_task_id() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "## Phase 1 - Phase\n");
        seed(&plan, &[("baseline:BS.5", "BS.5")]);

        let report = drop_mapping(&plan, "baseline:BS.5").unwrap();
        assert_eq!(report.dropped, vec!["baseline:BS.5".to_string()]);
        let state = State::load(&default_state_path_for(&plan)).unwrap();
        assert!(state.mappings.is_empty());
    }

    #[test]
    fn no_match_is_clean_noop() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "## Phase 1 - Phase\n");
        seed(&plan, &[("70", "1.1")]);

        let report = drop_mapping(&plan, "ZZ.9").unwrap();
        assert!(report.dropped.is_empty());
        // Untouched.
        let state = State::load(&default_state_path_for(&plan)).unwrap();
        assert_eq!(state.plan_path("70"), Some("1.1"));
    }
}
