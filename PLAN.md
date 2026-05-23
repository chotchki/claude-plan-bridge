# PLAN: plan-to-task bridge

Spec: see [SPEC.md](./SPEC.md). This plan sequences the implementation.

Phase exit rule (per global CLAUDE.md workflow): every box ticked, unit + e2e tests pass, docs updated. Then summarize and sweep to PLAN_ARCHIVE.md.
- [ ] 40.0 Activation focus (per-project active phase)
  - [ ] 40.1 Add `active_phase: Option<String>` to state file + accessors
  - [ ] 40.2 `plan_activate <PHASE>` / `plan_deactivate` MCP + CLI verbs
  - [ ] 40.3 Resume scopes rehydration prompt to active phase (backlog always loaded)
  - [ ] 40.4 Writeback warns on cross-phase TaskCreate (warn-but-allow); archive auto-clears active_phase
  - [ ] 40.5 Reconcile foregrounds active-phase drift; `plan_activate` notes unmet hard deps
  - [ ] 40.6 Activation e2e tests + docs (SPEC/README update)
  - [ ] 40.7 CLI `plan-bridge phase-scaffold <ID> <title> --tasks "...,...,..."`

## Backlog (not yet phased)

- ~~Refactor checkbox-line grammar to **winnow**~~ — done 2026-05-16. `id_and_title` / `bold_id` / `bare_id` / `id_chars` / `skip_separator` are winnow combinators now; the outer `- [STATE] ` matcher, the code-fence state machine, and the indent-stack tree assembly stay hand-rolled.
- ~~**Won't-do checkbox state (`[-]`)**~~ — done 2026-05-16. `Node.state: NodeState { Pending, Done, WontDo }` across the codebase. Parser accepts `[-]` and `[~]`; serializer emits `[-]`. Archive treats `WontDo` like `Done` (phase can exit). Reconcile emits `LeafStateChanged { old, new }` covering all transitions. Writeback's `TaskUpdate(deleted)` against a `[-]` leaf keeps the line and just drops the state mapping. MCP gained `plan_skip` (and `plan_phase_exit`). 128 tests passing.
- **Baseline subcommand for existing PLAN.md files** — when installing the bridge into a project that already has a populated PLAN.md, the first reconcile emits `LeafAdded` for every existing leaf (the state file is fresh). Loud and annoying on large plans. Mitigation: a `plan-bridge baseline` subcommand that seeds state with synthetic `baseline:<plan_path>` task ids for each current leaf, suppressing the spam. When Claude later TaskCreates against a baselined plan_path, writeback should replace the baseline mapping with the real one rather than duplicate.
- **Drop `fs4` in favor of `std::fs::File::lock`** — `File::lock` / `File::try_lock` / `File::unlock` stabilized in std (this is what drove fs4 to 1.0 — the crate is now a thin polyfill). `src/lock.rs` only uses two fs4 APIs: `try_lock_exclusive` (returns `Result<bool, io::Error>` — true acquired / false would-block / Err real I/O failure) and `FileExt::unlock`. Both map 1:1 onto the std versions (`try_lock` returns the same shape). Plus the test at line 126-137. Migration: delete `use fs4::fs_std::FileExt`, rename `try_lock_exclusive` → `try_lock` and `FileExt::unlock(&h)` → `h.unlock()`, drop `fs4` from Cargo.toml. MSRV bump if necessary — we're on edition 2024 with rustc 1.95 locally, so likely fine. Tighter dependency tree than bumping to fs4 1.x.
