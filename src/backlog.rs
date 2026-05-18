use anyhow::{Context, Result};
use std::path::Path;

use crate::ast::NodeState;
use crate::parser::parse;
use crate::serializer::serialize;
use crate::state::{State, default_state_path_for};

/// Defer a node at `plan_path`: flip its checkbox to `[>]` (Backlog) and
/// append a bullet under `## Backlog (not yet phased)` recording the source
/// path + date. Drops any state mapping pointing at this path so the harness
/// UI stops tracking the deferred task.
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
        let title = node.title.clone();

        if let Some(node) = plan.find_mut(id) {
            node.state = NodeState::Backlog;
        }
        plan.append_backlog_entry(id, &title, date);

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
    use std::path::PathBuf;

    fn scratch_dir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "plan-bridge-backlog-{}-{}",
            std::process::id(),
            uuid_like()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn uuid_like() -> String {
        format!("{:?}", std::time::SystemTime::now())
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect()
    }

    fn write_plan(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("PLAN.md");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn backlog_flips_pending_leaf_and_promotes() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        let msg = backlog(&plan, "1.1", "2026-05-17").unwrap();
        assert!(msg.contains("backlogged 1.1"));
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(after.contains("- [>] 1.1 Task"));
        assert!(after.contains("## Backlog (not yet phased)"));
        assert!(after.contains("- **Task** — deferred from 1.1 on 2026-05-17."));
    }

    #[test]
    fn backlog_idempotent() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [>] 1.1 Already deferred\n");
        let msg = backlog(&plan, "1.1", "2026-05-17").unwrap();
        assert!(msg.contains("already deferred"));
    }

    #[test]
    fn backlog_refuses_done_leaf() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [x] 1.1 Done\n");
        let err = backlog(&plan, "1.1", "2026-05-17").unwrap_err();
        assert!(err.to_string().contains("refusing"), "got: {err}");
    }

    #[test]
    fn backlog_refuses_wont_do_leaf() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [-] 1.1 Skipped\n");
        let err = backlog(&plan, "1.1", "2026-05-17").unwrap_err();
        assert!(err.to_string().contains("refusing"), "got: {err}");
    }

    #[test]
    fn backlog_errors_on_unknown_id() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        let err = backlog(&plan, "9.9", "2026-05-17").unwrap_err();
        assert!(err.to_string().contains("no node with id"));
    }
}
