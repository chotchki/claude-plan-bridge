# PLAN: plan-to-task bridge

Spec: see [SPEC.md](./SPEC.md). This plan sequences the implementation.

Phase exit rule (per global CLAUDE.md workflow): every box ticked, unit + e2e tests pass, docs updated. Then summarize and sweep to PLAN_ARCHIVE.md.

## Backlog (not yet phased)

- ~~Refactor checkbox-line grammar to **winnow**~~ — done 2026-05-16. `id_and_title` / `bold_id` / `bare_id` / `id_chars` / `skip_separator` are winnow combinators now; the outer `- [STATE] ` matcher, the code-fence state machine, and the indent-stack tree assembly stay hand-rolled.
- ~~**Won't-do checkbox state (`[-]`)**~~ — done 2026-05-16. `Node.state: NodeState { Pending, Done, WontDo }` across the codebase. Parser accepts `[-]` and `[~]`; serializer emits `[-]`. Archive treats `WontDo` like `Done` (phase can exit). Reconcile emits `LeafStateChanged { old, new }` covering all transitions. Writeback's `TaskUpdate(deleted)` against a `[-]` leaf keeps the line and just drops the state mapping. MCP gained `plan_skip` (and `plan_phase_exit`). 128 tests passing.
- **Baseline subcommand for existing PLAN.md files** — when installing the bridge into a project that already has a populated PLAN.md, the first reconcile emits `LeafAdded` for every existing leaf (the state file is fresh). Loud and annoying on large plans. Mitigation: a `plan-bridge baseline` subcommand that seeds state with synthetic `baseline:<plan_path>` task ids for each current leaf, suppressing the spam. When Claude later TaskCreates against a baselined plan_path, writeback should replace the baseline mapping with the real one rather than duplicate.

---

- [x] 13.0 MCP plan_rename tool: typed-API parity with TaskUpdate(subject=...)
  - [x] 13.1 MCP plan_rename tool: impl + unit tests in src/mcp.rs
  - [x] 13.2 README: add `plan_rename` row to the MCP tools table
  - [x] 13.3 Phase 13 exit — cargo test green; README MCP table updated
- [x] 14.0 Release workflow: also create a GitHub Release after `cargo publish` succeeds
- [ ] 15.0 Bug fixes from ocr_pdf_latex shakeout: stop mangling non-canonical PLAN.md
  - [x] 15.1 writeback pre-flight: detect markdown headers attached as annotations and refuse rather than silently demote
  - [x] 15.2 writeback: clearer parent-not-found error suggests canonical phase-checkbox format
  - [x] 15.3 README: prominent warning that `### Phase N` section headers don't work — canonical format only
  - [ ] 15.3a Bump Cargo.toml to 0.1.2; tag v0.1.2; push tag (release workflow ships to crates.io + creates GH release)
  - [ ] 15.3b Install released v0.1.2 from crates.io to replace local dev build (dogfood the published version)
  - [ ] 15.4 Phase 15 exit — cargo test green; fixture test passes against captured ocr_pdf_latex PLAN.md format; README warns clearly
