use anyhow::{Context, Result};
use std::path::Path;

use crate::ast::{NodeState, cmp_ids};
use crate::parser;
use crate::state::{State, default_state_path_for};

/// Build the SessionStart additionalContext that drives Claude to rehydrate
/// the in-session task list from the persisted state file. Returns `None`
/// when there's nothing to rehydrate (no state file, no mappings, or every
/// mapping points at a resolved/missing node).
///
/// Contract: emit one bullet per open mapping with its `plan_path` and the
/// live PLAN.md title. Claude is expected to `TaskCreate` each with the
/// same `metadata.plan_path`; writeback's plan_path-dedup logic (Phase 25.3)
/// replaces the stale `task_id` mapping in place — no PLAN.md churn.
pub fn build_resume_message(plan_path: &Path) -> Result<Option<String>> {
    let state_path = default_state_path_for(plan_path);
    if !state_path.exists() {
        return Ok(None);
    }
    let state =
        State::load(&state_path).with_context(|| format!("load {}", state_path.display()))?;
    if state.mappings.is_empty() {
        return Ok(None);
    }
    // PLAN.md is the source of truth for live node state; `last_synced_state`
    // can lag if reconcile hasn't run since the last external edit.
    let plan_text = std::fs::read_to_string(plan_path)
        .with_context(|| format!("read {}", plan_path.display()))?;
    let plan = parser::parse(&plan_text)
        .with_context(|| format!("parse {}", plan_path.display()))?;

    let mut open: Vec<(String, String)> = state
        .mappings
        .values()
        .filter_map(|m| {
            let node = plan.find(&m.plan_path)?;
            if node.state != NodeState::Pending {
                return None;
            }
            Some((m.plan_path.clone(), node.title.clone()))
        })
        .collect();
    if open.is_empty() {
        return Ok(None);
    }
    open.sort_by(|a, b| cmp_ids(&a.0, &b.0));

    let mut out = String::new();
    out.push_str(
        "claude-plan-bridge: session restart — the harness task list is empty, but PLAN.md \
         has open tasks tracked in the state file. Please re-create them with TaskCreate using \
         the original `plan_path` metadata so writeback can re-link cleanly.\n\n",
    );
    for (path, title) in &open {
        out.push_str(&format!("  - {path} — {title}\n"));
    }
    out.push_str(
        "\nPass `metadata.plan_path = \"<id>\"` on each TaskCreate. Writeback will detect \
         the existing PLAN.md line and update the state mapping in place (no PLAN.md \
         insertion).",
    );
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Mapping, State};
    use std::path::PathBuf;

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "plan-bridge-resume-{}-{}",
            std::process::id(),
            uniq()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        dir
    }

    fn uniq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    fn write_plan(dir: &Path, text: &str) -> PathBuf {
        let p = dir.join("PLAN.md");
        std::fs::write(&p, text).unwrap();
        p
    }

    fn write_state(plan: &Path, state: &State) {
        let sp = default_state_path_for(plan);
        state.save(&sp).unwrap();
    }

    #[test]
    fn returns_none_when_state_file_missing() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        assert!(build_resume_message(&plan).unwrap().is_none());
    }

    #[test]
    fn returns_none_when_no_mappings() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        write_state(&plan, &State::default());
        assert!(build_resume_message(&plan).unwrap().is_none());
    }

    #[test]
    fn returns_none_when_all_mappings_resolved() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [x] 1.0 Done phase\n- [-] 2.0 Skipped\n");
        let mut state = State::default();
        state.record(
            "5",
            Mapping {
                plan_path: "1.0".to_string(),
                last_synced_title: "Done phase".to_string(),
                ..Default::default()
            },
        );
        state.record(
            "6",
            Mapping {
                plan_path: "2.0".to_string(),
                last_synced_title: "Skipped".to_string(),
                ..Default::default()
            },
        );
        write_state(&plan, &state);
        assert!(build_resume_message(&plan).unwrap().is_none());
    }

    #[test]
    fn returns_none_when_mapping_points_at_missing_node() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        let mut state = State::default();
        state.record(
            "5",
            Mapping {
                plan_path: "9.9".to_string(),
                last_synced_title: "Ghost".to_string(),
                ..Default::default()
            },
        );
        write_state(&plan, &state);
        assert!(build_resume_message(&plan).unwrap().is_none());
    }

    #[test]
    fn emits_bullet_per_open_mapping_sorted_by_id() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 First phase\n  - [ ] 1.1 First task\n- [ ] 2.0 Second phase\n",
        );
        let mut state = State::default();
        // Insert in non-sorted order to verify cmp_ids sort.
        state.record(
            "10",
            Mapping {
                plan_path: "2.0".to_string(),
                last_synced_title: "Second phase".to_string(),
                ..Default::default()
            },
        );
        state.record(
            "5",
            Mapping {
                plan_path: "1.1".to_string(),
                last_synced_title: "First task".to_string(),
                ..Default::default()
            },
        );
        state.record(
            "3",
            Mapping {
                plan_path: "1.0".to_string(),
                last_synced_title: "First phase".to_string(),
                ..Default::default()
            },
        );
        write_state(&plan, &state);

        let msg = build_resume_message(&plan).unwrap().unwrap();
        let bullets: Vec<&str> = msg.lines().filter(|l| l.starts_with("  - ")).collect();
        assert_eq!(bullets.len(), 3);
        assert!(bullets[0].contains("1.0 — First phase"), "got: {bullets:?}");
        assert!(bullets[1].contains("1.1 — First task"), "got: {bullets:?}");
        assert!(
            bullets[2].contains("2.0 — Second phase"),
            "got: {bullets:?}"
        );
    }

    #[test]
    fn prefers_live_plan_title_over_stale_state_title() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Updated title\n");
        let mut state = State::default();
        state.record(
            "5",
            Mapping {
                plan_path: "1.0".to_string(),
                last_synced_title: "Stale title".to_string(),
                ..Default::default()
            },
        );
        write_state(&plan, &state);

        let msg = build_resume_message(&plan).unwrap().unwrap();
        assert!(msg.contains("Updated title"), "got: {msg}");
        assert!(!msg.contains("Stale title"), "got: {msg}");
    }

    #[test]
    fn filters_resolved_mappings_keeps_pending() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Open\n- [x] 2.0 Closed\n- [-] 3.0 Skipped\n",
        );
        let mut state = State::default();
        for (id, path, title) in [("5", "1.0", "Open"), ("6", "2.0", "Closed"), ("7", "3.0", "Skipped")] {
            state.record(
                id,
                Mapping {
                    plan_path: path.to_string(),
                    last_synced_title: title.to_string(),
                    ..Default::default()
                },
            );
        }
        write_state(&plan, &state);

        let msg = build_resume_message(&plan).unwrap().unwrap();
        assert!(msg.contains("1.0 — Open"), "got: {msg}");
        assert!(!msg.contains("Closed"), "got: {msg}");
        assert!(!msg.contains("Skipped"), "got: {msg}");
    }
}
