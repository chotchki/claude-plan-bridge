# PLAN: plan-to-task bridge

Spec: see [SPEC.md](./SPEC.md). This plan sequences the implementation.

Phase exit rule (per global CLAUDE.md workflow): every box ticked, unit + e2e tests pass, docs updated. Then summarize and sweep to PLAN_ARCHIVE.md.

## Backlog (not yet phased)

- **Out of scope — session-feedback items the bridge can't / shouldn't own** (recorded 2026-05-28 so they don't resurface as confusion):
  - *"Task tools haven't been used recently" reminder is noise* — **not bridge-emitted.** It's a Claude Code harness built-in (no such string anywhere in `src/`); it fires on turns with no task-tool call. The bridge has no lever to silence a reminder it doesn't produce.
  - *First-class phase/umbrella parent type with child progress (9/10 done)* — that's the **harness task model**, not the bridge. Also cuts against `parent-tick-is-validation`: the bridge deliberately does not rehydrate parents as tasks (`resume.rs` Phase 27.1), so progress-bar parents would contradict the design.
  - *Reconcile one-turn lag after a hand-edit* — **wontfix, by design.** Reconcile runs on `UserPromptSubmit`; eventual consistency is the intended tradeoff (the feedback author agreed). Tightening it would mean reconciling on every PLAN.md write, which the lock/atomic-write model intentionally avoids.
  - (Note: a third feedback item — render `[3.3.2]` inline in TaskList via `subject="[N.M] title"` — was declined. It reverses the deliberate choice at `resume.rs:186` to keep plan_path in `metadata`/`description` and out of the title for a clean harness UI.)
