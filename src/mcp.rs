//! Minimal MCP server. Stdio JSON-RPC 2.0 with the MCP method names
//! (`initialize`, `tools/list`, `tools/call`). Tools are thin wrappers over
//! the same parse/serialize/mutate primitives the CLI subcommands use.
//!
//! The wire loop is tiny; the interesting code is the tool dispatcher and the
//! per-tool argument handling.

use crate::ast::{Node, NodeState, parent_id_for};
use crate::parser::parse;
use crate::serializer::serialize;
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "plan-bridge";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct McpServer {
    plan_path: PathBuf,
}

impl McpServer {
    pub fn new(plan_path: PathBuf) -> Self {
        Self { plan_path }
    }

    /// Read JSON-RPC lines from stdin, dispatch, write responses to stdout.
    /// Blocks until stdin closes.
    pub fn serve(&self) -> Result<()> {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let reader = stdin.lock();
        let mut writer = stdout.lock();
        for line in reader.lines() {
            let line = line.context("read stdin line")?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(resp) = self.handle_line(&line) {
                writeln!(writer, "{resp}").context("write response")?;
                writer.flush().context("flush stdout")?;
            }
        }
        Ok(())
    }

    /// Dispatch one request line. Returns `Some(json_string)` for requests
    /// (anything with an `id`); `None` for notifications.
    pub fn handle_line(&self, line: &str) -> Option<String> {
        let req: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse::error(Value::Null, -32700, format!("parse error: {e}"));
                return Some(serde_json::to_string(&resp).unwrap_or_default());
            }
        };
        // Notifications (id absent) — handle silently. We don't care about
        // the `notifications/initialized` ack today; ignore everything.
        let id = req.id.clone()?;
        let resp = match self.dispatch(&req) {
            Ok(result) => JsonRpcResponse::ok(id, result),
            Err(e) => JsonRpcResponse::error(id, -32603, format!("{e:#}")),
        };
        Some(serde_json::to_string(&resp).unwrap_or_default())
    }

    fn dispatch(&self, req: &JsonRpcRequest) -> Result<Value> {
        match req.method.as_str() {
            "initialize" => Ok(initialize_result()),
            "tools/list" => Ok(tools_list()),
            "tools/call" => self.call_tool(&req.params),
            other => Err(anyhow!("unknown method: {other}")),
        }
    }

    fn call_tool(&self, params: &Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("tools/call: missing 'name'"))?;
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        match name {
            "plan_list" => self.tool_plan_list(),
            "plan_check" => self.tool_plan_check(&args),
            "plan_uncheck" => self.tool_plan_uncheck(&args),
            "plan_skip" => self.tool_plan_skip(&args),
            "plan_add" => self.tool_plan_add(&args),
            "plan_archive" => self.tool_plan_archive(&args),
            "plan_phase_exit" => self.tool_plan_phase_exit(&args),
            other => Err(anyhow!("unknown tool: {other}")),
        }
    }

    fn tool_plan_list(&self) -> Result<Value> {
        let text = std::fs::read_to_string(&self.plan_path)
            .with_context(|| format!("read {}", self.plan_path.display()))?;
        let plan = parse(&text)?;
        let json = serde_json::to_string_pretty(&plan)?;
        Ok(tool_text_result(&json))
    }

    fn tool_plan_check(&self, args: &Value) -> Result<Value> {
        self.set_state(args, NodeState::Done, "checked")
    }

    fn tool_plan_uncheck(&self, args: &Value) -> Result<Value> {
        self.set_state(args, NodeState::Pending, "unchecked")
    }

    fn tool_plan_skip(&self, args: &Value) -> Result<Value> {
        self.set_state(args, NodeState::WontDo, "marked won't-do")
    }

    fn set_state(&self, args: &Value, target: NodeState, verb: &str) -> Result<Value> {
        let id = require_string(args, "plan_path")?;
        let text = std::fs::read_to_string(&self.plan_path)?;
        let mut plan = parse(&text)?;
        let node = plan
            .find_mut(&id)
            .ok_or_else(|| anyhow!("no node with id `{id}` in PLAN.md"))?;
        if node.state == target {
            return Ok(tool_text_result(&format!("{id} was already {verb}")));
        }
        node.state = target;
        std::fs::write(&self.plan_path, serialize(&plan))?;
        Ok(tool_text_result(&format!("{verb} {id}")))
    }

    fn tool_plan_add(&self, args: &Value) -> Result<Value> {
        let plan_path = require_string(args, "plan_path")?;
        let subject = require_string(args, "subject")?;
        let text = std::fs::read_to_string(&self.plan_path)?;
        let mut plan = parse(&text)?;
        if plan.find(&plan_path).is_some() {
            return Err(anyhow!("node `{plan_path}` already exists"));
        }
        let new_node = Node {
            id: plan_path.clone(),
            title: subject.clone(),
            state: NodeState::Pending,
            children: vec![],
            annotations: vec![],
        };
        match parent_id_for(&plan_path) {
            None => plan.phases.push(new_node),
            Some(pid) => plan.add_child_of(&pid, new_node).map_err(|e| anyhow!(e))?,
        }
        std::fs::write(&self.plan_path, serialize(&plan))?;
        Ok(tool_text_result(&format!("added {plan_path} `{subject}`")))
    }

    /// Mark a phase (or any non-leaf) as ready to exit: validate every leaf in
    /// its subtree is resolved (`[x]` or `[-]`), then archive just that phase
    /// to PLAN_ARCHIVE.md. Use this for the "I'm officially done with phase X"
    /// ceremony — `plan_archive` (no args) sweeps every fully-complete phase
    /// at once; `plan_phase_exit` is the surgical variant.
    fn tool_plan_phase_exit(&self, args: &Value) -> Result<Value> {
        let id = require_string(args, "plan_path")?;
        let date = args
            .get("date")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(crate::today::today_utc);
        let report = crate::archive::archive_phase(&self.plan_path, &id, &date)?;
        Ok(tool_text_result(&format!(
            "exited and archived phase {}: {} plan paths cleared",
            id,
            report.archived_plan_paths.len()
        )))
    }

    fn tool_plan_archive(&self, args: &Value) -> Result<Value> {
        let dry_run = args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let date = args
            .get("date")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(crate::today::today_utc);
        let report = crate::archive::archive(&self.plan_path, dry_run, &date)?;
        if report.is_empty() {
            return Ok(tool_text_result("nothing to archive"));
        }
        let verb = if report.dry_run {
            "would archive"
        } else {
            "archived"
        };
        let phases = report.archived_phase_ids.join(", ");
        Ok(tool_text_result(&format!(
            "{verb} {} phase(s): {phases}",
            report.archived_phase_ids.len()
        )))
    }
}

fn require_string(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| anyhow!("missing required argument `{key}`"))
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": SERVER_VERSION
        }
    })
}

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "plan_list",
                "description": "Read PLAN.md and return its AST as pretty-printed JSON.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_check",
                "description": "Mark the node with `plan_path` as completed ([x]). No-op if already checked.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "plan_path": {
                            "type": "string",
                            "description": "Dotted id of the node, e.g. `1.2.3`."
                        }
                    },
                    "required": ["plan_path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_uncheck",
                "description": "Mark the node with `plan_path` as not completed ([ ]). No-op if already unchecked.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "plan_path": {"type": "string"}
                    },
                    "required": ["plan_path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_skip",
                "description": "Mark the node with `plan_path` as won't-do ([-]). Resolved-but-not-done; archive treats this like checked. No-op if already skipped.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "plan_path": {"type": "string"}
                    },
                    "required": ["plan_path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_phase_exit",
                "description": "Exit a specific phase: validate every leaf in its subtree is resolved ([x] or [-]), then archive just that phase to PLAN_ARCHIVE.md. Errors out if the subtree still has [ ] leaves.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "plan_path": {"type": "string", "description": "Id of the phase to exit, e.g. `1.0`."},
                        "date": {"type": "string", "description": "YYYY-MM-DD header for the archive section. Defaults to today."}
                    },
                    "required": ["plan_path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_add",
                "description": "Add a new leaf at `plan_path` with the given subject. Parent must already exist. Use canonical numbering (e.g. `1.2.3`, `Inbox.4`).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "plan_path": {"type": "string"},
                        "subject": {"type": "string"}
                    },
                    "required": ["plan_path", "subject"],
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_archive",
                "description": "Sweep every fully-complete top-level phase to PLAN_ARCHIVE.md. Optional `dry_run` and `date` (YYYY-MM-DD) arguments.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "dry_run": {"type": "boolean"},
                        "date": {"type": "string", "description": "YYYY-MM-DD header"}
                    },
                    "additionalProperties": false
                }
            }
        ]
    })
}

fn tool_text_result(text: &str) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": text
        }]
    })
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }
    fn error(id: Value, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch_plan(contents: &str) -> (PathBuf, McpServer) {
        let dir =
            std::env::temp_dir().join(format!("plan-bridge-mcp-{}-{}", std::process::id(), uniq()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("PLAN.md");
        std::fs::write(&p, contents).unwrap();
        let s = McpServer::new(p.clone());
        (p, s)
    }

    fn uniq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    fn rpc(server: &McpServer, json: Value) -> Value {
        let line = serde_json::to_string(&json).unwrap();
        let resp = server.handle_line(&line).expect("server returned None");
        serde_json::from_str(&resp).unwrap()
    }

    #[test]
    fn initialize_returns_capabilities() {
        let (_, s) = scratch_plan("- [ ] 1.0 Phase\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        );
        let result = resp.get("result").expect("ok response");
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], SERVER_NAME);
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tools_list_includes_all_tools() {
        let (_, s) = scratch_plan("- [ ] 1.0 Phase\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        );
        let tools = resp["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        for expected in &[
            "plan_list",
            "plan_check",
            "plan_uncheck",
            "plan_add",
            "plan_archive",
        ] {
            assert!(names.contains(expected), "missing {expected}: {names:?}");
        }
    }

    #[test]
    fn plan_list_returns_ast_text() {
        let (_, s) = scratch_plan("- [ ] 1.0 Phase\n  - [x] 1.1 Done\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "plan_list", "arguments": {}}}),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"id\": \"1.0\""));
        assert!(text.contains("\"id\": \"1.1\""));
    }

    #[test]
    fn plan_check_mutates_plan_md() {
        let (p, s) = scratch_plan("- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "plan_check", "arguments": {"plan_path": "1.1"}}}),
        );
        assert!(resp.get("error").is_none(), "unexpected error: {resp}");
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("- [x] 1.1 Task"), "got: {after}");
    }

    #[test]
    fn plan_check_unknown_id_errors() {
        let (_, s) = scratch_plan("- [ ] 1.0 Phase\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "plan_check", "arguments": {"plan_path": "9.9"}}}),
        );
        assert!(resp.get("error").is_some(), "expected error: {resp}");
        let msg = resp["error"]["message"].as_str().unwrap();
        assert!(msg.contains("9.9"));
    }

    #[test]
    fn plan_uncheck_works() {
        let (p, s) = scratch_plan("- [x] 1.0 Phase\n");
        rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "plan_uncheck", "arguments": {"plan_path": "1.0"}}}),
        );
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("- [ ] 1.0 Phase"), "got: {after}");
    }

    #[test]
    fn plan_add_inserts_new_leaf() {
        let (p, s) = scratch_plan("- [ ] 1.0 Phase\n");
        rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "plan_add", "arguments": {"plan_path": "1.1", "subject": "new task"}}}),
        );
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("  - [ ] 1.1 new task"), "got: {after}");
    }

    #[test]
    fn plan_add_rejects_existing_id() {
        let (_, s) = scratch_plan("- [ ] 1.0 Phase\n  - [ ] 1.1 Old\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {"name": "plan_add", "arguments": {"plan_path": "1.1", "subject": "x"}}}),
        );
        assert!(resp.get("error").is_some());
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains("already exists")
        );
    }

    #[test]
    fn unknown_method_errors() {
        let (_, s) = scratch_plan("- [ ] 1.0 Phase\n");
        let resp = rpc(&s, json!({"jsonrpc": "2.0", "id": 9, "method": "blarg"}));
        assert!(resp.get("error").is_some());
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains("unknown method")
        );
    }

    #[test]
    fn malformed_json_returns_parse_error_with_null_id() {
        let (_, s) = scratch_plan("- [ ] 1.0 Phase\n");
        let resp = s.handle_line("not json").expect("got something");
        let parsed: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["id"], Value::Null);
        assert_eq!(parsed["error"]["code"], -32700);
    }

    #[test]
    fn notifications_get_no_response() {
        let (_, s) = scratch_plan("- [ ] 1.0 Phase\n");
        // Notification: no `id` field.
        let resp = s.handle_line(r#"{"jsonrpc": "2.0", "method": "notifications/initialized"}"#);
        assert!(resp.is_none());
    }

    #[test]
    fn plan_skip_marks_wont_do() {
        let (p, s) = scratch_plan("- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 20, "method": "tools/call", "params": {"name": "plan_skip", "arguments": {"plan_path": "1.1"}}}),
        );
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("- [-] 1.1 Task"), "got: {after}");
    }

    #[test]
    fn plan_skip_no_op_when_already_skipped() {
        let (_, s) = scratch_plan("- [-] 1.0 Skipped\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 21, "method": "tools/call", "params": {"name": "plan_skip", "arguments": {"plan_path": "1.0"}}}),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("already"), "got: {text}");
    }

    #[test]
    fn plan_phase_exit_archives_one_phase() {
        let (p, s) = scratch_plan("- [x] 1.0 Done\n  - [x] 1.1 Sub\n- [x] 2.0 Also done\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 22, "method": "tools/call", "params": {"name": "plan_phase_exit", "arguments": {"plan_path": "1.0", "date": "2026-05-16"}}}),
        );
        assert!(resp.get("error").is_none(), "{resp}");
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(!after.contains("1.0 Done"));
        assert!(
            after.contains("2.0 Also done"),
            "untargeted phase should remain"
        );
    }

    #[test]
    fn plan_phase_exit_refuses_unresolved_phase() {
        let (_, s) = scratch_plan("- [ ] 1.0 Phase\n  - [ ] 1.1 Pending\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 23, "method": "tools/call", "params": {"name": "plan_phase_exit", "arguments": {"plan_path": "1.0"}}}),
        );
        assert!(resp.get("error").is_some());
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not fully resolved")
        );
    }

    #[test]
    fn plan_archive_via_mcp() {
        let (p, s) = scratch_plan("- [x] 1.0 Done\n  - [x] 1.1 Sub done\n- [ ] 2.0 Pending\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": {"name": "plan_archive", "arguments": {"date": "2026-05-16"}}}),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("archived"));
        assert!(text.contains("1.0"));
        let plan_md = std::fs::read_to_string(&p).unwrap();
        assert!(!plan_md.contains("1.0 Done"));
        assert!(plan_md.contains("2.0 Pending"));
    }
}
