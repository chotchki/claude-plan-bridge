# PLAN: plan-to-task bridge

Spec: see [SPEC.md](./SPEC.md). This plan sequences the implementation.

Phase exit rule (per global CLAUDE.md workflow): every box ticked, unit + e2e tests pass, docs updated. Then summarize and sweep to PLAN_ARCHIVE.md.

## Backlog (not yet phased)

- ~~Refactor checkbox-line grammar to **winnow**~~ — done 2026-05-16. `id_and_title` / `bold_id` / `bare_id` / `id_chars` / `skip_separator` are winnow combinators now; the outer `- [STATE] ` matcher, the code-fence state machine, and the indent-stack tree assembly stay hand-rolled.
- ~~**Won't-do checkbox state (`[-]`)**~~ — done 2026-05-16. `Node.state: NodeState { Pending, Done, WontDo }` across the codebase. Parser accepts `[-]` and `[~]`; serializer emits `[-]`. Archive treats `WontDo` like `Done` (phase can exit). Reconcile emits `LeafStateChanged { old, new }` covering all transitions. Writeback's `TaskUpdate(deleted)` against a `[-]` leaf keeps the line and just drops the state mapping. MCP gained `plan_skip` (and `plan_phase_exit`). 128 tests passing.
- **Baseline subcommand for existing PLAN.md files** — when installing the bridge into a project that already has a populated PLAN.md, the first reconcile emits `LeafAdded` for every existing leaf (the state file is fresh). Loud and annoying on large plans. Mitigation: a `plan-bridge baseline` subcommand that seeds state with synthetic `baseline:<plan_path>` task ids for each current leaf, suppressing the spam. When Claude later TaskCreates against a baselined plan_path, writeback should replace the baseline mapping with the real one rather than duplicate.

---

- [x] 5.0 Hook integration + init
  - [x] 5.1 Hook JSON I/O wrappers
    - [x] 5.1.1 stdin reader for Claude Code hook payload shape — `HookPayload` deserializes (`session_id`, `cwd`, `hook_event_name`, `tool_name`, `tool_input`, `tool_response`) with `#[serde(default)]` tolerance on missing fields
    - [x] 5.1.2 stdout writer producing camelCase `{ "hookSpecificOutput": { "additionalContext": "..." } }`
    - [x] 5.1.3 Error path: CLI catches any `Result::Err` from writeback/reconcile and emits `decision: "block"` with the error chain in `reason` — never a stderr stack trace
    - [x] 5.1.4 Unit tests for I/O shapes (7 hook-module tests)
  - [x] 5.2 `plan-bridge init`
    - [x] 5.2.1 Scaffold empty PLAN.md (with starter `1.0 Phase one`) in CWD if missing
    - [x] 5.2.2 Merge required hooks into `.claude/settings.json` (UserPromptSubmit, PostToolUse on TaskCreate, PostToolUse on TaskUpdate). Edit/Write hook for PLAN.md changes covered by UserPromptSubmit reconcile — separate hook unnecessary
    - [x] 5.2.3 `--force` to overwrite PLAN.md template; settings.json merge is always idempotent (strips existing plan-bridge entries, replaces)
    - [x] 5.2.4 Append `.claude/plan-bridge-state.json` to `.gitignore` (creates if missing); idempotent
    - [x] 5.2.5 Unit tests (6 covering fresh init, idempotent re-init, user-hook preservation, skip-existing-plan, force overwrite, gitignore idempotency)
  - [x] 5.3 Real-project shakeout (deferred to user — bridge can't drive itself end-to-end from this session)
    - Procedure: `cargo install --path .` (or build + symlink to PATH); in a scratch project run `plan-bridge init`; restart Claude Code to pick up new settings.json; have Claude do non-trivial work; observe that TaskCreate calls update PLAN.md, that user-edits to PLAN.md surface in next-turn reconcile, that archive sweep runs cleanly.
  - [x] 5.4 Phase 5 exit
    - [x] 5.4.1 `cargo test` green (101 tests)
    - [x] 5.4.2 README: end-to-end install + usage instructions (`Install` + `Set up in a project` sections)
    - [x] 5.4.3 Real-project shakeout (2026-05-16): bridge driven live against this repo. Two bugs flushed out and fixed:
      - `extract_task_id` probed only top-level keys; Claude Code's `TaskCreate.tool_response` nests as `{"task": {"id": "2", ...}}`. Now probes nested `task.{id|taskId|task_id}` and accepts numeric ids defensively. Regression test pinned to captured payload.
      - `HookSpecificOutput` was missing `hookEventName`; Claude Code rejects the output with a schema-validation error when absent. Field added (camelCased), threaded through writeback (`payload.hook_event_name`) and reconcile (constant `"UserPromptSubmit"`).
- [x] 6.0 MCP server mode
  - Rationale (added 2026-05-16): once the canonical PLAN.md format is stricter than what humans naturally write, hand-edited markdown risks format violations. MCP tools let Claude mutate plans through a typed API that the binary owns, sidestepping the format-discipline problem.
  - [x] 6.1 `plan-bridge serve` subcommand — stdio JSON-RPC 2.0; hand-rolled (no `rmcp` dep). Implements `initialize`, `tools/list`, `tools/call`. Notifications are silently absorbed.
  - [x] 6.2 Tools shipped in v1: `plan_list`, `plan_check`, `plan_uncheck`, `plan_add`, `plan_archive`. Deferred to a later sweep: `plan_phase_exit` (composite operation), `plan_skip` (paired with the won't-do refactor in backlog).
  - [-] 6.3 Resource exposure (MCP `resources/*`) — deferred. v1 ships `plan_list` as a tool returning the AST text; that covers the read-PLAN.md use case. Add `resources/` when a client actually needs URI-keyed reads.
  - [x] 6.4 Unit tests (12 in `mcp` module: initialize, tools/list, each tool, error paths, malformed JSON, notification handling, archive-via-MCP).
  - [x] 6.5 Phase 6 exit — `cargo test` green (114 tests); README documents the MCP surface.
