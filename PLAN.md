# PLAN: plan-to-task bridge

Spec: see [SPEC.md](./SPEC.md). This plan sequences the implementation.

Phase exit rule (per global CLAUDE.md workflow): every box ticked, unit + e2e tests pass, docs updated. Then summarize and sweep to PLAN_ARCHIVE.md.

## Backlog (not yet phased)

- ~~Refactor checkbox-line grammar to **winnow**~~ — done 2026-05-16. `id_and_title` / `bold_id` / `bare_id` / `id_chars` / `skip_separator` are winnow combinators now; the outer `- [STATE] ` matcher, the code-fence state machine, and the indent-stack tree assembly stay hand-rolled.
- ~~**Won't-do checkbox state (`[-]`)**~~ — done 2026-05-16. `Node.state: NodeState { Pending, Done, WontDo }` across the codebase. Parser accepts `[-]` and `[~]`; serializer emits `[-]`. Archive treats `WontDo` like `Done` (phase can exit). Reconcile emits `LeafStateChanged { old, new }` covering all transitions. Writeback's `TaskUpdate(deleted)` against a `[-]` leaf keeps the line and just drops the state mapping. MCP gained `plan_skip` (and `plan_phase_exit`). 128 tests passing.
- **Baseline subcommand for existing PLAN.md files** — when installing the bridge into a project that already has a populated PLAN.md, the first reconcile emits `LeafAdded` for every existing leaf (the state file is fresh). Loud and annoying on large plans. Mitigation: a `plan-bridge baseline` subcommand that seeds state with synthetic `baseline:<plan_path>` task ids for each current leaf, suppressing the spam. When Claude later TaskCreates against a baselined plan_path, writeback should replace the baseline mapping with the real one rather than duplicate.

---

- [ ] 24.0 Phase 24 — Restart-test README note + parent-inference fix
  - [ ] 24.1 Add failing test for bare-N phase root
  - [ ] 24.2 Fix parent-inference to accept bare N
  - [ ] 24.3 Note restart-test cycle in README
  - [ ] 24.4 Phase 24 exit — tests, version bump, archive

- [ ] 25.0 Phase 25 — Session-restart rehydration via SessionStart hook
  - [x] 25.1 Add `plan-bridge resume` subcommand
  - [x] 25.2 Wire SessionStart hook into installer + add `upgrade-hooks` subcommand
  - [x] 25.3 Writeback: dedup mappings by plan_path on TaskCreate
  - [x] 25.4 Reconcile/writeback warn loudly when SessionStart hook is missing
  - [ ] 25.5 e2e test: full restart cycle
  - [ ] 25.6 README: document session-restart behavior
  - [ ] 25.7 Phase 25 exit — tests, version bump, archive
