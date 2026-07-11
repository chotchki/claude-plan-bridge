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
const SERVER_NAME: &str = "claude-plan-bridge";
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
            "plan_backlog" => self.tool_plan_backlog(&args),
            "plan_add" => self.tool_plan_add(&args),
            "plan_add_phase" => self.tool_plan_add_phase(&args),
            "plan_new_phase" => self.tool_plan_new_phase(&args),
            "plan_breakdown" => self.tool_plan_breakdown(&args),
            "plan_promote" => self.tool_plan_promote(&args),
            "plan_next_phase" => self.tool_plan_next_phase(),
            "plan_archive" => self.tool_plan_archive(&args),
            "plan_phase_exit" => self.tool_plan_phase_exit(&args),
            "plan_rename" => self.tool_plan_rename(&args),
            "plan_rename_phase" => self.tool_plan_rename_phase(&args),
            "plan_set_phase_deps" => self.tool_plan_set_phase_deps(&args),
            "plan_activate" => self.tool_plan_activate(&args),
            "plan_deactivate" => self.tool_plan_deactivate(&args),
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

    /// Defer a node: flip to `[>]` (Backlog) and append a bullet under
    /// `## Backlog (not yet phased)` recording the source plan_path + date.
    /// Also drops any state mapping pointing at this path so the harness UI
    /// stops tracking the deferred task. Optional `date` argument overrides
    /// the default (today UTC) for reproducibility.
    fn tool_plan_backlog(&self, args: &Value) -> Result<Value> {
        let id = require_string(args, "plan_path")?;
        let date = args
            .get("date")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(crate::today::today_utc);
        crate::backlog::backlog(&self.plan_path, &id, &date)
            .map(|msg| tool_text_result(&msg))
            .map_err(|e| anyhow!(e))
    }

    fn set_state(&self, args: &Value, target: NodeState, verb: &str) -> Result<Value> {
        let id = require_string(args, "plan_path")?;
        let text = std::fs::read_to_string(&self.plan_path)?;
        let mut plan = parse(&text)?;
        let mut item = plan
            .find_item_mut(&id)
            .ok_or_else(|| anyhow!("no node with id `{id}` in PLAN.md"))?;
        if item.state() == target {
            return Ok(tool_text_result(&format!("{id} was already {verb}")));
        }
        item.set_state(target);
        std::fs::write(&self.plan_path, serialize(&plan))?;
        Ok(tool_text_result(&format!("{verb} {id}")))
    }

    fn tool_plan_add(&self, args: &Value) -> Result<Value> {
        let plan_path = require_string(args, "plan_path")?;
        let subject = require_string(args, "subject")?;
        let text = std::fs::read_to_string(&self.plan_path)?;
        let mut plan = parse(&text)?;
        if plan.contains_id(&plan_path) {
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
            None => plan.phases.push(crate::ast::Phase::from_node(new_node)),
            Some(pid) => {
                plan.add_child_of(&pid, new_node).map_err(|e| anyhow!(e))?;
            }
        }
        std::fs::write(&self.plan_path, serialize(&plan))?;
        Ok(tool_text_result(&format!("added {plan_path} `{subject}`")))
    }

    /// Phase 38.1: create a new FORMATv2 phase header explicitly. Use when
    /// you want to set `depends_on` / `prefer_after` at creation time, or
    /// pre-create a phase with no tasks yet. For the common "just start typing
    /// tasks" path, TaskCreate's auto-anchor still works — this verb is the
    /// surgical option for richer setup.
    /// Phase CE: create a new phase from the phase template (the built-in
    /// default, or a project `PHASE_TEMPLATE.md`), auto-assigning the next
    /// uppercase-letter id. Optionally activates it. MCP mirror of `phase-new`.
    fn tool_plan_new_phase(&self, args: &Value) -> Result<Value> {
        let title = args
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let activate = args
            .get("activate")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let root = self
            .plan_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let text = std::fs::read_to_string(&self.plan_path)?;
        let mut plan = parse(&text)?;
        let id = crate::phase_seq::next_phase_id_for_plan(&self.plan_path);
        if plan.find_phase(&id).is_some() {
            return Err(anyhow!("phase `{id}` already exists"));
        }
        let beats = crate::phase_template::load_template(root);
        let children: Vec<Node> = beats
            .iter()
            .enumerate()
            .map(|(i, subject)| Node {
                id: format!("{id}.{}", i + 1),
                title: subject.clone(),
                state: NodeState::Pending,
                children: vec![],
                annotations: vec![],
            })
            .collect();
        let count = children.len();
        let new_phase = crate::ast::Phase {
            children,
            ..crate::ast::Phase::header_v2_with_deps(id.clone(), title.clone(), vec![], vec![])
        };
        plan.insert_phase(new_phase);
        std::fs::write(&self.plan_path, serialize(&plan))?;
        let activated = if activate {
            let state_path = crate::state::default_state_path_for(&self.plan_path);
            let mut state = crate::state::State::load(&state_path)?;
            state.set_active_phase(Some(id.clone()));
            state.save(&state_path)?;
            " (activated)"
        } else {
            ""
        };
        Ok(tool_text_result(&format!(
            "created phase `{id}` - `{title}` with {count} template task(s){activated}"
        )))
    }

    /// Phase CE: break an existing phase or task into auto-numbered child
    /// tasks. Generic + recursive (any depth) and repeatable (appends after
    /// existing children). MCP mirror of the `phase-breakdown` CLI.
    fn tool_plan_breakdown(&self, args: &Value) -> Result<Value> {
        let parent = require_string(args, "parent")?;
        let tasks: Vec<String> = args
            .get("tasks")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let text = std::fs::read_to_string(&self.plan_path)?;
        let mut plan = parse(&text)?;
        let added = plan.breakdown(&parent, &tasks).map_err(|e| anyhow!(e))?;
        std::fs::write(&self.plan_path, serialize(&plan))?;
        Ok(tool_text_result(&format!(
            "broke `{parent}` into {} task(s): {}",
            added.len(),
            added.join(", ")
        )))
    }

    /// Phase CG/CI: promote a backlog entry into a phase. With no `index`,
    /// returns the numbered backlog entries so the caller can pick one. With an
    /// `index` (1-based) and no `into`, the entry's headline becomes a NEW
    /// phase's title (override via `title`) and its stanza becomes phase prose.
    /// With `into` (CI), the entry is filed as a task under an existing phase or
    /// task instead — `after` positions it via an alpha suffix. Either way the
    /// entry is removed from the backlog. MCP mirror of the `promote` CLI.
    fn tool_plan_promote(&self, args: &Value) -> Result<Value> {
        let text = std::fs::read_to_string(&self.plan_path)?;
        let mut plan = parse(&text)?;
        let index = args.get("index").and_then(Value::as_u64);
        match index {
            None => {
                let entries = plan.backlog_entries();
                if entries.is_empty() {
                    return Ok(tool_text_result("no backlog entries to promote"));
                }
                let mut out = format!("{} backlog entr(y/ies):", entries.len());
                for (i, e) in entries.iter().enumerate() {
                    out.push_str(&format!("\n  {}. {}", i + 1, e.headline));
                }
                Ok(tool_text_result(&out))
            }
            Some(n) if args.get("into").is_some() || args.get("after").is_some() => {
                // CI: file the entry as a task under an existing phase/task.
                let title = args.get("title").and_then(Value::as_str);
                let into = args.get("into").and_then(Value::as_str);
                let after = args.get("after").and_then(Value::as_str);
                let activate = args
                    .get("activate")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let report = plan
                    .promote_backlog_into(n as usize, into, after, title)
                    .map_err(|e| anyhow!(e))?;
                std::fs::write(&self.plan_path, serialize(&plan))?;
                let activated_phase = report
                    .target
                    .split('.')
                    .next()
                    .unwrap_or(&report.target)
                    .to_string();
                let activated = if activate {
                    let state_path = crate::state::default_state_path_for(&self.plan_path);
                    let mut state = crate::state::State::load(&state_path)?;
                    state.set_active_phase(Some(activated_phase.clone()));
                    state.save(&state_path)?;
                    format!(" (activated `{activated_phase}`)")
                } else {
                    String::new()
                };
                let how = if report.faithful {
                    format!(
                        "reconstructed {} task(s): {}",
                        report.created_ids.len(),
                        report.created_ids.join(", ")
                    )
                } else {
                    "filed as a single task (body kept as prose)".to_string()
                };
                Ok(tool_text_result(&format!(
                    "promoted backlog entry {n} into `{}` as `{}` - `{}`{activated} — {how}. Reconcile will surface the new leaf(s) for TaskCreate.",
                    report.target, report.root_id, report.title
                )))
            }
            Some(n) => {
                let title = args.get("title").and_then(Value::as_str);
                let activate = args
                    .get("activate")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let id = crate::phase_seq::next_phase_id_for_plan(&self.plan_path);
                if plan.find_phase(&id).is_some() {
                    return Err(anyhow!("phase `{id}` already exists"));
                }
                let phase_title = plan
                    .promote_backlog_entry(n as usize, title, &id)
                    .map_err(|e| anyhow!(e))?;
                std::fs::write(&self.plan_path, serialize(&plan))?;
                let activated = if activate {
                    let state_path = crate::state::default_state_path_for(&self.plan_path);
                    let mut state = crate::state::State::load(&state_path)?;
                    state.set_active_phase(Some(id.clone()));
                    state.save(&state_path)?;
                    " (activated)"
                } else {
                    ""
                };
                Ok(tool_text_result(&format!(
                    "promoted backlog entry {n} to phase `{id}` - `{phase_title}`{activated}"
                )))
            }
        }
    }

    /// Phase BZ: read-only — report the next uppercase-letter phase id without
    /// mutating PLAN.md. The companion to `plan_add_phase`'s auto-assign, for
    /// the TaskCreate-`plan_path` flow where the agent needs to know the new
    /// phase id (e.g. `CB`) before creating tasks under it.
    fn tool_plan_next_phase(&self) -> Result<Value> {
        let id = crate::phase_seq::next_phase_id_for_plan(&self.plan_path);
        Ok(tool_text_result(&id))
    }

    fn tool_plan_add_phase(&self, args: &Value) -> Result<Value> {
        let title = args
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let depends_on: Vec<String> = args
            .get("depends_on")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let prefer_after: Vec<String> = args
            .get("prefer_after")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let after = args.get("after").and_then(Value::as_str).map(String::from);

        // Phase BZ: `id` is optional. Provided → require it to be an
        // uppercase-letter id (BZ.6 guardrail: phase ids are letters going
        // forward; legacy numeric ids are not created here). Omitted →
        // auto-assign the next id by scanning PLAN.md + PLAN_ARCHIVE.md, so
        // callers don't hand-pick (or collide on) a letter.
        let id = match args.get("id").and_then(Value::as_str) {
            Some(explicit) => {
                if !crate::phase_seq::is_sequence_phase_id(explicit) {
                    return Err(anyhow!(
                        "phase id `{explicit}` must be uppercase letters \
                         (A, B, ..., Z, AA, AB, ...) and at most {} long. Omit `id` to \
                         auto-assign the next one, or call `plan_next_phase` to see it.",
                        crate::phase_seq::MAX_PHASE_ID_LEN
                    ));
                }
                explicit.to_string()
            }
            None => crate::phase_seq::next_phase_id_for_plan(&self.plan_path),
        };

        let text = std::fs::read_to_string(&self.plan_path)?;
        let mut plan = parse(&text)?;
        if plan.find_phase(&id).is_some() {
            return Err(anyhow!("phase `{id}` already exists"));
        }
        let new_phase = crate::ast::Phase::header_v2_with_deps(
            id.clone(),
            title.clone(),
            depends_on,
            prefer_after,
        );
        if let Some(after_id) = after {
            let pos = plan
                .phases
                .iter()
                .position(|p| p.id == after_id)
                .ok_or_else(|| anyhow!("--after target `{after_id}` not found at top level"))?;
            plan.phases.insert(pos + 1, new_phase);
        } else {
            plan.insert_phase(new_phase);
        }
        std::fs::write(&self.plan_path, serialize(&plan))?;
        Ok(tool_text_result(&format!("added phase `{id}` - `{title}`")))
    }

    /// Phase 38.2: rename a phase's title. Phase-specific variant of
    /// `plan_rename` — refuses task ids loudly so callers don't accidentally
    /// rename a task when they meant a phase.
    fn tool_plan_rename_phase(&self, args: &Value) -> Result<Value> {
        let id = require_string(args, "id")?;
        let new_title = require_string(args, "new_title")?;
        let text = std::fs::read_to_string(&self.plan_path)?;
        let mut plan = parse(&text)?;
        let phase = plan
            .find_phase_mut(&id)
            .ok_or_else(|| anyhow!("no phase with id `{id}` at top level"))?;
        if phase.title == new_title {
            return Ok(tool_text_result(&format!(
                "phase `{id}` already titled `{new_title}`"
            )));
        }
        phase.title = new_title.clone();
        std::fs::write(&self.plan_path, serialize(&plan))?;
        Ok(tool_text_result(&format!(
            "renamed phase `{id}` → `{new_title}`"
        )))
    }

    /// Phase 38.3: replace a phase's `depends_on` and/or `prefer_after` lists.
    /// Either field can be set independently (omit to leave unchanged) or
    /// cleared by passing an empty array. Phase ids only — refuses tasks.
    fn tool_plan_set_phase_deps(&self, args: &Value) -> Result<Value> {
        let id = require_string(args, "id")?;
        // None means "don't touch this list"; Some(vec![]) means "clear it".
        let new_depends_on: Option<Vec<String>> = args.get("depends_on").and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
        });
        let new_prefer_after: Option<Vec<String>> = args.get("prefer_after").and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
        });
        if new_depends_on.is_none() && new_prefer_after.is_none() {
            return Err(anyhow!(
                "plan_set_phase_deps: pass at least one of `depends_on` or `prefer_after`"
            ));
        }

        let text = std::fs::read_to_string(&self.plan_path)?;
        let mut plan = parse(&text)?;
        let phase = plan
            .find_phase_mut(&id)
            .ok_or_else(|| anyhow!("no phase with id `{id}` at top level"))?;
        if let Some(deps) = new_depends_on {
            phase.depends_on = deps;
        }
        if let Some(after) = new_prefer_after {
            phase.prefer_after = after;
        }
        let summary = format!(
            "updated deps for phase `{id}`: depends_on={:?}, prefer_after={:?}",
            phase.depends_on, phase.prefer_after
        );
        std::fs::write(&self.plan_path, serialize(&plan))?;
        Ok(tool_text_result(&summary))
    }

    /// Phase 40.2: focus subsequent resume/reconcile/writeback flows on a
    /// single phase. Persists `state.active_phase = Some(id)`. Surfaces any
    /// unmet hard `*(depends on: ...)*` markers in the response so the agent
    /// sees the sequencing constraint up front (40.5).
    fn tool_plan_activate(&self, args: &Value) -> Result<Value> {
        let id = require_string(args, "id")?;
        let text = std::fs::read_to_string(&self.plan_path)?;
        let plan = parse(&text)?;
        let phase = plan
            .find_phase(&id)
            .ok_or_else(|| anyhow!("no phase with id `{id}` at top level"))?;

        // Note unmet hard deps on the response so the agent sees the
        // sequencing constraint without a separate reconcile round-trip.
        let active_ids: std::collections::HashSet<&str> =
            plan.phases.iter().map(|p| p.id.as_str()).collect();
        let unmet_deps: Vec<&String> = phase
            .depends_on
            .iter()
            .filter(|d| active_ids.contains(d.as_str()))
            .collect();
        let dep_note = if unmet_deps.is_empty() {
            String::new()
        } else {
            format!(
                " (NOTE: depends on {} — not yet archived)",
                unmet_deps
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        let state_path = crate::state::default_state_path_for(&self.plan_path);
        let mut state = crate::state::State::load(&state_path)?;
        let prior = state.active_phase().map(String::from);
        state.set_active_phase(Some(id.clone()));
        state.save(&state_path)?;

        let prior_note = match prior {
            Some(p) if p == id => String::from(" (already active)"),
            Some(p) => format!(" (was active: `{p}`)"),
            None => String::new(),
        };
        Ok(tool_text_result(&format!(
            "activated phase `{id}`{prior_note}{dep_note}"
        )))
    }

    /// Phase 40.2: clear any active phase focus. After this, resume +
    /// reconcile + writeback behave as if activation had never been set
    /// (load all open leaves, no cross-phase warnings).
    fn tool_plan_deactivate(&self, _args: &Value) -> Result<Value> {
        let state_path = crate::state::default_state_path_for(&self.plan_path);
        let mut state = crate::state::State::load(&state_path)?;
        match state.active_phase().map(String::from) {
            Some(prior) => {
                state.set_active_phase(None);
                state.save(&state_path)?;
                Ok(tool_text_result(&format!(
                    "deactivated focus (was `{prior}`)"
                )))
            }
            None => Ok(tool_text_result("no active phase to deactivate (no-op)")),
        }
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
        let descope_pending = args
            .get("descope_pending")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let report = crate::archive::archive_phase(&self.plan_path, &id, &date, descope_pending)?;
        Ok(tool_text_result(&format!(
            "exited and archived phase {}: {} plan paths cleared",
            id,
            report.archived_plan_paths.len()
        )))
    }

    /// Rewrite the title of the node at `plan_path`. Parallels the writeback
    /// `TaskUpdate(subject=...)` path: if a tracked task points at this
    /// plan_path, also refresh its `last_synced_title` so reconcile doesn't
    /// redundantly fire `LeafTitleChanged` on the next prompt.
    fn tool_plan_rename(&self, args: &Value) -> Result<Value> {
        let id = require_string(args, "plan_path")?;
        let new_subject = require_string(args, "new_subject")?;
        let state_path = crate::state::default_state_path_for(&self.plan_path);

        crate::lock::with_state_lock(&state_path, crate::lock::DEFAULT_TIMEOUT, || {
            let text = std::fs::read_to_string(&self.plan_path)
                .with_context(|| format!("read {}", self.plan_path.display()))?;
            let mut plan = parse(&text)?;

            let mut item = plan
                .find_item_mut(&id)
                .ok_or_else(|| anyhow!("no node with id `{id}` in PLAN.md"))?;

            if item.title() == new_subject {
                return Ok(tool_text_result(&format!(
                    "{id} already titled `{new_subject}`"
                )));
            }

            item.set_title(new_subject.clone());
            std::fs::write(&self.plan_path, serialize(&plan))
                .with_context(|| format!("write {}", self.plan_path.display()))?;

            // Refresh `last_synced_title` for any tracked task at this path —
            // typically zero or one entry. State file may not exist yet on a
            // fresh project; load() returns default in that case.
            let mut state = crate::state::State::load(&state_path)?;
            let tracked_tids: Vec<String> = state
                .mappings
                .iter()
                .filter(|(_, m)| m.plan_path == id)
                .map(|(tid, _)| tid.clone())
                .collect();
            let touched = !tracked_tids.is_empty();
            for tid in &tracked_tids {
                if let Some(m) = state.mappings.get_mut(tid) {
                    m.last_synced_title = new_subject.clone();
                }
            }
            if touched {
                state.save(&state_path)?;
            }

            Ok(tool_text_result(&format!(
                "renamed {id} to `{new_subject}`"
            )))
        })
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
                "name": "plan_backlog",
                "description": "Defer the node with `plan_path`: flip to [>] (Backlog) and append a bullet under `## Backlog (not yet phased)` recording the source plan_path + date. Drops any state mapping pointing at this path. Archive treats Backlog like resolved. No-op if already deferred; errors if the node is already [x] or [-].",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "plan_path": {"type": "string"},
                        "date": {"type": "string", "description": "YYYY-MM-DD for the backlog bullet. Defaults to today."}
                    },
                    "required": ["plan_path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_phase_exit",
                "description": "Exit a specific phase: validate every leaf in its subtree is resolved ([x] / [-] / [>]), then archive just that phase to PLAN_ARCHIVE.md. Errors out if the subtree still has [ ] leaves — pass `descope_pending: true` to move them to the bottom `# Backlog (not yet phased)` section first.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "plan_path": {"type": "string", "description": "Id of the phase to exit, e.g. `1.0`."},
                        "date": {"type": "string", "description": "YYYY-MM-DD header for the archive section. Defaults to today."},
                        "descope_pending": {"type": "boolean", "description": "When true, move any `[ ]` Pending leaves to `# Backlog` as `- <id> - descoped from phase <PHASE> on <date>` notes before archiving. Default false (hard error)."}
                    },
                    "required": ["plan_path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_add",
                "description": "Add a new leaf at `plan_path` with the given subject. Parent must already exist. Use canonical numbering (e.g. `1.2.3`, `AH.4`).",
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
                "name": "plan_add_phase",
                "description": "Create a new FORMATv2 phase header (`## Phase <id> - <title>`) with optional `depends_on` and `prefer_after` markers. Omit `id` to auto-assign the next uppercase-letter phase id (`A`..`Z` -> `AA`.. -> ...) — the assigned id is echoed in the result. Use when you want explicit phase metadata at creation time, or to pre-create a phase with no tasks. TaskCreate's auto-anchor still handles the common 'just start typing tasks' path.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "Phase id (uppercase letters, e.g. `AI`, `AS`). Omit to auto-assign the next id in the sequence."},
                        "title": {"type": "string"},
                        "depends_on": {"type": "array", "items": {"type": "string"}, "description": "Hard sequencing — surfaced in reconcile as 'depends on X — X not archived'."},
                        "prefer_after": {"type": "array", "items": {"type": "string"}, "description": "Soft sequencing hint."},
                        "after": {"type": "string", "description": "Insert immediately after this existing phase id (positional). Defaults to id-sort order."}
                    },
                    "required": [],
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_next_phase",
                "description": "Read-only: report the next uppercase-letter phase id (`A`..`Z` -> `AA`.. -> ...), reconstructed by scanning PLAN.md + PLAN_ARCHIVE.md for the highest existing uppercase-letter phase id. Returns `A` for a fresh project; legacy numeric phase ids are ignored. Use before TaskCreate-ing a new phase so you don't hand-pick or collide on the next letter.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_new_phase",
                "description": "Create a new phase from the phase template in one shot: auto-assigns the next uppercase-letter id, fills it with the template's beats (the built-in default — Plan & breakdown, Implement, Tests + docs, Review, Release — or a project `PHASE_TEMPLATE.md` if present), and optionally activates it. The templated counterpart to `plan_add_phase`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "title": {"type": "string", "description": "Phase title."},
                        "activate": {"type": "boolean", "description": "Activate (focus the working set on) the new phase after creating it."}
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_breakdown",
                "description": "Break an existing phase or task into auto-numbered child tasks in one write. Generic + recursive: `parent` may be a phase id (`CE`) or a task id at any depth (`CE.3`, `CE.3.2`), and new children append after any that already exist, so it can be run repeatedly. This is the breakdown half of the plan-&-breakdown beat.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "parent": {"type": "string", "description": "Phase or task id to break down, e.g. `CE.3`."},
                        "tasks": {"type": "array", "items": {"type": "string"}, "description": "Child task subjects, in order."}
                    },
                    "required": ["parent", "tasks"],
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_promote",
                "description": "Promote a backlog entry into a phase. Omit `index` to list the numbered backlog entries (a backlog entry is a top-level `- ` bullet plus everything beneath it up to the next top-level bullet). Default: the entry becomes a NEW top-level phase — headline is the title (override with `title`), the rest becomes phase-level prose (NOT tasks; break down afterward with `plan_breakdown`). With `into` (a phase id like `CE` or a task id like `CE.3`), the entry is filed as a TASK under that existing node instead: a descoped subtree (`- X.1 - …`) is rebuilt faithfully with ids remapped onto the target, anything else lands as a single leaf. `after` (a sibling id) slots the new task right after it via an alpha suffix (`CE.1` → `CE.1a`) without renumbering. The promoted leaves surface for TaskCreate via reconcile on the next turn. The entry is removed from the backlog.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "index": {"type": "integer", "description": "1-based index of the backlog entry to promote. Omit to list entries."},
                        "title": {"type": "string", "description": "Override the title (defaults to the entry headline)."},
                        "activate": {"type": "boolean", "description": "Activate (focus the working set on) the destination phase after promoting."},
                        "into": {"type": "string", "description": "File the entry as a task under this existing phase or task id (e.g. `CE` or `CE.3`) instead of minting a new phase."},
                        "after": {"type": "string", "description": "With `into`, position the new task immediately after this sibling id via an alpha suffix. Implies the parent when `into` is omitted."}
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_rename_phase",
                "description": "Rename a phase's title. Phase-specific variant of `plan_rename` — refuses task ids to keep operations explicit. Use `plan_rename` for tasks.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "Phase id (e.g. `AI`, `AS`)."},
                        "new_title": {"type": "string"}
                    },
                    "required": ["id", "new_title"],
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_set_phase_deps",
                "description": "Replace a phase's `depends_on` / `prefer_after` sequencing markers. Pass either field as an array (empty array clears it); omit to leave that field unchanged. At least one of the two must be provided. Flips the phase to FORMATv2 header form if it was a legacy v1 anchor.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "depends_on": {"type": "array", "items": {"type": "string"}, "description": "Replace `*(depends on: ...)*`. Empty array clears."},
                        "prefer_after": {"type": "array", "items": {"type": "string"}, "description": "Replace `*(prefer after: ...)*`. Empty array clears."}
                    },
                    "required": ["id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_activate",
                "description": "Focus the bridge on one phase: subsequent SessionStart rehydration loads only that phase's leaves, reconcile foregrounds its drift, and writeback emits a soft warning on cross-phase TaskCreates. Surfaces any unmet `*(depends on)*` markers in the response so the agent sees sequencing constraints up front. Persists in state.json — survives /clear.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "Phase id (e.g. `AS`, `1.0`)."}
                    },
                    "required": ["id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "plan_deactivate",
                "description": "Clear the active phase focus. After this, resume + reconcile + writeback behave as if activation had never been set. No-op when nothing was active.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
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
            },
            {
                "name": "plan_rename",
                "description": "Rewrite the title of the node at `plan_path` to `new_subject`. Refreshes the synced baseline for any tracked task at that path so reconcile is quiet next turn. No-op when the title already matches.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "plan_path": {"type": "string"},
                        "new_subject": {"type": "string"}
                    },
                    "required": ["plan_path", "new_subject"],
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
        let p = crate::test_utils::write_plan(&crate::test_utils::scratch_dir("mcp"), contents);
        let s = McpServer::new(p.clone());
        (p, s)
    }

    fn rpc(server: &McpServer, json: Value) -> Value {
        let line = serde_json::to_string(&json).unwrap();
        let resp = server.handle_line(&line).expect("server returned None");
        serde_json::from_str(&resp).unwrap()
    }

    #[test]
    fn initialize_returns_capabilities() {
        let (_, s) = scratch_plan("## Phase 1 - Phase\n");
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
        let (_, s) = scratch_plan("## Phase 1 - Phase\n");
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
            "plan_skip",
            "plan_backlog",
            "plan_add",
            "plan_add_phase",
            "plan_next_phase",
            "plan_new_phase",
            "plan_breakdown",
            "plan_promote",
            "plan_archive",
            "plan_phase_exit",
            "plan_rename",
            "plan_rename_phase",
            "plan_set_phase_deps",
            "plan_activate",
            "plan_deactivate",
        ] {
            assert!(names.contains(expected), "missing {expected}: {names:?}");
        }
    }

    #[test]
    fn plan_add_phase_inserts_v2_header_with_markers() {
        let (_, s) = scratch_plan("## Phase AI - Existing\n\n- [ ] AI.0 task\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_add_phase",
                "arguments": {
                    "id": "AS",
                    "title": "Spine",
                    "depends_on": ["AR", "AQ"],
                    "prefer_after": ["AB"]
                }
            }}),
        );
        assert!(resp.get("error").is_none(), "got: {resp}");
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        assert!(
            after.contains("## Phase AS - Spine *(depends on: AR, AQ)* *(prefer after: AB)*"),
            "v2 header with markers landed:\n{after}"
        );
    }

    #[test]
    fn plan_promote_lists_then_promotes_backlog_entry() {
        let (_, s) = scratch_plan(
            "## Phase A - Existing\n- [ ] A.1 - t\n\n# Backlog (not yet phased)\n\n- **Auth hardening** — rotate keys\n  - login flow\n- **Drop fs4** — std lock\n",
        );
        // List mode (no index) enumerates the entries.
        let list = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_promote", "arguments": {}
            }}),
        );
        assert!(
            list.to_string().contains("1. Auth hardening"),
            "got: {list}"
        );
        assert!(list.to_string().contains("2. Drop fs4"), "got: {list}");

        // Promote entry 1 → Phase B, prose carried, entry removed from backlog.
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                "name": "plan_promote", "arguments": {"index": 1}
            }}),
        );
        assert!(resp.get("error").is_none(), "got: {resp}");
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        assert!(
            after.contains("## Phase B - Auth hardening"),
            "phase:\n{after}"
        );
        assert!(after.contains("rotate keys"), "prose carried:\n{after}");
        assert!(
            !after.contains("**Auth hardening**"),
            "promoted entry removed from backlog:\n{after}"
        );
        assert!(
            after.contains("- **Drop fs4**"),
            "other entry remains:\n{after}"
        );
    }

    #[test]
    fn plan_promote_into_files_entry_as_task_under_existing_phase() {
        let (_, s) = scratch_plan(
            "## Phase CE - Existing\n- [ ] CE.1 - a\n- [ ] CE.2 - b\n\n\
             # Backlog (not yet phased)\n\n\
             - X.1 - Descoped *(deferred from phase `X` on 2026-01-01)*\n  - X.1.1 - child\n",
        );
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_promote", "arguments": {"index": 1, "into": "CE"}
            }}),
        );
        assert!(resp.get("error").is_none(), "got: {resp}");
        assert!(
            resp.to_string().contains("reconstructed 2 task(s)"),
            "faithful reported: {resp}"
        );
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        assert!(
            after.contains("- [ ] CE.3 - Descoped"),
            "root task:\n{after}"
        );
        assert!(
            after.contains("  - [ ] CE.3.1 - child"),
            "child task:\n{after}"
        );
        assert!(!after.contains("X.1.1"), "old ids remapped:\n{after}");
    }

    #[test]
    fn plan_promote_after_positions_via_alpha_suffix() {
        let (_, s) = scratch_plan(
            "## Phase CE - x\n- [ ] CE.1 - a\n- [ ] CE.2 - b\n\n\
             # Backlog (not yet phased)\n\n- loose idea\n",
        );
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_promote", "arguments": {"index": 1, "into": "CE", "after": "CE.1"}
            }}),
        );
        assert!(resp.to_string().contains("as `CE.1a`"), "suffix id: {resp}");
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        let a = after.find("CE.1 - a").unwrap();
        let mid = after.find("CE.1a - loose idea").unwrap();
        let b = after.find("CE.2 - b").unwrap();
        assert!(a < mid && mid < b, "wedged between siblings:\n{after}");
    }

    #[test]
    fn plan_add_phase_auto_assigns_next_id_when_omitted() {
        // BY is the high-water mark; omitting `id` must auto-assign BZ, echo it
        // in the result, and land the header in PLAN.md.
        let (_, s) = scratch_plan("## Phase BY - Prior\n\n- [ ] BY.1 task\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_add_phase",
                "arguments": {"title": "Auto"}
            }}),
        );
        assert!(resp.get("error").is_none(), "got: {resp}");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("BZ"),
            "result should echo assigned id BZ: {text}"
        );
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        assert!(
            after.contains("## Phase BZ - Auto"),
            "auto-assigned phase landed:\n{after}"
        );
    }

    #[test]
    fn plan_add_phase_auto_assign_clears_archived_ids() {
        // CA already lives in the sibling PLAN_ARCHIVE.md. Auto-assign must
        // clear it (CB), never re-hand-out the swept id.
        let (_, s) = scratch_plan("## Phase BZ - Live\n\n- [ ] BZ.1 task\n");
        std::fs::write(
            s.plan_path.with_file_name("PLAN_ARCHIVE.md"),
            "## Phase BY - swept\n## Phase CA - swept\n",
        )
        .unwrap();
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_add_phase",
                "arguments": {"title": "Next"}
            }}),
        );
        assert!(resp.get("error").is_none(), "got: {resp}");
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        assert!(
            after.contains("## Phase CB - Next"),
            "expected CB:\n{after}"
        );
        assert!(!after.contains("## Phase CA"), "must not reuse archived CA");
    }

    #[test]
    fn plan_next_phase_reports_next_id_without_mutating() {
        let (_, s) = scratch_plan("## Phase BY - Prior\n\n- [ ] BY.1 task\n");
        let before = std::fs::read_to_string(&s.plan_path).unwrap();
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_next_phase",
                "arguments": {}
            }}),
        );
        assert!(resp.get("error").is_none(), "got: {resp}");
        assert_eq!(resp["result"]["content"][0]["text"].as_str().unwrap(), "BZ");
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        assert_eq!(before, after, "plan_next_phase must be read-only");
    }

    #[test]
    fn plan_new_phase_applies_default_template() {
        let (_, s) = scratch_plan("## Phase BY - prior\n- [x] BY.1 - x\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_new_phase",
                "arguments": {"title": "Templated"}
            }}),
        );
        assert!(resp.get("error").is_none(), "got: {resp}");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("BZ"), "should report the assigned id: {text}");
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        assert!(after.contains("## Phase BZ - Templated"), "{after}");
        assert!(after.contains("- [ ] BZ.1 - Plan & breakdown"), "{after}");
        assert!(after.contains("- [ ] BZ.5 - Release"), "{after}");
    }

    #[test]
    fn plan_new_phase_uses_project_template_and_activates() {
        let (_, s) = scratch_plan("## Phase BY - prior\n- [x] BY.1 - x\n");
        let root = s.plan_path.parent().unwrap();
        std::fs::write(root.join("PHASE_TEMPLATE.md"), "- Spike\n- Build\n").unwrap();
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_new_phase",
                "arguments": {"title": "Custom", "activate": true}
            }}),
        );
        assert!(resp.get("error").is_none(), "got: {resp}");
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        assert!(after.contains("- [ ] BZ.1 - Spike"), "{after}");
        assert!(after.contains("- [ ] BZ.2 - Build"), "{after}");
        assert!(
            !after.contains("BZ.3"),
            "template had only 2 beats: {after}"
        );
        let state_path = crate::state::default_state_path_for(&s.plan_path);
        let state = crate::state::State::load(&state_path).unwrap();
        assert_eq!(state.active_phase(), Some("BZ"));
    }

    #[test]
    fn plan_breakdown_adds_recursive_children() {
        let (_, s) = scratch_plan("## Phase CE - x\n- [ ] CE.3 - Implement\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_breakdown",
                "arguments": {"parent": "CE.3", "tasks": ["codec", "scan"]}
            }}),
        );
        assert!(resp.get("error").is_none(), "got: {resp}");
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        assert!(after.contains("- [ ] CE.3.1 - codec"), "{after}");
        assert!(after.contains("- [ ] CE.3.2 - scan"), "{after}");
    }

    #[test]
    fn plan_add_phase_rejects_non_letter_explicit_id() {
        // BZ.6 guardrail: an explicit id must be uppercase letters. Numeric,
        // lowercase, dotted, and mixed ids are refused with guidance.
        let (_, s) = scratch_plan("## Phase BY - Prior\n\n- [ ] BY.1 task\n");
        for bad in ["42", "bz", "BZ.1", "A1"] {
            let resp = rpc(
                &s,
                json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                    "name": "plan_add_phase",
                    "arguments": {"id": bad, "title": "Nope"}
                }}),
            );
            let err = resp["error"]["message"].as_str().unwrap_or("");
            assert!(
                err.contains("uppercase letters"),
                "id {bad:?} should be rejected with guidance: {resp}"
            );
        }
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        assert!(
            !after.contains("Nope"),
            "rejected phases must not land:\n{after}"
        );
    }

    #[test]
    fn plan_add_phase_refuses_duplicate_id() {
        let (_, s) = scratch_plan("## Phase AI - Already there\n\n- [ ] AI.0 task\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                "name": "plan_add_phase",
                "arguments": {"id": "AI", "title": "Duplicate"}
            }}),
        );
        let err = resp.get("error").expect("duplicate must error: {resp}");
        let msg = err["message"].as_str().unwrap_or("");
        assert!(
            msg.contains("AI") && msg.to_lowercase().contains("already"),
            "error mentions duplicate: {msg}"
        );
    }

    #[test]
    fn plan_rename_phase_rewrites_title_only() {
        let (_, s) =
            scratch_plan("## Phase AI - Old title *(depends on: AR)*\n\n- [ ] AI.0 task\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_rename_phase",
                "arguments": {"id": "AI", "new_title": "Fresh title"}
            }}),
        );
        assert!(resp.get("error").is_none(), "got: {resp}");
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        assert!(
            after.contains("## Phase AI - Fresh title *(depends on: AR)*"),
            "title rewritten, markers preserved:\n{after}"
        );
        // Tasks untouched (serializer normalizes to the ` - ` separator).
        assert!(after.contains("- [ ] AI.0 - task"));
    }

    #[test]
    fn plan_rename_phase_refuses_task_ids() {
        let (_, s) = scratch_plan("## Phase AI - Title\n\n- [ ] AI.0 task\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_rename_phase",
                "arguments": {"id": "AI.0", "new_title": "Nope"}
            }}),
        );
        let err = resp.get("error").expect("must error: {resp}");
        assert!(
            err["message"].as_str().unwrap_or("").contains("AI.0"),
            "error mentions the bad id"
        );
    }

    #[test]
    fn plan_set_phase_deps_replaces_both_lists() {
        let (_, s) = scratch_plan("## Phase AS - Spine *(depends on: AR)*\n\n- [ ] AS.0 plan\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_set_phase_deps",
                "arguments": {
                    "id": "AS",
                    "depends_on": ["AR", "AQ"],
                    "prefer_after": ["AB"]
                }
            }}),
        );
        assert!(resp.get("error").is_none(), "got: {resp}");
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        assert!(
            after.contains("## Phase AS - Spine *(depends on: AR, AQ)* *(prefer after: AB)*"),
            "both markers updated:\n{after}"
        );
    }

    #[test]
    fn plan_set_phase_deps_clears_with_empty_array() {
        let (_, s) = scratch_plan(
            "## Phase AS - Spine *(depends on: AR)* *(prefer after: AB)*\n\n- [ ] AS.0 plan\n",
        );
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_set_phase_deps",
                "arguments": {"id": "AS", "depends_on": []}
            }}),
        );
        assert!(resp.get("error").is_none(), "got: {resp}");
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        // depends_on cleared, prefer_after untouched.
        assert!(
            !after.contains("*(depends on:"),
            "depends_on marker gone:\n{after}"
        );
        assert!(
            after.contains("*(prefer after: AB)*"),
            "prefer_after preserved:\n{after}"
        );
    }

    #[test]
    fn plan_set_phase_deps_requires_at_least_one_field() {
        let (_, s) = scratch_plan("## Phase AS - Spine\n\n- [ ] AS.0 plan\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_set_phase_deps",
                "arguments": {"id": "AS"}
            }}),
        );
        assert!(resp.get("error").is_some(), "must error: {resp}");
    }

    #[test]
    fn plan_activate_sets_state_active_phase_and_notes_unmet_deps() {
        let (_, s) = scratch_plan(
            "## Phase AR - View rollout\n\n- [ ] AR.0 plan\n\n## Phase AS - Spine *(depends on: AR)*\n\n- [ ] AS.0 plan\n",
        );
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_activate", "arguments": {"id": "AS"}
            }}),
        );
        assert!(resp.get("error").is_none(), "got: {resp}");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("activated phase `AS`"));
        assert!(
            text.contains("depends on AR"),
            "unmet hard dep surfaced: {text}"
        );
        // Verify the state file actually persisted.
        let state_path = crate::state::default_state_path_for(&s.plan_path);
        let state = crate::state::State::load(&state_path).unwrap();
        assert_eq!(state.active_phase(), Some("AS"));
    }

    #[test]
    fn plan_activate_no_unmet_dep_note_when_deps_already_archived() {
        // AS depends on AR, but only AS is in the plan — AR is implicitly
        // archived. No "depends on" suffix in the response.
        let (_, s) = scratch_plan("## Phase AS - Spine *(depends on: AR)*\n\n- [ ] AS.0 plan\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_activate", "arguments": {"id": "AS"}
            }}),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("activated phase `AS`"));
        assert!(
            !text.contains("depends on"),
            "no unmet-dep note when deps already archived: {text}"
        );
    }

    #[test]
    fn plan_activate_refuses_unknown_phase() {
        let (_, s) = scratch_plan("## Phase AI - Studio\n\n- [ ] AI.0 task\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_activate", "arguments": {"id": "ZZ"}
            }}),
        );
        let err = resp.get("error").expect("ZZ doesn't exist");
        assert!(
            err["message"].as_str().unwrap_or("").contains("ZZ"),
            "error mentions the bad id"
        );
    }

    #[test]
    fn plan_activate_when_already_active_is_silent_noop() {
        let (_, s) = scratch_plan("## Phase AI - Studio\n\n- [ ] AI.0 task\n");
        // First activation.
        rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_activate", "arguments": {"id": "AI"}
            }}),
        );
        // Re-activate same phase.
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                "name": "plan_activate", "arguments": {"id": "AI"}
            }}),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("already active"),
            "second activate of same id reports no-op: {text}"
        );
    }

    #[test]
    fn plan_deactivate_clears_focus() {
        let (_, s) = scratch_plan("## Phase AI - Studio\n\n- [ ] AI.0 task\n");
        rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_activate", "arguments": {"id": "AI"}
            }}),
        );
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                "name": "plan_deactivate", "arguments": {}
            }}),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("deactivated"));
        let state_path = crate::state::default_state_path_for(&s.plan_path);
        let state = crate::state::State::load(&state_path).unwrap();
        assert_eq!(state.active_phase(), None);
    }

    #[test]
    fn plan_deactivate_with_no_active_phase_is_no_op() {
        let (_, s) = scratch_plan("## Phase AI - Studio\n\n- [ ] AI.0 task\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "plan_deactivate", "arguments": {}
            }}),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("no active phase"));
    }

    #[test]
    fn plan_add_phase_with_after_inserts_after_target() {
        let (_, s) = scratch_plan(
            "## Phase AI - First\n\n- [ ] AI.0 a\n\n## Phase AS - Third\n\n- [ ] AS.0 c\n",
        );
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
                "name": "plan_add_phase",
                "arguments": {"id": "AM", "title": "Middle", "after": "AI"}
            }}),
        );
        assert!(resp.get("error").is_none(), "got: {resp}");
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        let ai_pos = after.find("## Phase AI").unwrap();
        let am_pos = after.find("## Phase AM").unwrap();
        let as_pos = after.find("## Phase AS").unwrap();
        assert!(
            ai_pos < am_pos && am_pos < as_pos,
            "AM inserted between AI and AS:\n{after}"
        );
    }

    #[test]
    fn plan_backlog_flips_pending_leaf() {
        let (_, s) = scratch_plan("## Phase 1 - Phase\n  - [ ] 1.1 Task\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 90, "method": "tools/call", "params": {"name": "plan_backlog", "arguments": {"plan_path": "1.1", "date": "2026-05-17"}}}),
        );
        assert!(resp.get("error").is_none(), "got: {resp}");
        let after = std::fs::read_to_string(&s.plan_path).unwrap();
        assert!(after.contains("- [>] 1.1 - Task"), "got: {after}");
        // Phase 38.6: FORMATv2 backlog bullet ` - id - title *(...)*`.
        assert!(
            after.contains("- 1.1 - Task *(deferred from phase `1` on 2026-05-17)*"),
            "v2 bullet form: {after}"
        );
    }

    #[test]
    fn plan_list_returns_ast_text() {
        let (_, s) = scratch_plan("## Phase 1 - Phase\n  - [x] 1.1 Done\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "plan_list", "arguments": {}}}),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        // FORMATv2: a `## Phase 1` header yields phase id "1" (not "1.0").
        assert!(text.contains("\"id\": \"1\""));
        assert!(text.contains("\"id\": \"1.1\""));
    }

    #[test]
    fn plan_check_mutates_plan_md() {
        let (p, s) = scratch_plan("## Phase 1 - Phase\n  - [ ] 1.1 Task\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "plan_check", "arguments": {"plan_path": "1.1"}}}),
        );
        assert!(resp.get("error").is_none(), "unexpected error: {resp}");
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("- [x] 1.1 - Task"), "got: {after}");
    }

    #[test]
    fn plan_check_unknown_id_errors() {
        let (_, s) = scratch_plan("## Phase 1 - Phase\n");
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
        // Uncheck flips a Done leaf back to Pending.
        let (p, s) = scratch_plan("## Phase 1 - Phase\n- [x] 1.1 - Done task\n");
        rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "plan_uncheck", "arguments": {"plan_path": "1.1"}}}),
        );
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("- [ ] 1.1 - Done task"), "got: {after}");
    }

    #[test]
    fn plan_add_inserts_new_leaf() {
        let (p, s) = scratch_plan("## Phase 1 - Phase\n");
        rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "plan_add", "arguments": {"plan_path": "1.1", "subject": "new task"}}}),
        );
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("- [ ] 1.1 - new task"), "got: {after}");
    }

    #[test]
    fn plan_add_rejects_existing_id() {
        let (_, s) = scratch_plan("## Phase 1 - Phase\n  - [ ] 1.1 Old\n");
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
        let (_, s) = scratch_plan("## Phase 1 - Phase\n");
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
        let (_, s) = scratch_plan("## Phase 1 - Phase\n");
        let resp = s.handle_line("not json").expect("got something");
        let parsed: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["id"], Value::Null);
        assert_eq!(parsed["error"]["code"], -32700);
    }

    #[test]
    fn notifications_get_no_response() {
        let (_, s) = scratch_plan("## Phase 1 - Phase\n");
        // Notification: no `id` field.
        let resp = s.handle_line(r#"{"jsonrpc": "2.0", "method": "notifications/initialized"}"#);
        assert!(resp.is_none());
    }

    #[test]
    fn plan_skip_marks_wont_do() {
        let (p, s) = scratch_plan("## Phase 1 - Phase\n  - [ ] 1.1 Task\n");
        rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 20, "method": "tools/call", "params": {"name": "plan_skip", "arguments": {"plan_path": "1.1"}}}),
        );
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("- [-] 1.1 - Task"), "got: {after}");
    }

    #[test]
    fn plan_skip_no_op_when_already_skipped() {
        let (_, s) = scratch_plan("## Phase 1 - Phase\n- [-] 1.1 - Skipped\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 21, "method": "tools/call", "params": {"name": "plan_skip", "arguments": {"plan_path": "1.1"}}}),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("already"), "got: {text}");
    }

    #[test]
    fn plan_phase_exit_archives_one_phase() {
        let (p, s) = scratch_plan(
            "## Phase 1 - Done\n- [x] 1.1 - Sub\n## Phase 2 - Also done\n- [x] 2.1 - Sub\n",
        );
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 22, "method": "tools/call", "params": {"name": "plan_phase_exit", "arguments": {"plan_path": "1", "date": "2026-05-16"}}}),
        );
        assert!(resp.get("error").is_none(), "{resp}");
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(!after.contains("## Phase 1 - Done"));
        assert!(
            after.contains("## Phase 2 - Also done"),
            "untargeted phase should remain"
        );
    }

    #[test]
    fn plan_phase_exit_refuses_unresolved_phase() {
        let (_, s) = scratch_plan("## Phase 1 - Phase\n  - [ ] 1.1 Pending\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 23, "method": "tools/call", "params": {"name": "plan_phase_exit", "arguments": {"plan_path": "1"}}}),
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
    fn plan_rename_leaf_rewrites_title() {
        let (p, s) = scratch_plan("## Phase 1 - Phase\n  - [ ] 1.1 Old title\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 30, "method": "tools/call", "params": {"name": "plan_rename", "arguments": {"plan_path": "1.1", "new_subject": "New title"}}}),
        );
        assert!(resp.get("error").is_none(), "got error: {resp}");
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("- [ ] 1.1 - New title"), "got:\n{after}");
        assert!(!after.contains("Old title"));
    }

    #[test]
    fn plan_rename_parent_preserves_children() {
        let (p, s) =
            scratch_plan("## Phase 1 - Phase\n  - [ ] 1.1 Parent\n    - [ ] 1.1.1 Child\n");
        rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 31, "method": "tools/call", "params": {"name": "plan_rename", "arguments": {"plan_path": "1.1", "new_subject": "Renamed parent"}}}),
        );
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(
            after.contains("- [ ] 1.1 - Renamed parent"),
            "got:\n{after}"
        );
        assert!(
            after.contains("- [ ] 1.1.1 - Child"),
            "child preserved:\n{after}"
        );
    }

    #[test]
    fn plan_rename_identical_title_is_no_op() {
        let (p, s) = scratch_plan("## Phase 1 - Same\n");
        let before = std::fs::read_to_string(&p).unwrap();
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 32, "method": "tools/call", "params": {"name": "plan_rename", "arguments": {"plan_path": "1", "new_subject": "Same"}}}),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("already titled"), "got: {text}");
        let after = std::fs::read_to_string(&p).unwrap();
        assert_eq!(before, after, "identical title: no write");
    }

    #[test]
    fn plan_rename_unknown_path_errors() {
        let (_, s) = scratch_plan("## Phase 1 - Phase\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 33, "method": "tools/call", "params": {"name": "plan_rename", "arguments": {"plan_path": "9.9", "new_subject": "doesn't matter"}}}),
        );
        assert_eq!(resp["error"]["code"], -32603);
        assert!(resp["error"]["message"].as_str().unwrap().contains("9.9"));
    }

    #[test]
    fn plan_rename_refreshes_tracked_task_baseline() {
        // Set up: a tracked task at 1.1 with last_synced_title = "Old".
        // After plan_rename, the state's last_synced_title should be the new
        // title so reconcile is silent.
        let (p, s) = scratch_plan("## Phase 1 - Phase\n  - [ ] 1.1 Old\n");
        let state_path = crate::state::default_state_path_for(&p);
        let mut state = crate::state::State::default();
        state.record(
            "t-1",
            crate::state::Mapping {
                plan_path: "1.1".to_string(),
                last_synced_title: "Old".to_string(),
                last_synced_state: NodeState::Pending,
                last_synced_annotations: vec![],
                ..Default::default()
            },
        );
        state.save(&state_path).unwrap();

        rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 34, "method": "tools/call", "params": {"name": "plan_rename", "arguments": {"plan_path": "1.1", "new_subject": "Brand new"}}}),
        );

        let reloaded = crate::state::State::load(&state_path).unwrap();
        let m = reloaded.mappings.get("t-1").expect("mapping preserved");
        assert_eq!(m.last_synced_title, "Brand new", "baseline should refresh");
    }

    #[test]
    fn plan_archive_via_mcp() {
        let (p, s) =
            scratch_plan("## Phase 1 - Done\n- [x] 1.1 - Sub done\n## Phase 2 - Pending\n");
        let resp = rpc(
            &s,
            json!({"jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": {"name": "plan_archive", "arguments": {"date": "2026-05-16"}}}),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("archived"));
        // FORMATv2 header phase id is bare "1".
        assert!(text.contains("1"));
        let plan_md = std::fs::read_to_string(&p).unwrap();
        assert!(!plan_md.contains("## Phase 1 - Done"));
        assert!(plan_md.contains("## Phase 2 - Pending"));
    }
}
