# PLAN: plan-to-task bridge

Spec: see [SPEC.md](./SPEC.md). This plan sequences the implementation.

Phase exit rule (per global CLAUDE.md workflow): every box ticked, unit + e2e tests pass, docs updated. Then summarize and sweep to PLAN_ARCHIVE.md.

## Phase BY - Bridge feedback: plan_path ergonomics, stale-mapping cleanup, naming

Source: session feedback (2026-05-30) on the dogfood bridge. Items keyed to the 7 reported points plus live finds from acting on them.

**ROOT CAUSE (confirmed via live payload capture, BY.13):** the harness forwards `metadata` fully intact — `tool_input.metadata.plan_path`, `plan_phase`, even arbitrary keys all survive — and `description` too (`tool_response` is `{"task":{"id":"<n>"}}`, id a string). The bridge honors all of it. The early "everything landed in `## Backlog`" failures were **`TaskCreate` schema eviction**: `TaskCreate` is a *deferred* tool, and whenever its schema isn't loaded the model serializes `metadata` as a string → the client parser rejects it (`metadata expected as record, provided as string`) → the hook sees no `plan_path` → correct Backlog landing. Not a bridge bug; very likely also what hit the reporter (#1/#2/#5). The documented plan_path-shape confusion is real and still worth fixing, but it was not the mechanism here. Phase BY was hand-authored + `baseline`d because the schema kept getting evicted mid-session.

- [ ] BY.1 - Document plan_path shape (per-leaf id like `BT.5`, not a file path) in global CLAUDE.md + README; note plan_path also rides in `description` per the resume convention [feedback #1]
- [x] BY.2 - writeback: when a create lands in Backlog with no plan_path, say so loudly + remind that `TaskCreate`'s schema must be loaded for `metadata` to survive; detect file-path-shaped input and name the per-leaf shape [feedback #2, #5]
- [x] BY.3 - writeback: say "attached to existing BY.N" instead of "added" when the leaf already exists [feedback #7]
- [x] BY.4 - main.rs: add `plan_activate` / `plan_deactivate` CLI aliases for `activate` / `deactivate` [feedback #4]
- [x] BY.5 - archive: sweep state mappings for archived leaves atomically [feedback #3]
- [ ] BY.6 - add `drop-mapping <plan_path>` CLI verb to release a stale mapping [feedback #6]
- [ ] BY.7 - reconcile: auto-release mappings whose leaf is already archived (passive backstop) [feedback #3]
- [ ] BY.8 - resume: tighten rehydration batch hint to require a distinct plan_path per create [feedback #5]
- [x] BY.9 - writeback: suppress the "pass metadata.plan_phase" anchor hint when plan_phase was already provided [live find]
- [ ] BY.10 - writeback: don't no-op a TaskCreate against a stale prior-session mapping for a reused harness id (cross-session id reuse silently drops the task) [live find]
- [x] BY.11 - writeback: fall back to plan_path parsed from `description` when `metadata.plan_path` is absent (id-grammar gated to a dotted id that already exists in PLAN.md; metadata still preferred). Rescues rehydration-burst creates against schema eviction — the resume prompt already puts plan_path in `description`. `is_valid_plan_id` added in ast.rs; 4 tests; 399 unit tests green [root-cause mitigation]
- [ ] BY.12 - docs (README for `debug` verb + plan_path shape), fix pre-existing ast.rs doctest, cargo fmt + clippy + full suite green, then sweep Phase BY to PLAN_ARCHIVE.md
- [x] BY.13 - state.debug flag + `claude-plan-bridge debug on|off` toggle; writeback appends verbatim hook payloads to `.claude/plan-bridge-debug.jsonl` when on. Off by default, omitted from state when false, per-project scoped, gitignored. Confirmed the root cause above. Tests added; 395 unit tests green [investigation tooling]

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
