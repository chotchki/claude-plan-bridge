# PLAN: plan-to-task bridge

Spec: see [SPEC.md](./SPEC.md). This plan sequences the implementation.

Phase exit rule (per global CLAUDE.md workflow): every box ticked, unit + e2e tests pass, docs updated. Then summarize and sweep to PLAN_ARCHIVE.md.
- [ ] 36.0 FORMATv2 AST split + bidirectional parser
  - [x] 36.1 Add Phase + Backlog types to AST; refactor Plan struct
  - [x] 36.2 Refactor consumers (archive/reconcile/writeback/resume/mcp/baseline/canonicalize) for new AST
  - [x] 36.3 Parser recognizes `## Phase X - Title *(depends on: Y)*` + `*(prefer after: Z)*` headers
  - [x] 36.4 Parser recognizes `# Backlog (not yet phased)` h1 + nested descoped subtrees
  - [x] 36.5 Parser buckets phase-level prose (under-header lines not attached to a leaf)
  - [ ] 36.6 Bidirectional parse tests + quicksight PLAN.md fixture under tests/fixtures/
- [ ] 37.0 FORMATv2 write path (canonicalize on first write)
  - [ ] 37.1 Serializer emits `## Phase X - Title` headers + optional `*(depends on)*`/`*(prefer after)*`
  - [ ] 37.2 Serializer emits `# Backlog (not yet phased)` h1 (was h2)
  - [ ] 37.3 Serializer emits ` - ` hyphen-space separator on task/subtask lines
  - [ ] 37.4 Serializer emits phase-level prose
  - [ ] 37.5 Canonicalize flips v1 (checkbox phases) → v2 (header phases) on first write
  - [ ] 37.6 Round-trip + v1→v2 flip tests
- [ ] 38.0 Phase verbs + per-phase archive
  - [ ] 38.1 `plan_add_phase(id, title, depends_on=[], after=None)` MCP + CLI
  - [ ] 38.2 `plan_rename_phase(id, new_title)` MCP + CLI
  - [ ] 38.3 `plan_set_phase_deps(id, depends_on=[], prefer_after=[])` MCP + CLI
  - [ ] 38.4 `archive <PHASE>` errors on `[ ]` Pending tasks
  - [ ] 38.5 `archive <PHASE> --descope-pending` moves pending subtrees to `# Backlog`
  - [ ] 38.6 `backlog <plan_path>` preserves subtree under `# Backlog` (was: flat bullet)
  - [ ] 38.7 Auto-anchor synthesizes `## Phase X - <title>` header (was `- [ ] X.0 ...`)
  - [ ] 38.8 Verb tests (add/rename/deps/archive/backlog/auto-anchor)
- [ ] 39.0 Reconcile dep surfacing + dogfood + cut
  - [ ] 39.1 Reconcile surfaces both `*(depends on)*` and `*(prefer after)*` in additionalContext
  - [ ] 39.2 e2e: parse + canonicalize a copy of ../quicksight/PLAN.md without losing content
  - [ ] 39.3 SPEC.md + README.md + CLAUDE.md hint docs updates for FORMATv2
  - [ ] 39.4 Cut release (version bump, RELEASE_NOTES, tag, push)

## Backlog (not yet phased)

- ~~Refactor checkbox-line grammar to **winnow**~~ — done 2026-05-16. `id_and_title` / `bold_id` / `bare_id` / `id_chars` / `skip_separator` are winnow combinators now; the outer `- [STATE] ` matcher, the code-fence state machine, and the indent-stack tree assembly stay hand-rolled.
- ~~**Won't-do checkbox state (`[-]`)**~~ — done 2026-05-16. `Node.state: NodeState { Pending, Done, WontDo }` across the codebase. Parser accepts `[-]` and `[~]`; serializer emits `[-]`. Archive treats `WontDo` like `Done` (phase can exit). Reconcile emits `LeafStateChanged { old, new }` covering all transitions. Writeback's `TaskUpdate(deleted)` against a `[-]` leaf keeps the line and just drops the state mapping. MCP gained `plan_skip` (and `plan_phase_exit`). 128 tests passing.
- **Baseline subcommand for existing PLAN.md files** — when installing the bridge into a project that already has a populated PLAN.md, the first reconcile emits `LeafAdded` for every existing leaf (the state file is fresh). Loud and annoying on large plans. Mitigation: a `plan-bridge baseline` subcommand that seeds state with synthetic `baseline:<plan_path>` task ids for each current leaf, suppressing the spam. When Claude later TaskCreates against a baselined plan_path, writeback should replace the baseline mapping with the real one rather than duplicate.
- **Drop `fs4` in favor of `std::fs::File::lock`** — `File::lock` / `File::try_lock` / `File::unlock` stabilized in std (this is what drove fs4 to 1.0 — the crate is now a thin polyfill). `src/lock.rs` only uses two fs4 APIs: `try_lock_exclusive` (returns `Result<bool, io::Error>` — true acquired / false would-block / Err real I/O failure) and `FileExt::unlock`. Both map 1:1 onto the std versions (`try_lock` returns the same shape). Plus the test at line 126-137. Migration: delete `use fs4::fs_std::FileExt`, rename `try_lock_exclusive` → `try_lock` and `FileExt::unlock(&h)` → `h.unlock()`, drop `fs4` from Cargo.toml. MSRV bump if necessary — we're on edition 2024 with rustc 1.95 locally, so likely fine. Tighter dependency tree than bumping to fs4 1.x.
