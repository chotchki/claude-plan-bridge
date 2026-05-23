# Project Name — Active Plan

any amount of markdown prose this is outside a phase

## Phase AI - Phase Title *(depends on: AB)*
Random prose or phase details. not a task unless it starts with - [ ] x.x.x.x - on a line

- [x] AI.0 - Completed Task
  Prose for the task

- [ ] AI.1 - Uncompleted Task
  - [x] AI.1.0 - Completed Subtask
    More prose
  - [>] AI.1.1 - Backlog
  - [-] AI.1.2 - Won't do subtask

Random prose that shouldn't be as task, will be swept with the phase.

When archiving a phase, the bridge should error unless every task and subtask is decided

## Phase AS — invariant spine (D6) *(depends on: AR)*

The destination: invariant as single source of truth, with generators + views referencing
it. Biggest lift; AS.0 re-plans the decomposition from the spike findings first (the layer
most likely to redecompose). Gated on AR (the spine references views).

- [ ] AS.0 - Plan/spike the spine rollout decomposition (lock the `src/` home + the taxonomy
migration order before building)
- [ ] AS.1 - promote `Violation` / `Invariant` / `ViolationGenerator` / `View` types to `src/`
- [ ] AS.2 - unify the fractured taxonomy: `PlantKind` (20) ⋈ `check_type` (~10 untyped) → one
closed `Violation` taxonomy; total `invariant→{generators,views}` maps, exhaustiveness-checked
(data/deadline windows stay invariant-owned)
- [ ] AS.3 - generator = stateful fold carrying `(balances, active-violation-set)`;
`Invariant.scenario_for(shape, selector)`; non-violating = perturbation off
- [ ] AS.4 - cross-account VECTOR state (AP.2's honest limit): legs net to zero across accounts;
`ledger_drift` parent rollup; cross-boundary propagation
- [ ] AS.5 - retire byte-locked seed SQL → semantic self-validation (`detect(gen) ⊇ intended`)
replaces byte-identity
- [ ] AS.6 - 4-way agreement + `TestScenarioCoverage` become the runtime linkage assertion over
the spine
- [ ] AS.7 - training/docs scenarios self-validated (can't lie / can't silently fail to
demonstrate)

# Backlog (not yet phased)
- Backlog item
- X.1 - Descoped item from above, populated when the phase was swept to PLAN_ARCHIVE.md
  - X.1.1 - Subtask swept with parent to backlog
    Prose for the task
