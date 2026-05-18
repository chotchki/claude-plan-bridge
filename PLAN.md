# PLAN: plan-to-task bridge

Spec: see [SPEC.md](./SPEC.md). This plan sequences the implementation.

Phase exit rule (per global CLAUDE.md workflow): every box ticked, unit + e2e tests pass, docs updated. Then summarize and sweep to PLAN_ARCHIVE.md.

## Backlog (not yet phased)

- ~~Refactor checkbox-line grammar to **winnow**~~ — done 2026-05-16. `id_and_title` / `bold_id` / `bare_id` / `id_chars` / `skip_separator` are winnow combinators now; the outer `- [STATE] ` matcher, the code-fence state machine, and the indent-stack tree assembly stay hand-rolled.
- ~~**Won't-do checkbox state (`[-]`)**~~ — done 2026-05-16. `Node.state: NodeState { Pending, Done, WontDo }` across the codebase. Parser accepts `[-]` and `[~]`; serializer emits `[-]`. Archive treats `WontDo` like `Done` (phase can exit). Reconcile emits `LeafStateChanged { old, new }` covering all transitions. Writeback's `TaskUpdate(deleted)` against a `[-]` leaf keeps the line and just drops the state mapping. MCP gained `plan_skip` (and `plan_phase_exit`). 128 tests passing.
- **Baseline subcommand for existing PLAN.md files** — when installing the bridge into a project that already has a populated PLAN.md, the first reconcile emits `LeafAdded` for every existing leaf (the state file is fresh). Loud and annoying on large plans. Mitigation: a `plan-bridge baseline` subcommand that seeds state with synthetic `baseline:<plan_path>` task ids for each current leaf, suppressing the spam. When Claude later TaskCreates against a baselined plan_path, writeback should replace the baseline mapping with the real one rather than duplicate.

---

- [ ] 30.0 Phase 30 — Green CI + coverage badge
  - [x] 30.1 Fix `cargo fmt --check` drift: `maybe_warn_missing_session_start`'s `Some(msg)` arm in src/main.rs is currently multi-line where rustfmt wants single-line. Reformatted locally + verified `cargo fmt --check` passes. Root cause: my Phase 29 edits broke fmt without me running `cargo fmt` before committing.
  - [ ] 30.2 Diagnose + fix Windows CI flake: `Swatinem/rust-cache@v2` died mid-restore on `windows-latest` with no error output (half a second between "Restoring cache..." and "Post job cleanup"). Try: bump to `Swatinem/rust-cache@v3` (or pinned latest SHA); fall back to `cache: false` on Windows if upstream issue. Confirm with a green run before tagging anything.
  - [ ] 30.3 Bump workflow action versions to silence Node.js 20 deprecation warnings: `actions/checkout@v4` → @v5 (when released; verify), `dtolnay/rust-toolchain@stable` and `Swatinem/rust-cache@v2` to current. Validate that the bumps don't reintroduce 30.2's flake.
  - [ ] 30.4 Add `cargo-llvm-cov` coverage step on Linux: install via `cargo install cargo-llvm-cov --locked` (or use `taiki-e/install-action`), run `cargo llvm-cov --all-features --workspace --lcov --output-path lcov.info` and `--json --summary-only > coverage.json` in CI on push:main + PR. Report goes to GHA Step Summary as a markdown table.
  - [ ] 30.5 Generate a shields.io-compatible coverage badge JSON from llvm-cov summary: `{"schemaVersion": 1, "label": "coverage", "message": "82.4%", "color": "brightgreen|yellow|orange|red"}`. Color thresholds: ≥80 green, ≥60 yellow, ≥40 orange, <40 red.
  - [ ] 30.6 Publish the badge JSON + HTML coverage report to an orphan `badges` branch on push:main only (skip on PRs to avoid spam). Force-push via `peaceiris/actions-gh-pages` or a hand-rolled `git checkout --orphan badges` + commit + force-push step. Branch contains: `coverage.json` (badge endpoint), `coverage-html/` (browsable report), no source.
  - [ ] 30.7 Add the coverage badge to README. Source URL: `https://img.shields.io/endpoint?url=https://raw.githubusercontent.com/chotchki/claude-plan-bridge/badges/coverage.json`. Links to the rendered HTML report on the same branch.
  - [ ] 30.8 Upload `lcov.info` + `coverage-html.tar.gz` as a CI artifact on every CI run (PR + push) so reviewers can download even without the badges-branch publication. Retention: 14 days.
  - [ ] 30.9 README: document the coverage workflow + how to run `cargo llvm-cov` locally in the "Contributing" section. Note color thresholds + how to regen the badge after a coverage drop.
  - [ ] 30.10 Phase 30 exit — tests, version bump, archive
