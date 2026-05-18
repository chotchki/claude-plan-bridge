use crate::ast::{Annotation, Node, NodeState, Plan, parent_id_for};
use crate::hook::{HookOutput, HookPayload, TaskCreateInput, TaskUpdateInput, extract_task_id};
use crate::parser::parse;
use crate::serializer::serialize;
use crate::state::{Mapping, State, default_state_path_for};
use anyhow::{Context, Result, anyhow};
use std::path::Path;

/// Render an annotation as a single string for `last_synced_annotations`.
/// Keep these stable across save/load so the reconcile diff is byte-stable.
/// `Blank` annotations are intentionally skipped — reconcile shouldn't drift
/// on whitespace alone.
pub fn annotation_to_string(a: &Annotation) -> Option<String> {
    match a {
        Annotation::Text { text, .. } => Some(text.clone()),
        Annotation::Bullet { text, .. } => Some(format!("- {text}")),
        Annotation::CodeBlock { lang, content, .. } => {
            let l = lang.clone().unwrap_or_default();
            Some(format!("```{l}\n{content}```"))
        }
        Annotation::Blank { .. } => None,
    }
}

pub fn annotations_to_strings(annotations: &[Annotation]) -> Vec<String> {
    annotations
        .iter()
        .filter_map(annotation_to_string)
        .collect()
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
        let requested_path = input.metadata.as_ref().and_then(|m| m.plan_path.clone());

        // No-op check: same task_id already mapped, and either no incoming
        // plan_path or one that matches the existing mapping. Different
        // plan_path is an inconsistency — refuse to silently re-link.
        if let Some(existing) = state.plan_path(&task_id) {
            let existing = existing.to_string();
            match requested_path.as_deref() {
                None => {
                    return Ok(HookOutput::context(
                        &payload.hook_event_name,
                        format!(
                            "claude-plan-bridge: task {task_id} already at {existing} in PLAN.md (no-op)"
                        ),
                    ));
                }
                Some(req) if req == existing => {
                    return Ok(HookOutput::context(
                        &payload.hook_event_name,
                        format!(
                            "claude-plan-bridge: task {task_id} already at {existing} in PLAN.md (no-op)"
                        ),
                    ));
                }
                Some(req) => {
                    return Ok(HookOutput::context(
                        &payload.hook_event_name,
                        format!(
                            "claude-plan-bridge: WARNING task {task_id} is already mapped to {existing}, \
                             but TaskCreate carries plan_path={req}. Refusing to silently move it. \
                             If you meant to retarget, delete the task and re-create with the desired plan_path."
                        ),
                    ));
                }
            }
        }

        let plan_text = std::fs::read_to_string(plan_path)
            .with_context(|| format!("read {}", plan_path.display()))?;
        let parsed = parse(&plan_text)?;

        let (plan, assigned_path, action) = match requested_path {
            Some(p) => {
                let plan = insert_at_path(parsed, &p, &input.subject)?;
                (plan, p, "added".to_string())
            }
            None => {
                let mut plan = parsed;
                let p = plan.append_to_inbox(&input.subject);
                (plan, p, "added to Inbox".to_string())
            }
        };

        std::fs::write(plan_path, serialize(&plan))
            .with_context(|| format!("write {}", plan_path.display()))?;

        // Evict any baseline mapping for the same plan_path — once a real task
        // lands, the synthetic baseline:<path> entry is stale.
        crate::baseline::evict_baseline_for(&mut state, &assigned_path);

        // Plan_path dedup: when rehydrating across a session restart, the
        // same plan_path may already map to a stale task_id from a prior
        // session. Drop those so the state file holds at most one mapping
        // per plan_path. Excludes the incoming task_id (defensive — it
        // shouldn't be in state here since the no-op check above bailed).
        //
        // Phase 26.6: distinguish cross-session (safe to evict) from
        // same-session (a duplicate TaskCreate — bug). A mapping with a
        // non-empty `created_in_session` matching the incoming payload's
        // session_id is live in the current session; silently replacing it
        // would orphan the older harness task from writeback. Refuse and
        // warn instead — Claude can TaskUpdate(deleted) the duplicate.
        let same_session_owner: Option<String> = if payload.session_id.is_empty() {
            None
        } else {
            state.mappings.iter().find_map(|(id, m)| {
                if id.as_str() != task_id
                    && m.plan_path == assigned_path
                    && m.created_in_session == payload.session_id
                {
                    Some(id.clone())
                } else {
                    None
                }
            })
        };
        if let Some(owner) = same_session_owner {
            return Ok(HookOutput::context(
                &payload.hook_event_name,
                format!(
                    "claude-plan-bridge: WARNING refused to map task {task_id} to \
                     {assigned_path} — task {owner} already owns that plan_path in \
                     this session. Likely a duplicate TaskCreate; call \
                     TaskUpdate(taskId={task_id}, status=deleted) to retire it. \
                     {owner} remains canonical (no PLAN.md change, no state change)."
                ),
            ));
        }
        let stale_ids: Vec<String> = state
            .mappings
            .iter()
            .filter(|(id, m)| id.as_str() != task_id && m.plan_path == assigned_path)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &stale_ids {
            state.remove(id);
        }

        let mapping = match plan.find(&assigned_path) {
            Some(node) => Mapping {
                plan_path: assigned_path.clone(),
                last_synced_title: node.title.clone(),
                last_synced_state: node.state,
                last_synced_annotations: annotations_to_strings(&node.annotations),
                created_in_session: payload.session_id.clone(),
            },
            None => Mapping {
                plan_path: assigned_path.clone(),
                last_synced_title: input.subject.clone(),
                created_in_session: payload.session_id.clone(),
                ..Default::default()
            },
        };
        state.record(&task_id, mapping);
        // Phase 26.5: a rehydration TaskCreate just landed — evict its
        // plan_path from the pending set so the next reconcile no longer
        // suppresses it (and so the set can drain to empty as a signal
        // the rehydration is complete).
        let evicted_from_rehydration = state.pending_rehydration.remove(&assigned_path);
        // Phase 26.7: when this eviction drained the rehydration set, the
        // bridge can confirm end-to-end success ("N/N mapped"). Reset the
        // announced count so the signal fires exactly once.
        let rehydration_total = if evicted_from_rehydration
            && state.pending_rehydration.is_empty()
            && state.rehydration_announced > 0
        {
            let n = state.rehydration_announced;
            state.rehydration_announced = 0;
            Some(n)
        } else {
            None
        };
        state.save(&state_path)?;

        let mut message = format!(
            "claude-plan-bridge: {action} `{}` at {} in {}",
            input.subject,
            assigned_path,
            plan_path.display()
        );
        if let Some(n) = rehydration_total {
            message.push_str(&format!(
                "\nrehydration complete: {n}/{n} mapped — state file synced"
            ));
        }
        if !stale_ids.is_empty() {
            message.push_str(&format!(
                " (replaced stale mapping(s) for task(s) {})",
                stale_ids.join(", ")
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
        let mut plan = parse(&plan_text)?;

        let mut changes: Vec<String> = Vec::new();

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
                // TaskUpdate(deleted) NEVER hard-deletes a PLAN.md line. Per
                // Phase 28: on a Pending leaf, flip it to `[>]` (backlog) and
                // append a bullet under `## Backlog (not yet phased)`. On
                // non-Pending (Done / WontDo / Backlog) or missing leaves,
                // just unlink the harness mapping — the prior contract.
                // Hand-edit PLAN.md or run archive to actually remove a line.
                let pending_leaf = plan
                    .find(&node_path)
                    .map(|n| n.state == NodeState::Pending && n.is_leaf())
                    .unwrap_or(false);
                if pending_leaf {
                    let title = plan
                        .find(&node_path)
                        .map(|n| n.title.clone())
                        .unwrap_or_default();
                    if let Some(node) = plan.find_mut(&node_path) {
                        node.state = NodeState::Backlog;
                    }
                    plan.append_backlog_entry(&node_path, &title, &crate::today::today_utc());
                    std::fs::write(plan_path, serialize(&plan))
                        .with_context(|| format!("write {}", plan_path.display()))?;
                    state.remove(&input.task_id);
                    state.save(&state_path)?;
                    return Ok(HookOutput::context(
                        &payload.hook_event_name,
                        format!(
                            "claude-plan-bridge: backlogged {node_path} (flipped to [>], promoted to ## Backlog); unlinked task {}",
                            input.task_id
                        ),
                    ));
                }
                state.remove(&input.task_id);
                state.save(&state_path)?;
                return Ok(HookOutput::context(
                    &payload.hook_event_name,
                    format!(
                        "claude-plan-bridge: unlinked task {} from {node_path}; PLAN.md preserved (delete via archive or hand-edit)",
                        input.task_id
                    ),
                ));
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
            // Preserve the mapping's `created_in_session` stamp across the
            // refresh — it's only meaningful to writeback_create's
            // duplicate-detection logic and shouldn't be reset by an
            // update event.
            let created_in_session = state
                .mappings
                .get(&input.task_id)
                .map(|m| m.created_in_session.clone())
                .unwrap_or_default();
            let updated = Mapping {
                plan_path: node_path.clone(),
                last_synced_title: node.title.clone(),
                last_synced_state: node.state,
                last_synced_annotations: annotations_to_strings(&node.annotations),
                created_in_session,
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

fn insert_at_path(plan: Plan, plan_path: &str, subject: &str) -> Result<Plan> {
    let mut plan = plan;
    if plan.find(plan_path).is_some() {
        // Already in the plan — leave it alone, just record the mapping.
        return Ok(plan);
    }
    let new_node = Node {
        id: plan_path.to_string(),
        title: subject.to_string(),
        state: NodeState::Pending,
        id_style: crate::ast::IdStyle::Plain,
        separator: crate::ast::Separator::Space,
        children: vec![],
        annotations: vec![],
    };
    match parent_id_for(plan_path) {
        None => plan.insert_phase(new_node),
        Some(parent_id) => {
            if plan.find(&parent_id).is_none() {
                // Conditional canonicalize fallback: if the parent isn't found,
                // it may be living as a `### Phase N — Title` markdown header
                // (Annotation::Text) rather than a `- [ ] N.0` checkbox.
                // Standardize, retry the lookup; if the parent's now visible
                // use the standardized plan, otherwise surface a clean error
                // (don't blow up the file just to fail).
                let (standardized, _notes) = plan
                    .clone()
                    .standardize_to_canonical()
                    .map_err(|e| anyhow!(e))?;
                if standardized.find(&parent_id).is_some() {
                    plan = standardized;
                } else {
                    anyhow::bail!(
                        "inserting {plan_path}: parent `{parent_id}` not found in PLAN.md. \
                         Either the parent phase doesn't exist yet (create it first with a \
                         `- [ ] {parent_id} ...` checkbox), or your plan uses an unrecognized \
                         section-header format. Try `plan-bridge canonicalize --dry-run` to \
                         see how the bridge would normalize the structure."
                    );
                }
            }
            plan.add_child_of(&parent_id, new_node)
                .map_err(|e| anyhow!(e))
                .with_context(|| format!("inserting {plan_path}"))?;
        }
    }
    Ok(plan)
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
            source: String::new(),
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
    fn rehydrate_evicts_stale_plan_path_mapping_with_new_task_id() {
        // Phase 25.3: simulate session restart. Prior session left a state
        // mapping for task "5" → "1.1". Fresh session re-TaskCreates the same
        // plan_path under a new task_id "99". Result: state holds only the
        // new mapping; PLAN.md line stays put; no duplicate.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        // Seed prior-session state.
        let state_path = default_state_path_for(&plan);
        let mut prior = State::default();
        prior.record(
            "5",
            Mapping {
                plan_path: "1.1".to_string(),
                last_synced_title: "Task".to_string(),
                ..Default::default()
            },
        );
        prior.save(&state_path).unwrap();

        let payload = payload_for_create("99", "Task", Some("1.1"));
        let out = writeback_create(&payload, &plan).unwrap();
        let state = State::load(&state_path).unwrap();
        assert_eq!(state.plan_path("99"), Some("1.1"), "new mapping missing");
        assert_eq!(state.plan_path("5"), None, "stale mapping not evicted");
        assert_eq!(state.mappings.len(), 1, "duplicate mappings left behind");
        // PLAN.md untouched (line already existed).
        let contents = std::fs::read_to_string(&plan).unwrap();
        assert_eq!(
            contents.matches("1.1 Task").count(),
            1,
            "PLAN.md duplicated the line: {contents}"
        );
        assert!(
            out.to_json().contains("replaced stale mapping"),
            "should announce eviction: {}",
            out.to_json()
        );
    }

    #[test]
    fn taskcreate_refuses_to_silently_move_existing_task_to_different_path() {
        // Phase 25.3: if task_id is already mapped but TaskCreate arrives with
        // a different plan_path, refuse to move it. Caller likely confused
        // task_id semantics with "retarget"; better to warn than silently
        // rewrite the mapping.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [ ] 1.1 First\n  - [ ] 1.2 Second\n",
        );
        writeback_create(&payload_for_create("t-1", "First", Some("1.1")), &plan).unwrap();
        let before = std::fs::read_to_string(&plan).unwrap();
        let state_before = State::load(&default_state_path_for(&plan)).unwrap();

        let out =
            writeback_create(&payload_for_create("t-1", "First", Some("1.2")), &plan).unwrap();

        let after = std::fs::read_to_string(&plan).unwrap();
        let state_after = State::load(&default_state_path_for(&plan)).unwrap();
        assert_eq!(before, after, "PLAN.md mutated despite refusal");
        assert_eq!(state_before, state_after, "state mutated despite refusal");
        assert!(
            out.to_json().contains("WARNING"),
            "should warn loudly: {}",
            out.to_json()
        );
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
            msg.contains("section-header") || msg.contains("canonicalize"),
            "should hint at format issue + canonicalize escape hatch: {msg}"
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

    #[test]
    fn taskcreate_refuses_same_session_duplicate_for_same_plan_path() {
        // Phase 26.6: if a TaskCreate arrives with a plan_path that is
        // already owned by a different task_id stamped with the *same*
        // session_id, refuse rather than silently replace. Silently
        // replacing would orphan the original harness task from writeback
        // — its future TaskUpdates would no-op.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        let mut first = payload_for_create("t-first", "Task", Some("1.1"));
        first.session_id = "sess-A".to_string();
        writeback_create(&first, &plan).unwrap();

        let mut dupe = payload_for_create("t-second", "Task", Some("1.1"));
        dupe.session_id = "sess-A".to_string();
        let out = writeback_create(&dupe, &plan).unwrap().to_json();
        assert!(
            out.contains("WARNING"),
            "expected refusal warning, got: {out}"
        );
        assert!(
            out.contains("t-first") && out.contains("t-second"),
            "warning should name both tasks, got: {out}"
        );

        // State unchanged: t-first still owns 1.1, t-second never registered.
        let state = State::load(&default_state_path_for(&plan)).unwrap();
        assert_eq!(state.plan_path("t-first"), Some("1.1"));
        assert_eq!(state.plan_path("t-second"), None);
        assert_eq!(state.mappings.len(), 1, "got: {:?}", state.mappings);
    }

    #[test]
    fn taskcreate_cross_session_eviction_still_works() {
        // Phase 26.6 contract: only SAME-session duplicates get refused.
        // A new task_id from a different session is the legitimate cross-
        // session rehydration path (fallback when SessionStart hook
        // didn't run / wipe state). It must still evict cleanly so the
        // bridge stays usable when the hook is missing.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        let state_path = default_state_path_for(&plan);
        let mut prior = State::default();
        prior.record(
            "t-old",
            Mapping {
                plan_path: "1.1".to_string(),
                last_synced_title: "Task".to_string(),
                created_in_session: "sess-PRIOR".to_string(),
                ..Default::default()
            },
        );
        prior.save(&state_path).unwrap();

        let mut fresh = payload_for_create("t-new", "Task", Some("1.1"));
        fresh.session_id = "sess-CURRENT".to_string();
        let out = writeback_create(&fresh, &plan).unwrap().to_json();
        assert!(
            out.contains("replaced stale mapping"),
            "cross-session eviction should announce; got: {out}"
        );
        let state = State::load(&state_path).unwrap();
        assert_eq!(state.plan_path("t-new"), Some("1.1"));
        assert_eq!(state.plan_path("t-old"), None);
    }

    #[test]
    fn taskcreate_evicts_path_from_pending_rehydration() {
        // Phase 26.5: when SessionStart seeds pending_rehydration with the
        // open plan_paths it announced, each subsequent TaskCreate must
        // evict its plan_path from the set. This both unblocks the next
        // reconcile (the entry no longer suppresses real drift) and lets
        // the bridge detect rehydration completion when the set drains to
        // empty (foundation for the 26.7 confirmation signal).
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [ ] 1.1 First\n  - [ ] 1.2 Second\n",
        );
        let state_path = default_state_path_for(&plan);
        let mut seed = State::default();
        seed.pending_rehydration.insert("1.1".to_string());
        seed.pending_rehydration.insert("1.2".to_string());
        seed.save(&state_path).unwrap();

        writeback_create(&payload_for_create("t-1", "First", Some("1.1")), &plan).unwrap();
        let after_first = State::load(&state_path).unwrap();
        assert_eq!(
            after_first
                .pending_rehydration
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["1.2".to_string()],
            "1.1 should have been evicted; got: {:?}",
            after_first.pending_rehydration
        );

        writeback_create(&payload_for_create("t-2", "Second", Some("1.2")), &plan).unwrap();
        let after_second = State::load(&state_path).unwrap();
        assert!(
            after_second.pending_rehydration.is_empty(),
            "set should drain to empty after final TaskCreate; got: {:?}",
            after_second.pending_rehydration
        );
    }

    #[test]
    fn taskcreate_emits_rehydration_complete_when_final_path_evicted() {
        // Phase 26.7: the bridge announces N at SessionStart; each
        // matching TaskCreate evicts one path. When the final eviction
        // drains the set to empty, writeback's hook message gains a
        // "rehydration complete: N/N mapped" line so the agent and the
        // user see the end-to-end success signal. Intermediate
        // TaskCreates stay quiet.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [ ] 1.1 First\n  - [ ] 1.2 Second\n",
        );
        let state_path = default_state_path_for(&plan);
        let mut seed = State::default();
        seed.pending_rehydration.insert("1.1".to_string());
        seed.pending_rehydration.insert("1.2".to_string());
        seed.rehydration_announced = 2;
        seed.save(&state_path).unwrap();

        let out_first = writeback_create(&payload_for_create("t-1", "First", Some("1.1")), &plan)
            .unwrap()
            .to_json();
        assert!(
            !out_first.contains("rehydration complete"),
            "first TaskCreate (1/2) should stay quiet; got: {out_first}"
        );

        let out_last = writeback_create(&payload_for_create("t-2", "Second", Some("1.2")), &plan)
            .unwrap()
            .to_json();
        assert!(
            out_last.contains("rehydration complete: 2/2 mapped"),
            "final TaskCreate should announce completion; got: {out_last}"
        );

        // Announced count resets so a second drain doesn't double-fire.
        let after = State::load(&state_path).unwrap();
        assert_eq!(after.rehydration_announced, 0);
    }

    #[test]
    fn taskcreate_outside_rehydration_does_not_emit_completion() {
        // Genuine new tasks (no pending_rehydration entry for their
        // plan_path) must not pretend rehydration is complete. The set is
        // empty before AND after — `evicted` is false, so the signal
        // suppresses cleanly.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        // No seed; pending_rehydration starts empty.
        let out = writeback_create(&payload_for_create("t-1", "Task", Some("1.1")), &plan)
            .unwrap()
            .to_json();
        assert!(
            !out.contains("rehydration complete"),
            "non-rehydration TaskCreate must not emit completion; got: {out}"
        );
    }

    fn payload_for_update(task_id: &str, status: &str) -> HookPayload {
        HookPayload {
            session_id: String::new(),
            cwd: String::new(),
            hook_event_name: "PostToolUse".to_string(),
            tool_name: "TaskUpdate".to_string(),
            tool_input: serde_json::json!({"taskId": task_id, "status": status}),
            tool_response: serde_json::json!({}),
            source: String::new(),
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
    fn update_deleted_on_pending_leaf_flips_to_backlog() {
        // Phase 28: TaskUpdate(deleted) on a Pending leaf flips the line
        // to `[>]` and appends a bullet under `## Backlog (not yet phased)`.
        // The mapping is dropped from state. The line is never hard-deleted.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        writeback_create(&payload_for_create("t-1", "Task", Some("1.1")), &plan).unwrap();
        writeback_update(&payload_for_update("t-1", "deleted"), &plan).unwrap();
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(
            after.contains("- [>] 1.1 Task"),
            "leaf should flip to [>]: {after}"
        );
        assert!(
            after.contains("## Backlog (not yet phased)"),
            "Backlog section should be created: {after}"
        );
        assert!(
            after.contains("- **Task** — deferred from 1.1 on"),
            "Backlog entry should be appended: {after}"
        );
        let state = State::load(&default_state_path_for(&plan)).unwrap();
        assert_eq!(state.plan_path("t-1"), None, "mapping should be cleared");
    }

    #[test]
    fn update_deleted_on_non_leaf_unlinks_only() {
        // Regression guard for the destruction class: a stale cross-session
        // mapping pointing at a phase root previously caused TaskUpdate(deleted)
        // to wipe the phase and every subtask under it. The Phase 28 backlog
        // flip is leaf-only — non-leaves fall back to unlink-only behavior so
        // a stale mapping can't turn into a destructive backlog flip on a
        // parent node.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [ ] 1.1 Child A\n  - [ ] 1.2 Child B\n",
        );
        writeback_create(&payload_for_create("t-phase", "Phase", Some("1.0")), &plan).unwrap();
        let before = std::fs::read_to_string(&plan).unwrap();
        writeback_update(&payload_for_update("t-phase", "deleted"), &plan).unwrap();
        let after = std::fs::read_to_string(&plan).unwrap();
        assert_eq!(before, after, "phase + subtree must survive a delete");
        assert!(after.contains("1.1 Child A"), "child A wiped: {after}");
        assert!(after.contains("1.2 Child B"), "child B wiped: {after}");
        assert!(
            !after.contains("[>]"),
            "non-leaf must not flip to backlog: {after}"
        );
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
            source: String::new(),
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
            source: String::new(),
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
    fn writeback_create_falls_back_to_canonicalize_when_parent_is_header_only() {
        // Phase 29.2: writeback no longer canonicalizes implicitly. But if the
        // requested plan_path's parent ONLY exists as a `### Phase N — Title`
        // markdown header (not a checkbox), insert_at_path's conditional
        // fallback runs standardize_to_canonical so the new task can land.
        // This is the narrow rescue path — the alternative is failing the
        // hook entirely on Phase-N-header projects.
        let dir = scratch_dir();
        let original = "- [x] 0.1 First\n\n### Phase 1 — Build\n\n- [ ] 1.1 Build it\n";
        let plan = write_plan(&dir, original);

        let payload = payload_for_create("t-x", "new sub", Some("1.2"));
        writeback_create(&payload, &plan).expect("conditional canonicalize fallback");
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(
            after.contains("- [ ] 1.0 Build"),
            "phase header promoted by fallback:\n{after}"
        );
        assert!(
            after.contains("  - [ ] 1.2 new sub"),
            "new task lands under 1.0:\n{after}"
        );
    }

    #[test]
    fn writeback_create_preserves_narrative_sub_headers_when_parent_already_checkbox() {
        // Phase 29.2 (regression class). When the parent is already a
        // checkbox phase, writeback parses + inserts without invoking
        // standardize_to_canonical. Any `### X.4.a — Sub-section` headers
        // that the user uses for grouping inside the phase stay verbatim.
        let dir = scratch_dir();
        let original = "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n\n### X.4.a — Sub-section grouping\n\n  - [ ] 1.2 Existing\n";
        let plan = write_plan(&dir, original);
        let payload = payload_for_create("t-x", "new sub", Some("1.3"));
        writeback_create(&payload, &plan).expect("clean append");
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(
            after.contains("### X.4.a — Sub-section grouping"),
            "sub-section header preserved verbatim:\n{after}"
        );
        assert!(
            after.contains("- [ ] 1.3 new sub"),
            "new leaf inserted:\n{after}"
        );
    }

    #[test]
    fn writeback_create_proceeds_through_narrative_headers() {
        // Phase 19 — `## Notes` doesn't match Phase-N shape so it stays as
        // narrative; writeback should proceed (insert the new task) and the
        // header should still be in the file afterward at its original column.
        let dir = scratch_dir();
        let original = "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n\n## Notes\n\nSome stuff.\n";
        let plan = write_plan(&dir, original);
        let payload = payload_for_create("t-x", "new sub", Some("1.2"));
        writeback_create(&payload, &plan).expect("narrative headers don't block");
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(
            after.contains("- [ ] 1.2 new sub"),
            "new task inserted:\n{after}"
        );
        assert!(
            after.contains("## Notes"),
            "narrative header preserved:\n{after}"
        );
        // Critically — `## Notes` should NOT be indented; it stays at column 0.
        for line in after.lines() {
            if line.contains("## Notes") {
                assert!(
                    line.starts_with("## "),
                    "## Notes demoted to indented:\n{line}"
                );
            }
        }
    }

    #[test]
    fn writeback_update_does_not_promote_unrelated_headers() {
        // Phase 29.2: writeback_update ticks the mapped leaf without
        // canonicalizing the rest of the file. A `### Phase 2 — Other work`
        // header elsewhere in the document stays put — the user's chosen
        // format isn't collateral damage from a routine status change.
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
                ..Default::default()
            },
        );
        state.save(&default_state_path_for(&plan)).unwrap();

        writeback_update(&payload_for_update("t-1", "completed"), &plan).expect("tick succeeds");
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(after.contains("  - [x] 1.1 Task"), "1.1 ticked:\n{after}");
        assert!(
            after.contains("### Phase 2 — Other work"),
            "unrelated narrative header preserved:\n{after}"
        );
        assert!(
            !after.contains("- [ ] 2.0 Other work"),
            "Phase 2 must NOT be promoted by a routine update:\n{after}"
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
