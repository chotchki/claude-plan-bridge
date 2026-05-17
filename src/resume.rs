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
    fn full_restart_cycle_rehydrates_cleanly() {
        // Phase 25.5 e2e: simulate the full restart flow end-to-end.
        //   1. Prior session: state file has 3 mappings under task_ids 5/6/7
        //      for plan_paths 1.0/1.1/1.2; PLAN.md has those leaves.
        //   2. Session restart: in-session task_ids are gone, but state file
        //      persists. `build_resume_message` produces the rehydration
        //      prompt.
        //   3. Claude calls TaskCreate for each suggested mapping with FRESH
        //      task_ids (101/102/103) — simulated here by invoking
        //      writeback_create directly with the same plan_paths.
        //   4. Post-rehydration: state file has only the new task_ids, no
        //      zombies; PLAN.md is byte-identical.
        use crate::writeback;
        use crate::hook::HookPayload;

        let dir = scratch_dir();
        let plan_text = "- [ ] 1.0 Phase one\n  - [ ] 1.1 First\n  - [ ] 1.2 Second\n";
        let plan = write_plan(&dir, plan_text);

        // (1) Seed prior-session state.
        let state_path = default_state_path_for(&plan);
        let mut prior = State::default();
        for (id, path, title) in [
            ("5", "1.0", "Phase one"),
            ("6", "1.1", "First"),
            ("7", "1.2", "Second"),
        ] {
            prior.record(
                id,
                Mapping {
                    plan_path: path.to_string(),
                    last_synced_title: title.to_string(),
                    ..Default::default()
                },
            );
        }
        prior.save(&state_path).unwrap();

        // (2) Resume produces a prompt listing all three.
        let msg = build_resume_message(&plan).unwrap().expect("expected msg");
        for path in ["1.0", "1.1", "1.2"] {
            assert!(msg.contains(path), "resume missing {path}: {msg}");
        }

        // (3) Apply rehydration with fresh task_ids (no overlap with prior 5/6/7).
        let plan_before = std::fs::read_to_string(&plan).unwrap();
        for (new_id, path, title) in [
            ("101", "1.0", "Phase one"),
            ("102", "1.1", "First"),
            ("103", "1.2", "Second"),
        ] {
            let payload = HookPayload {
                session_id: String::new(),
                cwd: String::new(),
                hook_event_name: "PostToolUse".to_string(),
                tool_name: "TaskCreate".to_string(),
                tool_input: serde_json::json!({
                    "subject": title,
                    "description": title,
                    "metadata": {"plan_path": path},
                }),
                tool_response: serde_json::json!({"id": new_id}),
            };
            writeback::writeback_create(&payload, &plan).unwrap();
        }

        // (4) Final state: only the new task_ids, no zombies, PLAN.md unchanged.
        let plan_after = std::fs::read_to_string(&plan).unwrap();
        assert_eq!(plan_before, plan_after, "PLAN.md mutated during rehydration");

        let final_state = State::load(&state_path).unwrap();
        assert_eq!(final_state.mappings.len(), 3, "expected exactly 3 mappings, got {:?}", final_state.mappings);
        assert_eq!(final_state.plan_path("101"), Some("1.0"));
        assert_eq!(final_state.plan_path("102"), Some("1.1"));
        assert_eq!(final_state.plan_path("103"), Some("1.2"));
        for stale in ["5", "6", "7"] {
            assert_eq!(
                final_state.plan_path(stale),
                None,
                "stale mapping for {stale} survived rehydration"
            );
        }

        // Resume on the rehydrated state still produces the same prompt
        // (state file is current; resume doesn't know whether harness has
        // already created the tasks). That's expected — subsequent
        // TaskCreates with same task_ids would no-op via the same-path
        // branch, so it's safe.
        let msg2 = build_resume_message(&plan).unwrap().expect("expected msg");
        for path in ["1.0", "1.1", "1.2"] {
            assert!(msg2.contains(path), "resume missing {path}: {msg2}");
        }
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
