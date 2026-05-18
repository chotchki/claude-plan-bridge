# claude-plan-bridge

[![CI](https://github.com/chotchki/claude-plan-bridge/actions/workflows/ci.yml/badge.svg)](https://github.com/chotchki/claude-plan-bridge/actions/workflows/ci.yml) [![Crates.io](https://img.shields.io/crates/v/claude-plan-bridge.svg)](https://crates.io/crates/claude-plan-bridge) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Bridge Claude Code's per-session task list and a durable `PLAN.md` checked in with the code.

## Why

Claude Code's task list (`TaskCreate`/`TaskList`) is **per-session** — it resets when you start a new conversation. That's fine for short, self-contained work, but anything spanning a day or a multi-step refactor needs to live somewhere durable. `PLAN.md` is the natural home: a checkbox tree in the repo, reviewed in PRs, version-controlled alongside the code it describes. The plan stays *with the code* — no external task tracker to drift out of sync, no separate login to chase.

Running both means living with two task lists that both want to be the source of truth. Without coordination, Claude ticks tasks but `PLAN.md` grows stale, or you hand-edit `PLAN.md` and Claude can't see what changed. `claude-plan-bridge` installs four Claude Code hooks (`SessionStart`, `UserPromptSubmit`, `PostToolUse(TaskCreate)`, `PostToolUse(TaskUpdate)`) that keep the two views aligned: new `TaskCreate`s land in `PLAN.md`, completed tasks tick the right boxes, edits you make to `PLAN.md` between turns surface as `additionalContext` on your next message, and a fresh Claude Code session rehydrates the in-session task list from `PLAN.md` automatically. An MCP server mode is also available for clients that prefer typed tool calls over markdown.

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

> [!NOTE]
> **Headers, briefly.** The bridge only promotes `##` or `###` headers
> matching `<id> — Title` (e.g. `### Phase 1 — Build`, `### AA.A —
> Dropdowns`) into canonical phase checkboxes — everything else stays as
> narrative. `####+` headers are sub-section labels inside a phase and
> are never promoted (the real hierarchy lives in dotted ids like
> `X.4.a.1`). Generic headers (`## Notes`, `### Architecture`, `## Phase
> history`) are preserved verbatim at their original column on writeback
> — no refusal, no demotion. See [Canonical `PLAN.md` format](#canonical-planmd-format)
> for the full rules.

From the next session, the bridge runs invisibly:

| Claude does… | The bridge does… |
|---|---|
| Starts a new session | `SessionStart` hook emits a rehydration prompt listing every open *leaf* `plan_path` (parents shown only as context headers) so Claude re-`TaskCreate`s them into the fresh task list (writeback re-links to existing PLAN.md lines, no duplicates) |
| `TaskCreate` (any source) | Appends `- [ ] N.M new task` to `PLAN.md` (under `Inbox.0` if no `plan_path` metadata, else at the requested id) |
| `TaskUpdate(status="completed")` | Ticks `[ ]` → `[x]` at the mapped line |
| `TaskUpdate(status="deleted")` | Removes the line (orphaned empty parents stay; you prune by hand) |
| `TaskUpdate(subject="...")` | Rewrites the node's title in `PLAN.md` (works with or without a status change) |
| *you* edit `PLAN.md` between turns | Next `UserPromptSubmit` feeds the diff to Claude as `additionalContext` |

When a phase is fully resolved, sweep it:

```sh
claude-plan-bridge archive
```

…moves every fully-`[x]` (or `[-]`) top-level phase into `PLAN_ARCHIVE.md` under a dated section, and drops the associated state mappings.

## CLI reference

Every project-scoped subcommand below accepts the same two scope flags:

- `--cwd PATH` — project directory (default `.`). Useful for scripting from outside the project, e.g. `claude-plan-bridge baseline --cwd ../some-project/`.
- `--plan PATH` — explicit PLAN.md path override. When set, takes precedence over `--cwd`-derived default.

If neither is given, the plan resolves to `./PLAN.md`. Both are interchangeable for the common case; pick whichever matches what you have on hand.

### `parse`

Emit the parsed `PLAN.md` AST as pretty-printed JSON on stdout. (Reports top-level *phases*; for a leaf-recursive count, run `baseline`.)

### `writeback --event {create|update}`


PostToolUse hook handler. Reads the hook payload from stdin, mutates `PLAN.md` + the state file under an advisory file lock, and writes a JSON hook response.

- **create** (`TaskCreate`): insert at `tool_input.metadata.plan_path` if set (parent must exist), else append to auto-managed `Inbox.0`. Idempotent on `task_id`.
- **update** (`TaskUpdate`): `status="completed"` flips `[ ]` → `[x]`; `status="deleted"` removes the line; `status="pending"`/`"in_progress"` is a no-op. A `subject` field (with or without a status change) rewrites the node's title in `PLAN.md` and refreshes the synced baseline — useful when task text gets refined mid-work.

### `resume`

SessionStart hook handler. Reads the state file and emits a rehydration prompt so Claude re-creates the in-session task list after a restart. Output is `{}` when there's no state file, no mappings, or every mapping points at a resolved/missing node. Re-`TaskCreate`s land via writeback's `plan_path`-keyed dedup — the existing PLAN.md line is reused and the stale `task_id` mapping is replaced in place, no duplicate inserted.

**Leaves only.** The prompt lists open *leaf* nodes — childless checkboxes that the agent actually works on. Parent phase nodes (like `- [ ] 27.0 Phase 27 — …`) are NOT emitted as `TaskCreate` asks; instead they appear as `## N.M Title` context headers grouping their leaves, so the agent still sees the phase goal at restart time without polluting the harness task list. Ticking a parent box is a deliberate validation step — "did the children actually meet this phase's goal?" — that the user/agent takes at phase exit, *not* an automatic consequence of all children being done. Archive (`phase_fully_done`) operates on subtree state and doesn't require the parent box ticked, so leaving validation manual costs nothing structurally.

**Source-aware framing.** The hook adapts its wording to Claude Code's `SessionStart.source`:
- `startup` / `clear` — harness task list is provably empty; the prompt is assertive ("TaskCreate is deferred on a fresh harness — fetch it first with `ToolSearch query=\"select:TaskCreate\"`"). All stale state mappings are also wiped (the harness IDs they reference no longer exist), with one row per drop appended to `.claude/plan-bridge-cleared.jsonl` for traceability.
- `resume` / `compact` — prior tool history is preserved; TaskCreate is almost always already loaded, so the ToolSearch hint stays a light conditional fallback. State mappings are NOT wiped — they still point at live harness IDs.

The first line of the prompt itself shows the call shape `TaskCreate(subject=<title>, description=<plan_path>, metadata={"plan_path": <plan_path>})` so the agent doesn't accidentally embed the `plan_path` in `subject` or duplicate the title into `description`. The bridge ignores `description`; using the plan_path keeps the harness UI from showing the same text twice.

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

### `archive [--dry-run] [--date YYYY-MM-DD]`

Sweep every fully-resolved top-level phase from `PLAN.md` into `PLAN_ARCHIVE.md`. New section is appended at the **bottom** under a `## YYYY-MM-DD` header, so history reads chronological-ascending. Both files written atomically (tmp + rename). State mappings under archived subtrees are dropped.

**IDs are never renumbered.** Gaps after a sweep are intentional — downstream commit messages and code comments may reference the original ids.

### `baseline`

Seed the state file with synthetic `baseline:<plan_path>` mappings for every leaf currently in `PLAN.md`. Run once when installing into a project with an existing plan. Idempotent. When Claude later `TaskCreate`s against a baselined path, the baseline mapping is silently replaced.

**Adopting baselined leaves into TaskList.** On a fresh session against a pre-populated `PLAN.md`, the harness's TaskList starts empty even though state has baseline mappings for every leaf. Reconcile surfaces a one-line advisory listing the baseline-only `plan_path`s so the agent sees them. To adopt: `TaskCreate(metadata.plan_path="N.M", subject="...")` — `writeback` dedupes against the existing line (no duplicate inserted) and replaces the `baseline:` mapping with the real `task_id`. Hand-editing the line directly also works; the bridge picks up the change via the next reconcile.

**Leaves with empty ids** (bare-checkbox bullets like `- [ ] no id here`) are *skipped*: they have no stable `plan_path` to key state by, so the bridge can't track them. `baseline` reports the count under "NOTE: skipped N bare-checkbox leaf(s)…" — add a dotted id (`- [ ] 1.2.3 description`) to make a leaf trackable.

### `serve` (MCP)

Run an MCP server over stdio that exposes typed plan-mutation tools. Useful when you'd rather drive plans through a structured API than through markdown editing.

Tools: `plan_list`, `plan_check`, `plan_uncheck`, `plan_skip`, `plan_add`, `plan_rename`, `plan_archive`, `plan_phase_exit`. Errors surface as JSON-RPC error responses (`code: -32603`); unknown tools and missing args produce clean errors the client can show. `plan_rename(plan_path, new_subject)` mirrors writeback's `TaskUpdate(subject=...)` and refreshes the synced baseline so reconcile is quiet next turn.

Wire it into your MCP client config — for Claude Code, point an `mcpServers` entry at `claude-plan-bridge serve`.

### `init [--cwd PATH] [--force]`

Scaffold a new project. Creates a starter `PLAN.md`, merges the four hooks into `.claude/settings.json` (preserving any other hooks you've configured), and appends the state file + lock file to `.gitignore`. Idempotent. `--force` overwrites an existing `PLAN.md` with the template.

### `upgrade-hooks [--cwd PATH]`

Re-merge the latest hook set into an existing `.claude/settings.json` without touching `PLAN.md` or `.gitignore`. Use after upgrading the bridge binary on a project installed with an older version — notably anything predating the `SessionStart` hook (added in v0.1.11). Idempotent: a no-op when the file is already current. The `reconcile` and `writeback` hooks yell a one-line warning on every fire when the `SessionStart` hook is missing, so you'll notice quickly without having to remember to check.

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
      "last_synced_annotations": [],
      "created_in_session": "<session-id>"
    }
  },
  "pending_rehydration": ["1.2.3"],
  "rehydration_announced": 1
}
```

Atomic tmp+rename writes; cross-process advisory lock on the read-modify-write critical section. Gitignored by `init`. A sidecar `.lock` file (also gitignored) hosts the lock.

`pending_rehydration` and `rehydration_announced` are seeded by the `SessionStart` hook when it wipes stale mappings on `source=startup|clear`. While paths sit in `pending_rehydration`, reconcile suppresses duplicate "Added [ ] … (consider TaskCreate)" drift for them — the rehydration prompt already asked Claude to create them. As each matching `TaskCreate` lands, writeback evicts the path. When the set drains to empty, writeback's `PostToolUse` message gains a `rehydration complete: N/N mapped` line so end-to-end success is visible.

`created_in_session` lets writeback distinguish a same-session duplicate `TaskCreate` (refused with a warning) from a legitimate cross-session re-mapping (silently evicts the stale mapping).

Sidecar `.claude/plan-bridge-cleared.jsonl` is an append-only JSON Lines audit log; one row per state mapping the bridge drops during a SessionStart wipe. Timestamps are Unix-epoch seconds — decode with `date -r <n>` or `jq '.epoch_secs | strftime("%FT%TZ")'`. Useful when chasing a "where did my task go?" report.

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
