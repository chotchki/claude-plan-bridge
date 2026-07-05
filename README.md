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

## Upgrade

Already installed and want to pick up a newer release?

```sh
# 1) Bump the binary in place
cargo install --force claude-plan-bridge

# 2) In each project that already uses the bridge,
#    re-merge the latest hook set into settings.json
cd your-project/
claude-plan-bridge upgrade-hooks
```

`upgrade-hooks` is idempotent and only touches `.claude/settings.json` — your `PLAN.md` and `.gitignore` stay untouched. Restart Claude Code so it picks up any new hook wiring. See [`upgrade-hooks`](#upgrade-hooks---cwd-path) for details on when this is needed (notably anything predating the `SessionStart` hook or the portable `--cwd "$CLAUDE_PROJECT_DIR"` wiring, or a project carrying a stale baked `--cwd` after a rename).

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

- `--cwd PATH` — project directory (default `.`). Useful for scripting from outside the project, e.g. `claude-plan-bridge baseline --cwd ../some-project/`. When left at the default, the bridge falls back to `$CLAUDE_PROJECT_DIR` (the project root Claude Code sets for hooks) before the literal working directory — which is why the installed hooks pass `--cwd "$CLAUDE_PROJECT_DIR"` and resolve correctly no matter the subprocess cwd.
- `--plan PATH` — explicit PLAN.md path override. When set, takes precedence over `--cwd`-derived default.

If neither is given, the plan resolves to `./PLAN.md`. Both are interchangeable for the common case; pick whichever matches what you have on hand.

### `parse`

Emit the parsed `PLAN.md` AST as pretty-printed JSON on stdout. (Reports top-level *phases*; for a leaf-recursive count, run `baseline`.)

### `next-phase`

Print the next phase id in the uppercase-letter sequence (`A`..`Z` → `AA`..`AZ` → `BA`..`BZ` → ...; bijective base-26, like spreadsheet columns), reconstructed by scanning `PLAN.md` and the sibling `PLAN_ARCHIVE.md` for the highest existing uppercase-letter phase id and incrementing it. Outputs `A` for a fresh project; legacy numeric phase ids (`1`, `42`) are ignored. Scanning the archive too means a swept id is never re-handed-out. Call it before creating a new phase so you don't hand-pick — or collide on — the next letter:

```
$ claude-plan-bridge next-phase
CB
```

### `writeback --event {create|update}`


PostToolUse hook handler. Reads the hook payload from stdin, mutates `PLAN.md` + the state file under an advisory file lock, and writes a JSON hook response.

- **create** (`TaskCreate`): insert at `tool_input.metadata.plan_path` if set. With no `plan_path`, the work is unphased — it's recorded as a tracked note in the canonical `## Backlog (not yet phased)` section at the bottom of `PLAN.md` (mapped to a synthetic `backlog:<task_id>` path) and promoted into a real phase later by a deliberate planning move. Idempotent on `task_id`.
  - **Auto-anchor.** When the first `TaskCreate(plan_path=N.X)` for a brand-new phase arrives and no top-level phase `N` exists, the bridge synthesizes a `## Phase N - <title>` header for you using `metadata.plan_phase` as the title (or `Phase N` as a fallback), with the new task hyphen-separated under it. No more manual "add the phase header, then retry" dance. Intermediate parents (e.g. a missing `1.2` blocking a `1.2.3` insert) still error asking you to create the parent first — auto-creating intermediate nesting would invent structure the user didn't ask for.
  - **Title backfill (burst-safe).** In a `TaskCreate` burst the title-bearing create often isn't first, so the phase can get auto-anchored with the bland `Phase N` placeholder before the good title arrives. Any *later* create for that phase carrying `metadata.plan_phase` backfills the real title — **first real title wins and locks**, so a stray later `plan_phase` won't clobber a deliberate one. A genuine retitle is the explicit `phase-rename N "..."` / `plan_rename_phase`. Order no longer matters: as long as *one* create in the burst supplies `plan_phase`, the phase ends up correctly titled.
  - **What `plan_path` is.** It's the **per-leaf id** of the task — a dotted id like `BT.5`, `AT.2`, or `1.3.2` (phase id + dotted index). It is **not** a path to the `PLAN.md` file. File-shaped input (`PLAN.md`, `docs/PLAN.md`) matches no leaf, so the item lands in `## Backlog`; the hook output flags the file-path shape. The same id is mirrored into `description`, so the bridge recovers it from there if `metadata` is missing — recovering both an existing leaf (re-link) and a **new** leaf whose parent already exists (parent-exists guard; it never fabricates a phase from a description).
  - **Gotcha: load the `TaskCreate` schema first.** `TaskCreate` is a *deferred* tool. If its schema isn't loaded, the `metadata` object can serialize as a string or get dropped before the hook sees it. The bridge is hardened against both (Phase CC): a string-form `metadata` is parsed back into `plan_path`/`plan_phase` rather than hard-failing the create, and a dropped `metadata` is recovered from `description` when it carries the id. Recovery isn't a substitute for the clean path, though — run `ToolSearch select:TaskCreate` before metadata-carrying creates so `metadata` arrives intact. Only a create with no usable `plan_path` *and* no recoverable `description` falls to `## Backlog`.
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
| `leaf_added` | New checkbox in `PLAN.md` with no state mapping | `TaskCreate` to mirror (`## Phase X` headers are skipped — they're document structure, not tasks) |
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

### `phase-new [TITLE] [--activate]`

Phase CE. Create a new phase from the **phase template** in one shot — the high-level "start a standard phase" affordance. Auto-assigns the next uppercase-letter id (see `next-phase`), fills it with the template's beats, and optionally activates it (scoping the working set to it):

```sh
$ claude-plan-bridge phase-new "Payments rework" --activate
claude-plan-bridge: created phase `CF` - `Payments rework` with 5 template task(s) in PLAN.md
  activated `CF` — working set scoped to this phase
```

The built-in default template is **Plan & breakdown → Implement → Tests + docs → Review → Release (bump + tag + push)**. The first beat is where you `phase-breakdown` the Implement / Tests beats; Review and Release are the human gates that ride along as explicit reminders. Templates are a scaffold, not a gate — prune/extend per phase.

**Customize per project** by dropping a `PHASE_TEMPLATE.md` at the repo root: each `- ` bullet line is one beat, in order (a leading `[ ] ` checkbox is tolerated). When present it replaces the default entirely:

```
- Spike
- Build
- Verify
- Ship
```

`phase-new` is the templated counterpart to the lower-level `phase-scaffold` (explicit id + task list) and `phase-add` (empty phase + dependency metadata).

### `phase-breakdown <PARENT-ID> --tasks "A,B,C"`

Phase CE. Break an existing phase or task into auto-numbered child tasks in one atomic write — the "breakdown" half of the plan-&-breakdown beat. Generic and recursive: `<PARENT-ID>` can be a phase id (`CF`) or a task id at any depth (`CF.2`, `CF.2.1`), and new children append after any that already exist, so you can run it repeatedly:

```sh
$ claude-plan-bridge phase-breakdown CF.2 --tasks "codec,scan,CLI"
claude-plan-bridge: broke `CF.2` into 3 task(s): CF.2.1, CF.2.2, CF.2.3
```

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
""`) to clear.

### `promote [<index>] [--title T] [--activate] [--into <id>] [--after <sibling>]`

Promote a **backlog entry** — the planning move that turns a parked idea into
scheduled work. A backlog entry is a top-level `- ` bullet plus everything
beneath it up to the next top-level bullet. Run with no `<index>` to **list**
the entries (1-based) so you can pick one; run with an index to promote it.
The entry is removed from the backlog either way.

**New phase (default).** Without `--into`, the entry becomes a new top-level
phase: its headline is the title (override with `--title`), the rest of the
stanza becomes phase-level prose (**not** tasks — break it down afterward with
`phase-breakdown`), and the phase takes the next `next-phase` id. `--activate`
focuses the working set on it.

**Into an existing phase or task (`--into`).** Files the entry as a **task**
under an existing node instead of minting a phase — the "I already have a home
for this" move. `--into` takes a phase id (`CE`) or a task id at any depth
(`CE.3`). Two reconstruction paths, picked automatically:

- **Faithful subtree** — when the entry's top bullet carries a dotted id
  (`- X.1 - …`, the shape a phase sweep leaves in the backlog), the whole
  subtree is rebuilt as real tasks with its stale ids **remapped** onto the
  target's numbering (`X.1`/`X.1.1` → `CE.4`/`CE.4.1`), the deferral marker
  stripped.
- **Single leaf + prose** — anything else (a one-line idea, a freeform note)
  lands as one task; any body rides along as prose. Break it down later with
  `phase-breakdown`.

`--after <sibling>` positions the new task **immediately after** a named
sibling using an alpha suffix (`CE.1` → `CE.1a`) so nothing renumbers; the
sibling must be a child of the target (with `--after` alone the parent is
derived from it). The promoted leaves surface for `TaskCreate` via reconcile
on your next turn, so the in-session task list picks them up.

```sh
# List, then file entry 2 as a task under phase CE, wedged after CE.1:
$ claude-plan-bridge promote
  1. A loose idea
  2. X.1 - Descoped parent *(deferred from phase `X` on 2026-01-01)*
$ claude-plan-bridge promote 2 --into CE --after CE.1
claude-plan-bridge: promoted backlog entry 2 into `CE` as `CE.1a` - `Descoped parent` in ./PLAN.md
  reconstructed 3 task(s): CE.1a, CE.1a.1, CE.1a.2
  next: reconcile will surface the new leaf(s) for TaskCreate on your next turn
```

### `activate <PHASE>` / `deactivate`

Aliases `plan_activate` / `plan_deactivate` are also accepted, matching the
MCP tool names and the wording the bridge emits in hook output.

Phase 40 focus mode. `activate AS` scopes the bridge's surface to a
single phase:

- **Resume**'s rehydration prompt only loads leaves under that phase
  (other-phase leaves stay in PLAN.md but aren't surfaced to the harness
  TaskList).
- **Reconcile** still emits drift for every phase, but partitions the
  output into `Active phase AS drift:` first, then `Other phases /
  cross-cutting:`.
- **Writeback** is warn-but-allow on cross-phase TaskCreate — the new
  task lands, and the hook output appends a one-line nudge
  (`NOTE: cross-phase TaskCreate — AM.5 is in phase AM, but active
  phase is AS. Run plan_activate AM to switch focus, or plan_deactivate
  to widen.`). Never blocks; matches the bridge's other peripheral
  patterns.
- **Archive** auto-clears the focus when the swept phase is the focused
  one — no orphan focus pointing at a vanished phase.
- **Backlog** notes are cross-cutting and always load on resume.

State persists in `.claude/plan-bridge-state.json` (`active_phase`
field) — survives `/clear` and outlives the Claude session.

`activate` also surfaces any unmet `*(depends on)*` markers in its
response so sequencing constraints land up front:

```sh
$ claude-plan-bridge activate AS
claude-plan-bridge: activated phase `AS`
  NOTE: depends on AR — not yet archived (informational, not a gate)
```

`deactivate` clears the focus. Idempotent — silent no-op when nothing
was active.

### Long-term planning loop

Phase CD. `reconcile` (the `UserPromptSubmit` hook) keeps long-term planning self-sustaining by appending **soft, low-noise nudges** to its output. Each is a hint — the bridge never blocks, and out-of-order work is always fine:

- **Phase-exit auto-advance.** When the focused phase becomes fully resolved, a one-time line suggests the next step: `` Phase CD is complete — `claude-plan-bridge archive CD`, then `plan_activate CE`… ``. It names the next pending phase, but you can activate any phase (or none). Fires once per completion, not every prompt.
- **Working-set focus hint.** When no phase is focused and 2+ phases still have pending work, one gentle line suggests `plan_activate <X>` to scope the TaskList to a single phase. Shows once per unfocused stretch.
- **Status-on-change heartbeat.** A compact `active: CD (5/6 done); next: CB` line, emitted **only when the focused phase's progress changed** since the last turn (deduped via a fingerprint in state). Heads-down, no-change turns stay silent — the heartbeat re-orients you after a tick without becoming noise itself.

Together with focus mode (above), this makes the harness TaskList behave as a small, current *working set* of the active phase while `PLAN.md` holds the durable backlog — so the harness's own "use task tools" / "clean up stale tasks" reminders either land next to real plan state or never have cause to fire.

### `baseline`

Seed the state file with synthetic `baseline:<plan_path>` mappings for every leaf currently in `PLAN.md`. Run once when installing into a project with an existing plan. Idempotent. When Claude later `TaskCreate`s against a baselined path, the baseline mapping is silently replaced.

**Adopting baselined leaves into TaskList.** On a fresh session against a pre-populated `PLAN.md`, the harness's TaskList starts empty even though state has baseline mappings for every leaf. Reconcile surfaces a one-line advisory listing the baseline-only `plan_path`s so the agent sees them. To adopt: `TaskCreate(metadata.plan_path="N.M", subject="...")` — `writeback` dedupes against the existing line (no duplicate inserted) and replaces the `baseline:` mapping with the real `task_id`. Hand-editing the line directly also works; the bridge picks up the change via the next reconcile.

**Leaves with empty ids** (bare-checkbox bullets like `- [ ] no id here`) are *skipped*: they have no stable `plan_path` to key state by, so the bridge can't track them. `baseline` reports the count under "NOTE: skipped N bare-checkbox leaf(s)…" — add a dotted id (`- [ ] 1.2.3 description`) to make a leaf trackable.

### `drop-mapping <target>`

Release a stale state mapping without touching `PLAN.md`. `<target>` matches either the dotted leaf id (`BT.5`) or the raw task id (`68`, `baseline:BT.5`). The recovery path when a leaf was hand-archived or hand-deleted so its mapping no longer points at a live line — the `archive` command already drops mappings for the leaves it moves, and reconcile auto-releases mappings whose leaf landed in `PLAN_ARCHIVE.md`, so reach for this only when `PLAN.md` changed outside the bridge in a way neither covers. Idempotent: a target with no match is a clean no-op.

### `debug on|off`

Toggle raw hook-payload capture for this project. When on, every writeback hook appends the verbatim stdin payload to `.claude/plan-bridge-debug.jsonl` (one `{"ts","raw"}` line) — ground truth for diagnosing whether `metadata.plan_path` actually reaches the bridge. Off by default, omitted from the state file when off, per-project scoped (won't affect other projects sharing the binary), and gitignored. With no argument, prints the current state.

### `serve` (MCP)

Run an MCP server over stdio that exposes typed plan-mutation tools. Useful when you'd rather drive plans through a structured API than through markdown editing.

Tools:

- **Tasks**: `plan_list`, `plan_check`, `plan_uncheck`, `plan_skip`,
  `plan_backlog`, `plan_add`, `plan_rename`
- **Phases**: `plan_new_phase` (templated, auto-id, optional activate),
  `plan_breakdown` (auto-numbered children under any phase/task, recursive),
  `plan_promote` (omit `index` to list backlog entries; promote one into a new
  phase, or into an existing phase/task with `into`/`after`),
  `plan_add_phase` (omit `id` to auto-assign the next letter id; an
  explicit `id` must be uppercase letters), `plan_next_phase` (read-only —
  report the next id), `plan_rename_phase`, `plan_set_phase_deps`
- **Archive**: `plan_archive` (bulk), `plan_phase_exit` (single — accepts
  optional `descope_pending: bool` matching the CLI's `--descope-pending`)
- **Activation focus** (Phase 40): `plan_activate(id)`, `plan_deactivate()`

Errors surface as JSON-RPC error responses (`code: -32603`); unknown
tools and missing args produce clean errors the client can show.
`plan_rename(plan_path, new_subject)` mirrors writeback's
`TaskUpdate(subject=...)` and refreshes the synced baseline so reconcile
is quiet next turn.

Wire it into your MCP client config — for Claude Code, point an `mcpServers` entry at `claude-plan-bridge serve`.

### `init [--cwd PATH] [--force]`

Scaffold a new project. Creates a starter `PLAN.md`, merges the four hooks into `.claude/settings.json` (preserving any other hooks you've configured), and appends the state file + lock file to `.gitignore`. Idempotent. `--force` overwrites an existing `PLAN.md` with the template.

Each installed hook command passes `--cwd "$CLAUDE_PROJECT_DIR"` — the absolute project root Claude Code injects into every hook event. This is both **drift-proof** (if Claude `cd`s into a subdirectory mid-session, the hook still resolves the right `PLAN.md` — the Phase 32 fix) and **checkout-portable**: the generated `settings.json` is byte-identical no matter where the repo lives on disk, so it is safe to commit and survives renames, fresh clones, other machines, and git worktrees. (Earlier versions baked a machine-specific absolute path, which silently broke the bridge the moment the repo moved — see Troubleshooting.)

### `upgrade-hooks [--cwd PATH]`

Re-merge the latest hook set into an existing `.claude/settings.json` without touching `PLAN.md` or `.gitignore`. Use after upgrading the bridge binary on a project installed with an older version — notably anything predating the `SessionStart` hook (added in v0.1.11) or the portable `--cwd "$CLAUDE_PROJECT_DIR"` wiring. Running it on a project whose hooks still carry a baked absolute path (or a stale one left behind by a rename) matches every plan-bridge entry by its `claude-plan-bridge` command prefix, drops them, and re-adds one canonical, portable set — so duplicates collapse and the old path is gone. Idempotent: a no-op when the file is already current. The `reconcile` and `writeback` hooks yell a one-line warning on every fire when the `SessionStart` hook is missing or a hook's `--cwd` isn't drift-proof, so you'll notice quickly without having to remember to check.

## Troubleshooting

**Symptom**: `TaskCreate` / `TaskUpdate` report success, but `PLAN.md` never moves.

**Cause**: a hook's `--cwd` points somewhere the bridge can't find `PLAN.md` — most often a machine-specific absolute path baked by an older bridge that's now stale because the repo was renamed, cloned to a different path, or checked out on another machine. (`.claude/settings.json` is committed, so a baked absolute path travels to every checkout and breaks everywhere but the original.) The `PostToolUse` hook resolves nothing and no-ops.

**Diagnose**: `claude-plan-bridge status` flags any hook whose absolute `--cwd` points at a directory that no longer exists or has no `PLAN.md`. And because the bridge is configured here, `reconcile` / `writeback` now surface a loud, **non-blocking** notice on your next prompt instead of failing silently.

**Fix**: `claude-plan-bridge upgrade-hooks` rewrites every hook to the portable `--cwd "$CLAUDE_PROJECT_DIR"` form; then restart Claude Code so it reloads `settings.json`.

**Historical symptom** (pre-v0.1.20): every prompt blocked with `read ./PLAN.md: No such file or directory`, including innocuous commands like `ls`, because a relative `--cwd` plus a mid-session `cd` made `./PLAN.md` unreadable and the bridge converted the error into `decision: "block"`. The block path is gone, and `--cwd "$CLAUDE_PROJECT_DIR"` makes relative-cwd resolution moot.

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

**Across checkouts:** the state file is intentionally **not** committed — it's machine- and session-local (it references in-session `task_id`s and the originating session id, neither of which survives a clone). What *is* portable is the hook wiring: because each hook uses `--cwd "$CLAUDE_PROJECT_DIR"`, a committed `.claude/settings.json` works unchanged on every checkout. So the fresh-clone flow is: clone (hooks are already wired via the committed `settings.json`), then run `claude-plan-bridge baseline` to seed local state from the existing `PLAN.md` so your first `reconcile` isn't a wall of `LeafAdded`.

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

- **Phases**: `## Phase <ID> - <Title>` (h2 header). New ids use the
  uppercase-letter sequence — `A`..`Z` → `AA`..`AZ` → `BA`..`BZ` → ... (bijective
  base-26, like spreadsheet columns); `claude-plan-bridge next-phase` /
  `plan_next_phase` hand out the next one, scanning PLAN.md + PLAN_ARCHIVE.md so
  swept ids aren't reused. Numeric ids (`1`, `42`) are **legacy** — still parsed,
  but not generated. A legacy `.0` anchor suffix is stripped on read
  (`## Phase 1.0` → phase id `1`). Title optional.
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

### FORMATv2-only (since v1.0.0)

The bridge speaks **FORMATv2 exclusively**: phases are `## Phase X - Title`
headers, tasks are `- [ ] X.N - title` lines beneath them. The legacy v1
"anchor" form — a column-0 checkbox like `- [ ] N.0 Title` that older
versions auto-promoted into a phase — is gone. A column-0 checkbox with no
`## Phase` header above it is now a parse error (`OrphanCheckbox`) rather
than a silently-promoted phase.

Reads are still forgiving about the **separator**: `- [ ] X.5 thing` (bare
space) and `- [ ] X.5 - thing` both parse; the serializer always writes
` - ` (hyphen-space). Bold-wrapped ids (`**X.5**`) and em-dash separators
(` — `) are **no longer recognized** — a line using them parses with an
empty id and the raw text as the title.

**Migrating a pre-1.0 plan:** if your PLAN.md still has v1 anchors, bold
ids, or em-dash separators, run `claude-plan-bridge canonicalize` on the
**0.9.x** release first (it promotes anchors to headers, normalizes
separators, and preserves any v1 phase-state marker as a prose breadcrumb),
then upgrade to 1.0.0. The `canonicalize` verb does not exist in 1.0.0 —
there's nothing left to canonicalize.

<details>
<summary>Full JSON schema (output of <code>parse</code>)</summary>

```jsonc
{
  "preamble": ["raw markdown lines before the first checkbox"],
  "phases": [
    {
      "id": "AI",             // bare phase id (letter-sequence or legacy numeric)
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

Round-trip is **AST-stable**, not byte-stable: `parse(serialize(parse(x))) == parse(x)` holds. The serializer always writes the canonical ` - ` separator, so a hand-edited space-separated line (`X.5 thing`) normalizes to `X.5 - thing` on the next write.

</details>

## Backlog state (`[>]` 🔜)

`[>]` marks a leaf as **deferred from its current phase** — work you've consciously decided not to ship as part of this phase, but want to remember for later. Distinct from `[-]` (won't-do, abandoned) and `[ ]` (still active).

All deferred and unphased work collects in a single **`## Backlog (not yet phased)` section pinned to the bottom of PLAN.md** — a visible, named holding pen, not an auto-discovered `Inbox` sub-list. When the time comes you make the planning move explicitly with **`promote <index>`** (or the `plan_promote` MCP tool): plain, it lifts a backlog entry into a **new phase**; with `--into <phase|task>` it files the entry as a **task under an existing node** (`--after` to position it), reconstructing a descoped subtree faithfully or falling back to a single leaf. The bridge never auto-phases backlog items.

Four ways work lands in Backlog:

1. **`TaskCreate` with no `metadata.plan_path`** — unphased work. The hook records a tracked note (`- **<subject>** — added <date>.`) in the bottom section, mapped to a synthetic `backlog:<task_id>` path so the harness task stays linked. Completing or deleting that task removes the note; a subject rename updates it.
2. **`TaskUpdate(taskId=X, status="deleted")` against a pending leaf** — the hook flips the line to `[>]` and appends a deferral bullet (source plan_path + date) to the bottom section. State mapping is dropped. **Pending leaves are never hard-deleted from PLAN.md via `TaskUpdate`** — this is the safety contract. To actually remove a line, edit PLAN.md by hand or let archive sweep it.
3. **MCP `plan_backlog(plan_path, date?)`** — same effect as (2), callable directly without going through the harness task list. Useful when there's no active mapping (e.g., baselined plan).
4. **CLI `plan-bridge backlog <plan_path>`** — same effect from the shell.

The section is owned as a first-class trailing region: it always serializes below every phase and survives phase-appends without drifting. A `## Backlog` that's still sitting in the preamble (or split across duplicate sections) gets merged down to the bottom on the next backlog-mutating write (`plan-bridge backlog`, a no-`plan_path` TaskCreate, a `TaskUpdate(deleted)`) — routine ticks and renames leave its placement alone. Conservative by design: only the bridge-owned `## Backlog (not yet phased)` heading is touched; operator sections like `### Backlog (rehomed from ...)` or `## Sustainment` are left exactly where they are.

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
