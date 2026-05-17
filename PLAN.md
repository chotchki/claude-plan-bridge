# PLAN: plan-to-task bridge

Spec: see [SPEC.md](./SPEC.md). This plan sequences the implementation.

Phase exit rule (per global CLAUDE.md workflow): every box ticked, unit + e2e tests pass, docs updated. Then summarize and sweep to PLAN_ARCHIVE.md.

## Backlog (not yet phased)

- ~~Refactor checkbox-line grammar to **winnow**~~ — done 2026-05-16. `id_and_title` / `bold_id` / `bare_id` / `id_chars` / `skip_separator` are winnow combinators now; the outer `- [STATE] ` matcher, the code-fence state machine, and the indent-stack tree assembly stay hand-rolled.
- ~~**Won't-do checkbox state (`[-]`)**~~ — done 2026-05-16. `Node.state: NodeState { Pending, Done, WontDo }` across the codebase. Parser accepts `[-]` and `[~]`; serializer emits `[-]`. Archive treats `WontDo` like `Done` (phase can exit). Reconcile emits `LeafStateChanged { old, new }` covering all transitions. Writeback's `TaskUpdate(deleted)` against a `[-]` leaf keeps the line and just drops the state mapping. MCP gained `plan_skip` (and `plan_phase_exit`). 128 tests passing.
- **Baseline subcommand for existing PLAN.md files** — when installing the bridge into a project that already has a populated PLAN.md, the first reconcile emits `LeafAdded` for every existing leaf (the state file is fresh). Loud and annoying on large plans. Mitigation: a `plan-bridge baseline` subcommand that seeds state with synthetic `baseline:<plan_path>` task ids for each current leaf, suppressing the spam. When Claude later TaskCreates against a baselined plan_path, writeback should replace the baseline mapping with the real one rather than duplicate.

---

- [x] 17.0 CLI consistency: every project-scoped subcommand accepts `--cwd` (init's convention)
  - [x] 17.1 Refactor: shared ProjectArgs (--cwd + --plan) across parse/writeback/reconcile/archive/serve/baseline
  - [x] 17.2 README: document the `--cwd` / `--plan` convention in CLI reference
  - [ ] 17.3 Phase 17 exit + bump v0.1.5 + tag + install
- [x] 18.0 Empty-id leaves: stop colliding under `baseline:` key, stop emitting false drift on every reconcile
  - [x] 18.1 baseline: skip empty-id leaves entirely (no state entry, no synthetic key)
  - [x] 18.2 reconcile: skip empty-id leaves in the diff walk (no LeafAdded / LeafTitleChanged etc.)
  - [x] 18.3 README: document that empty-id leaves are untracked; explain `parse` phases vs `baseline` leaves counts
