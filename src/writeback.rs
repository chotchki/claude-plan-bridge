use crate::ast::{Annotation, Node, NodeState, Plan, parent_id_for};
use crate::hook::{HookOutput, HookPayload, TaskCreateInput, TaskUpdateInput, extract_task_id};
use crate::parser::parse;
use crate::serializer::serialize;
use crate::state::{Mapping, State, default_state_path_for};
use anyhow::{Context, Result, anyhow};
use std::path::Path;

/// Render an annotation as a single string for `last_synced_annotations`.
/// Keep these stable across save/load so the reconcile diff is byte-stable.
pub fn annotation_to_string(a: &Annotation) -> String {
    match a {
        Annotation::Text { text, .. } => text.clone(),
        Annotation::Bullet { text, .. } => format!("- {text}"),
        Annotation::CodeBlock { lang, content, .. } => {
            let l = lang.clone().unwrap_or_default();
            format!("```{l}\n{content}```")
        }
    }
}

pub fn annotations_to_strings(annotations: &[Annotation]) -> Vec<String> {
    annotations.iter().map(annotation_to_string).collect()
}

/// Apply a `PostToolUse(TaskCreate)` event to PLAN.md.
///
/// - If `metadata.plan_path` is set, insert at that exact id; parent must
///   already exist in PLAN.md (otherwise we error out instead of silently
///   inventing structure).
/// - If `metadata.plan_path` is absent, append to the `Inbox.0` phase
///   (auto-created at the end of PLAN.md if missing).
///
/// Idempotent: re-running with the same `task_id` is a no-op.
pub fn writeback_create(payload: &HookPayload, plan_path: &Path) -> Result<HookOutput> {
    let input: TaskCreateInput = serde_json::from_value(payload.tool_input.clone())
        .context("parse TaskCreate tool_input")?;
    let task_id = extract_task_id(&payload.tool_response)
        .ok_or_else(|| anyhow!("tool_response is missing a task id"))?;

    let state_path = default_state_path_for(plan_path);
    let mut state = State::load(&state_path)?;

    if let Some(existing) = state.plan_path(&task_id) {
        return Ok(HookOutput::context(format!(
            "plan-bridge: task {task_id} already at {existing} in PLAN.md (no-op)"
        )));
    }

    let plan_text = std::fs::read_to_string(plan_path)
        .with_context(|| format!("read {}", plan_path.display()))?;
    let mut plan = parse(&plan_text)?;

    let requested_path = input
        .metadata
        .as_ref()
        .and_then(|m| m.plan_path.clone());

    let (assigned_path, action) = match requested_path {
        Some(p) => {
            insert_at_path(&mut plan, &p, &input.subject)?;
            (p, "added".to_string())
        }
        None => {
            let p = plan.append_to_inbox(&input.subject);
            (p, "added to Inbox".to_string())
        }
    };

    std::fs::write(plan_path, serialize(&plan))
        .with_context(|| format!("write {}", plan_path.display()))?;

    // Capture last_synced_* off the leaf as it actually exists in PLAN.md.
    // For the just-inserted case that's the new node; for the idempotent
    // already-exists case it's whatever was there (possibly with a different
    // title or annotations the user added by hand).
    let mapping = match plan.find(&assigned_path) {
        Some(node) => Mapping {
            plan_path: assigned_path.clone(),
            last_synced_title: node.title.clone(),
            last_synced_state: node.state,
            last_synced_annotations: annotations_to_strings(&node.annotations),
        },
        None => Mapping {
            plan_path: assigned_path.clone(),
            last_synced_title: input.subject.clone(),
            ..Default::default()
        },
    };
    state.record(&task_id, mapping);
    state.save(&state_path)?;

    Ok(HookOutput::context(format!(
        "plan-bridge: {action} `{}` at {} in {}",
        input.subject,
        assigned_path,
        plan_path.display()
    )))
}

/// Apply a `PostToolUse(TaskUpdate)` event to PLAN.md.
///
/// - `status: "completed"` → flip `[ ]` to `[x]` at the mapped plan_path.
/// - `status: "deleted"`   → remove the line; drop the state mapping.
/// - `status: "pending" | "in_progress"` (or absent) → no-op; transient state
///   stays inside TaskCreate.
///
/// Silently no-ops when the `taskId` isn't in our state map — that means the
/// task wasn't created via the bridge in the first place, so we have nothing
/// to write back.
pub fn writeback_update(payload: &HookPayload, plan_path: &Path) -> Result<HookOutput> {
    let input: TaskUpdateInput = serde_json::from_value(payload.tool_input.clone())
        .context("parse TaskUpdate tool_input")?;

    let state_path = default_state_path_for(plan_path);
    let mut state = State::load(&state_path)?;

    let Some(node_path) = state.plan_path(&input.task_id).map(String::from) else {
        return Ok(HookOutput::silent());
    };

    let status = input.status.as_deref();

    let plan_text = std::fs::read_to_string(plan_path)
        .with_context(|| format!("read {}", plan_path.display()))?;
    let mut plan = parse(&plan_text)?;

    let action = match status {
        Some("completed") => {
            let Some(node) = plan.find_mut(&node_path) else {
                return Ok(HookOutput::silent());
            };
            if node.is_done() {
                return Ok(HookOutput::context(format!(
                    "plan-bridge: {node_path} already complete (no-op)"
                )));
            }
            node.state = NodeState::Done;
            "marked complete".to_string()
        }
        Some("deleted") => {
            // If PLAN.md already records the leaf as `[-]` (won't-do), the
            // user has expressed intent to keep it visible. Just drop the
            // state mapping; don't remove the line.
            let is_wont_do = plan
                .find(&node_path)
                .map(|n| n.state == NodeState::WontDo)
                .unwrap_or(false);
            if is_wont_do {
                state.remove(&input.task_id);
                state.save(&state_path)?;
                return Ok(HookOutput::context(format!(
                    "plan-bridge: {node_path} is [-] in PLAN.md; mapping cleared, line preserved"
                )));
            }
            if plan.remove(&node_path).is_none() {
                state.remove(&input.task_id);
                state.save(&state_path)?;
                return Ok(HookOutput::context(format!(
                    "plan-bridge: {node_path} already removed (mapping cleared)"
                )));
            }
            state.remove(&input.task_id);
            "removed".to_string()
        }
        _ => return Ok(HookOutput::silent()),
    };

    std::fs::write(plan_path, serialize(&plan))
        .with_context(|| format!("write {}", plan_path.display()))?;

    // Refresh last_synced_* off the post-mutation leaf so reconcile has a
    // current baseline. Skip if the mapping was removed (deleted case).
    if state.plan_path(&input.task_id).is_some() {
        if let Some(node) = plan.find(&node_path) {
            let updated = Mapping {
                plan_path: node_path.clone(),
                last_synced_title: node.title.clone(),
                last_synced_state: node.state,
                last_synced_annotations: annotations_to_strings(&node.annotations),
            };
            state.record(&input.task_id, updated);
        }
    }
    state.save(&state_path)?;

    Ok(HookOutput::context(format!(
        "plan-bridge: {action} {node_path}"
    )))
}

fn insert_at_path(plan: &mut Plan, plan_path: &str, subject: &str) -> Result<()> {
    if plan.find(plan_path).is_some() {
        // Already in the plan — leave it alone, just record the mapping.
        return Ok(());
    }
    let new_node = Node {
        id: plan_path.to_string(),
        title: subject.to_string(),
        state: NodeState::Pending,
        children: vec![],
        annotations: vec![],
    };
    match parent_id_for(plan_path) {
        None => plan.phases.push(new_node),
        Some(parent_id) => {
            plan.add_child_of(&parent_id, new_node)
                .map_err(|e| anyhow!(e))
                .with_context(|| format!("inserting {plan_path}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::default_state_path_for;
    use std::path::PathBuf;

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "plan-bridge-writeback-{}-{}",
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
        let path = dir.join("PLAN.md");
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn payload_for_create(task_id: &str, subject: &str, plan_path: Option<&str>) -> HookPayload {
        let metadata = plan_path.map(|p| serde_json::json!({"plan_path": p}));
        let tool_input = match metadata {
            Some(m) => serde_json::json!({
                "subject": subject,
                "description": subject,
                "metadata": m,
            }),
            None => serde_json::json!({
                "subject": subject,
                "description": subject,
            }),
        };
        HookPayload {
            session_id: String::new(),
            cwd: String::new(),
            hook_event_name: "PostToolUse".to_string(),
            tool_name: "TaskCreate".to_string(),
            tool_input,
            tool_response: serde_json::json!({"id": task_id}),
        }
    }

    #[test]
    fn inserts_leaf_under_existing_parent() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n",
        );
        let payload = payload_for_create("t-1", "New subtask", Some("1.1.1"));
        let out = writeback_create(&payload, &plan).unwrap();
        let new_contents = std::fs::read_to_string(&plan).unwrap();
        assert!(
            new_contents.contains("    - [ ] 1.1.1 New subtask"),
            "got:\n{new_contents}"
        );
        assert!(out.to_json().contains("added"));
    }

    #[test]
    fn inserts_task_under_phase() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        let payload = payload_for_create("t-1", "First task", Some("1.1"));
        writeback_create(&payload, &plan).unwrap();
        let new_contents = std::fs::read_to_string(&plan).unwrap();
        assert!(new_contents.contains("  - [ ] 1.1 First task"), "got:\n{new_contents}");
    }

    #[test]
    fn records_state_mapping() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        let payload = payload_for_create("task-abc", "x", Some("1.1"));
        writeback_create(&payload, &plan).unwrap();
        let state_path = default_state_path_for(&plan);
        let state = State::load(&state_path).unwrap();
        assert_eq!(state.plan_path("task-abc"), Some("1.1"));
    }

    #[test]
    fn idempotent_when_task_id_already_mapped() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        let payload = payload_for_create("task-abc", "x", Some("1.1"));
        writeback_create(&payload, &plan).unwrap();
        let first = std::fs::read_to_string(&plan).unwrap();
        // Run again with same task_id but different subject — should be no-op.
        let again = payload_for_create("task-abc", "should not appear", Some("1.1"));
        writeback_create(&again, &plan).unwrap();
        let second = std::fs::read_to_string(&plan).unwrap();
        assert_eq!(first, second, "second run mutated PLAN.md: {second}");
    }

    #[test]
    fn appends_to_inbox_when_no_plan_path() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        let payload = payload_for_create("t-loose", "loose task", None);
        writeback_create(&payload, &plan).unwrap();
        let new_contents = std::fs::read_to_string(&plan).unwrap();
        assert!(new_contents.contains("- [ ] Inbox.0 Inbox"), "got:\n{new_contents}");
        assert!(new_contents.contains("  - [ ] Inbox.1 loose task"), "got:\n{new_contents}");
    }

    #[test]
    fn errors_when_parent_missing() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        let payload = payload_for_create("t-1", "x", Some("9.9.9"));
        let err = writeback_create(&payload, &plan).unwrap_err();
        assert!(err.to_string().contains("9.9"), "err: {err}");
    }

    #[test]
    fn errors_when_no_task_id_in_response() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        let mut payload = payload_for_create("ignored", "x", Some("1.1"));
        payload.tool_response = serde_json::json!({});
        let err = writeback_create(&payload, &plan).unwrap_err();
        assert!(err.to_string().contains("task id"));
    }

    fn payload_for_update(task_id: &str, status: &str) -> HookPayload {
        HookPayload {
            session_id: String::new(),
            cwd: String::new(),
            hook_event_name: "PostToolUse".to_string(),
            tool_name: "TaskUpdate".to_string(),
            tool_input: serde_json::json!({"taskId": task_id, "status": status}),
            tool_response: serde_json::json!({}),
        }
    }

    #[test]
    fn update_completed_flips_checkbox() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        writeback_create(&payload_for_create("t-1", "Task", Some("1.1")), &plan).unwrap();
        writeback_update(&payload_for_update("t-1", "completed"), &plan).unwrap();
        let contents = std::fs::read_to_string(&plan).unwrap();
        assert!(contents.contains("  - [x] 1.1 Task"), "got:\n{contents}");
    }

    #[test]
    fn update_completed_idempotent() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        writeback_create(&payload_for_create("t-1", "Task", Some("1.1")), &plan).unwrap();
        writeback_update(&payload_for_update("t-1", "completed"), &plan).unwrap();
        let after_first = std::fs::read_to_string(&plan).unwrap();
        writeback_update(&payload_for_update("t-1", "completed"), &plan).unwrap();
        let after_second = std::fs::read_to_string(&plan).unwrap();
        assert_eq!(after_first, after_second);
    }

    #[test]
    fn update_deleted_removes_line_and_mapping() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        writeback_create(&payload_for_create("t-1", "Task", Some("1.1")), &plan).unwrap();
        writeback_update(&payload_for_update("t-1", "deleted"), &plan).unwrap();
        let contents = std::fs::read_to_string(&plan).unwrap();
        assert!(!contents.contains("1.1 Task"), "should be gone:\n{contents}");
        let state = State::load(&default_state_path_for(&plan)).unwrap();
        assert_eq!(state.plan_path("t-1"), None);
    }

    #[test]
    fn update_pending_is_no_op() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        writeback_create(&payload_for_create("t-1", "Task", Some("1.1")), &plan).unwrap();
        let before = std::fs::read_to_string(&plan).unwrap();
        writeback_update(&payload_for_update("t-1", "pending"), &plan).unwrap();
        writeback_update(&payload_for_update("t-1", "in_progress"), &plan).unwrap();
        let after = std::fs::read_to_string(&plan).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn update_unmapped_task_is_silent_no_op() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        let before = std::fs::read_to_string(&plan).unwrap();
        let out = writeback_update(&payload_for_update("never-created", "completed"), &plan).unwrap();
        let after = std::fs::read_to_string(&plan).unwrap();
        assert_eq!(before, after);
        assert_eq!(out.to_json(), "{}", "should be silent");
    }

    #[test]
    fn update_deleted_on_wont_do_leaf_keeps_line() {
        let dir = scratch_dir();
        // Pre-existing PLAN.md with a `[-]` leaf the user added by hand.
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [-] 1.1 Skipped\n");
        // Bridge tracks it.
        writeback_create(&payload_for_create("t-1", "Skipped", Some("1.1")), &plan).unwrap();
        // Claude calls TaskUpdate(deleted). The line should remain.
        writeback_update(&payload_for_update("t-1", "deleted"), &plan).unwrap();
        let contents = std::fs::read_to_string(&plan).unwrap();
        assert!(
            contents.contains("- [-] 1.1 Skipped"),
            "the [-] line should be preserved: {contents}"
        );
        let state = State::load(&default_state_path_for(&plan)).unwrap();
        assert_eq!(state.plan_path("t-1"), None, "mapping should be cleared");
    }

    #[test]
    fn no_op_when_plan_path_already_exists_but_state_missing() {
        // PLAN.md already has the node; state file doesn't track this task yet.
        // Expected: don't double-insert; do record the mapping.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [ ] 1.1 Already here\n",
        );
        let payload = payload_for_create("t-new", "Already here", Some("1.1"));
        writeback_create(&payload, &plan).unwrap();
        let new_contents = std::fs::read_to_string(&plan).unwrap();
        // Should only contain ONE "1.1 Already here" line.
        let count = new_contents.matches("- [ ] 1.1").count();
        assert_eq!(count, 1, "PLAN.md got duplicated:\n{new_contents}");
        let state = State::load(&default_state_path_for(&plan)).unwrap();
        assert_eq!(state.plan_path("t-new"), Some("1.1"));
    }
}
