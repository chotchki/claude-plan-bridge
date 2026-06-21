use anyhow::{Context, Result};
use std::path::Path;

use crate::ast::NodeState;
use crate::parser::parse;
use crate::serializer::serialize;
use crate::state::{State, default_state_path_for};

/// Defer a node at `plan_path`: flip its checkbox to `[>]` (Backlog) and
/// append a FORMATv2 nested-subtree entry under the bottom backlog section
/// recording the source path + date. Drops any state mapping pointing at
/// this path so the harness UI stops tracking the deferred task.
///
/// Phase 38.6: the backlog entry preserves subtree structure. A leaf becomes
/// a single bullet `- <id> - <title> *(deferred from phase X on date)*`. A
/// non-leaf node also emits its children nested below, so the entire
/// deferred subtree is captured as a record. The original subtree stays in
/// place under the phase (state-flipped to `[>]`) — archive sweeps it with
/// the phase, while the backlog entry survives the sweep.
///
/// Errors when:
/// - the node doesn't exist in PLAN.md
/// - the node is already `[x]` (Done) or `[-]` (WontDo) — deferring resolved
///   work doesn't make sense; the user should `plan_uncheck` first if they
///   really meant it
///
/// No-ops on a node that's already `[>]` (idempotent).
pub fn backlog(plan_path: &Path, id: &str, date: &str) -> Result<String> {
    let state_path = default_state_path_for(plan_path);
    crate::lock::with_state_lock(&state_path, crate::lock::DEFAULT_TIMEOUT, || {
        let text = std::fs::read_to_string(plan_path)
            .with_context(|| format!("read {}", plan_path.display()))?;
        let mut plan = parse(&text)?;

        // Locate the source phase first — needed for the backlog note's
        // "deferred from phase X" provenance. The phase id is the first
        // dot-separated segment of the leaf's plan_path.
        let source_phase = id.split('.').next().unwrap_or(id).to_string();

        let node = plan
            .find(id)
            .ok_or_else(|| anyhow::anyhow!("no node with id `{id}` in PLAN.md"))?;
        match node.state {
            NodeState::Backlog => return Ok(format!("{id} was already deferred")),
            NodeState::Done | NodeState::WontDo => {
                return Err(anyhow::anyhow!(
                    "refusing to backlog `{id}`: state is `{:?}` (uncheck first if you really mean it)",
                    node.state
                ));
            }
            NodeState::Pending => {}
        }
        // Snapshot the subtree before mutating state — the backlog entry
        // records the node's children as they are at deferral time.
        let snapshot = node.clone();

        if let Some(node) = plan.find_mut(id) {
            node.state = NodeState::Backlog;
        }
        // Phase 35.2a: deferrals go to the canonical bottom Backlog section.
        // Consolidate first so any legacy preamble Backlog merges down.
        plan.consolidate_backlog();
        plan.append_backlog_subtree(&snapshot, &source_phase, date);

        std::fs::write(plan_path, serialize(&plan))
            .with_context(|| format!("write {}", plan_path.display()))?;

        // Drop state mappings pointing at this path so the harness UI stops
        // tracking the deferred task. Multiple mappings are possible in
        // principle (rehydration pending) — clear all of them.
        let mut state = State::load(&state_path)?;
        let to_drop: Vec<String> = state
            .mappings
            .iter()
            .filter(|(_, m)| m.plan_path == id)
            .map(|(tid, _)| tid.clone())
            .collect();
        for tid in &to_drop {
            state.remove(tid);
        }
        if !to_drop.is_empty() {
            state.save(&state_path)?;
        }

        Ok(format!(
            "backlogged {id} (flipped to [>], promoted to ## Backlog); dropped {} mapping(s)",
            to_drop.len()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::write_plan;
    use std::path::PathBuf;

    fn scratch_dir() -> PathBuf {
        crate::test_utils::scratch_dir("backlog")
    }

    #[test]
    fn backlog_flips_pending_leaf_and_promotes_in_v2_form() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "## Phase 1 - Phase\n  - [ ] 1.1 Task\n");
        let msg = backlog(&plan, "1.1", "2026-05-17").unwrap();
        assert!(msg.contains("backlogged 1.1"));
        let after = std::fs::read_to_string(&plan).unwrap();
        // Subtree stays in phase with state flipped. Serializer writes the
        // task at column 0 with the ` - ` (hyphen-space) separator.
        assert!(
            after.contains("- [>] 1.1 - Task"),
            "v2 flipped leaf:\n{after}"
        );
        // Backlog entry uses FORMATv2 ` - id - title *(deferred from …)*`.
        assert!(
            after.contains("- 1.1 - Task *(deferred from phase `1` on 2026-05-17)*"),
            "v2 backlog bullet:\n{after}"
        );
    }

    #[test]
    fn backlog_subtree_preserves_children_as_nested_bullets() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "## Phase 1 - Phase\n  - [ ] 1.1 Parent task\n    - [ ] 1.1.0 first child\n    - [ ] 1.1.1 second child\n",
        );
        let msg = backlog(&plan, "1.1", "2026-05-22").unwrap();
        assert!(msg.contains("backlogged 1.1"));
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(
            after.contains("- 1.1 - Parent task *(deferred from phase `1` on 2026-05-22)*"),
            "top-line with markers:\n{after}"
        );
        assert!(
            after.contains("  - 1.1.0 - first child"),
            "first child nested:\n{after}"
        );
        assert!(
            after.contains("  - 1.1.1 - second child"),
            "second child nested:\n{after}"
        );
    }

    #[test]
    fn backlog_subtree_is_idempotent_on_top_line() {
        // Running backlog twice on the same plan_path shouldn't duplicate
        // the backlog top-line (would-be re-deferral after the parent
        // already flipped to `[>]` is an early-return "already deferred").
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "## Phase 1 - Phase\n  - [ ] 1.1 Task\n    - [ ] 1.1.0 child\n",
        );
        backlog(&plan, "1.1", "2026-05-22").unwrap();
        let after_first = std::fs::read_to_string(&plan).unwrap();
        // Second call short-circuits because state is already `[>]`.
        let msg = backlog(&plan, "1.1", "2026-05-22").unwrap();
        assert!(msg.contains("already deferred"));
        let after_second = std::fs::read_to_string(&plan).unwrap();
        assert_eq!(after_first, after_second, "second call doesn't mutate");
    }

    #[test]
    fn backlog_idempotent() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "## Phase 1 - Phase\n  - [>] 1.1 Already deferred\n");
        let msg = backlog(&plan, "1.1", "2026-05-17").unwrap();
        assert!(msg.contains("already deferred"));
    }

    #[test]
    fn backlog_refuses_done_leaf() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "## Phase 1 - Phase\n  - [x] 1.1 Done\n");
        let err = backlog(&plan, "1.1", "2026-05-17").unwrap_err();
        assert!(err.to_string().contains("refusing"), "got: {err}");
    }

    #[test]
    fn backlog_refuses_wont_do_leaf() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "## Phase 1 - Phase\n  - [-] 1.1 Skipped\n");
        let err = backlog(&plan, "1.1", "2026-05-17").unwrap_err();
        assert!(err.to_string().contains("refusing"), "got: {err}");
    }

    #[test]
    fn backlog_errors_on_unknown_id() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "## Phase 1 - Phase\n");
        let err = backlog(&plan, "9.9", "2026-05-17").unwrap_err();
        assert!(err.to_string().contains("no node with id"));
    }
}
