# claude-plan-bridge

[![CI](https://github.com/chotchki/claude-plan-bridge/actions/workflows/ci.yml/badge.svg)](https://github.com/chotchki/claude-plan-bridge/actions/workflows/ci.yml) [![Crates.io](https://img.shields.io/crates/v/claude-plan-bridge.svg)](https://crates.io/crates/claude-plan-bridge) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Bridge Claude Code's per-session task list and a durable `PLAN.md` checked in with the code.

## Why

Claude Code's task list (`TaskCreate`/`TaskList`) is **per-session** — it resets when you start a new conversation. That's fine for short, self-contained work, but anything spanning a day or a multi-step refactor needs to live somewhere durable. `PLAN.md` is the natural home: a checkbox tree in the repo, reviewed in PRs, version-controlled alongside the code it describes. The plan stays *with the code* — no external task tracker to drift out of sync, no separate login to chase.

Running both means living with two task lists that both want to be the source of truth. Without coordination, Claude ticks tasks but `PLAN.md` grows stale, or you hand-edit `PLAN.md` and Claude can't see what changed. `claude-plan-bridge` installs three Claude Code hooks (`PostToolUse(TaskCreate)`, `PostToolUse(TaskUpdate)`, `UserPromptSubmit`) that keep the two views aligned: new `TaskCreate`s land in `PLAN.md`, completed tasks tick the right boxes, and any edits you make to `PLAN.md` between turns surface as `additionalContext` on your next message. An MCP server mode is also available for clients that prefer typed tool calls over markdown.

**Scope honesty:** designed for small teams (1–5 contributors). Many parallel branches editing `PLAN.md` will produce merge conflicts the bridge doesn't resolve — at that scale you'd want a real ticketing system. Flip side: the bridge imposes a **canonical** `PLAN.md` format (dotted-decimal ids, three-state checkboxes, two-space indent, suffix-positioning for in-between inserts) and the reconcile loop catches format drift early. It's useful even as a strict-format linter for plan documents.

## Install

```sh
# Once published to crates.io:
cargo install claude-plan-bridge

# Or from source:
cargo install --git https://github.com/chotchki/claude-plan-bridge
```

This puts a `claude-plan-bridge` binary in `~/.cargo/bin` — make sure that's on your `PATH`.

## Quickstart

```sh
cd your-project/
claude-plan-bridge init
```

`init` scaffolds a starter `PLAN.md` if you don't have one, wires the three hooks into `.claude/settings.json`, and adds the state file to `.gitignore`. **Restart Claude Code** so it picks up the new `settings.json`.

If you're installing into a project with an *existing* `PLAN.md`, also run:

```sh
claude-plan-bridge baseline
```

— it seeds the state file with synthetic mappings for every current leaf, so your first `reconcile` isn't a wall of `LeafAdded` deltas.

From the next session, the bridge runs invisibly:

| Claude does… | The bridge does… |
|---|---|
| `TaskCreate` (any source) | Appends `- [ ] N.M new task` to `PLAN.md` (under `Inbox.0` if no `plan_path` metadata, else at the requested id) |
| `TaskUpdate(status="completed")` | Ticks `[ ]` → `[x]` at the mapped line |
| `TaskUpdate(status="deleted")` | Removes the line (orphaned empty parents stay; you prune by hand) |
| *you* edit `PLAN.md` between turns | Next `UserPromptSubmit` feeds the diff to Claude as `additionalContext` |

When a phase is fully resolved, sweep it:

```sh
claude-plan-bridge archive
```

…moves every fully-`[x]` (or `[-]`) top-level phase into `PLAN_ARCHIVE.md` under a dated section, and drops the associated state mappings.

## CLI reference

### `parse [--plan PATH]`

Emit the parsed `PLAN.md` AST as pretty-printed JSON on stdout. `PATH` defaults to `./PLAN.md`.

### `writeback --event {create|update}`

PostToolUse hook handler. Reads the hook payload from stdin, mutates `PLAN.md` + the state file under an advisory file lock, and writes a JSON hook response.

- **create** (`TaskCreate`): insert at `tool_input.metadata.plan_path` if set (parent must exist), else append to auto-managed `Inbox.0`. Idempotent on `task_id`.
- **update** (`TaskUpdate`): `completed` flips `[ ]` → `[x]`; `deleted` removes the line; `pending`/`in_progress` is a no-op.

### `reconcile`

UserPromptSubmit hook handler. Diffs `PLAN.md` against the bridge's `last_synced_*` baselines and emits any drift as `additionalContext`. Output is `{}` when there's no drift.

Delta variants Claude can act on:

| Kind | Meaning | Suggested response |
|---|---|---|
| `leaf_added` | New checkbox in `PLAN.md` with no state mapping | `TaskCreate` to mirror |
| `leaf_removed` | Tracked task missing from `PLAN.md` | `TaskUpdate(status="deleted")` |
| `leaf_state_changed` | Checkbox flipped between `[ ]`/`[x]`/`[-]` | `TaskUpdate` matching the new state |
| `leaf_title_changed` | Title text edited | `TaskUpdate(subject=...)` |
| `leaf_annotation_changed` | Notes / sub-bullets / code blocks under the leaf differ | Read; act if needed |
| `parent_inconsistent` | A parent is `[x]` but a descendant leaf isn't | Resolve before archive — sweep refuses inconsistent phases |

### `archive [--plan PATH] [--dry-run] [--date YYYY-MM-DD]`

Sweep every fully-resolved top-level phase from `PLAN.md` into `PLAN_ARCHIVE.md`. New section is appended at the **bottom** under a `## YYYY-MM-DD` header, so history reads chronological-ascending. Both files written atomically (tmp + rename). State mappings under archived subtrees are dropped.

**IDs are never renumbered.** Gaps after a sweep are intentional — downstream commit messages and code comments may reference the original ids.

### `baseline [--plan PATH]`

Seed the state file with synthetic `baseline:<plan_path>` mappings for every leaf currently in `PLAN.md`. Run once when installing into a project with an existing plan. Idempotent. When Claude later `TaskCreate`s against a baselined path, the baseline mapping is silently replaced.

### `serve [--plan PATH]` (MCP)

Run an MCP server over stdio that exposes typed plan-mutation tools. Useful when you'd rather drive plans through a structured API than through markdown editing.

Tools: `plan_list`, `plan_check`, `plan_uncheck`, `plan_skip`, `plan_add`, `plan_archive`, `plan_phase_exit`. Errors surface as JSON-RPC error responses (`code: -32603`); unknown tools and missing args produce clean errors the client can show.

Wire it into your MCP client config — for Claude Code, point an `mcpServers` entry at `claude-plan-bridge serve`.

### `init [--cwd PATH] [--force]`

Scaffold a new project. Creates a starter `PLAN.md`, merges the three hooks into `.claude/settings.json` (preserving any other hooks you've configured), and appends the state file + lock file to `.gitignore`. Idempotent. `--force` overwrites an existing `PLAN.md` with the template.

## State file

`.claude/plan-bridge-state.json` stores the `taskId ↔ plan_path` mapping the bridge needs to apply later `TaskUpdate` events:

```jsonc
{
  "version": 1,
  "mappings": {
    "<task-id>": {
      "plan_path": "1.2.3",
      "last_synced_title": "...",
      "last_synced_state": "pending",
      "last_synced_annotations": []
    }
  }
}
```

Atomic tmp+rename writes; cross-process advisory lock on the read-modify-write critical section. Gitignored by `init`. A sidecar `.lock` file (also gitignored) hosts the lock.

## Canonical `PLAN.md` format

- Checkboxes: `- [ ]` / `- [x]` / `- [-]` followed by a dotted id and the title. No bold wrapping, plain-space separator.
- Three states: `[ ]` pending, `[x]` done, `[-]` won't-do (the parser also accepts `[~]` on read, always writes `[-]`).
- IDs are project-scoped and stable across archive sweeps. Suffix-positioning works: `7.2a` slots between `7.2` and `7.3` without renumbering.
- Two-space indent per tree level.
- Code-block fences re-emit at the normalized indent; their content is preserved verbatim.

The parser also tolerates several human-friendly variants (bold-wrapped ids, em-dash separators, alphanumeric components, bare checkboxes) and normalizes them on write.

<details>
<summary>Full JSON schema (output of <code>parse</code>)</summary>

```jsonc
{
  "preamble": ["raw markdown lines before the first checkbox"],
  "phases": [
    {
      "id": "1.0",            // dotted-decimal, project-scoped
      "title": "Phase title",
      "state": "pending",     // "pending" | "done" | "wont_do"
      "children": [ /* same Node shape, nested */ ],
      "annotations": [ /* tagged unions, see below */ ]
    }
  ]
}
```

Annotations:

```jsonc
{ "kind": "text",       "text": "...", "indent": 2 }
{ "kind": "bullet",     "text": "...", "indent": 2 }
{ "kind": "code_block", "lang": "rust", "content": "...", "indent": 2 }
```

Round-trip is **AST-stable**, not byte-stable: `parse(serialize(parse(x))) == parse(x)` holds, but source format (bold wrapping, em-dash separator, etc.) is intentionally normalized away.

</details>

## Contributing

Bug reports and PRs welcome at [github.com/chotchki/claude-plan-bridge](https://github.com/chotchki/claude-plan-bridge). The implementation sequence is documented in [PLAN.md](./PLAN.md); design rationale in [SPEC.md](./SPEC.md). The bridge that ships in the binary drives both files in this repo, so dogfooding is the default development mode.

## License

MIT — see [LICENSE](./LICENSE).
