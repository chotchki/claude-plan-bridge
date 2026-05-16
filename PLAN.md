# PLAN: plan-to-task bridge

Spec: see [SPEC.md](./SPEC.md). This plan sequences the implementation.

Phase exit rule (per global CLAUDE.md workflow): every box ticked, unit + e2e tests pass, docs updated. Then summarize and sweep to PLAN_ARCHIVE.md.

## Backlog (not yet phased)

- Refactor checkbox-line grammar to **winnow** for declarative parsing + nicer error messages. Indent-tree stays hand-rolled regardless. Considered 2026-05-16, deferred — the hand-rolled parser passes 101 tests on real-world input; no concrete pain point yet. Revisit when grammar grows (e.g. inline metadata syntax) or when parser error messages become a user-facing problem.
- **Won't-do checkbox state (`[-]`)** — accepted 2026-05-16, to be folded into a post-5.3 fix-up sweep (likely Phase 7). Needs:
  - `Node.checked: bool` → `Node.state: NodeState { Pending, Done, WontDo }` refactor across ast / parser / serializer / archive / reconcile / writeback.
  - Archive treats `WontDo` like `Done` (phase can exit).
  - Reconcile gets a `LeafSkipped` (or similar) delta variant.
  - Writeback: `TaskUpdate(status="deleted")` against a `[-]` leaf must keep the line and just drop the state mapping — currently it removes the line.
  - MCP angle: a `plan_skip` tool in Phase 6 is the cleaner driver (TaskUpdate's status enum has no won't-do equivalent). Land the AST/parser piece first so MCP has somewhere to write to.

---

- [ ] 5.0 Hook integration + init
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
  - [ ] 5.4 Phase 5 exit
    - [x] 5.4.1 `cargo test` green (101 tests)
    - [x] 5.4.2 README: end-to-end install + usage instructions (`Install` + `Set up in a project` sections)
    - [ ] 5.4.3 e2e: gated on 5.3 — replaced by real-project shakeout findings. Until that runs, Phase 5 stays open.
- [ ] 6.0 MCP server mode (DEFERRED — only start after Phase 5 ships and concrete need surfaces)
  - Rationale (added 2026-05-16): once the canonical PLAN.md format is stricter than what humans naturally write, hand-edited markdown risks format violations. MCP tools let Claude mutate plans through a typed API that the binary owns, sidestepping the format-discipline problem. This makes Phase 6 more likely than originally scoped — not less.
  - [ ] 6.1 `plan-bridge serve` subcommand (stdio-based MCP)
  - [ ] 6.2 Tools: `plan_add`, `plan_check`, `plan_uncheck`, `plan_archive`, `plan_list`, `plan_phase_exit`
  - [ ] 6.3 Resource exposure: PLAN.md state as a readable resource
  - [ ] 6.4 Unit + e2e tests
  - [ ] 6.5 Phase 6 exit (docs + e2e)
