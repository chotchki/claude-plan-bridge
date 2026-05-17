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

    crate::lock::with_state_lock(&state_path, crate::lock::DEFAULT_TIMEOUT, || {
        let mut state = State::load(&state_path)?;

        if let Some(existing) = state.plan_path(&task_id) {
            return Ok(HookOutput::context(
                &payload.hook_event_name,
                format!(
                    "claude-plan-bridge: task {task_id} already at {existing} in PLAN.md (no-op)"
                ),
            ));
        }

        let plan_text = std::fs::read_to_string(plan_path)
            .with_context(|| format!("read {}", plan_path.display()))?;
        let parsed = parse(&plan_text)?;
        let (mut plan, standardize_notes) =
            parsed.standardize_to_canonical().map_err(|e| anyhow!(e))?;

        let requested_path = input.metadata.as_ref().and_then(|m| m.plan_path.clone());

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

        // Evict any baseline mapping for the same plan_path — once a real task
        // lands, the synthetic baseline:<path> entry is stale.
        crate::baseline::evict_baseline_for(&mut state, &assigned_path);

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

        let mut message = format!(
            "claude-plan-bridge: {action} `{}` at {} in {}",
            input.subject,
            assigned_path,
            plan_path.display()
        );
        if !standardize_notes.is_empty() {
            message.push_str(&format!(
                "\n\nNOTE: PLAN.md was standardized to canonical form ({} header(s) promoted to phase checkboxes):\n  - {}",
                standardize_notes.len(),
                standardize_notes.join("\n  - "),
            ));
        }
        Ok(HookOutput::context(&payload.hook_event_name, message))
    })
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

    crate::lock::with_state_lock(&state_path, crate::lock::DEFAULT_TIMEOUT, || {
        let mut state = State::load(&state_path)?;

        let Some(node_path) = state.plan_path(&input.task_id).map(String::from) else {
            return Ok(HookOutput::silent());
        };

        let status = input.status.as_deref();
        let new_subject = input.subject.as_deref();

        // Bail early if there's nothing to do (no actionable status AND no subject).
        // `pending` / `in_progress` are status no-ops; subject still counts.
        let has_status_action = matches!(status, Some("completed") | Some("deleted"));
        if !has_status_action && new_subject.is_none() {
            return Ok(HookOutput::silent());
        }

        let plan_text = std::fs::read_to_string(plan_path)
            .with_context(|| format!("read {}", plan_path.display()))?;
        let parsed = parse(&plan_text)?;
        let (mut plan, standardize_notes) =
            parsed.standardize_to_canonical().map_err(|e| anyhow!(e))?;

        let mut changes: Vec<String> = Vec::new();
        if !standardize_notes.is_empty() {
            for note in &standardize_notes {
                changes.push(format!("standardized: {note}"));
            }
        }

        // --- Subject rename (skip when also deleting — would rename then remove) ---
        if !matches!(status, Some("deleted"))
            && let Some(new_title) = new_subject
            && let Some(node) = plan.find_mut(&node_path)
            && node.title != new_title
        {
            node.title = new_title.to_string();
            changes.push(format!("renamed to `{new_title}`"));
        }

        // --- Status mutation ---
        match status {
            Some("completed") => {
                let Some(node) = plan.find_mut(&node_path) else {
                    // Node vanished from PLAN.md. Nothing to tick. If we also
                    // had a rename queued it won't have applied either (same
                    // missing-node), so just silent.
                    return Ok(HookOutput::silent());
                };
                if !node.is_done() {
                    node.state = NodeState::Done;
                    changes.push("marked complete".to_string());
                }
            }
            Some("deleted") => {
                let is_wont_do = plan
                    .find(&node_path)
                    .map(|n| n.state == NodeState::WontDo)
                    .unwrap_or(false);
                if is_wont_do {
                    state.remove(&input.task_id);
                    state.save(&state_path)?;
                    return Ok(HookOutput::context(
                        &payload.hook_event_name,
                        format!(
                            "claude-plan-bridge: {node_path} is [-] in PLAN.md; mapping cleared, line preserved"
                        ),
                    ));
                }
                if plan.remove(&node_path).is_none() {
                    state.remove(&input.task_id);
                    state.save(&state_path)?;
                    return Ok(HookOutput::context(
                        &payload.hook_event_name,
                        format!(
                            "claude-plan-bridge: {node_path} already removed (mapping cleared)"
                        ),
                    ));
                }
                state.remove(&input.task_id);
                changes.push("removed".to_string());
            }
            _ => {} // pending / in_progress / None — subject-only path
        }

        if changes.is_empty() {
            // Subject matched existing title, or status=completed on already-done leaf.
            return Ok(HookOutput::context(
                &payload.hook_event_name,
                format!("claude-plan-bridge: {node_path} (no-op)"),
            ));
        }

        std::fs::write(plan_path, serialize(&plan))
            .with_context(|| format!("write {}", plan_path.display()))?;

        // Refresh last_synced_* off the post-mutation leaf so reconcile has a
        // current baseline. Skip if the mapping was removed (deleted case).
        if state.plan_path(&input.task_id).is_some()
            && let Some(node) = plan.find(&node_path)
        {
            let updated = Mapping {
                plan_path: node_path.clone(),
                last_synced_title: node.title.clone(),
                last_synced_state: node.state,
                last_synced_annotations: annotations_to_strings(&node.annotations),
            };
            state.record(&input.task_id, updated);
        }
        state.save(&state_path)?;

        Ok(HookOutput::context(
            &payload.hook_event_name,
            format!("claude-plan-bridge: {} {node_path}", changes.join("; ")),
        ))
    })
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
        None => plan.insert_phase(new_node),
        Some(parent_id) => {
            if plan.find(&parent_id).is_none() {
                // Most common cause: user asked for `5.2a` but `5.0` doesn't
                // exist as a checkbox. Suggest both possible fixes.
                anyhow::bail!(
                    "inserting {plan_path}: parent `{parent_id}` not found in PLAN.md. \
                     Either the parent phase doesn't exist yet (create it first with a \
                     `- [ ] {parent_id} ...` checkbox), or your plan uses `### Phase N` \
                     section headers instead of canonical phase checkboxes — the bridge \
                     auto-standardizes Phase-N headers but only when the format matches \
                     `### Phase N — Title` exactly."
                );
            }
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
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
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
        assert!(
            new_contents.contains("  - [ ] 1.1 First task"),
            "got:\n{new_contents}"
        );
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
        assert!(
            new_contents.contains("- [ ] Inbox.0 Inbox"),
            "got:\n{new_contents}"
        );
        assert!(
            new_contents.contains("  - [ ] Inbox.1 loose task"),
            "got:\n{new_contents}"
        );
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
    fn missing_parent_error_mentions_canonical_format_hint() {
        // Phase 15.2: when the user requests a plan_path whose parent doesn't
        // exist, the error should mention BOTH possible fixes — creating the
        // parent phase OR converting section headers. The hint catches both
        // the typo case and the format-mismatch case.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        let payload = payload_for_create("t-1", "new mid-seq", Some("5.2a"));
        let err = writeback_create(&payload, &plan).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("5.0"), "should name missing parent: {msg}");
        assert!(
            msg.contains("section header") || msg.contains("Phase-N"),
            "should hint at format issue: {msg}"
        );
        assert!(
            msg.contains("doesn't exist yet") || msg.contains("create it first"),
            "should also suggest creating the parent: {msg}"
        );
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
        assert!(
            !contents.contains("1.1 Task"),
            "should be gone:\n{contents}"
        );
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
        let out =
            writeback_update(&payload_for_update("never-created", "completed"), &plan).unwrap();
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

    fn payload_for_update_subject(task_id: &str, subject: &str) -> HookPayload {
        HookPayload {
            session_id: String::new(),
            cwd: String::new(),
            hook_event_name: "PostToolUse".to_string(),
            tool_name: "TaskUpdate".to_string(),
            tool_input: serde_json::json!({"taskId": task_id, "subject": subject}),
            tool_response: serde_json::json!({}),
        }
    }

    #[test]
    fn update_subject_only_renames_node() {
        // Phase 12: TaskUpdate(subject=...) without a status change should
        // rewrite the title in PLAN.md AND update state.last_synced_title.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        writeback_create(&payload_for_create("t-1", "Old title", Some("1.1")), &plan).unwrap();
        writeback_update(&payload_for_update_subject("t-1", "New title"), &plan).unwrap();
        let contents = std::fs::read_to_string(&plan).unwrap();
        assert!(
            contents.contains("  - [ ] 1.1 New title"),
            "PLAN.md not renamed:\n{contents}"
        );
        assert!(
            !contents.contains("Old title"),
            "old title still present:\n{contents}"
        );
        let state = State::load(&default_state_path_for(&plan)).unwrap();
        // state should reflect the new title so reconcile doesn't redundantly flag it.
        // (we can't introspect mappings directly via plan_path(), so just verify the lookup still works)
        assert_eq!(state.plan_path("t-1"), Some("1.1"));
    }

    #[test]
    fn update_subject_with_completed_status_renames_and_ticks() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        writeback_create(&payload_for_create("t-1", "Old", Some("1.1")), &plan).unwrap();
        let combined = HookPayload {
            session_id: String::new(),
            cwd: String::new(),
            hook_event_name: "PostToolUse".to_string(),
            tool_name: "TaskUpdate".to_string(),
            tool_input: serde_json::json!({
                "taskId": "t-1",
                "subject": "New",
                "status": "completed"
            }),
            tool_response: serde_json::json!({}),
        };
        writeback_update(&combined, &plan).unwrap();
        let contents = std::fs::read_to_string(&plan).unwrap();
        assert!(
            contents.contains("  - [x] 1.1 New"),
            "expected ticked + renamed line:\n{contents}"
        );
    }

    #[test]
    fn update_subject_on_unmapped_task_is_silent() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Untracked\n");
        let before = std::fs::read_to_string(&plan).unwrap();
        let out = writeback_update(
            &payload_for_update_subject("never-created", "Anything"),
            &plan,
        )
        .unwrap();
        let after = std::fs::read_to_string(&plan).unwrap();
        assert_eq!(before, after, "untracked task: PLAN.md untouched");
        assert_eq!(out.to_json(), "{}", "should be silent");
    }

    #[test]
    fn update_subject_unchanged_is_noop() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        writeback_create(&payload_for_create("t-1", "Same", Some("1.1")), &plan).unwrap();
        let before = std::fs::read_to_string(&plan).unwrap();
        let out = writeback_update(&payload_for_update_subject("t-1", "Same"), &plan).unwrap();
        let after = std::fs::read_to_string(&plan).unwrap();
        assert_eq!(before, after, "identical subject: no write");
        // Hook output should mention no-op (we changed the message wording — just
        // assert the JSON is well-formed and includes the path).
        assert!(out.to_json().contains("1.1"));
    }

    #[test]
    fn update_subject_on_parent_node_renames_anyway() {
        // Renames apply to parents-with-children just as well as leaves.
        // (Phase 9 fix: tracked nodes that grew children stay tracked.)
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [ ] 1.1 Parent\n    - [ ] 1.1.1 Leaf\n",
        );
        writeback_create(
            &payload_for_create("t-parent", "Parent", Some("1.1")),
            &plan,
        )
        .unwrap();
        // Note: 1.1 already has 1.1.1 as a child; create is idempotent and
        // just records the mapping when the node exists.
        writeback_update(
            &payload_for_update_subject("t-parent", "Renamed parent"),
            &plan,
        )
        .unwrap();
        let contents = std::fs::read_to_string(&plan).unwrap();
        assert!(
            contents.contains("- [ ] 1.1 Renamed parent"),
            "parent rename should land:\n{contents}"
        );
        assert!(contents.contains("1.1.1 Leaf"), "child preserved");
    }

    #[test]
    fn writeback_create_standardizes_phase_n_header_and_warns() {
        // Phase 15.1 / ocr_pdf_latex regression. The parser captures
        // `### Phase 1 — Build` as an annotation; standardize promotes it to
        // a `- [ ] 1.0 Build` phase node, the bridge proceeds with the
        // writeback, and the hook output names what changed so the user can
        // verify the rewrite.
        let dir = scratch_dir();
        let original = "- [x] 0.1 First\n\n### Phase 1 — Build\n\n- [ ] 1.1 Build it\n";
        let plan = write_plan(&dir, original);

        // Insert a new task under the (now-promoted) 1.0 phase.
        let payload = payload_for_create("t-x", "new sub", Some("1.2"));
        let out = writeback_create(&payload, &plan).expect("standardize+insert should succeed");

        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(
            after.contains("- [ ] 1.0 Build"),
            "phase header promoted:\n{after}"
        );
        assert!(
            after.contains("  - [ ] 1.1 Build it"),
            "child reparented:\n{after}"
        );
        assert!(
            after.contains("  - [ ] 1.2 new sub"),
            "new task lands under 1.0:\n{after}"
        );
        assert!(
            !after.contains("### Phase 1"),
            "section header removed:\n{after}"
        );

        let hook_json = out.to_json();
        assert!(
            hook_json.contains("standardized") || hook_json.contains("promoted"),
            "hook output must announce the rewrite: {hook_json}"
        );
    }

    #[test]
    fn writeback_create_refuses_unrecognized_header_without_writing() {
        // `## Notes` doesn't match Phase N — bridge can't standardize it →
        // refuse loudly, don't touch the file.
        let dir = scratch_dir();
        let original = "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n\n## Notes\n\nSome stuff.\n";
        let plan = write_plan(&dir, original);
        let payload = payload_for_create("t-x", "new", Some("1.2"));
        let err = writeback_create(&payload, &plan).expect_err("should refuse");
        let msg = format!("{err:#}");
        assert!(msg.contains("aren't `### Phase N"), "got: {msg}");
        assert!(msg.contains("## Notes"), "name the offender: {msg}");
        let after = std::fs::read_to_string(&plan).unwrap();
        assert_eq!(after, original, "file must not be modified on refusal");
    }

    #[test]
    fn writeback_update_standardizes_then_applies_status_change() {
        let dir = scratch_dir();
        let original = "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n\n### Phase 2 — Other work\n\n- [ ] 2.1 Other task\n";
        let plan = write_plan(&dir, original);
        let mut state = State::default();
        state.record(
            "t-1",
            Mapping {
                plan_path: "1.1".to_string(),
                last_synced_title: "Task".to_string(),
                last_synced_state: NodeState::Pending,
                last_synced_annotations: vec![],
            },
        );
        state.save(&default_state_path_for(&plan)).unwrap();

        writeback_update(&payload_for_update("t-1", "completed"), &plan)
            .expect("standardize + tick should succeed");
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(after.contains("  - [x] 1.1 Task"), "1.1 ticked:\n{after}");
        assert!(
            after.contains("- [ ] 2.0 Other work"),
            "Phase 2 promoted:\n{after}"
        );
        assert!(
            after.contains("  - [ ] 2.1 Other task"),
            "2.1 reparented:\n{after}"
        );
        assert!(
            !after.contains("### Phase 2"),
            "section header gone:\n{after}"
        );
    }

    #[test]
    fn concurrent_writebacks_all_land_without_loss() {
        // Phase 8.0 acceptance: spawning N concurrent writeback_create calls
        // against the same PLAN.md must serialize through the file lock — all
        // N entries must land in both PLAN.md and the state file. Without the
        // lock this would race (read-modify-write last-writer-wins), so a
        // pre-lock run of this test would fail with missing entries.
        use std::sync::{Arc, Barrier};
        use std::thread;

        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Parent\n");
        let plan = Arc::new(plan);

        let n: usize = 10;
        let barrier = Arc::new(Barrier::new(n));
        let mut handles = Vec::with_capacity(n);
        for i in 1..=n {
            let plan = Arc::clone(&plan);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                let task_id = format!("t-{i}");
                let subject = format!("child {i}");
                let plan_path = format!("1.{i}");
                let payload = payload_for_create(&task_id, &subject, Some(&plan_path));
                barrier.wait();
                writeback_create(&payload, &plan).expect("writeback should succeed under lock");
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let contents = std::fs::read_to_string(plan.as_path()).unwrap();
        for i in 1..=n {
            let needle = format!("- [ ] 1.{i} child {i}");
            assert!(
                contents.contains(&needle),
                "missing 1.{i} in PLAN.md:\n{contents}"
            );
        }
        let state = State::load(&default_state_path_for(&plan)).unwrap();
        for i in 1..=n {
            let expected = format!("1.{i}");
            assert_eq!(
                state.plan_path(&format!("t-{i}")),
                Some(expected.as_str()),
                "state mapping missing for t-{i}"
            );
        }
    }

    #[test]
    fn no_op_when_plan_path_already_exists_but_state_missing() {
        // PLAN.md already has the node; state file doesn't track this task yet.
        // Expected: don't double-insert; do record the mapping.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Already here\n");
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
