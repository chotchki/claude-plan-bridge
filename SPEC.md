# SPEC: plan-to-task bridge

## Problem

Two task-tracking systems collide inside a single Claude Code session:

- **PLAN.md** (user convention, encoded in global CLAUDE.md): hierarchical `phase.task.subtask` checkboxes in a markdown file. Durable, git-tracked, swept to `PLAN_ARCHIVE.md` on phase exit. Required for any non-trivial work.
- **TaskCreate** (Claude Code built-in): flat task list with `blocks`/`blockedBy` deps, session-scoped, surfaced in the harness UI. The system prompt instructs Claude to use it proactively.

Today, Claude either suppresses TaskCreate (fighting the system prompt) or duplicates work in both (fragile and noisy). Neither is acceptable.

## Goal

Make PLAN.md the single source of truth. TaskCreate becomes a mirror, populated and kept consistent by hooks. Neither system needs to be suppressed; both reflect the same state.

## Non-goals (v1)

- Multi-user simultaneous editing of PLAN.md.
- Sync with external trackers (Linear, GitHub Issues, etc.).
- Live file-watching (mid-session external edits to PLAN.md don't auto-reflect; they reconcile at next user-prompt boundary).
- GUI / TUI for plan management.

## Source-of-truth model

- PLAN.md wins on conflict. Always.
- TaskCreate state is derived; it can be regenerated from PLAN.md at any time.
- External edits to PLAN.md (made outside Claude) are reconciled at the next `UserPromptSubmit` hook fire.

## PLAN.md schema (conventions)

```
- [ ] 1.0 Phase title
  - [ ] 1.1 Task title
    - [ ] 1.1.1 Subtask title
    - [x] 1.1.2 Completed subtask
  - [ ] 1.2 Another task
- [ ] 2.0 Next phase
```

- Top-level: `- [ ] N.0 Phase title`
- Nested tasks: `  - [ ] N.M Task title`
- Leaf subtasks: `    - [ ] N.M.K Subtask title`
- Two-space indent per level (parser accepts 2 or 4; normalizes to 2 on write).
- Three checkbox states: `[ ]` pending, `[x]` done, `[-]` won't-do. The parser also accepts `[~]` as an alias for won't-do on input but normalizes to `[-]` on write.
- Archive treats `[x]` and `[-]` equivalently — both count as "resolved" for the phase-exit completeness check. The semantic difference is recorded in PLAN.md for the reader's benefit; it doesn't affect sweep behavior.
- A phase exits only when every nested box is `[x]`. On exit, the phase is swept to `PLAN_ARCHIVE.md`.
- Sibling bullets without checkboxes are notes/context, not tasks.

### Tolerated input variants (read-only liberality)

The parser accepts and normalizes-away the following common informal formats on read. The serializer **always** writes canonical (no bold, plain space separator, ID-first, no section headers within the tree):

- **Bold-wrapped IDs**: `- [x] **X.4.a.1** — title` parses to the same AST as `- [x] X.4.a.1 title`.
- **Em-dash / hyphen separator** between ID and title: ` — `, ` - `, or plain whitespace are all accepted.
- **Alphanumeric IDs** with dots (`X.4.a.1`, `Y.2.gate`) in addition to pure numeric. IDs must contain at least one dot OR be all-digits, to disambiguate from title text.
- **Bare checkboxes without an ID** (`- [ ] do the thing`); the AST records `id: ""` and re-emits in that form.
- **Markdown section headers** (`## Phase X`, `### X.1`) appearing inside the tree; attached as text annotations on the most recent open node. Structural fidelity is **not** preserved for inputs that use sections-as-implicit-parents — the canonical model is checkboxes all the way down. Migration is the user's responsibility.

This is intentionally lossy on format (bold/em-dash → plain) so the canonical output stays unambiguous. AST is round-trip stable; source-format preservation is not a v1 concern.

## PLAN.md → TaskCreate mapping

- Every **leaf node** (no nested `- [ ]` children) becomes exactly one TaskCreate item.
- Non-leaf nodes (phases, intermediate tasks) live only in PLAN.md. They auto-tick when all their children tick — never represented in TaskCreate directly.
- TaskCreate fields:
  - `subject`: leaf title (without the `N.M.K` prefix).
  - `description`: leaf title + any inline notes captured immediately below the leaf line.
  - `metadata.plan_path`: dotted address (e.g., `"1.2.3"`).
  - `metadata.plan_phase`: phase title (e.g., `"Phase 1: Core parser"`).
  - `status`: `pending` for `[ ]`, `completed` for `[x]`. (`in_progress` is set by Claude during active work; it doesn't reflect to PLAN.md until completion.)
- `blocks` / `blockedBy` dependencies are **not** auto-populated in v1. The hierarchical structure of PLAN.md is the only ordering signal.

## Bridge components

### 1. `plan-bridge` Rust binary

Single binary, multiple subcommands:

- `plan-bridge parse [PATH]` — read PLAN.md (default `./PLAN.md`), emit JSON tree of current state to stdout.
- `plan-bridge writeback --event <create|update> --tool-input <json> [--tool-response <json>]` — applies a TaskCreate / TaskUpdate result back to PLAN.md. Idempotent.
- `plan-bridge reconcile --task-list <json>` — diffs parsed PLAN.md against current TaskList, emits a JSON delta describing what TaskCreate calls Claude should make to mirror PLAN.md.
- `plan-bridge archive` — sweeps any phase whose subtree is entirely `[x]` from PLAN.md into PLAN_ARCHIVE.md (with timestamp header).
- `plan-bridge init` — scaffolds an empty PLAN.md and `.claude/settings.json` hook config in the current project.

### 2. Per-project state

`.claude/plan-bridge-state.json` — gitignored. Holds `taskId ↔ plan_path` mapping for the active session so writeback can locate the right PLAN.md line from a `taskId` alone.

### 3. Hooks (registered in `.claude/settings.json`)

| Event | Matcher | Command | Purpose |
|---|---|---|---|
| `UserPromptSubmit` | — | `plan-bridge reconcile --task-list "$(claude-task-list)"` | Surface PLAN.md state + drift before each Claude turn. |
| `PostToolUse` | `TaskCreate` | `plan-bridge writeback --event create` | Append a checkbox if the new task isn't already in PLAN.md; record taskId↔plan_path. |
| `PostToolUse` | `TaskUpdate` | `plan-bridge writeback --event update` | Toggle `[ ]`/`[x]` based on status. `deleted` removes the line. |
| `PostToolUse` | `Edit\|Write` | `plan-bridge reconcile` (only when path ends in `PLAN.md`) | Re-derive task list when Claude edits PLAN.md directly. |

Hook output uses `hookSpecificOutput.additionalContext` to feed PLAN.md state back into Claude's next turn. `decision: "block"` is reserved for hard failures (malformed PLAN.md, write conflict).

## Hook contract

- Hooks receive Claude Code's standard hook JSON via stdin (`session_id`, `cwd`, `hook_event_name`, `tool_name`, `tool_input`, `tool_response`).
- Hooks emit JSON via stdout. v1 fields used:
  - `hookSpecificOutput.additionalContext`: free-form text shown to Claude before its next response.
  - `decision`: `"block"` only on malformed PLAN.md or write conflict; never to suppress TaskCreate as a category.
- Hooks **never** call Claude tools directly. They emit guidance; Claude executes.

## CLI surface (v1, stable)

```
plan-bridge parse [PATH]
plan-bridge writeback --event <create|update> --tool-input <json> [--tool-response <json>]
plan-bridge reconcile [--task-list <json>]
plan-bridge archive
plan-bridge init
```

All commands read/write `PLAN.md` in the current working directory unless `--plan <PATH>` is passed.

## MCP surface (deferred to Phase 3+)

Same binary, `plan-bridge serve` entry point. Exposes:

- `plan_add`, `plan_check`, `plan_uncheck`, `plan_archive`, `plan_list`, `plan_phase_exit`.

Useful when TaskCreate's flat model is too lossy — e.g., explicit reordering, phase-exit gates, querying archival. Adds Claude-callable tools that operate on PLAN.md natively without going through TaskCreate at all.

## Language / tooling

- Rust (2024 edition).
- `clap` for CLI surface.
- `serde` / `serde_json` for hook JSON I/O.
- `pulldown-cmark` or a hand-rolled line parser for PLAN.md (line-oriented; no need for full CommonMark).
- Tests live next to the code (`#[cfg(test)]` modules). Phase exit requires unit + e2e passing per global CLAUDE.md workflow.

## Risks / open questions

- **TaskList is opaque to hooks.** A hook can't enumerate Claude's current tasks; it only sees the tool input/output of the most recent call. v1 sidesteps this by writing through on every TaskCreate/TaskUpdate; full reconcile relies on Claude passing `TaskList` output to the binary. Worth revisiting if drift becomes common.
- **`UserPromptSubmit` reconcile cost.** Re-parsing PLAN.md every prompt is cheap (small file, line-oriented). Re-emitting full state in `additionalContext` is the bigger concern — should emit a compact delta, not the whole tree.

  The delta MUST cover, at minimum:
  - Leaves added, removed, or moved.
  - Box-state flips (`[ ]` ↔ `[x]`).
  - Leaf title edits.
  - **Sub-leaf annotations** — any non-checkbox bullet, indented note, or trailing prose attached to a leaf. Common case: user adds context under an existing item between turns and tells Claude "go look." Reconcile must surface the new annotation text, not just structural diffs.
  - **Parent-child consistency**: a non-leaf node marked `[x]` whose subtree still has `[ ]` descendants. Reconcile surfaces this loudly (the user may have ticked a parent prematurely). Archive enforces the same invariant by refusing to sweep an inconsistent phase, so the two checks form a layered safety net.
- **Indent / numbering ambiguity.** Parser should tolerate `1`/`1.0`, `1.1`/`1.1.0`, 2- or 4-space indent. Writer always normalizes.
- **Concurrent edits.** If Claude edits PLAN.md and the user edits PLAN.md in the same second, the hook race is unresolved. Acceptable in v1; document the constraint.
- **Bootstrapping CLAUDE.md.** Users of the bridge may need a one-line addition to their CLAUDE.md teaching Claude about `metadata.plan_path`. `plan-bridge init` can offer to add it.

## Acceptance criteria

- `plan-bridge parse` on a fixture PLAN.md emits a JSON tree matching expected structure (snapshot test).
- After `TaskCreate(subject="foo", metadata={plan_path: "1.1.1"})`, PostToolUse hook appends `    - [ ] 1.1.1 foo` to the right phase in PLAN.md.
- After `TaskUpdate(taskId=X, status=completed)`, PostToolUse hook flips the corresponding `[ ]` to `[x]`.
- User manually edits PLAN.md to tick `1.1.1`; next UserPromptSubmit emits a delta telling Claude to `TaskUpdate` the matching task to completed.
- `plan-bridge archive` moves any phase whose entire subtree is `[x]` into `PLAN_ARCHIVE.md` with a timestamped header, and removes it from PLAN.md.
- End-to-end: a fresh project + `plan-bridge init` + a Claude session that creates and completes a small plan produces a clean PLAN.md and PLAN_ARCHIVE.md, with TaskCreate state matching at every point.
