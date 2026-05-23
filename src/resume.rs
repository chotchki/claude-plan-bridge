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

/// Build the SessionStart additionalContext that drives Claude to reconcile
/// the in-session task list against the persisted state file. Returns `None`
/// when there's nothing to reconcile (no state file, no mappings, or every
/// mapping points at a resolved/missing node).
///
/// Phase 33.1: prompt body is branched by `source`. The two branches answer
/// different questions:
///   - `startup` / `clear`: the harness task list is provably empty and the
///     bridge drops every pending-state mapping before returning the prompt.
///     The prompt asks the agent to `TaskCreate` each leaf below. Bullets
///     emit `<plan_path> <title>` only — the prior-session task_ids are
///     stale and would mislead a fresh harness.
///   - `resume` / `compact`: the harness preserved its task list and the
///     state file is NOT wiped. The prompt asks the agent to `TaskList`
///     first and only `TaskCreate` plan_paths whose mapped `task_id` is
///     missing from TaskList. Bullets emit `<plan_path> <title>
///     (task_id=<id>)` so dedup is precise (Phase 33.2).
///
/// In both branches, parent-phase nodes are filtered (see Phase 27.1) and
/// rendered as `## <plan_path> <title>` context headers above their leaves
/// rather than as TaskCreate asks.
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
        let plan =
            parser::parse(&plan_text).with_context(|| format!("parse {}", plan_path.display()))?;

        // Phase 27.1: leaves-only rehydration. Parent phase nodes represent
        // goal-validation gates the user owns (see `parent-tick-is-validation`
        // memory) — they should never come back as harness tasks on restart.
        // Archive already operates on subtree state, so leaving parent boxes
        // unticked doesn't block phase sweeping. A newly-stubbed phase with no
        // children is itself a leaf by `is_leaf()` and still emits.
        //
        // Phase 33.2: carry the harness task_id alongside plan_path + title so
        // the resume/compact branch of the prompt can ask the agent to dedup
        // against TaskList by harness id (precise) instead of by subject text
        // (fuzzy when titles drift mid-session).
        // Phase 40.3: when a phase is active, scope rehydration to leaves
        // whose plan_path falls under that phase. Backlog entries
        // (synthetic `backlog:` mappings, no real leaf in PLAN.md) ride
        // through the existing filter unchanged — they're cross-cutting
        // context that always loads. When no active_phase is set, the
        // filter is a pass-through (today's behavior).
        let active = state.active_phase().map(String::from);
        let mut open: Vec<(String, String, String)> = state
            .mappings
            .iter()
            .filter_map(|(task_id, m)| {
                let item = plan.find_item(&m.plan_path)?;
                if item.state() != NodeState::Pending {
                    return None;
                }
                if !item.is_leaf() {
                    return None;
                }
                if let Some(active_id) = active.as_deref()
                    && crate::state::phase_id_of(&m.plan_path) != active_id
                {
                    return None;
                }
                Some((
                    m.plan_path.clone(),
                    item.title().to_string(),
                    task_id.clone(),
                ))
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
        //
        // Also seed `pending_rehydration` with the open plan_paths we just
        // told Claude to TaskCreate. Reconcile uses that set to suppress
        // duplicate "Added [ ] … (consider TaskCreate)" drift on the very
        // next UserPromptSubmit; writeback evicts paths as their matching
        // TaskCreates land.
        let dropped = if harness_is_fresh(source) {
            // Phase 26.8: capture an audit entry per mapping before it's
            // dropped, so a "where did my task go?" investigation can be
            // traced to a specific SessionStart event. Best-effort — if
            // the log write fails we still proceed with the wipe, since
            // a missing audit row is strictly less harmful than refusing
            // to rehydrate.
            let stale: Vec<String> = state.mappings.keys().cloned().collect();
            let entries: Vec<crate::audit::ClearedEntry> = stale
                .iter()
                .filter_map(|id| {
                    state
                        .mappings
                        .get(id)
                        .map(|m| crate::audit::entry_now(source, id, &m.plan_path))
                })
                .collect();
            if let Err(e) = crate::audit::append_cleared(&state_path_for_closure, &entries) {
                eprintln!("claude-plan-bridge: WARNING audit log append failed: {e:#}");
            }
            for id in &stale {
                state.remove(id);
            }
            state.pending_rehydration = open.iter().map(|(p, _, _)| p.clone()).collect();
            state.rehydration_announced = state.pending_rehydration.len() as u32;
            if !stale.is_empty() || !state.pending_rehydration.is_empty() {
                state.save(&state_path_for_closure)?;
            }
            stale.len()
        } else {
            0
        };

        // Phase 27.3: source-aware ToolSearch framing. On `startup`/`clear`
        // the harness process is fresh — no prior tool history — so
        // TaskCreate IS deferred and MUST be fetched via ToolSearch before
        // it can be called. On `resume`/`compact` the prior tool history is
        // preserved, so TaskCreate is almost always already loaded and the
        // fetch is only a defensive fallback.
        let tool_search_hint = if harness_is_fresh(source) {
            "TaskCreate is deferred on a fresh harness — fetch it first with \
             `ToolSearch query=\"select:TaskCreate\"`."
        } else {
            "If TaskCreate isn't loaded yet, fetch with \
             `ToolSearch query=\"select:TaskCreate\"`."
        };

        // Phase 33.1: branch the prompt body by `source`. The two branches
        // are answering different questions:
        //   * startup/clear: the harness task list is provably empty and the
        //     bridge just wiped state; the agent must TaskCreate every leaf
        //     fresh. The prior-session task_ids are stale, so they're NOT
        //     emitted (they would mislead the agent).
        //   * resume/compact: the harness preserved its task list and the
        //     bridge intentionally did NOT wipe state; the agent must
        //     TaskList first and only TaskCreate plan_paths whose mapped
        //     task_id is missing from TaskList. Bullets include
        //     `task_id=<id>` so dedup is by harness id, not by fuzzy
        //     subject text.
        let mut out = String::new();
        if harness_is_fresh(source) {
            out.push_str(
                "claude-plan-bridge: session restart — the harness task list is empty, but \
                 PLAN.md has open tasks tracked in the state file. Before responding to the \
                 user, call TaskCreate for each leaf below.\n\n\
                 Each indented bullet has the shape `<plan_path> <title>` (matching PLAN.md). \
                 For each, call `TaskCreate(subject=<title>, description=<plan_path>, \
                 metadata={\"plan_path\": <plan_path>})` — the leading `N.M` token goes into \
                 `metadata.plan_path` AND `description` (the bridge ignores `description`; \
                 using the plan_path keeps the harness UI clean instead of duplicating the \
                 title). `## <plan_path> <title>` lines are \
                 parent-phase headers shown for goal context; do NOT TaskCreate them — parent \
                 ticking is a deliberate validation step you take after confirming the leaves \
                 met the phase goal. ",
            );
            out.push_str(tool_search_hint);
            out.push_str(
                " All TaskCreate calls are independent — batch them in a single tool-call \
                 block.\n",
            );
        } else {
            // Phase 33.1: resume/compact path. The harness preserved its
            // task list, so the prompt must NOT claim "task list is empty"
            // (it isn't) and must NOT imperative-batch TaskCreates (that
            // would dupe live tasks). TaskList is the imperative first step
            // — only backfill leaves whose task_id is missing from it.
            out.push_str(&format!(
                "claude-plan-bridge: session {source} — your harness task list was preserved \
                 across this session-restart event along with the state-file mappings. \
                 PLAN.md has open tasks the bridge wants you to confirm are still loaded. \
                 Before responding to the user, call `TaskList` first.\n\n\
                 Each indented bullet has the shape `<plan_path> <title>  (task_id=<id>)`. \
                 For each, check whether that `task_id` appears in your TaskList output. \
                 If YES, the existing mapping is live — do nothing. \
                 If NO, the harness dropped it; backfill with \
                 `TaskCreate(subject=<title>, description=<plan_path>, \
                 metadata={{\"plan_path\": <plan_path>}})` — writeback re-attaches the fresh \
                 harness id to the existing PLAN.md line (no PLAN.md insertion). \
                 `## <plan_path> <title>` lines are \
                 parent-phase headers shown for goal context; do NOT TaskCreate them — parent \
                 ticking is a deliberate validation step you take after confirming the leaves \
                 met the phase goal. ",
            ));
            out.push_str(tool_search_hint);
            out.push_str(
                " Backfill TaskCreates (if any) are independent — batch them in a single \
                 tool-call block.\n",
            );
        }

        // Phase 40.3: announce the active-phase scope up front, before the
        // bullets, so the agent understands why the list is narrower than
        // PLAN.md would suggest. Cross-phase work is still possible — the
        // hint just frames the bullets as "your current focus."
        if let Some(active_id) = active.as_deref() {
            out.push_str(&format!(
                "\nActive phase: `{active_id}` (scoped). Other open phases are present in \
                 PLAN.md but skipped here. Run `plan_deactivate` to widen scope, or \
                 `plan_activate <other>` to switch focus.\n",
            ));
        }

        let mut current_parent: Option<String> = None;
        let show_task_id = !harness_is_fresh(source);
        for (path, title, task_id) in &open {
            let parent_id = crate::ast::parent_id_for(path);
            if parent_id != current_parent {
                if let Some(ref pid) = parent_id
                    && let Some(parent_item) = plan.find_item(pid)
                {
                    out.push_str(&format!(
                        "\n## {} {}\n",
                        parent_item.id(),
                        parent_item.title()
                    ));
                }
                current_parent = parent_id;
            }
            if show_task_id {
                out.push_str(&format!("  - {path} {title}  (task_id={task_id})\n"));
            } else {
                out.push_str(&format!("  - {path} {title}\n"));
            }
        }

        // Phase 33.5: footer is source-aware too. The fresh-harness branch
        // confirms the wipe stats; the preserved-harness branch confirms
        // mappings survived and explains what backfill (if any) will do.
        // Note: `dropped == 0 && harness_is_fresh(source)` is impossible —
        // we returned `Ok(None)` earlier when `state.mappings.is_empty()`,
        // and a fresh-source path with non-empty mappings always wipes
        // them all. So the `else` branch is purely the resume/compact case.
        if dropped > 0 {
            out.push_str(&format!(
                "\nNote: the bridge cleared {dropped} stale mapping(s) from the state \
                 file before emitting this prompt (source=`{source}` guarantees a fresh \
                 harness). Your TaskCreates will land cleanly — no `Refusing to silently \
                 move` warnings expected.",
            ));
        } else {
            out.push_str(&format!(
                "\nNote: the bridge preserved the state-file mappings across this \
                 session-restart (source=`{source}` keeps the harness task list intact). \
                 If TaskList shows every `task_id` above, no action is needed — this \
                 rehydration is a sanity check. Backfill TaskCreates (for ids missing from \
                 TaskList) land cleanly because writeback replaces the dead mapping with the \
                 fresh harness id and reuses the existing PLAN.md line.",
            ));
        }
        Ok(Some(out))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Mapping, State};
    use std::path::PathBuf;

    use crate::test_utils::write_plan;

    fn scratch_dir() -> PathBuf {
        // Resume tests want a pre-existing `.claude/` subdir for state
        // file writes; tack it on after the shared helper creates the
        // top-level temp dir.
        let dir = crate::test_utils::scratch_dir("resume");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        dir
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
    fn emits_bullet_per_open_leaf_mapping_sorted_by_id() {
        // Phase 27.1: parent phase nodes are filtered (1.0 has children); only
        // leaves (1.1, 1.2, 2.0) are emitted. A phase with no children
        // (e.g., 2.0) is itself a leaf and survives.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 First phase\n  - [ ] 1.1 First task\n  - [ ] 1.2 Second task\n- [ ] 2.0 Second phase\n",
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
            "7",
            Mapping {
                plan_path: "1.2".to_string(),
                last_synced_title: "Second task".to_string(),
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
        assert_eq!(
            bullets.len(),
            3,
            "expected 3 leaf bullets, got: {bullets:?}"
        );
        assert!(bullets[0].contains("1.1 First task"), "got: {bullets:?}");
        assert!(bullets[1].contains("1.2 Second task"), "got: {bullets:?}");
        assert!(bullets[2].contains("2.0 Second phase"), "got: {bullets:?}");
        assert!(
            !bullets.iter().any(|b| b.contains("First phase")),
            "parent phase 1.0 leaked into a TaskCreate bullet: {bullets:?}"
        );
    }

    #[test]
    fn filters_parent_phases_keeps_only_leaves() {
        // Phase 27.1: leaves-only rehydration. Even when a parent node has a
        // state mapping, it should be filtered out of the rehydration prompt —
        // parents represent goal-validation gates the user owns, not harness
        // tasks. The state-file drop on `startup`/`clear` still wipes the
        // parent mapping (no zombie state); the parent simply doesn't get
        // re-announced as a TaskCreate ask.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Parent phase\n  - [ ] 1.1 Leaf one\n  - [ ] 1.2 Leaf two\n",
        );
        let mut state = State::default();
        state.record(
            "1",
            Mapping {
                plan_path: "1.0".to_string(),
                last_synced_title: "Parent phase".to_string(),
                ..Default::default()
            },
        );
        state.record(
            "2",
            Mapping {
                plan_path: "1.1".to_string(),
                last_synced_title: "Leaf one".to_string(),
                ..Default::default()
            },
        );
        state.record(
            "3",
            Mapping {
                plan_path: "1.2".to_string(),
                last_synced_title: "Leaf two".to_string(),
                ..Default::default()
            },
        );
        write_state(&plan, &state);

        let msg = build_resume_message(&plan, "startup").unwrap().unwrap();
        let bullets: Vec<&str> = msg.lines().filter(|l| l.starts_with("  - ")).collect();

        // Only leaf bullets emitted; parent appears as context header, not bullet.
        assert!(msg.contains("1.1 Leaf one"), "leaf 1.1 missing: {msg}");
        assert!(msg.contains("1.2 Leaf two"), "leaf 1.2 missing: {msg}");
        assert!(
            !bullets.iter().any(|b| b.contains("Parent phase")),
            "parent 1.0 should not be a TaskCreate bullet: {bullets:?}"
        );

        // pending_rehydration should hold leaves only (matters for the
        // rehydration-complete signal — Phase 26.7 expects the set to drain
        // when the announced TaskCreates land).
        let after = State::load(&default_state_path_for(&plan)).unwrap();
        assert_eq!(
            after
                .pending_rehydration
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["1.1".to_string(), "1.2".to_string()],
            "pending_rehydration should be leaves only: {:?}",
            after.pending_rehydration
        );
        assert_eq!(
            after.rehydration_announced, 2,
            "announced count should match leaf count, got {}",
            after.rehydration_announced
        );
    }

    #[test]
    fn groups_leaves_under_parent_phase_header() {
        // Phase 27.1a: leaves are grouped under their parent phase using a
        // `## <plan_path> <title>` header line so the agent sees the phase
        // goal at restart time (the validation cue that leaves-only
        // rehydration would otherwise lose). Headers are context, NOT
        // TaskCreate asks — the prompt instructs accordingly.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 First phase\n  - [ ] 1.1 First task\n  - [ ] 1.2 Second task\n\
             - [ ] 3.0 Third phase\n  - [ ] 3.1 Lone task\n\
             - [ ] 5.0 Standalone phase\n",
        );
        let mut state = State::default();
        for (id, path, title) in [
            ("a", "1.1", "First task"),
            ("b", "1.2", "Second task"),
            ("c", "3.1", "Lone task"),
            ("d", "5.0", "Standalone phase"),
        ] {
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

        let msg = build_resume_message(&plan, "startup").unwrap().unwrap();

        // Parent headers appear for grouped leaves; childless top-level
        // leaf (5.0) has no header (parent_id_for returns None).
        assert!(
            msg.contains("## 1.0 First phase"),
            "missing parent header for 1.0: {msg}"
        );
        assert!(
            msg.contains("## 3.0 Third phase"),
            "missing parent header for 3.0: {msg}"
        );
        assert!(
            !msg.contains("## 5.0 Standalone phase"),
            "5.0 is itself a leaf (no parent), should not get a header: {msg}"
        );

        // Header position: each parent header precedes its leaves.
        let pos_h10 = msg.find("## 1.0").expect("header 1.0");
        let pos_11 = msg.find("  - 1.1").expect("leaf 1.1");
        let pos_12 = msg.find("  - 1.2").expect("leaf 1.2");
        let pos_h30 = msg.find("## 3.0").expect("header 3.0");
        let pos_31 = msg.find("  - 3.1").expect("leaf 3.1");
        assert!(
            pos_h10 < pos_11 && pos_11 < pos_12,
            "1.0 header should precede its leaves: header@{pos_h10} 1.1@{pos_11} 1.2@{pos_12}"
        );
        assert!(
            pos_12 < pos_h30 && pos_h30 < pos_31,
            "3.0 header should appear after 1.x leaves and before 3.1"
        );

        // Instruction makes clear that `## ` lines are not TaskCreate asks.
        assert!(
            msg.contains("do NOT TaskCreate them"),
            "missing parent-header exclusion instruction: {msg}"
        );
    }

    #[test]
    fn childless_phase_node_is_treated_as_leaf() {
        // A phase stubbed with no children (e.g., a freshly-added `- [ ] 5.0
        // Future phase` with nothing under it) IS a leaf by `is_leaf()` and
        // should rehydrate — otherwise newly-stubbed phases would silently
        // vanish from the harness on restart.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 5.0 Future phase\n");
        let mut state = State::default();
        state.record(
            "1",
            Mapping {
                plan_path: "5.0".to_string(),
                last_synced_title: "Future phase".to_string(),
                ..Default::default()
            },
        );
        write_state(&plan, &state);

        let msg = build_resume_message(&plan, "startup").unwrap().unwrap();
        assert!(
            msg.contains("5.0 Future phase"),
            "childless phase should emit as leaf: {msg}"
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
        // Phase 25.5 e2e (updated for 27.1 leaves-only): simulate the full
        // restart flow end-to-end.
        //   1. Prior session: state file has 3 mappings — parent 1.0 (task_id
        //      5) plus leaves 1.1/1.2 (task_ids 6/7).
        //   2. Session restart: in-session task_ids are gone. `build_resume_
        //      message` produces the rehydration prompt with LEAVES ONLY (1.0
        //      filtered as parent); state file's parent mapping is also
        //      dropped on `startup` source.
        //   3. Claude TaskCreates only the announced leaves with FRESH task_ids
        //      (102/103). The parent never comes back into the harness.
        //   4. Post-rehydration: state file has only the new leaf task_ids,
        //      no parent mapping, no zombies; PLAN.md byte-identical.
        use crate::hook::HookPayload;
        use crate::writeback;

        let dir = scratch_dir();
        let plan_text = "- [ ] 1.0 Phase one\n  - [ ] 1.1 First\n  - [ ] 1.2 Second\n";
        let plan = write_plan(&dir, plan_text);

        // (1) Seed prior-session state with parent + 2 leaves.
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

        // (2) Resume on `startup` emits leaves only; parent appears as
        // context header but never as a TaskCreate bullet.
        let msg = build_resume_message(&plan, "startup")
            .unwrap()
            .expect("expected msg");
        let bullets: Vec<&str> = msg.lines().filter(|l| l.starts_with("  - ")).collect();
        for path in ["1.1", "1.2"] {
            assert!(msg.contains(path), "resume missing leaf {path}: {msg}");
        }
        assert!(
            !bullets.iter().any(|b| b.contains("Phase one")),
            "parent 1.0 leaked into a TaskCreate bullet: {bullets:?}"
        );

        // (3) Apply rehydration with fresh task_ids for the leaves only.
        let plan_before = std::fs::read_to_string(&plan).unwrap();
        for (new_id, path, title) in [("102", "1.1", "First"), ("103", "1.2", "Second")] {
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

        // (4) Final state: only leaf task_ids, no parent, no zombies, PLAN.md unchanged.
        let plan_after = std::fs::read_to_string(&plan).unwrap();
        assert_eq!(
            plan_before, plan_after,
            "PLAN.md mutated during rehydration"
        );

        let final_state = State::load(&state_path).unwrap();
        assert_eq!(
            final_state.mappings.len(),
            2,
            "expected exactly 2 leaf mappings, got {:?}",
            final_state.mappings
        );
        assert_eq!(final_state.plan_path("102"), Some("1.1"));
        assert_eq!(final_state.plan_path("103"), Some("1.2"));
        for stale in ["5", "6", "7"] {
            assert_eq!(
                final_state.plan_path(stale),
                None,
                "stale mapping for {stale} survived rehydration"
            );
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
        let bullets: Vec<&str> = msg.lines().filter(|l| l.starts_with("  - ")).collect();
        // Prompt enumerates open LEAVES (Phase 27.1: parents filtered from
        // TaskCreate bullets; Phase 27.1a: parent may still appear as a `## `
        // context header).
        assert!(msg.contains("1.1 Open child"), "got: {msg}");
        assert!(
            !bullets.iter().any(|b| b.contains("1.0 Open")),
            "parent 1.0 leaked into a TaskCreate bullet: {bullets:?}"
        );
        // Prompt notes the drop so the reader knows what to expect — drop
        // still iterates all mappings (parent + child + closed = 3).
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
    fn prompt_includes_phase_26_polish_hints() {
        // Phase 26.1/26.3/26.4: the rehydration prompt should tell the model
        //   - to ToolSearch TaskCreate if it isn't loaded (26.1)
        //   - that subject/description can share the title (26.3)
        //   - that calls are independent and should batch (26.4)
        // Lock in the wording so future edits don't silently drop a hint.
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
            msg.contains("ToolSearch") && msg.contains("select:TaskCreate"),
            "26.1 ToolSearch hint missing: {msg}"
        );
        // Phase 27.2: subject/description guidance is embedded in an
        // explicit `TaskCreate(subject=<title>, description=<plan_path>, ...)`
        // call shape. Phase 27.4 narrowed description to the plan_path so the
        // harness UI doesn't show the subject twice.
        assert!(
            msg.contains("subject=<title>") && msg.contains("description=<plan_path>"),
            "26.3/27.4 subject/description hint missing: {msg}"
        );
        assert!(
            msg.contains("batch") && msg.contains("single tool-call block"),
            "26.4 parallel-batch hint missing: {msg}"
        );
    }

    #[test]
    fn prompt_suggests_plan_path_as_description() {
        // Phase 27.4: description should be the plan_path, not the title.
        // Locks in the change so future edits don't revert to the duplicated
        // subject=description=<title> shape.
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

        let msg = build_resume_message(&plan, "startup").unwrap().unwrap();
        assert!(
            msg.contains("description=<plan_path>"),
            "should suggest plan_path as description: {msg}"
        );
        assert!(
            !msg.contains("description=<title>"),
            "should NOT suggest title-as-description (duplicates subject): {msg}"
        );
        assert!(
            msg.contains("bridge ignores `description`"),
            "should explain why description is freely chosen: {msg}"
        );
    }

    #[test]
    fn startup_source_uses_assertive_toolsearch_wording() {
        // Phase 27.3: on a fresh harness, TaskCreate is provably deferred —
        // the prompt should state that flatly, not as a conditional. Locks in
        // the assertive wording so future edits don't soften it back.
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

        let msg = build_resume_message(&plan, "startup").unwrap().unwrap();
        assert!(
            msg.contains("TaskCreate is deferred on a fresh harness"),
            "startup should use assertive wording: {msg}"
        );
        assert!(
            !msg.contains("If TaskCreate isn't loaded yet"),
            "startup should NOT use conditional wording: {msg}"
        );
    }

    #[test]
    fn clear_source_uses_assertive_toolsearch_wording() {
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

        let msg = build_resume_message(&plan, "clear").unwrap().unwrap();
        assert!(
            msg.contains("TaskCreate is deferred on a fresh harness"),
            "clear should use assertive wording: {msg}"
        );
    }

    #[test]
    fn resume_compact_sources_use_conditional_toolsearch_wording() {
        // Phase 27.3: on resume/compact the prior tool history is preserved,
        // so TaskCreate is almost always already loaded. Keep the hint as a
        // light conditional fallback rather than an assertion that would be
        // misleading 99% of the time.
        for source in ["resume", "compact"] {
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

            let msg = build_resume_message(&plan, source).unwrap().unwrap();
            assert!(
                msg.contains("If TaskCreate isn't loaded yet"),
                "source={source} should use conditional wording: {msg}"
            );
            assert!(
                !msg.contains("TaskCreate is deferred on a fresh harness"),
                "source={source} should NOT use the fresh-harness assertion: {msg}"
            );
        }
    }

    #[test]
    fn startup_clear_appends_one_audit_entry_per_dropped_mapping() {
        // Phase 26.8: every mapping the bridge drops on startup/clear
        // gets a row in the cleared.jsonl audit log so users can trace
        // missing tasks back to a specific SessionStart event. Pending
        // and resolved mappings alike are recorded — the criterion is
        // "we dropped this from state", not "it was pending".
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Open\n- [x] 2.0 Closed\n");
        let mut state = State::default();
        state.record(
            "alpha",
            Mapping {
                plan_path: "1.0".to_string(),
                last_synced_title: "Open".to_string(),
                ..Default::default()
            },
        );
        state.record(
            "beta",
            Mapping {
                plan_path: "2.0".to_string(),
                last_synced_title: "Closed".to_string(),
                last_synced_state: NodeState::Done,
                ..Default::default()
            },
        );
        write_state(&plan, &state);

        let _ = build_resume_message(&plan, "startup").unwrap().unwrap();

        let log_path = crate::audit::cleared_log_path_for(&default_state_path_for(&plan));
        let log = std::fs::read_to_string(&log_path)
            .unwrap_or_else(|_| panic!("audit log missing at {}", log_path.display()));
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2, "expected 2 audit rows, got:\n{log}");
        let mut task_ids: Vec<String> = lines
            .iter()
            .map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).unwrap();
                assert_eq!(v["reason"], "startup");
                v["task_id"].as_str().unwrap().to_string()
            })
            .collect();
        task_ids.sort();
        assert_eq!(task_ids, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn resume_source_does_not_write_audit_log() {
        // source=resume/compact preserves state — no clears happen, so
        // no audit rows should land. Avoids polluting the log with
        // non-events.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Open\n");
        let mut state = State::default();
        state.record(
            "t-1",
            Mapping {
                plan_path: "1.0".to_string(),
                last_synced_title: "Open".to_string(),
                ..Default::default()
            },
        );
        write_state(&plan, &state);

        let _ = build_resume_message(&plan, "resume").unwrap().unwrap();

        let log_path = crate::audit::cleared_log_path_for(&default_state_path_for(&plan));
        assert!(
            !log_path.exists(),
            "audit log should not be created on resume-source; found at {}",
            log_path.display()
        );
    }

    #[test]
    fn startup_seeds_pending_rehydration_with_open_plan_paths() {
        // Phase 26.5: when resume wipes mappings on a fresh harness, it must
        // also record the open plan_paths it just told Claude to TaskCreate
        // so reconcile can suppress duplicate "Added [ ] … (consider
        // TaskCreate)" drift on the next UserPromptSubmit before those
        // TaskCreates land.
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

        let _msg = build_resume_message(&plan, "startup").unwrap().unwrap();
        let after = State::load(&default_state_path_for(&plan)).unwrap();
        // Phase 27.1: pending_rehydration mirrors what was *announced* in the
        // prompt — leaves only. Parent 1.0 is filtered upstream so it never
        // lands here.
        assert_eq!(
            after
                .pending_rehydration
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["1.1".to_string()],
            "pending_rehydration should hold open leaves only; got: {:?}",
            after.pending_rehydration
        );
    }

    #[test]
    fn resume_source_does_not_touch_pending_rehydration() {
        // source=resume/compact preserves the live harness task list, so
        // there's no rehydration ask — pending_rehydration must stay empty
        // (and stay empty in the state file).
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

        let _msg = build_resume_message(&plan, "resume").unwrap().unwrap();
        let after = State::load(&default_state_path_for(&plan)).unwrap();
        assert!(
            after.pending_rehydration.is_empty(),
            "resume source should not seed pending_rehydration; got: {:?}",
            after.pending_rehydration
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
        for (id, path, title) in [
            ("5", "1.0", "Open"),
            ("6", "2.0", "Closed"),
            ("7", "3.0", "Skipped"),
        ] {
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
        assert!(msg.contains("1.0 Open"), "got: {msg}");
        assert!(!msg.contains("Closed"), "got: {msg}");
        assert!(!msg.contains("Skipped"), "got: {msg}");
    }

    #[test]
    fn resume_compact_branch_uses_tasklist_first_framing() {
        // Phase 33.1/33.3: on source=resume/compact the harness preserves
        // its task list, so the prompt must NOT claim "the harness task
        // list is empty" and must direct the agent to TaskList first
        // before any backfill TaskCreates. Locks in the new branched
        // wording so future edits don't silently revert to the pre-Phase
        // -33 one-size-fits-all prompt that triggered duplicate-task
        // creation in the wild.
        for source in ["resume", "compact"] {
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
                !msg.contains("the harness task list is empty"),
                "source={source} must NOT claim the harness is empty (it isn't): {msg}"
            );
            assert!(
                msg.contains("`TaskList` first"),
                "source={source} should direct the agent to TaskList first: {msg}"
            );
            // "Before responding" remains imperative — different first
            // action, same urgency.
            assert!(
                msg.contains("Before responding"),
                "source={source} should keep imperative framing: {msg}"
            );
        }
    }

    #[test]
    fn startup_clear_branch_keeps_empty_harness_framing() {
        // Phase 33.1: the startup/clear branch retains the "task list is
        // empty" assertion — on those sources the harness contract
        // guarantees it. Locks the wording so the branch doesn't
        // accidentally lose this when the resume/compact branch evolves.
        for source in ["startup", "clear"] {
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
                msg.contains("the harness task list is empty"),
                "source={source} should keep the empty-harness framing: {msg}"
            );
        }
    }

    #[test]
    fn resume_compact_bullets_carry_task_id() {
        // Phase 33.2: on resume/compact the bullets must include the
        // harness task_id so the agent can dedup against TaskList by id
        // (precise) rather than by subject text (fuzzy when titles drift
        // mid-session).
        for source in ["resume", "compact"] {
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
                msg.contains("(task_id=42)"),
                "source={source} bullet should include task_id=42: {msg}"
            );
        }
    }

    #[test]
    fn resume_compact_multi_leaf_bullets_each_carry_correct_task_id() {
        // Phase 33.2: with multiple mappings, each bullet pairs to its own
        // task_id (no cross-talk from the BTreeMap iteration).
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [ ] 1.1 Alpha\n  - [ ] 1.2 Beta\n  - [ ] 1.3 Gamma\n",
        );
        let mut state = State::default();
        for (id, path, title) in [
            ("100", "1.1", "Alpha"),
            ("200", "1.2", "Beta"),
            ("300", "1.3", "Gamma"),
        ] {
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

        let msg = build_resume_message(&plan, "resume").unwrap().unwrap();
        assert!(msg.contains("1.1 Alpha  (task_id=100)"), "got: {msg}");
        assert!(msg.contains("1.2 Beta  (task_id=200)"), "got: {msg}");
        assert!(msg.contains("1.3 Gamma  (task_id=300)"), "got: {msg}");
    }

    #[test]
    fn startup_clear_bullets_omit_task_id() {
        // Phase 33.2: on startup/clear the prior-session task_ids are
        // stale (the bridge just wiped them), so they must NOT be emitted
        // — they would mislead the agent into TaskGet'ing dead ids before
        // TaskCreate.
        for source in ["startup", "clear"] {
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
                !msg.contains("task_id="),
                "source={source} must omit stale task_ids from bullets: {msg}"
            );
        }
    }

    #[test]
    fn resume_compact_footer_notes_preserved_mappings() {
        // Phase 33.5: the resume/compact footer should explain that state
        // was preserved (no "cleared" claim) and reference the source
        // explicitly so the agent knows why mappings survived. Pre-Phase
        // -33 the footer falsely advertised "Writeback will detect the
        // existing PLAN.md line" — the actual failure mode on
        // resume/compact is duplicate harness tasks, not duplicate
        // PLAN.md lines. Lock in the new framing.
        for source in ["resume", "compact"] {
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
                msg.contains("preserved the state-file mappings"),
                "source={source} footer should note preserved mappings: {msg}"
            );
            assert!(
                !msg.contains("cleared"),
                "source={source} must not claim anything was cleared: {msg}"
            );
            assert!(
                msg.contains(&format!("source=`{source}`")),
                "source={source} footer should reference the source label: {msg}"
            );
        }
    }

    // -----------------------------------------------------------------
    // Phase 40.3: resume scopes rehydration prompt to active phase
    // -----------------------------------------------------------------

    #[test]
    fn resume_scopes_open_leaves_to_active_phase() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "## Phase AI - Studio\n\n- [ ] AI.0 task A\n- [ ] AI.1 task B\n\n## Phase AS - Spine\n\n- [ ] AS.0 task C\n- [ ] AS.1 task D\n",
        );
        let mut state = State::default();
        state.record(
            "t-AI-0",
            Mapping {
                plan_path: "AI.0".to_string(),
                last_synced_title: "task A".to_string(),
                ..Default::default()
            },
        );
        state.record(
            "t-AI-1",
            Mapping {
                plan_path: "AI.1".to_string(),
                last_synced_title: "task B".to_string(),
                ..Default::default()
            },
        );
        state.record(
            "t-AS-0",
            Mapping {
                plan_path: "AS.0".to_string(),
                last_synced_title: "task C".to_string(),
                ..Default::default()
            },
        );
        state.record(
            "t-AS-1",
            Mapping {
                plan_path: "AS.1".to_string(),
                last_synced_title: "task D".to_string(),
                ..Default::default()
            },
        );
        state.set_active_phase(Some("AS".to_string()));
        write_state(&plan, &state);

        let msg = build_resume_message(&plan, "resume").unwrap().unwrap();
        // Active phase announced.
        assert!(
            msg.contains("Active phase: `AS`"),
            "active-phase header surfaced: {msg}"
        );
        // AS leaves present, AI leaves filtered out.
        assert!(msg.contains("AS.0"), "AS.0 in scope: {msg}");
        assert!(msg.contains("AS.1"), "AS.1 in scope: {msg}");
        assert!(!msg.contains("AI.0"), "AI.0 should be filtered: {msg}");
        assert!(!msg.contains("AI.1"), "AI.1 should be filtered: {msg}");
    }

    #[test]
    fn resume_with_no_active_phase_is_unchanged() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "## Phase AI - Studio\n\n- [ ] AI.0 task A\n\n## Phase AS - Spine\n\n- [ ] AS.0 task C\n",
        );
        let mut state = State::default();
        state.record(
            "t-AI-0",
            Mapping {
                plan_path: "AI.0".to_string(),
                ..Default::default()
            },
        );
        state.record(
            "t-AS-0",
            Mapping {
                plan_path: "AS.0".to_string(),
                ..Default::default()
            },
        );
        write_state(&plan, &state);

        let msg = build_resume_message(&plan, "resume").unwrap().unwrap();
        assert!(
            !msg.contains("Active phase:"),
            "no active-phase header when None: {msg}"
        );
        assert!(msg.contains("AI.0"), "AI.0 loads: {msg}");
        assert!(msg.contains("AS.0"), "AS.0 loads: {msg}");
    }

    #[test]
    fn resume_active_phase_with_no_matching_open_leaves_returns_none() {
        // Activated phase has no open leaves (every leaf already done).
        // build_resume_message returns None — nothing to rehydrate.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "## Phase AI - Studio\n\n- [ ] AI.0 still open\n\n## Phase AS - Spine\n\n- [x] AS.0 done\n",
        );
        let mut state = State::default();
        state.record(
            "t-AI-0",
            Mapping {
                plan_path: "AI.0".to_string(),
                ..Default::default()
            },
        );
        state.record(
            "t-AS-0",
            Mapping {
                plan_path: "AS.0".to_string(),
                last_synced_state: NodeState::Done,
                ..Default::default()
            },
        );
        state.set_active_phase(Some("AS".to_string()));
        write_state(&plan, &state);

        // No open leaves under AS → None.
        let msg = build_resume_message(&plan, "resume").unwrap();
        assert!(msg.is_none(), "no open leaves in active phase → None");
    }
}
