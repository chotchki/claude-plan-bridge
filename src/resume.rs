use anyhow::{Context, Result};
use std::path::Path;

use crate::ast::{NodeState, cmp_ids};
use crate::parser;
use crate::state::{State, default_state_path_for};

/// SessionStart `source` values that guarantee the harness task list starts
/// empty. On these, any pending mapping in the state file is stale by
/// definition (the harness IDs it references no longer exist), so resume
/// can safely drop them before rehydrating — preventing harness-ID
/// collisions when Claude's fresh TaskCreates reuse low IDs starting from 1.
fn harness_is_fresh(source: &str) -> bool {
    matches!(source, "startup" | "clear")
}

/// Build the SessionStart additionalContext that drives Claude to rehydrate
/// the in-session task list from the persisted state file. Returns `None`
/// when there's nothing to rehydrate (no state file, no mappings, or every
/// mapping points at a resolved/missing node).
///
/// When `source` is `startup` or `clear` (harness task list provably empty),
/// also drops every pending-state mapping from the state file before
/// returning the prompt. The prompt then notes the drop so the reader
/// doesn't mistake the lack-of-conflict-warnings for a writeback bug.
///
/// Contract: emit one bullet per open mapping with its `plan_path` and the
/// live PLAN.md title. Claude is expected to `TaskCreate` each with the
/// same `metadata.plan_path`; with stale mappings pre-cleared, writeback
/// links the fresh harness IDs without collision.
pub fn build_resume_message(plan_path: &Path, source: &str) -> Result<Option<String>> {
    let state_path = default_state_path_for(plan_path);
    if !state_path.exists() {
        return Ok(None);
    }

    let state_path_for_closure = state_path.clone();
    crate::lock::with_state_lock(&state_path, crate::lock::DEFAULT_TIMEOUT, move || {
        let mut state = State::load(&state_path_for_closure)
            .with_context(|| format!("load {}", state_path_for_closure.display()))?;
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

        // Drop ALL stale mappings when the harness is provably fresh.
        // Every existing mapping references a task_id from a prior session
        // that no longer exists in the harness — pending mappings would
        // collide with rehydration TaskCreates, and done mappings are a
        // latent destruction risk if a future TaskUpdate(*, deleted)
        // collides with one. Cleanest to wipe and let TaskCreate
        // re-populate. PLAN.md is unaffected (state file is just the
        // harness-id ↔ plan_path indirection).
        let dropped = if harness_is_fresh(source) {
            let stale: Vec<String> = state.mappings.keys().cloned().collect();
            for id in &stale {
                state.remove(id);
            }
            if !stale.is_empty() {
                state.save(&state_path_for_closure)?;
            }
            stale.len()
        } else {
            0
        };

        let mut out = String::new();
        out.push_str(
            "claude-plan-bridge: session restart — the harness task list is empty, but \
             PLAN.md has open tasks tracked in the state file. Before responding to the \
             user, call TaskCreate for each item below with `metadata.plan_path = \"<id>\"` \
             so writeback links the new harness IDs to the existing PLAN.md lines.\n\n",
        );
        for (path, title) in &open {
            out.push_str(&format!("  - {path} — {title}\n"));
        }
        if dropped > 0 {
            out.push_str(&format!(
                "\nNote: the bridge cleared {dropped} stale mapping(s) from the state \
                 file before emitting this prompt (source=`{source}` guarantees a fresh \
                 harness). Your TaskCreates will land cleanly — no `Refusing to silently \
                 move` warnings expected.",
            ));
        } else {
            out.push_str(
                "\nWriteback will detect the existing PLAN.md line and update the state \
                 mapping in place (no PLAN.md insertion).",
            );
        }
        Ok(Some(out))
    })
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
        assert!(build_resume_message(&plan, "").unwrap().is_none());
    }

    #[test]
    fn returns_none_when_no_mappings() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        write_state(&plan, &State::default());
        assert!(build_resume_message(&plan, "").unwrap().is_none());
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
        assert!(build_resume_message(&plan, "").unwrap().is_none());
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
        assert!(build_resume_message(&plan, "").unwrap().is_none());
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

        let msg = build_resume_message(&plan, "").unwrap().unwrap();
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

        let msg = build_resume_message(&plan, "").unwrap().unwrap();
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
        let msg = build_resume_message(&plan, "").unwrap().expect("expected msg");
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
                source: String::new(),
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
        let msg2 = build_resume_message(&plan, "").unwrap().expect("expected msg");
        for path in ["1.0", "1.1", "1.2"] {
            assert!(msg2.contains(path), "resume missing {path}: {msg2}");
        }
    }

    #[test]
    fn startup_source_drops_all_mappings_and_notes_in_prompt() {
        // Phase 25.6a: on source=startup the harness task list is provably
        // empty, so every existing mapping references a dead task_id —
        // pending (collision risk on TaskCreate) AND done (destruction
        // risk on TaskUpdate(deleted) before B′ landed; still wasteful
        // afterwards). Wipe the lot; TaskCreate repopulates from the
        // rehydration prompt. PLAN.md is untouched.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Open\n  - [ ] 1.1 Open child\n- [x] 2.0 Closed\n",
        );
        let mut state = State::default();
        state.record(
            "1",
            Mapping {
                plan_path: "1.0".to_string(),
                last_synced_title: "Open".to_string(),
                ..Default::default()
            },
        );
        state.record(
            "2",
            Mapping {
                plan_path: "1.1".to_string(),
                last_synced_title: "Open child".to_string(),
                ..Default::default()
            },
        );
        state.record(
            "3",
            Mapping {
                plan_path: "2.0".to_string(),
                last_synced_title: "Closed".to_string(),
                last_synced_state: NodeState::Done,
                ..Default::default()
            },
        );
        write_state(&plan, &state);

        let msg = build_resume_message(&plan, "startup").unwrap().unwrap();
        // Prompt enumerates open items.
        assert!(msg.contains("1.0 — Open"), "got: {msg}");
        assert!(msg.contains("1.1 — Open child"), "got: {msg}");
        // Prompt notes the drop so the reader knows what to expect.
        assert!(msg.contains("cleared 3 stale mapping"), "got: {msg}");
        assert!(msg.contains("source=`startup`"), "got: {msg}");

        // State file: every mapping wiped (pending and done alike).
        let after = State::load(&default_state_path_for(&plan)).unwrap();
        assert!(
            after.mappings.is_empty(),
            "state should be empty after startup clear, got: {:?}",
            after.mappings
        );
    }

    #[test]
    fn clear_source_also_drops_all_mappings() {
        // /clear empties the harness task list the same way startup does.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Open\n- [x] 2.0 Closed\n");
        let mut state = State::default();
        state.record(
            "7",
            Mapping {
                plan_path: "1.0".to_string(),
                last_synced_title: "Open".to_string(),
                ..Default::default()
            },
        );
        state.record(
            "8",
            Mapping {
                plan_path: "2.0".to_string(),
                last_synced_title: "Closed".to_string(),
                last_synced_state: NodeState::Done,
                ..Default::default()
            },
        );
        write_state(&plan, &state);

        let msg = build_resume_message(&plan, "clear").unwrap().unwrap();
        assert!(msg.contains("cleared 2 stale mapping"), "got: {msg}");
        let after = State::load(&default_state_path_for(&plan)).unwrap();
        assert!(after.mappings.is_empty(), "got: {:?}", after.mappings);
    }

    #[test]
    fn resume_and_compact_sources_preserve_pending_mappings() {
        // On source=resume or source=compact the harness preserves its task
        // list, so the existing mappings are still live. Dropping them would
        // silently break TaskUpdate writeback for tasks still alive in the
        // harness. The prompt is still emitted (Claude can dedup against its
        // live TaskList) but the state file is not mutated.
        for source in ["resume", "compact", "" /* unknown */] {
            let dir = scratch_dir();
            let plan = write_plan(&dir, "- [ ] 1.0 Open\n");
            let mut state = State::default();
            state.record(
                "42",
                Mapping {
                    plan_path: "1.0".to_string(),
                    last_synced_title: "Open".to_string(),
                    ..Default::default()
                },
            );
            write_state(&plan, &state);

            let msg = build_resume_message(&plan, source).unwrap().unwrap();
            assert!(
                !msg.contains("cleared"),
                "source={source} should NOT clear; got: {msg}"
            );
            let after = State::load(&default_state_path_for(&plan)).unwrap();
            assert_eq!(
                after.plan_path("42"),
                Some("1.0"),
                "source={source} mutated state file"
            );
        }
    }

    #[test]
    fn prompt_uses_imperative_before_responding_framing() {
        // Phase 25.6b: the prompt arrives as additionalContext without a
        // user prompt. "Please re-create them" reads as optional FYI; the
        // imperative "Before responding, call TaskCreate" reads as a
        // precondition. Lock in the tightened wording so future edits don't
        // regress it.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Open\n");
        let mut state = State::default();
        state.record(
            "5",
            Mapping {
                plan_path: "1.0".to_string(),
                last_synced_title: "Open".to_string(),
                ..Default::default()
            },
        );
        write_state(&plan, &state);

        let msg = build_resume_message(&plan, "").unwrap().unwrap();
        assert!(
            msg.contains("Before responding"),
            "imperative framing missing: {msg}"
        );
        assert!(
            !msg.contains("Please re-create"),
            "old polite wording survived: {msg}"
        );
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

        let msg = build_resume_message(&plan, "").unwrap().unwrap();
        assert!(msg.contains("1.0 — Open"), "got: {msg}");
        assert!(!msg.contains("Closed"), "got: {msg}");
        assert!(!msg.contains("Skipped"), "got: {msg}");
    }
}
