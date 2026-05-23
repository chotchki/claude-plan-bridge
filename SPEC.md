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

## PLAN.md schema (FORMATv2, Phase 36–39)

Canonical form:

```
## Phase AI - Studio dogfood *(depends on: AH)* *(prefer after: AG)*

Intro paragraph at the phase level — context that belongs to the phase
as a whole, not to any particular task.

- [ ] AI.0 - Lock decisions
- [ ] AI.1 - Implement driver
  - [x] AI.1.0 - protocol
  - [ ] AI.1.1 - transport
    Indented prose under the subtask — task-level, stays with AI.1.1.

Trailing prose — also phase-level, sweeps with the phase to PLAN_ARCHIVE.md.

# Backlog (not yet phased)

- **Drop the fs4 crate** — added 2026-05-22.
- AI.2 - descoped from phase `AI` on 2026-05-22
```

### Structure

- **Phases** are markdown h2 headers: `## Phase <ID> - <Title>`. The id is
  a bare alphanumeric prefix (`AI`, `AS`, `AO`, `1`, `1.0`). The title is
  optional; the separator is hyphen-space.
- **Tasks** under a phase sit at column 0: `- [<state>] <PHASE>.<N> -
  <title>`. Subtasks indent two spaces per level: `<PHASE>.<N>.<K>`.
- **Two checkbox separators tolerated on read** (` - `, ` — `, plain space);
  canonical write is ` - ` hyphen-space, applied by the `canonicalize` verb.
- **Four checkbox states**: `[ ]` pending, `[x]` done, `[-]` won't-do,
  `[>]` backlog. Parser also accepts `[~]` as an alias for won't-do.
- **Archive treats `[x]`, `[-]`, `[>]` equivalently** — all three count as
  "resolved" for the phase-exit completeness check.
- **Phases don't have state** under FORMATv2 (header form has no checkbox).
  v1 plans that had `- [x] N.0 Title` anchors preserve the validation
  marker as a prose breadcrumb on canonicalize: `*(was marked [x] in v1 —
  archive to make it official)*`.

### Phase dependency markers (informational)

- `*(depends on: AB, AC)*` — hard sequencing hint. Reconcile surfaces it
  loudly ("Phase AS depends on AR — not yet archived") when any listed
  phase is still in `plan.phases`.
- `*(prefer after: AB)*` — soft sequencing hint. Reconcile surfaces it
  more gently ("Phase AM prefers AI landed first — soft hint").
- Both are informational only. The bridge never blocks an operation
  based on these markers; the agent decides what to do.

### Phase-level prose

Lines under a `## Phase X` header that are NOT indented under a task
belong to the phase itself — they sweep with the phase to PLAN_ARCHIVE.md
on archive. Indented prose under a task (` ` or more leading spaces) stays
with that task as today.

### Backlog section (`# Backlog (not yet phased)`)

- Canonical heading is h1 `# Backlog (not yet phased)` (FORMATv2). Legacy
  h2 `## Backlog` is accepted on read; `canonicalize` flips it to h1.
- Lives as a first-class trailing region pinned to the bottom of PLAN.md.
- Two entry shapes:
  - Flat notes: `- **Subject** — added <date>.` (unphased work captured by
    no-`plan_path` TaskCreate)
  - Nested descoped subtrees:
    ```
    - AI.2 - descoped from phase `AI` on 2026-05-22
      - AI.2.1 - subtask carried along
        Prose continuation under the descoped subtask.
    ```
- Survives phase archive sweeps — the durable record of deferred work.

### Conservative format dispatch

Routine writes (TaskCreate, TaskUpdate, archive sweep) preserve the
on-disk format per phase. A v1 anchor stays a v1 anchor; a v2 header
stays a v2 header. The single operation that flips everything to
FORMATv2 canonical is `plan-bridge canonicalize` — explicit, idempotent,
itemizes every change in its report.

### Backlog state (`[>]`) semantics

`[>]` marks a leaf that was real planned work but is being **consciously deferred from its current phase** — distinct from `[-]` (won't-do, abandoned) and `[ ]` (still active). Use it when you want to ship the phase without dragging unfinished work along, but the deferred item is worth remembering for later.

- **On-disk marker**: `[>]` in PLAN.md (canonical). Parser accepts only `[>]`.
- **Human-facing output**: the bridge renders all four states with emoji in `additionalContext`, status output, reconcile drift, and hook prompts: ✅ done, ❌ won't-do, 🔜 backlog, ⬜ pending. The raw bracket form stays in PLAN.md itself; the emoji translation is presentation-only.
- **Backlog-section promotion**: when a leaf transitions Pending → Backlog (via `TaskUpdate(deleted)`, the `plan_backlog` MCP tool, or the `backlog` CLI subcommand), the bridge appends a bullet under `## Backlog (not yet phased)` recording the title, the source `plan_path`, and the date. This bullet outlives the phase sweep.
- **Archive equivalence**: `[>]` doesn't block phase exit. The phase-fully-done check treats it as resolved, and the entire phase (including `[>]` lines) is swept to PLAN_ARCHIVE.md. The Backlog section in PLAN.md remains as the durable record of deferred work.
- **Reconcile**: Pending ↔ Backlog transitions surface as `LeafStateChanged { old, new }`, same as other state flips. External edits to PLAN.md that introduce `[>]` reconcile on the next prompt.

### TaskUpdate(deleted) flow (post-Phase 28)

Calling `TaskUpdate(status="deleted")` against a mapped leaf has state-dependent behavior:

| Current PLAN.md state | What `TaskUpdate(deleted)` does |
|---|---|
| `[ ]` Pending | Flips the line to `[>]`, appends a bullet under `## Backlog (not yet phased)`, drops the state mapping. **Never hard-deletes a pending leaf.** |
| `[-]` Won't-do | Keeps the line, drops the state mapping. (Same as pre-Phase 28.) |
| `[x]` Done | Keeps the line, drops the state mapping. |
| `[>]` Backlog | No-op on the line, drops the state mapping if present. |

Rationale: hard-deletes via TaskUpdate were error-prone — an accidental "remove task" click in the harness UI silently deleted plan content. Backlogging instead preserves the work and forces explicit `Edit`/`Write` against PLAN.md for true removal.

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
- Non-leaf nodes (phases, intermediate tasks) live only in PLAN.md and are **never** represented as TaskCreate items. The bridge does NOT auto-tick parents when all children complete — parent ticking is a deliberate validation step ("did the children meet this phase's goal?") that the user/agent owns. Archive operates on subtree state, so leaving a parent box unticked doesn't block phase sweeping; the parent box ticked vs. unticked is signal about whether the phase was *validated*, not whether the work was *done*.
- **Top-level phase anchors (`N.0` form) are not tracked tasks either.** They're document-structural — the parent for a phase's children. The bridge does NOT emit `LeafAdded` drift for them (Phase 31.5): a manually-edited or auto-synthesized `- [ ] 10.0 Phase 10` won't nag the agent to `TaskCreate` it. Phases stay user/agent-owned by convention.
- TaskCreate fields the bridge cares about:
  - `subject`: leaf title (without the `N.M.K` prefix). Subjects are normalized on the way in — stray `\"` sequences (an over-escape pattern markdown doesn't need) get stripped to plain `"` before storage so PLAN.md never holds the ugly form.
  - `metadata.plan_path`: dotted address (e.g., `"1.2.3"`).
  - `metadata.plan_phase`: optional human-readable phase title. Used as the title when the bridge auto-synthesizes a missing top-level anchor (Phase 31.2). Fallback when absent: `Phase N` (e.g. `Phase 10` for a `10.0` synthesis).
  - `status`: `pending` for `[ ]`, `completed` for `[x]`. (`in_progress` is set by Claude during active work; it doesn't reflect to PLAN.md until completion.)
- TaskCreate fields the bridge does **not** read (Claude may set them freely for harness UI ergonomics):
  - `description` — recommended value is the `plan_path` itself, since the bridge ignores it and using the plan_path keeps the harness UI from showing the same text as `subject` twice.
- `blocks` / `blockedBy` dependencies are **not** auto-populated. PLAN.md only encodes parent-child nesting and document order, not explicit "this depends on that" — inferring dependencies from nesting alone would be guessing. Adding explicit dependency syntax to PLAN.md is a possible future direction, not a v1 concern.

### Anchor handling on first-child insert (Phase 31)

When `TaskCreate(plan_path=N.X)` arrives for a phase that doesn't yet have an `N.0` anchor:

| State of `N.0` | Bridge behavior |
|---|---|
| Missing entirely | Synthesizes `- [ ] N.0 <plan_phase or "Phase N">` at top level, then inserts `N.X` under it. Hook output announces the auto-creation. |
| Exists at top level | Inserts `N.X` as a child of the existing anchor (the canonical path). |
| Exists nested under another phase | Refuses with a clear "move it to column 0" error. Refusing is deliberate — silently parenting children under a misplaced anchor was the Phase 31.3 shakeout bug (10.1–10.13 landed under `6.13 Staging / beta deployment`). |
| Lives only as a `### Phase N — Title` markdown header | Falls back to `standardize_to_canonical()` to promote the header into a checkbox, then inserts. (Pre-Phase-31 fallback, preserved.) |

Missing **intermediate** parents (e.g. `1.2` blocking a `1.2.3` insert) still error with the canonicalize hint — auto-creating non-anchor structure would invent nesting the user didn't ask for.

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
| `PostToolUse` | `TaskUpdate` | `plan-bridge writeback --event update` | Toggle `[ ]`/`[x]` based on status. `deleted` flips Pending → `[>]` (backlog) and promotes the leaf to the `## Backlog (not yet phased)` section; for non-Pending leaves, drops the mapping and leaves the line alone. See "TaskUpdate(deleted) flow" above. |
| `PostToolUse` | `Edit\|Write` | `plan-bridge reconcile` (only when path ends in `PLAN.md`) | Re-derive task list when Claude edits PLAN.md directly. |

Hook output uses `hookSpecificOutput.additionalContext` to feed PLAN.md state back into Claude's next turn. The bridge **never** emits `decision: "block"` — see Hook contract below.

Hook commands written by `init` / `upgrade-hooks` carry an absolute `--cwd <project root>` so the subprocess CWD (which can drift if Claude `cd`s mid-session) doesn't determine where the bridge looks for PLAN.md. Legacy installs without `--cwd` are auto-detected and nagged via `additionalContext` until the user runs `claude-plan-bridge upgrade-hooks` (Phase 32).

## Hook contract

- Hooks receive Claude Code's standard hook JSON via stdin (`session_id`, `cwd`, `hook_event_name`, `tool_name`, `tool_input`, `tool_response`).
- Hooks emit JSON via stdout. v1 fields used:
  - `hookSpecificOutput.additionalContext`: free-form text shown to Claude before its next response.
- **No `decision: "block"`, ever.** The bridge is a peripheral that decorates context; it must not gate the user's ability to submit prompts. Missing PLAN.md → silent no-op. Handler errors → non-blocking `additionalContext` carrying the error text. (Phase 32: an adopter session imploded with every prompt — including `ls` — blocked because an inherited wrong cwd made `./PLAN.md` unreadable, and the bridge converted the I/O error into `decision: "block"`. Never again.)
- Hooks **never** call Claude tools directly. They emit guidance; Claude executes.

## CLI surface (FORMATv2)

```
plan-bridge parse [PATH]
plan-bridge writeback --event <create|update>            # PostToolUse hook handler
plan-bridge reconcile                                     # UserPromptSubmit hook handler
plan-bridge resume                                        # SessionStart hook handler
plan-bridge archive [<PHASE>] [--descope-pending]         # bulk sweep / per-phase
plan-bridge canonicalize [--dry-run]                      # explicit v1 → v2 flip
plan-bridge backlog <plan_path>                           # defer (subtree-preserving)
plan-bridge baseline                                      # seed state on install
plan-bridge init [--force]                                # scaffold project
plan-bridge upgrade-hooks                                 # re-merge hook config
plan-bridge status                                        # health check
plan-bridge phase-add <ID> [TITLE] [--depends-on X,Y] [--prefer-after A,B] [--after <ID>]
plan-bridge phase-rename <ID> <new-title>
plan-bridge phase-deps <ID> [--depends-on X,Y] [--prefer-after A,B]
```

All project-scoped commands accept `--cwd <PATH>` (project root) and
`--plan <PATH>` (explicit PLAN.md override).

### Archive variants (Phase 38.4 / 38.5)

- `plan-bridge archive` (no arg) — bulk sweep every fully-complete phase.
  Silent skip on phases with pending leaves.
- `plan-bridge archive <PHASE>` — per-phase archive. Errors loudly if the
  named phase has any `[ ]` Pending leaves. Error message points at the
  `--descope-pending` escape hatch.
- `plan-bridge archive <PHASE> --descope-pending` — move pending leaves
  into `# Backlog (not yet phased)` as `- <id> - descoped from phase
  <PHASE> on <date>` notes, then archive the now-fully-resolved phase.

### Phase verbs (Phase 38.1–38.3, 38.7)

- `phase-add` creates a `## Phase X - Title` header with optional dep
  markers and positional `--after` insertion (defaults to id-sort order).
  TaskCreate's auto-anchor still handles the common "just start typing
  tasks" path (and now synthesizes a v2 header, not a v1 anchor).
- `phase-rename` rewrites a phase title. Refuses task ids loudly.
- `phase-deps` replaces `depends_on` / `prefer_after` lists on a phase.
  Either field is independently settable; empty array clears. Flips a v1
  anchor to HeaderV2 form so the markers can render.

## MCP surface

Same binary, `plan-bridge serve` entry point. Exposes the full verb set
above as JSON-RPC tools:

- `plan_list`, `plan_check`, `plan_uncheck`, `plan_skip`, `plan_backlog`
- `plan_add` (leaf), `plan_add_phase`, `plan_rename` (any), `plan_rename_phase`
- `plan_set_phase_deps`
- `plan_archive` (bulk), `plan_phase_exit` (single, with optional
  `descope_pending: bool`)

Useful when TaskCreate's flat model is too lossy — e.g., explicit
reordering, phase-exit gates, dep edits, deferring without going through
`TaskUpdate(deleted)`. Operates on PLAN.md natively without going through
TaskCreate at all.

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
  - Leaves added, removed, or moved. (Top-level `N.0` phase anchors are excluded from the `LeafAdded` channel — they're not tracked tasks; see Phase 31.5.)
  - Box-state flips (`[ ]` ↔ `[x]`).
  - Leaf title edits.
  - **Sub-leaf annotations** — any non-checkbox bullet, indented note, or trailing prose attached to a leaf. Common case: user adds context under an existing item between turns and tells Claude "go look." Reconcile must surface the new annotation text, not just structural diffs. Column-0 markdown section headers (`## Phase 10 — …` between phase blocks) are filtered out of the diff: the parser attaches them to whichever leaf was open, but they're document-structural dividers, not leaf-scoped content (Phase 31.4).
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
- `TaskUpdate(taskId=X, status="deleted")` against a `[ ]` leaf flips it to `[>]`, appends a bullet under `## Backlog (not yet phased)` referencing the source `plan_path` and date, and drops the state mapping. The leaf does NOT get hard-deleted from PLAN.md.
- `plan-bridge archive` (or `plan_phase_exit` MCP) succeeds on a phase whose remaining leaves are a mix of `[x]`, `[-]`, and `[>]`; the entire phase is moved to `PLAN_ARCHIVE.md` and the `## Backlog` entries promoted earlier remain in PLAN.md.
