# PLAN: plan-to-task bridge

Spec: see [SPEC.md](./SPEC.md). This plan sequences the implementation.

Phase exit rule (per global CLAUDE.md workflow): every box ticked, unit + e2e tests pass, docs updated. Then summarize and sweep to PLAN_ARCHIVE.md.

## Backlog (not yet phased)

- ~~Refactor checkbox-line grammar to **winnow**~~ — done 2026-05-16. `id_and_title` / `bold_id` / `bare_id` / `id_chars` / `skip_separator` are winnow combinators now; the outer `- [STATE] ` matcher, the code-fence state machine, and the indent-stack tree assembly stay hand-rolled.
- ~~**Won't-do checkbox state (`[-]`)**~~ — done 2026-05-16. `Node.state: NodeState { Pending, Done, WontDo }` across the codebase. Parser accepts `[-]` and `[~]`; serializer emits `[-]`. Archive treats `WontDo` like `Done` (phase can exit). Reconcile emits `LeafStateChanged { old, new }` covering all transitions. Writeback's `TaskUpdate(deleted)` against a `[-]` leaf keeps the line and just drops the state mapping. MCP gained `plan_skip` (and `plan_phase_exit`). 128 tests passing.
- **Baseline subcommand for existing PLAN.md files** — when installing the bridge into a project that already has a populated PLAN.md, the first reconcile emits `LeafAdded` for every existing leaf (the state file is fresh). Loud and annoying on large plans. Mitigation: a `plan-bridge baseline` subcommand that seeds state with synthetic `baseline:<plan_path>` task ids for each current leaf, suppressing the spam. When Claude later TaskCreates against a baselined plan_path, writeback should replace the baseline mapping with the real one rather than duplicate.

---

- [x] 8.0 Serialize concurrent writebacks with a file lock — and surface lock failure as a loud hook block, never silent data loss
- [ ] 10.0 Productionalize the tool — public repo, CI, packaging, README polish
  - [x] 10.1 Public GitHub repo + LICENSE; update Cargo.toml `repository` and `license` fields
    - We'll go with an MIT license for this
  - [x] 10.2 CI builds — GitHub Actions workflow: cargo fmt --check, clippy -D warnings, test, on Linux/macOS/Windows stable
  - [x] 10.3 README polish — lead with a 30-second pitch and an example transcript; trim implementation detail or fold it lower
  - [ ] 10.4 Publish v0.1.0 to crates.io — fill out Cargo.toml metadata; cargo publish --dry-run; cargo publish
  - [ ] 10.4a Draft a hotchkiss.io entry on plan-bridge — motivation, design, install/usage
  - [x] 10.4b Internal-prefix rename: `plan-bridge:` → `claude-plan-bridge:` in source message strings (~10 files)
  - [ ] 10.5 Phase 10 exit — all sub-boxes ticked; GH repo public; CI green on main; v0.1.0 on crates.io; README badges live
- [ ] 11.0 Update global ~/.claude/CLAUDE.md to point at plan-bridge as the canonical PLAN.md driver
