# claude-plan-bridge

[![CI](https://github.com/chotchki/claude-plan-bridge/actions/workflows/ci.yml/badge.svg)](https://github.com/chotchki/claude-plan-bridge/actions/workflows/ci.yml) [![Coverage](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Fchotchki%2Fclaude-plan-bridge%2Fbadges%2Fcoverage.json)](https://github.com/chotchki/claude-plan-bridge/tree/badges/coverage-html) [![Crates.io](https://img.shields.io/crates/v/claude-plan-bridge.svg)](https://crates.io/crates/claude-plan-bridge) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

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
| `TaskCreate` (any source) | Inserts `- [ ] N.M new task` at the requested `plan_path`; with no `plan_path` metadata the work is unphased, so it lands as a tracked note in the bottom `## Backlog (not yet phased)` section instead |
| `TaskUpdate(status="completed")` | Ticks `[ ]` → `[x]` at the mapped line |
| `TaskUpdate(status="deleted")` | Removes the line (orphaned empty parents stay; you prune by hand) |
| `TaskUpdate(subject="...")` | Rewrites the node's title in `PLAN.md` (works with or without a status change) |
| *you* edit `PLAN.md` between turns | Next `UserPromptSubmit` feeds the diff to Claude as `additionalContext` |

When a phase is fully resolved, sweep it:

```sh
claude-plan-bridge archive
```

…moves every fully-resolved (`[x]`, `[-]`, or `[>]`) top-level phase into `PLAN_ARCHIVE.md` under a dated section, and drops the associated state mappings.

## CLI reference

Every project-scoped subcommand below accepts the same two scope flags:

- `--cwd PATH` — project directory (default `.`). Useful for scripting from outside the project, e.g. `claude-plan-bridge baseline --cwd ../some-project/`.
- `--plan PATH` — explicit PLAN.md path override. When set, takes precedence over `--cwd`-derived default.

If neither is given, the plan resolves to `./PLAN.md`. Both are interchangeable for the common case; pick whichever matches what you have on hand.

### `parse`

Emit the parsed `PLAN.md` AST as pretty-printed JSON on stdout. (Reports top-level *phases*; for a leaf-recursive count, run `baseline`.)

### `writeback --event {create|update}`


PostToolUse hook handler. Reads the hook payload from stdin, mutates `PLAN.md` + the state file under an advisory file lock, and writes a JSON hook response.

- **create** (`TaskCreate`): insert at `tool_input.metadata.plan_path` if set. With no `plan_path`, the work is unphased — it's recorded as a tracked note in the canonical `## Backlog (not yet phased)` section at the bottom of `PLAN.md` (mapped to a synthetic `backlog:<task_id>` path) and promoted into a real phase later by a deliberate planning move. Idempotent on `task_id`.
  - **Auto-anchor.** When the first `TaskCreate(plan_path=N.X)` for a brand-new phase arrives and no top-level `N.0` exists, the bridge synthesizes the anchor for you using `metadata.plan_phase` as the title (or `Phase N` as a fallback). No more manual "add the 10.0 row, then retry" dance. Intermediate parents (e.g. a missing `1.2` blocking a `1.2.3` insert) still error with the format-hint message — auto-creating non-anchor structure would invent nesting the user didn't ask for.
  - **Top-level enforcement.** If an `N.0` exists but is nested under another phase (e.g. hand-added at the wrong indent), the bridge refuses the insert rather than silently parking children under the misplaced anchor. Fix the indent and retry.
  - **Subject escape-normalization.** A subject like `Build \"/blog\" page` is normalized to `Build "/blog" page` before storage — markdown doesn't need `\"` escaping and the stray backslashes used to cause eternal title drift once the user hand-cleaned the file.
- **update** (`TaskUpdate`): `status="completed"` flips `[ ]` → `[x]`; `status="deleted"` removes the line; `status="pending"`/`"in_progress"` is a no-op. A `subject` field (with or without a status change) rewrites the node's title in `PLAN.md` and refreshes the synced baseline — useful when task text gets refined mid-work. Same `\"` → `"` normalization as create.

### `resume`

SessionStart hook handler. Reads the state file and emits a rehydration prompt so Claude re-creates the in-session task list after a restart. Output is `{}` when there's no state file, no mappings, or every mapping points at a resolved/missing node. Re-`TaskCreate`s land via writeback's `plan_path`-keyed dedup — the existing PLAN.md line is reused and the stale `task_id` mapping is replaced in place, no duplicate inserted.

**Leaves only.** The prompt lists open *leaf* nodes — childless checkboxes that the agent actually works on. Parent phase nodes (like `- [ ] 27.0 Phase 27 — …`) are NOT emitted as `TaskCreate` asks; instead they appear as `## N.M Title` context headers grouping their leaves, so the agent still sees the phase goal at restart time without polluting the harness task list. Ticking a parent box is a deliberate validation step — "did the children actually meet this phase's goal?" — that the user/agent takes at phase exit, *not* an automatic consequence of all children being done. Archive (`phase_fully_done`) operates on subtree state and doesn't require the parent box ticked, so leaving validation manual costs nothing structurally.

**Source-aware framing.** The hook adapts its wording to Claude Code's `SessionStart.source`:
- `startup` / `clear` — harness task list is provably empty; the prompt is assertive ("TaskCreate is deferred on a fresh harness — fetch it first with `ToolSearch query=\"select:TaskCreate\"`"). All stale state mappings are also wiped (the harness IDs they reference no longer exist), with one row per drop appended to `.claude/plan-bridge-cleared.jsonl` for traceability.
- `resume` / `compact` — prior tool history is preserved; TaskCreate is almost always already loaded, so the ToolSearch hint stays a light conditional fallback. State mappings are NOT wiped — they still point at live harness IDs.

The first line of the prompt itself shows the call shape `TaskCreate(subject=<title>, description=<plan_path>, metadata={"plan_path": <plan_path>})` so the agent doesn't accidentally embed the `plan_path` in `subject` or duplicate the title into `description`. The bridge ignores `description`; using the plan_path keeps the harness UI from showing the same text twice.

**Restart-test workflow.** When changing rehydration behavior (the SessionStart prompt, state-clear logic, source-aware framing), exercise the live cycle in addition to unit tests:

1. From within Claude Code, run `/clear` (forces a `source=clear` SessionStart).
2. The hook fires immediately on the next prompt; ask Claude "did the rehydration prompt arrive and did you reload the tasks?"
3. Confirm: the listed leaves match open PLAN.md leaves, parents appear only as `## N.M Title` context headers (no TaskCreate ask), and the footer notes how many stale mappings were cleared (source-aware).
4. Watch Claude `TaskCreate` each leaf — writeback should report "rehydration complete: N/N mapped" on the final one, and the harness task list should match PLAN.md leaf-for-leaf.

The unit tests in `resume.rs` cover the prompt-shape contract; the manual loop catches integration regressions the unit tests can't see (hook wiring, ToolSearch deferral, Claude's actual response shape).

### `reconcile`

UserPromptSubmit hook handler. Diffs `PLAN.md` against the bridge's `last_synced_*` baselines and emits any drift as `additionalContext`. Output is `{}` when there's no drift.

Delta variants Claude can act on:

| Kind | Meaning | Suggested response |
|---|---|---|
| `leaf_added` | New checkbox in `PLAN.md` with no state mapping | `TaskCreate` to mirror (top-level `N.0` phase anchors are skipped — they're document structure, not tasks) |
| `leaf_removed` | Tracked task missing from `PLAN.md` | `TaskUpdate(status="deleted")` |
| `leaf_state_changed` | Checkbox flipped between `[ ]`/`[x]`/`[-]`/`[>]` | `TaskUpdate` matching the new state |
| `leaf_title_changed` | Title text edited | `TaskUpdate(subject=...)` |
| `leaf_annotation_changed` | Notes / sub-bullets / code blocks under the leaf differ | Read; act if needed. Column-0 markdown section headers (`## Phase 10 …`) are excluded — they're document dividers, not leaf-scoped content |
| `parent_inconsistent` | A parent is `[x]` but a descendant leaf isn't | Resolve before archive — sweep refuses inconsistent phases |

### `archive [<PHASE>] [--descope-pending] [--dry-run] [--date YYYY-MM-DD]`

Two modes (Phase 38.4 / 38.5):

- **Bulk sweep** (no `<PHASE>` arg): every fully-resolved top-level phase
  flows from `PLAN.md` into `PLAN_ARCHIVE.md`. Phases with pending leaves
  are silently skipped. New section appended at the **bottom** under a
  `## YYYY-MM-DD` header (chronological-ascending). Both files written
  atomically. State mappings under archived subtrees are dropped.
- **Per-phase** (`archive AS`): archive the named phase. Errors loudly if
  the subtree has any `[ ]` Pending leaves. Add `--descope-pending` to
  move pending leaves into the bottom `# Backlog (not yet phased)`
  section as `- <id> - descoped from phase <PHASE> on <date>` notes,
  then archive the now-fully-resolved phase. State mappings for descoped
  paths are dropped.

**IDs are never renumbered.** Gaps after a sweep are intentional —
downstream commit messages and code comments may reference the original
ids.

### `phase-add <ID> [TITLE] [--depends-on X,Y] [--prefer-after A,B] [--after <ID>]`

Create a new FORMATv2 phase header explicitly. Surgical alternative to
TaskCreate's auto-anchor for cases that need dependency metadata at
creation time or an empty phase pre-created.

- `--depends-on` / `--prefer-after`: comma-separated phase ids for the
  hard / soft sequencing markers. Either or both, in any combination.
- `--after <ID>`: insert immediately after the named phase (positional);
  defaults to id-sort order.

### `phase-rename <ID> <new-title>`

Rewrite a phase's title. Refuses task ids — use the `plan_rename` MCP
tool (or edit PLAN.md directly) for task renames.

### `phase-deps <ID> [--depends-on X,Y] [--prefer-after A,B]`

Replace a phase's `*(depends on)*` / `*(prefer after)*` lists. At least
one of the two flags must be passed; pass an empty list (`--depends-on
""`) to clear. Flips a legacy v1 anchor to FORMATv2 header form so the
markers can render.

### `baseline`

Seed the state file with synthetic `baseline:<plan_path>` mappings for every leaf currently in `PLAN.md`. Run once when installing into a project with an existing plan. Idempotent. When Claude later `TaskCreate`s against a baselined path, the baseline mapping is silently replaced.

**Adopting baselined leaves into TaskList.** On a fresh session against a pre-populated `PLAN.md`, the harness's TaskList starts empty even though state has baseline mappings for every leaf. Reconcile surfaces a one-line advisory listing the baseline-only `plan_path`s so the agent sees them. To adopt: `TaskCreate(metadata.plan_path="N.M", subject="...")` — `writeback` dedupes against the existing line (no duplicate inserted) and replaces the `baseline:` mapping with the real `task_id`. Hand-editing the line directly also works; the bridge picks up the change via the next reconcile.

**Leaves with empty ids** (bare-checkbox bullets like `- [ ] no id here`) are *skipped*: they have no stable `plan_path` to key state by, so the bridge can't track them. `baseline` reports the count under "NOTE: skipped N bare-checkbox leaf(s)…" — add a dotted id (`- [ ] 1.2.3 description`) to make a leaf trackable.

### `serve` (MCP)

Run an MCP server over stdio that exposes typed plan-mutation tools. Useful when you'd rather drive plans through a structured API than through markdown editing.

Tools:

- **Tasks**: `plan_list`, `plan_check`, `plan_uncheck`, `plan_skip`,
  `plan_backlog`, `plan_add`, `plan_rename`
- **Phases**: `plan_add_phase`, `plan_rename_phase`, `plan_set_phase_deps`
- **Archive**: `plan_archive` (bulk), `plan_phase_exit` (single — accepts
  optional `descope_pending: bool` matching the CLI's `--descope-pending`)

Errors surface as JSON-RPC error responses (`code: -32603`); unknown
tools and missing args produce clean errors the client can show.
`plan_rename(plan_path, new_subject)` mirrors writeback's
`TaskUpdate(subject=...)` and refreshes the synced baseline so reconcile
is quiet next turn.

Wire it into your MCP client config — for Claude Code, point an `mcpServers` entry at `claude-plan-bridge serve`.

### `init [--cwd PATH] [--force]`

Scaffold a new project. Creates a starter `PLAN.md`, merges the four hooks into `.claude/settings.json` (preserving any other hooks you've configured), and appends the state file + lock file to `.gitignore`. Idempotent. `--force` overwrites an existing `PLAN.md` with the template.

Each installed hook command bakes the **absolute project root** as `--cwd '/abs/path'`. This makes the subprocess CWD irrelevant — if Claude `cd`s into a subdirectory mid-session, the hook still finds the right `PLAN.md`. (Added in Phase 32 after a session imploded with every prompt blocked because an inherited wrong cwd made `./PLAN.md` unreadable.)

### `upgrade-hooks [--cwd PATH]`

Re-merge the latest hook set into an existing `.claude/settings.json` without touching `PLAN.md` or `.gitignore`. Use after upgrading the bridge binary on a project installed with an older version — notably anything predating the `SessionStart` hook (added in v0.1.11) or the absolute `--cwd` baking (added in v0.1.20). Idempotent: a no-op when the file is already current. The `reconcile` and `writeback` hooks yell a one-line warning on every fire when either the `SessionStart` hook is missing or the installed commands lack absolute `--cwd`, so you'll notice quickly without having to remember to check.

## Troubleshooting

**Symptom**: every prompt blocked with `claude-plan-bridge: read ./PLAN.md: No such file or directory (os error 2)`, including innocuous commands like `ls`.

**Cause** (pre-v0.1.20): hook commands resolved `./PLAN.md` against the subprocess CWD. If Claude `cd`'d into a subdirectory mid-session, the hook inherited that cwd and couldn't find the plan, and the bridge converted the I/O error into `decision: "block"`.

**Fix**: upgrade to v0.1.20+ (`cargo install --force claude-plan-bridge`) and run `claude-plan-bridge upgrade-hooks` in the affected project. Hooks are rewritten with absolute `--cwd`, and v0.1.20 also drops the `decision: "block"` path entirely — missing `PLAN.md` is now a silent no-op no matter how the bridge gets misrouted.

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

## Canonical `PLAN.md` format (FORMATv2)

Phases are h2 markdown headers; tasks are checkboxes underneath. The full
shape:

```
## Phase AI - Studio dogfood *(depends on: AH)* *(prefer after: AG)*

Intro paragraph at the phase level — sweeps with the phase to archive.

- [ ] AI.0 - Lock decisions
- [ ] AI.1 - Implement driver
  - [x] AI.1.0 - protocol
  - [ ] AI.1.1 - transport
    Indented prose stays with the task it sits under.

# Backlog (not yet phased)

- **Drop the fs4 crate** — added 2026-05-22.
- AI.2 - descoped from phase `AI` on 2026-05-22
```

- **Phases**: `## Phase <ID> - <Title>` (h2 header). The id is a bare
  alphabetic prefix (`AI`, `AS`) or numeric (`1`, `1.0`). Title optional.
- **Phase dependency markers**: `*(depends on: X, Y)*` (hard sequencing
  hint) and/or `*(prefer after: A, B)*` (soft hint). Informational only —
  reconcile surfaces them; the bridge never blocks an operation.
- **Tasks**: `- [<state>] <PHASE>.<N> - <title>` at column 0 under the
  phase. Subtasks indent two spaces per level.
- **Four checkbox states**: `[ ]` pending, `[x]` done, `[-]` won't-do,
  `[>]` backlog. Parser accepts `[~]` as won't-do alias on read.
- **Human-facing output** renders state as emoji: ⬜ pending, ✅ done,
  ❌ won't-do, 🔜 backlog. PLAN.md itself always uses the bracket form.
- **IDs are project-scoped and stable across archive sweeps**. Suffix-
  positioning works: `7.2a` slots between `7.2` and `7.3` without renumbering.
- **`# Backlog (not yet phased)`** (h1) pins to the bottom. Accepts flat
  notes (`- **Subject** — added <date>.`) AND nested descoped subtrees
  (`- X.1 - …` with indented children).

### Conservative format dispatch + canonicalize

Routine writes (TaskCreate, TaskUpdate, archive sweep) **preserve format
per phase**. v1 plans with `- [ ] N.0 Title` anchors keep their anchor
form; v2 plans with `## Phase X` headers keep their header form. Both
shapes coexist in the same PLAN.md without friction.

The single operation that flips everything to FORMATv2 canonical is:

```sh
claude-plan-bridge canonicalize
```

It promotes v1 anchors to v2 headers, normalizes task separators to
` - ` hyphen-space, flips the backlog heading h2 → h1, and preserves any
v1 phase-state checkbox marker as a prose breadcrumb
(`*(was marked [x] in v1 — archive to make it official)*`) so nothing is
silently lost. Idempotent — second run is a no-op.

The parser tolerates human-friendly variants on read (bold-wrapped ids,
em-dash separators, alphanumeric components, bare checkboxes); only
canonicalize normalizes them.

<details>
<summary>Full JSON schema (output of <code>parse</code>)</summary>

```jsonc
{
  "preamble": ["raw markdown lines before the first checkbox"],
  "phases": [
    {
      "id": "1.0",            // dotted-decimal, project-scoped
      "title": "Phase title",
      "state": "pending",     // "pending" | "done" | "wont_do" | "backlog"
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

## Backlog state (`[>]` 🔜)

`[>]` marks a leaf as **deferred from its current phase** — work you've consciously decided not to ship as part of this phase, but want to remember for later. Distinct from `[-]` (won't-do, abandoned) and `[ ]` (still active).

All deferred and unphased work collects in a single **`## Backlog (not yet phased)` section pinned to the bottom of PLAN.md** — a visible, named holding pen, not an auto-discovered `Inbox` sub-list. You promote a coherent batch into a named phase when the time comes (the planning move); the bridge never auto-phases backlog items.

Four ways work lands in Backlog:

1. **`TaskCreate` with no `metadata.plan_path`** — unphased work. The hook records a tracked note (`- **<subject>** — added <date>.`) in the bottom section, mapped to a synthetic `backlog:<task_id>` path so the harness task stays linked. Completing or deleting that task removes the note; a subject rename updates it.
2. **`TaskUpdate(taskId=X, status="deleted")` against a pending leaf** — the hook flips the line to `[>]` and appends a deferral bullet (source plan_path + date) to the bottom section. State mapping is dropped. **Pending leaves are never hard-deleted from PLAN.md via `TaskUpdate`** — this is the safety contract. To actually remove a line, edit PLAN.md by hand or let archive sweep it.
3. **MCP `plan_backlog(plan_path, date?)`** — same effect as (2), callable directly without going through the harness task list. Useful when there's no active mapping (e.g., baselined plan).
4. **CLI `plan-bridge backlog <plan_path>`** — same effect from the shell.

The section is owned as a first-class trailing region: it always serializes below every phase and survives phase-appends without drifting. A `## Backlog` that's still sitting in the preamble (or split across duplicate sections) gets merged down to the bottom only when you run **`plan-bridge canonicalize`** (or on the next backlog-mutating write) — routine ticks and renames leave its placement alone. Conservative by design: only the bridge-owned `## Backlog (not yet phased)` h2 is touched; operator sections like `### Backlog (rehomed from ...)` or `## Sustainment` are left exactly where they are.

What happens at phase exit: archive treats `[>]` like `[x]` and `[-]` — all three count as "resolved" for the `phase_fully_done` gate. When the phase sweeps to `PLAN_ARCHIVE.md`, the `[>]` lines go with it. But the `## Backlog (not yet phased)` bullet you got at deferral time **stays in PLAN.md**, so the deferred work is preserved as a durable record.

When to use `[>]` vs `[-]`:

- `[-]` (won't-do): "we evaluated this and decided not to do it." Final.
- `[>]` (backlog): "this is real work; we're punting it out of THIS phase." Re-introduce later by hand-adding to a new phase or `TaskCreate` against a new plan_path.

If you're not sure: prefer `[>]`. Backlog preserves the work; won't-do discards it.

## Contributing

Bug reports and PRs welcome at [github.com/chotchki/claude-plan-bridge](https://github.com/chotchki/claude-plan-bridge). The implementation sequence is documented in [PLAN.md](./PLAN.md); design rationale in [SPEC.md](./SPEC.md). The bridge that ships in the binary drives both files in this repo, so dogfooding is the default development mode.

### Coverage

`cargo-llvm-cov` is the coverage tool. CI runs it on every push and PR; the badge above is regenerated from the latest `main` push and lives on the orphan [`badges` branch](https://github.com/chotchki/claude-plan-bridge/tree/badges) alongside an HTML report. Color thresholds: ≥80% brightgreen, ≥60% yellow, ≥40% orange, <40% red.

To run locally:

```
cargo install cargo-llvm-cov --locked   # one-time
cargo llvm-cov --no-report --all-features --workspace
cargo llvm-cov report --html --output-dir coverage-html
open coverage-html/html/index.html
```

`cargo llvm-cov report --json --summary-only` produces the JSON the CI badge generator consumes. The `lcov.info` + HTML report are also uploaded as a per-run CI artifact (`coverage`, 14-day retention) — open a run on GitHub Actions to download.

## License

MIT — see [LICENSE](./LICENSE).
