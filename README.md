# plan-bridge

Bridges Claude Code's `TaskCreate` task list to a canonical `PLAN.md` so the two systems stop fighting. Design rationale and acceptance criteria live in [SPEC.md](./SPEC.md); the implementation sequence in [PLAN.md](./PLAN.md).

**Status:** Phases 1–6 complete (ready for archive sweep). `parse`, `writeback`, `reconcile`, `archive`, `init`, `baseline`, `serve` all wired up and live-tested against this project.

## Build & test

```
cargo build
cargo test
```

## CLI

## Install

```
cargo install --path .
```

This produces a `plan-bridge` binary in `~/.cargo/bin`. Make sure that's on your `PATH` (`echo $PATH | grep -q ~/.cargo/bin`).

## Set up in a project

```
cd your-project/
plan-bridge init
```

This:
- Scaffolds a starter `PLAN.md` if you don't have one.
- Adds `plan-bridge` hooks to `.claude/settings.json` for `UserPromptSubmit`, `PostToolUse(TaskCreate)`, and `PostToolUse(TaskUpdate)`.
- Adds `.claude/plan-bridge-state.json` to `.gitignore`.

Restart Claude Code so it picks up the new `settings.json`. From the next session, `TaskCreate` / `TaskUpdate` calls flow into `PLAN.md`, and any edits you make to `PLAN.md` between turns surface as `additionalContext` on your next message.

### `plan-bridge parse`

Parse a `PLAN.md` and emit its AST as pretty-printed JSON on stdout.

```
plan-bridge parse [--plan PATH]    # PATH defaults to ./PLAN.md
```

### `plan-bridge writeback`

Apply a Claude Code `PostToolUse` hook event to `PLAN.md`. Reads the hook payload as JSON on stdin; writes any updates to `PLAN.md` and the project state file (`.claude/plan-bridge-state.json`); emits a hook response JSON on stdout.

```
plan-bridge writeback --event create [--plan PATH]
plan-bridge writeback --event update [--plan PATH]
```

**create** handles `PostToolUse(TaskCreate)`:
- If `tool_input.metadata.plan_path` is set, insert at that exact id. Parent must already exist in `PLAN.md`.
- Otherwise, append to an auto-managed `Inbox.0` phase (created at the end of `PLAN.md` if missing).
- Idempotent on `task_id`: re-running the same create is a no-op.

**update** handles `PostToolUse(TaskUpdate)`:
- `status: completed` flips `[ ]` to `[x]`.
- `status: deleted` removes the line and the state mapping. Orphaned empty parents are **not** cascade-removed in v1.
- `status: pending | in_progress` (or no status) is a no-op — those states live in `TaskCreate` only.
- Silent no-op when the `taskId` isn't in our state map (the task wasn't created via the bridge).

### `plan-bridge reconcile`

Diff `PLAN.md` against the bridge's recorded `last_synced_*` baseline and emit any drift on stdout as a hook response. Intended for the `UserPromptSubmit` hook — runs every time the user submits a message so external edits to `PLAN.md` between turns surface in Claude's context.

```
plan-bridge reconcile [--plan PATH]
```

Output JSON shape:

```jsonc
// No drift:
{}

// Drift detected:
{
  "hookSpecificOutput": {
    "additionalContext": "PLAN.md drift since last sync:\n  ~ Title 1.1 (task abc)\n     was: a task\n     now: a renamed task\n  v Checked 1.1  (task abc — consider TaskUpdate status=completed)\n  + Annotations changed under 1.1 (task abc)\n      - Note added by hand: investigate edge case\n"
  }
}
```

Delta variants (each carries `"kind"` plus delta-specific fields when emitted as structured JSON via `reconcile()` from the library):

| Kind | Meaning | Claude's response |
|---|---|---|
| `leaf_added` | New checkbox in PLAN.md that the bridge has no state mapping for | `TaskCreate` to mirror |
| `leaf_removed` | Tracked task is no longer in PLAN.md | `TaskUpdate(status="deleted")` |
| `leaf_checked` | PLAN.md says `[x]`, last sync said `[ ]` | `TaskUpdate(status="completed")` |
| `leaf_unchecked` | PLAN.md says `[ ]`, last sync said `[x]` | Informational only (TaskUpdate can't revive a completed task) |
| `leaf_title_changed` | Title text edited in PLAN.md | `TaskUpdate(subject=...)` |
| `leaf_annotation_changed` | Notes / sub-bullets / code blocks under the leaf differ | Read the rendered text; act if needed |
| `parent_inconsistent` | A non-leaf node is `[x]` but its subtree still has unchecked descendants | Either untick the parent, or tick the unfinished children. Archive will refuse the phase otherwise. |

### `plan-bridge archive`

Sweep every fully-complete top-level phase from `PLAN.md` into `PLAN_ARCHIVE.md` (newest section prepended at the top, dated `## YYYY-MM-DD`). State mappings whose `plan_path` lives inside an archived subtree are dropped.

```
plan-bridge archive [--plan PATH] [--dry-run] [--date YYYY-MM-DD]
```

- A phase is "fully complete" when every leaf in its subtree is `[x]`. The phase's own checkbox state is irrelevant — children determine.
- IDs are **never renumbered**. Gaps in numbering after a sweep are intentional and acceptable; downstream code comments and commit messages may reference the original ids.
- Both files are written atomically (tmp + rename).
- `--dry-run` lists what would be archived without touching the filesystem.

### `plan-bridge serve` (MCP)

Run an MCP server over stdio. Lets a Claude Code session (or any MCP client) drive PLAN.md through typed tools instead of going through the hook layer.

```
plan-bridge serve [--plan PATH]
```

Wire it into your MCP client config (the exact shape depends on the client; for Claude Code, point an `mcpServers` entry at `plan-bridge serve`).

#### Tools

| Name | Args | Effect |
|---|---|---|
| `plan_list` | — | Returns the parsed `PLAN.md` AST as JSON text. |
| `plan_check` | `plan_path` | Sets the node's checkbox to `[x]`. |
| `plan_uncheck` | `plan_path` | Sets the node's checkbox to `[ ]`. |
| `plan_skip` | `plan_path` | Sets the node's checkbox to `[-]` (won't-do). Archive treats `[-]` like `[x]`. |
| `plan_add` | `plan_path`, `subject` | Adds a new leaf at `plan_path` (parent must exist). |
| `plan_archive` | `dry_run?`, `date?` | Sweeps fully-complete phases to `PLAN_ARCHIVE.md`. |
| `plan_phase_exit` | `plan_path`, `date?` | Validates one phase's subtree is fully resolved, then archives just that phase. Errors if any `[ ]` leaf remains. |

MCP-style tool errors are surfaced as JSON-RPC error responses (`code: -32603`), not as protocol-level disconnects. Unknown tool names and missing required args both produce clean errors that the client surfaces back to the user.

### State file

`.claude/plan-bridge-state.json` stores the `taskId ↔ plan_path` mapping the bridge needs to apply later `TaskUpdate` events. Format:

```jsonc
{
  "version": 1,
  "mappings": {
    "<task-id>": { "plan_path": "1.2.3" }
  }
}
```

Atomic writes via tmp-file + rename; gitignored by default.

## JSON schema

`parse` emits a `Plan` object:

```jsonc
{
  "preamble": ["raw markdown lines before the first checkbox"],
  "phases": [
    {
      "id": "1.0",            // dotted decimal, project-scoped
      "title": "Phase title",
      "checked": false,
      "children": [ /* same Node shape, nested */ ],
      "annotations": [ /* see below */ ]
    }
  ]
}
```

Annotations are tagged unions:

```jsonc
{ "kind": "text",       "text": "...", "indent": 2 }
{ "kind": "bullet",     "text": "...", "indent": 2 }
{ "kind": "code_block", "lang": "rust", "content": "...", "indent": 2 }
```

### Canonical output format

- Each checkbox: `- [ ]` / `- [x]` / `- [-]` followed by an id and the title, plain space separator, no bold wrapping.
- Three states: `[ ]` pending, `[x]` done, `[-]` won't-do (the parser also accepts `[~]` on read but always writes `[-]`).
- Ids are **project-scoped** and **stable across archive sweeps**. Gaps in numbering are intentional.
- Two-space indent per tree level.
- Code-block fences re-emit at the normalized indent; their content is preserved verbatim.

### Tolerated input variants

The parser accepts and normalizes-away the following formats commonly seen in hand-authored plans. The serializer never emits them.

- **Bold-wrapped ids**: `- [x] **X.4.a.1** — title` parses to the same AST as `- [x] X.4.a.1 title`.
- **Em-dash / hyphen separators** between id and title.
- **Alphanumeric ids** with dots (`X.4.a.1`, `Y.2.gate`) alongside pure-numeric. An id must contain at least one dot OR be all-digits to disambiguate from title text.
- **Bare checkboxes without an id** — recorded as `id: ""`.
- **Markdown section headers** (`## Phase X`) appearing inside the tree — attached as text annotations on the most recent open node. Structural fidelity is **not** guaranteed for inputs that use section headers as implicit parents; the canonical model is checkboxes all the way down.

Round-trip is **AST-stable**, not byte-stable: `parse(serialize(parse(x))) == parse(x)` holds, but source format (bold, em-dash, etc.) is intentionally normalized away.
