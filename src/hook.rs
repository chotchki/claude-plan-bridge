use serde::{Deserialize, Serialize};

/// The JSON Claude Code passes to a hook on stdin. Fields not used by the
/// bridge are intentionally absent — `tool_input` and `tool_response` are kept
/// as opaque `Value`s so we can typed-decode them per-tool downstream.
#[derive(Debug, Deserialize)]
pub struct HookPayload {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub hook_event_name: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: serde_json::Value,
    #[serde(default)]
    pub tool_response: serde_json::Value,
    /// SessionStart-only: one of `startup`, `resume`, `clear`, `compact`.
    /// `startup`/`clear` guarantee an empty harness task list — resume uses
    /// this to drop stale pending mappings before rehydrating.
    #[serde(default)]
    pub source: String,
}

/// Typed view of `TaskCreate`'s `tool_input`.
#[derive(Debug, Deserialize, Default)]
pub struct TaskCreateInput {
    pub subject: String,
    #[serde(default)]
    pub description: String,
    // Phase CC: `metadata` is deserialized tolerantly. A deferred-schema
    // TaskCreate can ship `metadata` as a JSON *string* instead of an object
    // (`"{\"plan_path\":\"X.1\"}"`); a strict `Option<TaskMetadata>` would
    // hard-fail the whole `from_value`, losing the create entirely. The
    // tolerant path recovers the object from the string form and degrades any
    // other shape to `None` rather than erroring.
    #[serde(default, deserialize_with = "de_tolerant_metadata")]
    pub metadata: Option<TaskMetadata>,
}

/// Accept `metadata` as an object, a JSON string encoding that object, or
/// anything else (null / wrong type) → `None`. Never errors — a malformed
/// `metadata` must not torpedo the create; the caller falls through to the
/// description-recovery / Backlog path instead.
fn de_tolerant_metadata<'de, D>(deserializer: D) -> Result<Option<TaskMetadata>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(metadata_from_value(&value))
}

fn metadata_from_value(value: &serde_json::Value) -> Option<TaskMetadata> {
    match value {
        serde_json::Value::Object(_) => serde_json::from_value(value.clone()).ok(),
        // Degraded form: a JSON string encoding the metadata object. Parse it,
        // then accept only if it decodes to an object.
        serde_json::Value::String(s) => serde_json::from_str::<serde_json::Value>(s)
            .ok()
            .filter(serde_json::Value::is_object)
            .and_then(|v| serde_json::from_value(v).ok()),
        _ => None,
    }
}

/// Typed view of `TaskUpdate`'s `tool_input`. Status strings match the
/// TaskUpdate schema (`pending`, `in_progress`, `completed`, `deleted`).
#[derive(Debug, Deserialize, Default)]
pub struct TaskUpdateInput {
    #[serde(rename = "taskId")]
    pub task_id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    // Phase CK: a corrected `metadata.plan_path` on an update re-paths the task
    // (fix a typo like `B.7.3` -> `B.7.4`). Deserialized with the same tolerant
    // path as TaskCreate so a string-form `metadata` from a deferred-schema
    // client still recovers rather than torpedoing the update.
    #[serde(default, deserialize_with = "de_tolerant_metadata")]
    pub metadata: Option<TaskMetadata>,
}

/// Bridge-managed metadata smuggled through `TaskCreate.metadata`. `plan_path`
/// is the canonical dotted id the new node should occupy; `plan_phase` is the
/// human-readable phase title for cross-reference.
#[derive(Debug, Deserialize, Default)]
pub struct TaskMetadata {
    pub plan_path: Option<String>,
    pub plan_phase: Option<String>,
}

/// JSON the bridge writes back to stdout. The shape matches Claude Code's hook
/// output convention: `hookSpecificOutput.additionalContext` is fed to Claude
/// as a system message before the next turn.
#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HookOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_specific_output: Option<HookSpecificOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookSpecificOutput {
    /// Claude Code requires this on every `hookSpecificOutput` — it identifies
    /// which event class the additionalContext is in response to
    /// (`UserPromptSubmit`, `PostToolUse`, etc.). Without it Claude Code rejects
    /// the hook output with a schema-validation error.
    pub hook_event_name: String,
    pub additional_context: String,
}

impl HookOutput {
    pub fn silent() -> Self {
        Self::default()
    }

    pub fn context(event: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            hook_specific_output: Some(HookSpecificOutput {
                hook_event_name: event.into(),
                additional_context: msg.into(),
            }),
            ..Default::default()
        }
    }

    /// Prepend `prefix` to the existing additionalContext, or build a new
    /// context with just the prefix when the output is currently silent.
    /// Used to splice in warnings (e.g., missing SessionStart hook) without
    /// dropping the real hook payload.
    pub fn prepend_context(self, event: impl Into<String>, prefix: impl Into<String>) -> Self {
        let prefix = prefix.into();
        let event = event.into();
        match self.hook_specific_output {
            Some(hso) => Self {
                hook_specific_output: Some(HookSpecificOutput {
                    hook_event_name: hso.hook_event_name,
                    additional_context: format!("{prefix}\n\n{}", hso.additional_context),
                }),
                ..self
            },
            None => Self {
                hook_specific_output: Some(HookSpecificOutput {
                    hook_event_name: event,
                    additional_context: prefix,
                }),
                ..self
            },
        }
    }

    pub fn block(reason: impl Into<String>) -> Self {
        Self {
            decision: Some("block".to_string()),
            reason: Some(reason.into()),
            ..Default::default()
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("HookOutput serialization is infallible")
    }
}

/// Wrap a hook handler so a missing PLAN.md degrades gracefully:
///   - File absent → silent no-op (the bridge stays out of projects without a plan).
///   - Handler returns `Err` → non-blocking `additionalContext` carrying the
///     error text (visible but doesn't wall the user off).
///
/// Never emits `decision: "block"`. Originated from a session implode where
/// Claude `cd`'d mid-session, the hook subprocess inherited a wrong cwd, and
/// `./PLAN.md` not found became a hard block on every subsequent prompt
/// including `ls`. The contract: the bridge is a peripheral that decorates
/// context; it must not gate prompts even when its own state is broken.
pub fn guard_missing_plan<F>(plan: &std::path::Path, event: &str, f: F) -> HookOutput
where
    F: FnOnce() -> anyhow::Result<HookOutput>,
{
    if !plan.exists() {
        // Phase CA: a missing PLAN.md used to be a fully silent no-op, which
        // is exactly how a stale `--cwd` (or cwd drift) hid itself — the
        // bridge looked alive while mirroring nothing. Now, when the bridge is
        // actually configured in this project, surface a loud-but-NON-BLOCKING
        // notice so the misconfiguration is visible. Still never blocks (the
        // Phase 32 contract): an unconfigured project stays silent.
        return match missing_plan_notice(plan) {
            Some(msg) => HookOutput::context(event, msg),
            None => HookOutput::silent(),
        };
    }
    f().unwrap_or_else(|e| HookOutput::context(event, format!("claude-plan-bridge: {e:#}")))
}

/// Decide what to say when PLAN.md is absent at the resolved path. Returns
/// `None` (stay silent) when this project shows no sign of using the bridge —
/// no state file and no plan-bridge hooks — so we don't nag projects that
/// simply don't have a PLAN.md. Returns a non-blocking warning when the bridge
/// IS configured here, because then a missing PLAN.md means a real
/// misconfiguration (most often a stale/dead hook `--cwd`).
fn missing_plan_notice(plan: &std::path::Path) -> Option<String> {
    let claude_dir = plan.parent()?.join(".claude");
    let state_exists = claude_dir.join("plan-bridge-state.json").exists();
    let hooks_here = std::fs::read_to_string(claude_dir.join("settings.json"))
        .map(|t| t.contains("claude-plan-bridge"))
        .unwrap_or(false);
    if !state_exists && !hooks_here {
        return None;
    }
    Some(format!(
        "claude-plan-bridge: ⚠ PLAN.md not found at {} — the bridge is installed \
         in this project but can't read the plan, so task changes are NOT being \
         mirrored. This usually means a stale or dead hook `--cwd`. Run \
         `claude-plan-bridge status` to diagnose, then `claude-plan-bridge \
         upgrade-hooks` to rewrite the hooks to the portable \
         `--cwd \"$CLAUDE_PROJECT_DIR\"` form.",
        plan.display()
    ))
}

/// Pull a task id out of `tool_response`. Real-world shapes seen in Claude Code:
///   TaskUpdate → `{"taskId": "1", ...}` (flat)
///   TaskCreate → `{"task": {"id": "2", ...}}` (nested under `task`)
/// Probe both, and accept either a JSON string or a JSON integer (some
/// harness versions emit numeric ids).
pub fn extract_task_id(response: &serde_json::Value) -> Option<String> {
    fn pick(v: &serde_json::Value) -> Option<String> {
        for key in &["id", "task_id", "taskId"] {
            if let Some(field) = v.get(*key) {
                if let Some(s) = field.as_str() {
                    return Some(s.to_string());
                }
                if let Some(n) = field.as_i64() {
                    return Some(n.to_string());
                }
            }
        }
        None
    }
    pick(response).or_else(|| response.get("task").and_then(pick))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimum_payload() {
        let json = r#"{
            "tool_name": "TaskCreate",
            "tool_input": {"subject": "do the thing", "description": ""},
            "tool_response": {"id": "abc-123"}
        }"#;
        let p: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(p.tool_name, "TaskCreate");
        let input: TaskCreateInput = serde_json::from_value(p.tool_input).unwrap();
        assert_eq!(input.subject, "do the thing");
        assert_eq!(
            extract_task_id(&p.tool_response).as_deref(),
            Some("abc-123")
        );
    }

    #[test]
    fn parses_payload_with_metadata() {
        let json = r#"{
            "tool_name": "TaskCreate",
            "tool_input": {
                "subject": "x",
                "description": "y",
                "metadata": {"plan_path": "1.1.1", "plan_phase": "Phase 1"}
            },
            "tool_response": {"id": "t-1"}
        }"#;
        let p: HookPayload = serde_json::from_str(json).unwrap();
        let input: TaskCreateInput = serde_json::from_value(p.tool_input).unwrap();
        let meta = input.metadata.unwrap();
        assert_eq!(meta.plan_path.as_deref(), Some("1.1.1"));
        assert_eq!(meta.plan_phase.as_deref(), Some("Phase 1"));
    }

    #[test]
    fn cc_metadata_tolerates_string_object_garbage_and_absent() {
        // Phase CC: the metadata field must survive every degraded shape a
        // deferred-schema client might send, without ever erroring the parse.
        let de = |ti: serde_json::Value| -> TaskCreateInput {
            serde_json::from_value(ti).expect("tool_input parse must not fail")
        };

        // Object — the happy path.
        let obj = de(serde_json::json!({
            "subject": "s", "metadata": {"plan_path": "X.1", "plan_phase": "P"}
        }));
        assert_eq!(obj.metadata.unwrap().plan_path.as_deref(), Some("X.1"));

        // JSON string encoding the object — recovered, not hard-failed.
        let s = de(serde_json::json!({
            "subject": "s", "metadata": "{\"plan_path\":\"X.2\",\"plan_phase\":\"P\"}"
        }));
        let m = s.metadata.expect("string metadata recovered");
        assert_eq!(m.plan_path.as_deref(), Some("X.2"));
        assert_eq!(m.plan_phase.as_deref(), Some("P"));

        // Garbage string → None (degrade, don't error).
        assert!(
            de(serde_json::json!({"subject": "s", "metadata": "not json"}))
                .metadata
                .is_none()
        );
        // Null and absent → None.
        assert!(
            de(serde_json::json!({"subject": "s", "metadata": null}))
                .metadata
                .is_none()
        );
        assert!(de(serde_json::json!({"subject": "s"})).metadata.is_none());
    }

    #[test]
    fn extract_task_id_tries_multiple_keys() {
        let v = serde_json::json!({"task_id": "snake"});
        assert_eq!(extract_task_id(&v).as_deref(), Some("snake"));
        let v = serde_json::json!({"taskId": "camel"});
        assert_eq!(extract_task_id(&v).as_deref(), Some("camel"));
        let v = serde_json::json!({"id": "plain"});
        assert_eq!(extract_task_id(&v).as_deref(), Some("plain"));
        let v = serde_json::json!({"nope": "x"});
        assert_eq!(extract_task_id(&v), None);
    }

    #[test]
    fn extract_task_id_handles_nested_task_create_response() {
        // Real shape from Claude Code's TaskCreate hook (captured 2026-05-16).
        let v = serde_json::json!({
            "task": {"id": "2", "subject": "Second smoke-test"}
        });
        assert_eq!(extract_task_id(&v).as_deref(), Some("2"));
    }

    #[test]
    fn extract_task_id_handles_numeric_ids() {
        let v = serde_json::json!({"id": 42});
        assert_eq!(extract_task_id(&v).as_deref(), Some("42"));
        let v = serde_json::json!({"task": {"id": 7}});
        assert_eq!(extract_task_id(&v).as_deref(), Some("7"));
    }

    #[test]
    fn hook_output_emits_camel_case() {
        let out = HookOutput::context("UserPromptSubmit", "hello");
        let json = out.to_json();
        assert!(json.contains("hookSpecificOutput"), "got: {json}");
        assert!(json.contains("additionalContext"), "got: {json}");
        // Claude Code's hook-output schema requires hookEventName inside
        // hookSpecificOutput — omitting it triggers a validation rejection.
        assert!(
            json.contains("\"hookEventName\":\"UserPromptSubmit\""),
            "got: {json}"
        );
    }

    #[test]
    fn silent_hook_output_is_empty_object() {
        let out = HookOutput::silent();
        assert_eq!(out.to_json(), "{}");
    }

    #[test]
    fn block_hook_output_carries_reason() {
        let out = HookOutput::block("malformed PLAN.md");
        let json = out.to_json();
        assert!(json.contains("\"decision\":\"block\""), "got: {json}");
        assert!(json.contains("malformed"), "got: {json}");
    }

    #[test]
    fn prepend_context_on_silent_creates_context() {
        let out = HookOutput::silent().prepend_context("UserPromptSubmit", "WARNING: bad");
        let json = out.to_json();
        assert!(json.contains("WARNING: bad"), "got: {json}");
        assert!(
            json.contains("\"hookEventName\":\"UserPromptSubmit\""),
            "got: {json}"
        );
    }

    #[test]
    fn prepend_context_on_existing_keeps_payload() {
        let out = HookOutput::context("PostToolUse", "real payload")
            .prepend_context("PostToolUse", "WARNING: bad");
        let json = out.to_json();
        // Both warning and payload survive, warning first.
        let warn_pos = json.find("WARNING: bad").expect("warning missing");
        let payload_pos = json.find("real payload").expect("payload missing");
        assert!(
            warn_pos < payload_pos,
            "warning should precede payload: {json}"
        );
    }

    #[test]
    fn guard_missing_plan_silent_on_missing_file() {
        let p = std::env::temp_dir().join(format!(
            "plan-bridge-guard-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(!p.exists(), "scratch path should not exist");
        let out = guard_missing_plan(&p, "UserPromptSubmit", || {
            panic!("handler must not be invoked when PLAN.md is missing")
        });
        assert_eq!(out.to_json(), "{}", "expected silent no-op");
    }

    #[test]
    fn guard_missing_plan_warns_loudly_when_bridge_configured_but_plan_absent() {
        // Phase CA: a configured bridge (plan-bridge hooks present) with no
        // PLAN.md at the resolved path is the silent-no-op trap. It must now
        // surface a visible, NON-BLOCKING notice pointing at `status`.
        let dir = std::env::temp_dir().join(format!(
            "plan-bridge-guard-configured-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(
            dir.join(".claude/settings.json"),
            "{\"hooks\":{\"UserPromptSubmit\":[{\"hooks\":[{\"type\":\"command\",\
             \"command\":\"claude-plan-bridge reconcile --cwd x\"}]}]}}",
        )
        .unwrap();
        let plan = dir.join("PLAN.md");
        assert!(!plan.exists(), "PLAN.md must be absent for this test");
        let out = guard_missing_plan(&plan, "UserPromptSubmit", || {
            panic!("handler must not run when PLAN.md is missing")
        });
        let json = out.to_json();
        assert!(
            !json.contains("\"decision\":\"block\""),
            "must not block: {json}"
        );
        assert!(
            json.contains("PLAN.md not found"),
            "expected loud notice: {json}"
        );
        assert!(json.contains("status"), "expected a fix hint: {json}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn guard_missing_plan_passes_through_ok() {
        let dir = std::env::temp_dir().join(format!(
            "plan-bridge-guard-ok-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let plan = dir.join("PLAN.md");
        std::fs::write(&plan, "# plan\n").unwrap();
        let out = guard_missing_plan(&plan, "UserPromptSubmit", || {
            Ok(HookOutput::context("UserPromptSubmit", "real payload"))
        });
        assert!(out.to_json().contains("real payload"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn guard_missing_plan_converts_err_to_context_not_block() {
        let dir = std::env::temp_dir().join(format!(
            "plan-bridge-guard-err-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let plan = dir.join("PLAN.md");
        std::fs::write(&plan, "# plan\n").unwrap();
        let out = guard_missing_plan(&plan, "PostToolUse", || {
            Err(anyhow::anyhow!("synthetic parse failure"))
        });
        let json = out.to_json();
        assert!(
            !json.contains("\"decision\":\"block\""),
            "must not block: {json}"
        );
        assert!(json.contains("synthetic parse failure"), "got: {json}");
        assert!(
            json.contains("\"hookEventName\":\"PostToolUse\""),
            "got: {json}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_task_update_input() {
        let v = serde_json::json!({"taskId": "abc", "status": "completed"});
        let input: TaskUpdateInput = serde_json::from_value(v).unwrap();
        assert_eq!(input.task_id, "abc");
        assert_eq!(input.status.as_deref(), Some("completed"));
    }
}
