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
}

/// Typed view of `TaskCreate`'s `tool_input`.
#[derive(Debug, Deserialize, Default)]
pub struct TaskCreateInput {
    pub subject: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub metadata: Option<TaskMetadata>,
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
    fn parses_task_update_input() {
        let v = serde_json::json!({"taskId": "abc", "status": "completed"});
        let input: TaskUpdateInput = serde_json::from_value(v).unwrap();
        assert_eq!(input.task_id, "abc");
        assert_eq!(input.status.as_deref(), Some("completed"));
    }
}
