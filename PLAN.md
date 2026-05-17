# PLAN: plan-to-task bridge

Spec: see [SPEC.md](./SPEC.md). This plan sequences the implementation.

Phase exit rule (per global CLAUDE.md workflow): every box ticked, unit + e2e tests pass, docs updated. Then summarize and sweep to PLAN_ARCHIVE.md.

## Backlog (not yet phased)

- ~~Refactor checkbox-line grammar to **winnow**~~ — done 2026-05-16. `id_and_title` / `bold_id` / `bare_id` / `id_chars` / `skip_separator` are winnow combinators now; the outer `- [STATE] ` matcher, the code-fence state machine, and the indent-stack tree assembly stay hand-rolled.
- ~~**Won't-do checkbox state (`[-]`)**~~ — done 2026-05-16. `Node.state: NodeState { Pending, Done, WontDo }` across the codebase. Parser accepts `[-]` and `[~]`; serializer emits `[-]`. Archive treats `WontDo` like `Done` (phase can exit). Reconcile emits `LeafStateChanged { old, new }` covering all transitions. Writeback's `TaskUpdate(deleted)` against a `[-]` leaf keeps the line and just drops the state mapping. MCP gained `plan_skip` (and `plan_phase_exit`). 128 tests passing.
- **Baseline subcommand for existing PLAN.md files** — when installing the bridge into a project that already has a populated PLAN.md, the first reconcile emits `LeafAdded` for every existing leaf (the state file is fresh). Loud and annoying on large plans. Mitigation: a `plan-bridge baseline` subcommand that seeds state with synthetic `baseline:<plan_path>` task ids for each current leaf, suppressing the spam. When Claude later TaskCreates against a baselined plan_path, writeback should replace the baseline mapping with the real one rather than duplicate.

---

- [x] 11.0 Roll out the release: global CLAUDE.md guidance + dogfood crates.io install
  - [x] 11.1 Install claude-plan-bridge v0.1.0 from crates.io to replace the local `cargo install --path .` build (dogfood the published version)
- [x] 12.0 writeback: support TaskUpdate(subject=...) — rewrite PLAN.md title + last_synced_title without requiring a status change
  - [x] 12.1 writeback_update: rewrite title from `input.subject` (impl + tests, independent of status)
  - [x] 12.2 README: document TaskUpdate(subject) writeback path in the writeback section
  - [x] 12.3 Phase 12 exit — cargo test green; local install picked up; README reflects new behavior
