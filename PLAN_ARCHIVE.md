## 2026-05-16

- [x] 1.0 Foundation: Rust project + PLAN.md parser/serializer
  - [x] 1.1 Scaffold Cargo project
    - [x] 1.1.1 `cargo init --bin plan-bridge` (2024 edition)
    - [x] 1.1.2 Add dependencies: `clap` (derive), `serde`, `serde_json`, `anyhow`, `thiserror`
    - [x] 1.1.3 `.gitignore` for `target/`, `.claude/plan-bridge-state.json`
    - [x] 1.1.4 `cargo build` and `cargo test` green on empty project
  - [x] 1.2 AST + parser
    - [x] 1.2.1 Define types: `Plan`, `Node` (one type for phase/task/leaf, depth implied by position), `Annotation`
    - [x] 1.2.2 Line tokenizer: classify each line as phase/task/leaf checkbox vs annotation vs blank
    - [x] 1.2.3 Tree assembly via indent stack; mixed indent tolerated per-section
    - [x] 1.2.4 Tolerate `1` vs `1.0` numbering
    - [x] 1.2.5 Attach trailing annotations (notes, fenced code blocks, sub-bullets without checkboxes) to their parent node
    - [x] 1.2.6 Unit tests inline + fixture-based (16 passing, covering empty, nested, mixed indent, all annotation types, error paths)
    - [x] 1.2.7 Tolerate alphanumeric IDs (`X.4.a.1`, `Y.2.gate` style) — alphanumeric components separated by dots
    - [x] 1.2.8 Tolerate bold-wrapped IDs (`**X.4.a.1**`) — stripped on parse, never emitted on write
    - [x] 1.2.9 Tolerate em-dash / hyphen separator between ID and title (`— `, `- `, plain space)
    - [x] 1.2.10 Tolerate bare checkbox without an ID (`- [ ] title only`); recorded as `id: ""`
    - [x] 1.2.11 Tolerate markdown section headers (`## Header`) appearing within the tree — attached as text annotation on the most recent open node (structural fidelity not promised for non-canonical input)
    - [x] 1.2.12 Smoke test against `../quicksight/PLAN.md` — parses without error when present; skipped if absent
  - [x] 1.3 Serializer
    - [x] 1.3.1 Render `Plan` → markdown with normalized 2-space indent
    - [x] 1.3.2 Preserve annotation text verbatim (including fenced blocks)
    - [x] 1.3.3 Roundtrip property: `parse(serialize(parse(input))) == parse(input)` over fixtures
  - [x] 1.4 `plan-bridge parse` subcommand
    - [x] 1.4.1 Clap wiring with optional `--plan <PATH>` (default `./PLAN.md`)
    - [x] 1.4.2 Stable JSON output schema (documented in README)
    - [x] 1.4.3 Unit tests verifying JSON shape on fixtures (2 ast tests: serde roundtrip + camelCase tag check)
  - [x] 1.5 Phase 1 exit
    - [x] 1.5.1 `cargo test` green (35 tests passing)
    - [x] 1.5.2 README documents parser/serializer behavior, JSON schema, and tolerated input variants
    - [x] 1.5.3 e2e: smoke tests parse this repo's PLAN.md and `../quicksight/PLAN.md` without error

- [x] 2.0 Writeback + per-project state
  - [x] 2.1 State file
    - [x] 2.1.1 Define `.claude/plan-bridge-state.json` schema (`taskId ↔ plan_path` map via `Mapping` struct, schema version)
    - [x] 2.1.2 Atomic read/write helpers (tmp + rename)
    - [x] 2.1.3 Unit tests for state I/O (7 tests covering load-missing, roundtrip, parent-dir creation, atomicity, future-version refusal, lookup, default-path)
  - [x] 2.2 `writeback --event create`
    - [x] 2.2.1 Parse hook stdin JSON (`tool_input`, `tool_response`) — typed `HookPayload`, `TaskCreateInput`, `TaskMetadata` in `hook.rs`
    - [x] 2.2.2 If `metadata.plan_path` present, locate insertion point via `parent_id_for`; else append under `Inbox.0` (auto-created if missing)
    - [x] 2.2.3 Append `- [ ] N.M.K subject` at correct indent (AST mutation + serialize round-trip)
    - [x] 2.2.4 Record `taskId → plan_path` in state file (atomic save)
    - [x] 2.2.5 Idempotency: re-running same `task_id` is a no-op; pre-existing `plan_path` doesn't double-insert
    - [x] 2.2.6 Unit tests (7 covering parent-existing, task-under-phase, state recording, idempotency, inbox fallback, missing-parent error, missing-task-id error)
  - [x] 2.3 `writeback --event update`
    - [x] 2.3.1 Look up `plan_path` from `taskId` via state file
    - [x] 2.3.2 `status: completed` → `[ ]` becomes `[x]`
    - [x] 2.3.3 `status: deleted` → remove the line. Orphaned empty parents are NOT cascade-removed in v1 — too risky, user prunes manually
    - [x] 2.3.4 `status: in_progress` and `status: pending` → no PLAN.md change (in-flight state stays in TaskCreate only)
    - [x] 2.3.5 Idempotency: completed-twice is a no-op; deleted-on-already-deleted clears the lingering state mapping
    - [x] 2.3.6 Unit + e2e tests (5 unit tests; manual e2e: create → complete → delete on a scratch PLAN.md works as designed)
  - [x] 2.4 Phase 2 exit
    - [x] 2.4.1 `cargo test` green (72 tests)
    - [x] 2.4.2 README documents writeback contract + state file
    - [x] 2.4.3 e2e: manually piped `TaskCreate` + `TaskUpdate(completed)` + `TaskUpdate(deleted)` payloads through `plan-bridge writeback`; PLAN.md and state evolve as expected

- [x] 3.0 Reconcile (PLAN.md ↔ recorded state drift detection)
  - [x] 3.1 Delta types
    - [x] 3.1.1 Define delta variants: `LeafAdded`, `LeafRemoved`, `LeafChecked`, `LeafUnchecked`, `LeafTitleChanged`, `LeafAnnotationChanged`
    - [x] 3.1.2 Serde JSON representation (tagged enum with `kind` discriminator)
  - [x] 3.2 Diff engine
    - [x] 3.2.1 Build `Map<plan_path, Leaf>` from parsed PLAN.md
    - [x] 3.2.2 Build reverse map `plan_path → task_id` from state file (the `last_synced_*` fields on `Mapping` are the baseline — TaskList JSON not needed in v1)
    - [x] 3.2.3 Emit deltas: structural diffs (added/removed), box-state flips, title edits, annotation changes
    - [x] 3.2.4 Compact human-readable rendering of deltas for `additionalContext` (`render_deltas`)
  - [x] 3.3 `plan-bridge reconcile` subcommand
    - [x] 3.3.1 No `--task-list` flag in v1 — state-driven diff is sufficient. Backlog: add when TaskList introspection becomes needed
    - [x] 3.3.2 Emit hook-shaped JSON: `hookSpecificOutput.additionalContext` containing the rendered delta; `{}` silent when no drift
    - [x] 3.3.3 Unit tests on synthetic plan/state pairs (11 covering empty, no-drift, each delta type, multiple-deltas compound, render)
  - [x] 3.4 Phase 3 exit
    - [x] 3.4.1 `cargo test` green (83 tests)
    - [x] 3.4.2 README documents delta schema and the `additionalContext` envelope
    - [x] 3.4.3 e2e: PLAN.md user-edit scenario (tick + rename + add note) drives reconcile to emit Title / Checked / Annotation deltas as expected

- [x] 4.0 Archive sweep
  - [x] 4.1 Archive logic
    - [x] 4.1.1 Identify phases whose entire subtree is `[x]` (via `phase_fully_done`, parent's own checkbox state irrelevant — children determine)
    - [x] 4.1.2 Move them to `PLAN_ARCHIVE.md` under a `## YYYY-MM-DD` header; newest section prepended
    - [x] 4.1.3 Preserve annotations and structure on move (serialize the moved subtree intact)
    - [x] 4.1.4 Renumber remaining phases — decision: NO (per `plan-id-stability` memory)
  - [x] 4.2 `plan-bridge archive` subcommand
    - [x] 4.2.1 Clap wiring; `--dry-run` flag; `--date` override for testability
    - [x] 4.2.2 Atomic write of both files (PLAN.md + PLAN_ARCHIVE.md) via tmp + rename
    - [x] 4.2.3 No-op when no fully-complete phase exists
    - [x] 4.2.4 Unit tests (8 covering no-op, archive-completed, parent-unchecked-children-done, prepend-existing, dry-run, drop-state-mappings, empty-unchecked, empty-checked)
  - [x] 4.3 Phase 4 exit
    - [x] 4.3.1 `cargo test` green (91 tests)
    - [x] 4.3.2 README documents archive behavior + the renumbering decision
    - [x] 4.3.3 e2e: dogfooded archive on this repo's PLAN.md — Phases 1-4 swept to `PLAN_ARCHIVE.md`

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

---

## 2026-05-17

- [x] 7.0 Archive ordering: append newest sections at bottom (chronological-ascending)
  - [x] 7.1 Flip `src/archive.rs` to append the new dated section at the bottom of PLAN_ARCHIVE.md instead of prepending
  - [x] 7.2 Update archive unit tests for new ordering + add a regression test that an existing PLAN_ARCHIVE.md is appended-to (existing content stays at top, new section at bottom)
  - [x] 7.3 README: change "newest section prepended at the top" to "newest section appended at the bottom" in the `plan-bridge archive` section
  - [x] 7.4 One-time fixup of this repo's PLAN_ARCHIVE.md: move today's Phase 5/6 section below today's Phase 1–4 section so the file reads chronological-ascending
  - [x] 7.5 Phase 7 exit — cargo test green; README + PLAN_ARCHIVE.md consistent with new ordering
  - [x] 7.6 Reconcile renderer: don't double-prefix annotation bullets when the source line already starts with `- `
  - [x] 7.7 Bridge: id-positional insertion so `7.5a` lands between `7.5` and `7.6` instead of always appending

- [x] 9.0 Reconcile: stop emitting LeafRemoved when a tracked node becomes a parent (children added)

---

## 2026-05-17

- [x] 8.0 Serialize concurrent writebacks with a file lock — and surface lock failure as a loud hook block, never silent data loss

- [x] 10.0 Productionalize the tool — public repo, CI, packaging, README polish
  - [x] 10.1 Public GitHub repo + LICENSE; update Cargo.toml `repository` and `license` fields
    - We'll go with an MIT license for this
  - [x] 10.2 CI builds — GitHub Actions workflow: cargo fmt --check, clippy -D warnings, test, on Linux/macOS/Windows stable
  - [x] 10.3 README polish — lead with a 30-second pitch and an example transcript; trim implementation detail or fold it lower
  - [x] 10.4 Publish v0.1.0 to crates.io — fill out Cargo.toml metadata; cargo publish --dry-run; cargo publish
  - [x] 10.4a Draft a hotchkiss.io entry on plan-bridge — motivation, design, install/usage
  - [x] 10.4b Internal-prefix rename: `plan-bridge:` → `claude-plan-bridge:` in source message strings (~10 files)
  - [x] 10.5 Phase 10 exit — all sub-boxes ticked; GH repo public; CI green on main; v0.1.0 on crates.io; README badges live

---

## 2026-05-17

- [x] 11.0 Roll out the release: global CLAUDE.md guidance + dogfood crates.io install
  - [x] 11.1 Install claude-plan-bridge v0.1.0 from crates.io to replace the local `cargo install --path .` build (dogfood the published version)

- [x] 12.0 writeback: support TaskUpdate(subject=...) — rewrite PLAN.md title + last_synced_title without requiring a status change
  - [x] 12.1 writeback_update: rewrite title from `input.subject` (impl + tests, independent of status)
  - [x] 12.2 README: document TaskUpdate(subject) writeback path in the writeback section
  - [x] 12.3 Phase 12 exit — cargo test green; local install picked up; README reflects new behavior

---

## 2026-05-17

- [x] 13.0 MCP plan_rename tool: typed-API parity with TaskUpdate(subject=...)
  - [x] 13.1 MCP plan_rename tool: impl + unit tests in src/mcp.rs
  - [x] 13.2 README: add `plan_rename` row to the MCP tools table
  - [x] 13.3 Phase 13 exit — cargo test green; README MCP table updated

- [x] 14.0 Release workflow: also create a GitHub Release after `cargo publish` succeeds

- [x] 15.0 Bug fixes from ocr_pdf_latex shakeout: stop mangling non-canonical PLAN.md
  - [x] 15.1 writeback pre-flight: detect markdown headers attached as annotations and refuse rather than silently demote
  - [x] 15.2 writeback: clearer parent-not-found error suggests canonical phase-checkbox format
  - [x] 15.3 README: prominent warning that `### Phase N` section headers don't work — canonical format only
  - [x] 15.3a Bump Cargo.toml to 0.1.2; tag v0.1.2; push tag (release workflow ships to crates.io + creates GH release)
  - [x] 15.3b Install released v0.1.2 from crates.io to replace local dev build (dogfood the published version)
  - [x] 15.4 Phase 15 exit — cargo test green; fixture test passes against captured ocr_pdf_latex PLAN.md format; README warns clearly

---

## 2026-05-17

- [x] 16.0 Bug fixes from quicksight shakeout: UTF-8 panic + joined-bold-id + heading-as-parent
  - [x] 16.1 reconcile: fix UTF-8 byte-boundary panic in render_deltas annotation preview truncation
  - [x] 16.2 parser: tolerate joined-bold id+title format `**ID — Title.** rest`
  - [x] 16.3 standardize: generalize `### Phase N — Title` to `### <id> — Title` so headings like `### AA.A — ...` promote to phase parents
  - [x] 16.4 Bump + tag v0.1.3 (panic fix); cargo install --force; archive Phase 16

---

## 2026-05-17

- [x] 17.0 CLI consistency: every project-scoped subcommand accepts `--cwd` (init's convention)
  - [x] 17.1 Refactor: shared ProjectArgs (--cwd + --plan) across parse/writeback/reconcile/archive/serve/baseline
  - [x] 17.2 README: document the `--cwd` / `--plan` convention in CLI reference
  - [x] 17.3 Phase 17 exit + bump v0.1.5 + tag + install

- [x] 18.0 Empty-id leaves: stop colliding under `baseline:` key, stop emitting false drift on every reconcile
  - [x] 18.1 baseline: skip empty-id leaves entirely (no state entry, no synthetic key)
  - [x] 18.2 reconcile: skip empty-id leaves in the diff walk (no LeafAdded / LeafTitleChanged etc.)
  - [x] 18.3 README: document that empty-id leaves are untracked; explain `parse` phases vs `baseline` leaves counts

---

## 2026-05-17

- [x] 19.0 Quicksight v0.1.5 shakeout: narrative headers shouldn't block writeback
  - [x] 19.1 serializer: preserve original indent for markdown-header text annotations (no demotion)
  - [x] 19.2 parse_phase_header: depth-limit promotion to `##`/`###` only; `####+` always None (narrative)
  - [x] 19.3 standardize_to_canonical: drop refusal — non-matching headers stay as annotations (narrative)
  - [x] 19.4 README: document the new header policy (promote `##`/`###` Phase-N shape, narrative otherwise)
  - [x] 19.5 Phase 19 exit + bump v0.1.6 + tag + install

---

## 2026-05-17

- [x] 20.0 Quicksight v0.1.6 shakeout: multi-header subtree no longer refuses writeback
  - [x] 20.1 standardize: skip promotion when subtree has multiple Phase-N headers (leave as narrative)
  - [x] 20.2 Phase 20 exit + bump v0.1.7 + tag + install

---

## 2026-05-17

- [x] 21.0 Quicksight v0.1.7 readability: preserve `---`, blank lines between phases, quieter standardize note
  - [x] 21.1 serializer: preserve original indent for ALL Text annotations, not just markdown headers
  - [x] 21.2 serializer: emit a blank line between top-level phases (match archive style)
  - [x] 21.3 writeback: summarize the standardize note instead of dumping every promotion
  - [x] 21.4 Phase 21 exit + bump v0.1.8 + tag + install

---

## 2026-05-17

- [x] 22.0 Bootstrap UX from third-project shakeout: loud init, status command, oriented scaffold
  - [x] 22.1 init: print unmissable final warning about session reload + recovery path
  - [x] 22.2 `claude-plan-bridge status` subcommand for diagnostics
  - [x] 22.3 init scaffold PLAN.md: add orientation comment header for human + agent readers
  - [x] 22.4 Phase 22 exit + bump v0.1.9 + tag + install

---

## 2026-05-17

- [x] 23.0 Backflow gap: reconcile/baseline don't surface tracked-but-no-harness-task leaves so the agent can adopt them
  - [x] 23.1 reconcile: emit advisory when state has baseline-only mappings (leaves tracked but not in TaskList)
  - [x] 23.2 README: document TaskCreate-against-existing-line idempotency + the baseline-adopt workflow
  - [x] 23.3 Phase 23 exit + bump v0.1.10 + tag + install

---

## 2026-05-17

- [x] 26.0 Phase 26 — SessionStart rehydration polish
  - [x] 26.1 Preload TaskCreate schema (or hint ToolSearch) in rehydration prompt
  - [x] 26.2 Drift detector: skip or special-case [x] completed lines
  - [x] 26.3 Rehydration prompt: tell model description can mirror subject
  - [x] 26.4 Rehydration prompt: explicit parallel-batch hint
  - [x] 26.5 Drift detector: skip plan_paths just rehydrated by SessionStart
  - [x] 26.6 TaskCreate idempotency: skip duplicate plan_path within session
  - [x] 26.7 Rehydration confirmation: emit N/N complete on final TaskCreate
  - [x] 26.8 Audit log for cleared state mappings
  - [x] 26.9 Phase 26 exit — tests, version bump, archive

---

## 2026-05-18

- [x] 25.0 Phase 25 — Session-restart rehydration via SessionStart hook
  - [x] 25.1 Add `plan-bridge resume` subcommand
  - [x] 25.2 Wire SessionStart hook into installer + add `upgrade-hooks` subcommand
  - [x] 25.3 Writeback: dedup mappings by plan_path on TaskCreate
  - [x] 25.4 Reconcile/writeback warn loudly when SessionStart hook is missing
  - [x] 25.5 e2e test: full restart cycle
  - [x] 25.6 README: document session-restart behavior
  - [x] 25.6a Drop stale pending mappings on resume to prevent harness-ID collisions
  - [x] 25.6b Tighten resume prompt: imperative + before-responding framing
  - [x] 25.6c Broaden resume clear: drop all mappings on startup/clear, not just pending
  - [x] 25.6d TaskUpdate(deleted) becomes unlink-only — never mutate PLAN.md
  - [x] 25.7 Phase 25 exit — tests, version bump, archive

- [x] 27.0 Phase 27 — Rehydration prompt polish + leaves-only parent filter
  - [x] 27.1 Filter non-leaf nodes from build_resume_message (leaves-only rehydration)
  - [x] 27.1a Group leaves under parent phase header in rehydration prompt
  - [x] 27.2 Bullet format: PLAN.md-style `id title` + explicit plan_path-vs-subject instruction
  - [x] 27.3 Source-aware ToolSearch framing: assertive on startup/clear, light on resume/compact
  - [x] 27.4 Suggest minimal description (e.g., plan_path) in rehydration prompt
  - [x] 27.5 README: document leaves-only rehydration + parent-tick-is-validation
  - [x] 27.6 Phase 27 exit — tests, version bump, archive

---

## 2026-05-18

- [x] 28.0 Phase 28 — Backlog state marker (`[>]` on disk, 🔜 in human-facing output)
  - [x] 28.1 Restart test: validate Phase 27 leaves-only + parent-headers + source-aware prompt
  - [x] 28.2 SPEC.md: define Backlog state semantics + on-disk vs output markers + metadata-driven TaskUpdate flow
  - [x] 28.3 AST: add NodeState::Backlog (mirrors WontDo pattern)
  - [x] 28.4 Parser + serializer: accept and emit `[>]` for Backlog
  - [x] 28.5 Archive: treat Backlog as resolved in phase_fully_done
  - [x] 28.6 Reconcile: surface Pending↔Backlog transitions as LeafStateChanged
  - [x] 28.7 Writeback: TaskUpdate(deleted) on a Pending leaf flips PLAN.md to `[>]` and drops state mapping (no opt-in needed)
  - [x] 28.7a Writeback + CLI/MCP: appending a backlog entry also adds a bullet under `## Backlog (not yet phased)` with source plan_path + date
  - [x] 28.8 MCP `plan_backlog` tool + CLI `backlog <plan_path>` subcommand
  - [x] 28.9 Translate `[>]` → 🔜 in human-facing output (status, plan_list, reconcile drift, hook prompts)
  - [x] 28.9a Translate other checkbox states to emoji in human-facing output (`[x]` → ✅, `[-]` → ❌)
  - [x] 28.10 README + SPEC.md: document the marker, when to use vs `[-]`, and the auto-flip + Backlog-section-promotion behavior
  - [x] 28.11 Phase 28 exit — tests, version bump, archive

---

## 2026-05-18

- [x] 24.0 Phase 24 — Restart-test README note + parent-inference fix
  - [x] 24.1 Add failing test for bare-N phase root
  - [x] 24.2 Fix parent-inference to accept bare N
  - [x] 24.3 Note restart-test cycle in README
  - [x] 24.4 Phase 24 exit — tests, version bump, archive

---

## 2026-05-18

- [x] 29.0 Phase 29 — Format-preserving writeback (don't mow down user PLAN.md formatting on first contact)
  - [x] 29.1 Audit `standardize_to_canonical` call sites: confirm reconcile/archive don't rely on it as a side effect of writeback (research, not code). Identifies any callers that must keep canonicalization to function correctly.
  - [x] 29.2 Make `standardize_to_canonical` opt-in: writeback_create / writeback_update / MCP plan_* tools stop calling it implicitly. Add a `plan-bridge canonicalize` subcommand that runs it explicitly. The canonical form is still the only output shape — we just don't reach for it on routine writes.
  - [x] 29.3 Preserve `Annotation::Bullet` original indent in serializer (mirror `Annotation::Text` behavior). Fixes the `- **Phase N**` → `  - **Phase N**` regression observed on the dry-run artifact.
  - [x] 29.4 Preserve bold-wrapped IDs round-trip: AST gains `id_style: IdStyle { Plain, Bold }`, parser records, serializer respects. Canonical output is still Plain — but routine writebacks preserve user choice.
  - [x] 29.5 Preserve em-dash separator round-trip: AST gains `separator: Separator { Space, EmDash, Hyphen }`, parser records, serializer respects. Canonical output is still Space.
  - [x] 29.6 Preserve blank lines within phase trees: parser captures consecutive blank lines as `Annotation::Blank { count }`, serializer re-emits. Drops the "trees collapse vertically" surprise.
  - [x] 29.7 Fix bare-id leaf serializer double-space bug: `- [ ]  Make` should serialize as `- [ ] Make` when `node.id` is empty. Conditional formatting: omit the id field + its trailing space when empty.
  - [x] 29.8 Add `plan-bridge writeback --dry-run` flag: parse + apply mutation + serialize + diff, emit unified diff to stdout, don't write. Lets adopters preview the bridge's effect on their PLAN.md before committing. Falls out cheap once 29.2-29.7 stop the mass-mutation.
  - [x] 29.9 Phase 29 exit — tests, version bump, archive

---

## 2026-05-18

- [x] 30.0 Phase 30 — Green CI + coverage badge
  - [x] 30.1 Fix `cargo fmt --check` drift: `maybe_warn_missing_session_start`'s `Some(msg)` arm in src/main.rs is currently multi-line where rustfmt wants single-line. Reformatted locally + verified `cargo fmt --check` passes. Root cause: my Phase 29 edits broke fmt without me running `cargo fmt` before committing.
  - [x] 30.2 Diagnose + fix Windows CI flake: `Swatinem/rust-cache@v2` died mid-restore on `windows-latest` with no error output (half a second between "Restoring cache..." and "Post job cleanup"). Try: bump to `Swatinem/rust-cache@v3` (or pinned latest SHA); fall back to `cache: false` on Windows if upstream issue. Confirm with a green run before tagging anything.
  - [x] 30.3 Bump workflow action versions to silence Node.js 20 deprecation warnings: `actions/checkout@v4` → @v5 (when released; verify), `dtolnay/rust-toolchain@stable` and `Swatinem/rust-cache@v2` to current. Validate that the bumps don't reintroduce 30.2's flake.
  - [x] 30.4 Add `cargo-llvm-cov` coverage step on Linux: install via `cargo install cargo-llvm-cov --locked` (or use `taiki-e/install-action`), run `cargo llvm-cov --all-features --workspace --lcov --output-path lcov.info` and `--json --summary-only > coverage.json` in CI on push:main + PR. Report goes to GHA Step Summary as a markdown table.
  - [x] 30.5 Generate a shields.io-compatible coverage badge JSON from llvm-cov summary: `{"schemaVersion": 1, "label": "coverage", "message": "82.4%", "color": "brightgreen|yellow|orange|red"}`. Color thresholds: ≥80 green, ≥60 yellow, ≥40 orange, <40 red.
  - [x] 30.6 Publish the badge JSON + HTML coverage report to an orphan `badges` branch on push:main only (skip on PRs to avoid spam). Force-push via `peaceiris/actions-gh-pages` or a hand-rolled `git checkout --orphan badges` + commit + force-push step. Branch contains: `coverage.json` (badge endpoint), `coverage-html/` (browsable report), no source.
  - [x] 30.7 Add the coverage badge to README. Source URL: `https://img.shields.io/endpoint?url=https://raw.githubusercontent.com/chotchki/claude-plan-bridge/badges/coverage.json`. Links to the rendered HTML report on the same branch.
  - [x] 30.8 Upload `lcov.info` + `coverage-html.tar.gz` as a CI artifact on every CI run (PR + push) so reviewers can download even without the badges-branch publication. Retention: 14 days.
  - [x] 30.9 README: document the coverage workflow + how to run `cargo llvm-cov` locally in the "Contributing" section. Note color thresholds + how to regen the badge after a coverage drop.
  - [x] 30.10 Phase 30 exit — tests, version bump, archive

---

## 2026-05-18

- [ ] 31.0 Phase 31 anchor
  - [x] 31.1 Strip stray `\"`/`\\` escape sequences from incoming TaskCreate/TaskUpdate subjects so PLAN.md never gets `Build \"/blog\" page` and reconcile doesn't drift forever when the user hand-cleans the file
  - [x] 31.2 Auto-create the `N.0` phase anchor when the first TaskCreate(plan_path=`N.X`) arrives and no parent exists — prefer `metadata.plan_phase` for the anchor's title, else synthesize `Phase N`
  - [x] 31.3 Refuse to silently parent a `N.X` TaskCreate under a misplaced `N.0` (e.g. one indented under another phase). The bridge should treat `N.0` as a top-level phase root; an `N.0` found at non-top-level should be either relocated or refused with a clear error
  - [x] 31.4 Exclude column-0 markdown section headers (`## ...`, `### ...`) from the leaf annotation diff so a `## Phase 10 — Title` divider between leaves doesn't surface as `LeafAnnotationChanged` on the previous leaf every turn
  - [x] 31.5 Suppress `LeafAdded` drift for top-level `N.0` phase anchors. A manually-added phase root isn't a tracked task; nagging the agent to `TaskCreate` it forever creates persistent noise
  - [x] 31.6 Update SPEC.md + README.md for the new ergonomics (auto-anchor, header filter, escape normalization)
  - [x] 31.7 Release v0.1.19 — bump Cargo.toml, sweep Phase 31 to PLAN_ARCHIVE.md
---


Adopter session imploded with `read ./PLAN.md: No such file or directory` blocking every prompt (including `ls`). Root cause: Claude `cd`'d into a subdirectory mid-session, the hook subprocess inherited that cwd, and `./PLAN.md` (relative because `init` writes `--cwd` default `.`) didn't resolve. The bridge then converted the I/O error into `decision: "block"`, walling off the user. Two-layer fix: (a) hooks bake the absolute project root so subprocess cwd is irrelevant; (b) missing PLAN.md is **never** a blocking error — at most a non-blocking warning.


- [ ] 32.0 Hook cwd robustness (post-session-implode)
  - [x] 32.0 Phase 32 — Hook cwd robustness (post-session-implode)
    - [x] 32.1 Defensive: missing PLAN.md must never produce `decision: block` — reconcile, writeback, and resume gracefully degrade (silent no-op or non-blocking warning) so a misrouted hook can't wall off every prompt
    - [x] 32.2 Bake absolute `--cwd /abs/path` into `init`-installed hook commands so the subprocess cwd (which can drift if Claude `cd`s mid-session) doesn't determine where the bridge looks for PLAN.md
    - [x] 32.3 `upgrade-hooks` rewrites legacy relative-cwd hook entries to absolute `--cwd` form so existing installs (including this repo's `.claude/settings.json`) self-heal on next upgrade
    - [x] 32.4 Tests: missing-PLAN.md path is silent/non-blocking for reconcile, writeback(create/update), and resume; `init` writes absolute --cwd; `upgrade-hooks` migrates relative→absolute and is idempotent
    - [x] 32.5 Update SPEC.md and README.md to document the new hook-install behavior (absolute --cwd) and the missing-PLAN.md non-blocking contract
    - [x] 32.6 Release v0.1.20 — bump Cargo.toml, `cargo install --path .`, run `upgrade-hooks` on this project, sweep Phase 32 to PLAN_ARCHIVE.md
    - [x] 32.7 Auto-detect outdated hook entries (relative `--cwd`) and prepend a one-time `additionalContext` warning telling the user to run `upgrade-hooks` — same pattern as the existing missing-SessionStart-hook nag

---

## 2026-05-19

- [x] 33.0 Source-aware resume prompt
  - [x] 33.1 Branch resume prompt body by `source`: startup/clear keeps imperative TaskCreate flow; resume/compact pivots to imperative "TaskList first, only TaskCreate missing" framing
  - [x] 33.2 Inline expected `task_id=<id>` next to each plan_path bullet on resume/compact so the agent can dedup against TaskList by ID rather than subject text (and absent on startup/clear, where the IDs are stale)
  - [x] 33.3 Update `resume_and_compact_sources_preserve_pending_mappings` (+ siblings) to assert new branched wording — must not contain the old "task list is empty" claim on resume/compact
  - [x] 33.4 Add tests: inline `task_id=<id>` appears on resume/compact bullets; absent on startup/clear (stale ids would mislead a fresh harness post-wipe)
  - [x] 33.5 Rewrite the closing footer note (`src/resume.rs:170-182`) so the resume/compact branch addresses harness-collision risk, not PLAN.md-insertion (which is the startup/clear concern)
  - [x] 33.5a Fix pre-existing clippy + fmt drift blocking v0.1.21 release (Rust toolchain bump on main left 6 clippy errors + fmt drift in init.rs/main.rs that I didn't introduce but that 33.6 can't release through)
  - [x] 33.6 Release v0.1.21: bump Cargo.toml, fmt/clippy/test all green, sweep Phase 33 to PLAN_ARCHIVE.md

