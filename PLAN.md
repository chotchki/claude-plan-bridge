# PLAN: plan-to-task bridge

Spec: see [SPEC.md](./SPEC.md). This plan sequences the implementation.

Phase exit rule (per global CLAUDE.md workflow): every box ticked, unit + e2e tests pass, docs updated. Then summarize and sweep to PLAN_ARCHIVE.md.

## Backlog (not yet phased)

- **Out of scope — session-feedback items the bridge can't / shouldn't own** (recorded 2026-05-28 so they don't resurface as confusion):
  - *"Task tools haven't been used recently" reminder is noise* — **not bridge-emitted.** It's a Claude Code harness built-in (no such string anywhere in `src/`); it fires on turns with no task-tool call. The bridge has no lever to silence a reminder it doesn't produce.
  - *First-class phase/umbrella parent type with child progress (9/10 done)* — that's the **harness task model**, not the bridge. Also cuts against `parent-tick-is-validation`: the bridge deliberately does not rehydrate parents as tasks (`resume.rs` Phase 27.1), so progress-bar parents would contradict the design.
  - *Reconcile one-turn lag after a hand-edit* — **wontfix, by design.** Reconcile runs on `UserPromptSubmit`; eventual consistency is the intended tradeoff (the feedback author agreed). Tightening it would mean reconciling on every PLAN.md write, which the lock/atomic-write model intentionally avoids.
  - (Note: a third feedback item — render `[3.3.2]` inline in TaskList via `subject="[N.M] title"` — was declined. It reverses the deliberate choice at `resume.rs:186` to keep plan_path in `metadata`/`description` and out of the title for a clean harness UI.)

- **Non-Backlog `##` sections placed after the phases are absorbed into the last phase** (split from Phase CH, recorded 2026-06-25). CH made the trailing `## Backlog` peel robust; this is the residual general class CH did NOT fix.

  Root: the parser only lifts the trailing `## Backlog` into `plan.backlog`. Any OTHER top-level section that sits after the phases (`## Non-goals`, `## Risks`, `## Sustainment`, an operator `### Backlog (rehomed from …)`) has no first-class home in the AST, so the tree-walk absorbs it into the last phase's `annotations`. `archive` then sweeps it into PLAN_ARCHIVE.md with the phase, and writeback re-emits it before the phase's leaves — the same Symptom A/B shape CH fixed for the Backlog. README §"PLAN.md format" still claims operator sections "are left exactly where they are"; that holds only when they *precede* the phases.

  Same root surfaces two more ways (adversarial sweep, 2026-06-25): (a) a non-Backlog `##` section placed *after* a trailing `## Backlog` defeats the peel (backlog not lifted, absorbed into the prior phase); (b) a `## Phase N`-shaped line written *as backlog note content* is read as document structure and mints a phantom phase. Both are the same "no AST home for trailing non-phase content" gap.

  Fix sketch: add a first-class `trailer: Vec<String>` region to `Plan` (verbatim; serialized after every phase, above `## Backlog`); have the parser route a column-0 non-phase `#`/`##` section appearing after the first phase into it rather than into phase annotations. Wider blast radius than CH (ast.rs + parser.rs + serializer.rs + round-trip tests), hence deferred. Workaround until then: keep such sections above the phases, or inside `## Backlog`.
