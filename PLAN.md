# PLAN: plan-to-task bridge

Spec: see [SPEC.md](./SPEC.md). This plan sequences the implementation.

Phase exit rule (per global CLAUDE.md workflow): every box ticked, unit + e2e tests pass, docs updated. Then summarize and sweep to PLAN_ARCHIVE.md.

## Backlog (not yet phased)

- ~~Refactor checkbox-line grammar to **winnow**~~ — done 2026-05-16. `id_and_title` / `bold_id` / `bare_id` / `id_chars` / `skip_separator` are winnow combinators now; the outer `- [STATE] ` matcher, the code-fence state machine, and the indent-stack tree assembly stay hand-rolled.
- ~~**Won't-do checkbox state (`[-]`)**~~ — done 2026-05-16. `Node.state: NodeState { Pending, Done, WontDo }` across the codebase. Parser accepts `[-]` and `[~]`; serializer emits `[-]`. Archive treats `WontDo` like `Done` (phase can exit). Reconcile emits `LeafStateChanged { old, new }` covering all transitions. Writeback's `TaskUpdate(deleted)` against a `[-]` leaf keeps the line and just drops the state mapping. MCP gained `plan_skip` (and `plan_phase_exit`). 128 tests passing.
- **Baseline subcommand for existing PLAN.md files** — when installing the bridge into a project that already has a populated PLAN.md, the first reconcile emits `LeafAdded` for every existing leaf (the state file is fresh). Loud and annoying on large plans. Mitigation: a `plan-bridge baseline` subcommand that seeds state with synthetic `baseline:<plan_path>` task ids for each current leaf, suppressing the spam. When Claude later TaskCreates against a baselined plan_path, writeback should replace the baseline mapping with the real one rather than duplicate.

---

- [ ] 7.0 Archive ordering: append newest sections at bottom (chronological-ascending)
  - [ ] 7.1 Flip `src/archive.rs` to append the new dated section at the bottom of PLAN_ARCHIVE.md instead of prepending
  - [ ] 7.2 Update archive unit tests for new ordering + add a regression test that an existing PLAN_ARCHIVE.md is appended-to (existing content stays at top, new section at bottom)
  - [ ] 7.3 README: change "newest section prepended at the top" to "newest section appended at the bottom" in the `plan-bridge archive` section
  - [ ] 7.4 One-time fixup of this repo's PLAN_ARCHIVE.md: move today's Phase 5/6 section below today's Phase 1–4 section so the file reads chronological-ascending
  - [ ] 7.5 Phase 7 exit — cargo test green; README + PLAN_ARCHIVE.md consistent with new ordering
- [ ] 8.0 Serialize concurrent writebacks with a file lock — and surface lock failure as a loud hook block, never silent data loss
- [x] 9.0 Reconcile: stop emitting LeafRemoved when a tracked node becomes a parent (children added)
