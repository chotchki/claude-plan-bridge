# PLAN: plan-to-task bridge

Spec: see [SPEC.md](./SPEC.md). This plan sequences the implementation.

Phase exit rule (per global CLAUDE.md workflow): every box ticked, unit + e2e tests pass, docs updated. Then summarize and sweep to PLAN_ARCHIVE.md.
## Phase CB - Reliable phase titling on TaskCreate bursts
- [ ] CB.1 - Design spike: reliable phase titling under TaskCreate bursts
- [ ] CB.2 - Plan the titling fix fully
- [ ] CB.3 - Implement reliable phase titling
- [ ] CB.4 - Tests + docs + phase exit (titling)
## Phase CC - Harden the metadata.plan_path deferred-schema path
- [ ] CC.1 - Design spike: harden the metadata.plan_path / deferred-schema path
- [ ] CC.2 - Plan the deferred-schema hardening fully
- [ ] CC.3 - Implement deferred-schema hardening
- [ ] CC.4 - Tests + docs + phase exit (deferred-schema)

## Backlog (not yet phased)

- **Out of scope — session-feedback items the bridge can't / shouldn't own** (recorded 2026-05-28 so they don't resurface as confusion):
  - *"Task tools haven't been used recently" reminder is noise* — **not bridge-emitted.** It's a Claude Code harness built-in (no such string anywhere in `src/`); it fires on turns with no task-tool call. The bridge has no lever to silence a reminder it doesn't produce.
  - *First-class phase/umbrella parent type with child progress (9/10 done)* — that's the **harness task model**, not the bridge. Also cuts against `parent-tick-is-validation`: the bridge deliberately does not rehydrate parents as tasks (`resume.rs` Phase 27.1), so progress-bar parents would contradict the design.
  - *Reconcile one-turn lag after a hand-edit* — **wontfix, by design.** Reconcile runs on `UserPromptSubmit`; eventual consistency is the intended tradeoff (the feedback author agreed). Tightening it would mean reconciling on every PLAN.md write, which the lock/atomic-write model intentionally avoids.
  - (Note: a third feedback item — render `[3.3.2]` inline in TaskList via `subject="[N.M] title"` — was declined. It reverses the deliberate choice at `resume.rs:186` to keep plan_path in `metadata`/`description` and out of the title for a clean harness UI.)
- ~~Refactor checkbox-line grammar to **winnow**~~ — done 2026-05-16. `id_and_title` / `bold_id` / `bare_id` / `id_chars` / `skip_separator` are winnow combinators now; the outer `- [STATE] ` matcher, the code-fence state machine, and the indent-stack tree assembly stay hand-rolled.
- ~~**Won't-do checkbox state (`[-]`)**~~ — done 2026-05-16. `Node.state: NodeState { Pending, Done, WontDo }` across the codebase. Parser accepts `[-]` and `[~]`; serializer emits `[-]`. Archive treats `WontDo` like `Done` (phase can exit). Reconcile emits `LeafStateChanged { old, new }` covering all transitions. Writeback's `TaskUpdate(deleted)` against a `[-]` leaf keeps the line and just drops the state mapping. MCP gained `plan_skip` (and `plan_phase_exit`). 128 tests passing.
- **Baseline subcommand for existing PLAN.md files** — when installing the bridge into a project that already has a populated PLAN.md, the first reconcile emits `LeafAdded` for every existing leaf (the state file is fresh). Loud and annoying on large plans. Mitigation: a `plan-bridge baseline` subcommand that seeds state with synthetic `baseline:<plan_path>` task ids for each current leaf, suppressing the spam. When Claude later TaskCreates against a baselined plan_path, writeback should replace the baseline mapping with the real one rather than duplicate.
- **Drop `fs4` in favor of `std::fs::File::lock`** — `File::lock` / `File::try_lock` / `File::unlock` stabilized in std (this is what drove fs4 to 1.0 — the crate is now a thin polyfill). `src/lock.rs` only uses two fs4 APIs: `try_lock_exclusive` (returns `Result<bool, io::Error>` — true acquired / false would-block / Err real I/O failure) and `FileExt::unlock`. Both map 1:1 onto the std versions (`try_lock` returns the same shape). Plus the test at line 126-137. Migration: delete `use fs4::fs_std::FileExt`, rename `try_lock_exclusive` → `try_lock` and `FileExt::unlock(&h)` → `h.unlock()`, drop `fs4` from Cargo.toml. MSRV bump if necessary — we're on edition 2024 with rustc 1.95 locally, so likely fine. Tighter dependency tree than bumping to fs4 1.x.
- **Fix ast.rs extract_backlog_from_annotation_list doctest (pre-existing): the indented em-dash example block parses as Rust and fails `cargo test --doc`; fence it as ```text (or mark `ignore`)** — added 2026-05-29.
