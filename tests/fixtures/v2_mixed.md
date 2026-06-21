# FORMATv2 mixed-format fixture

Exercises every FORMATv2 feature the parser ships: header phases, `*(depends
on: ...)*` markers, `*(prefer after: ...)*` markers, phase-level prose (intro +
between tasks + trailing), nested subtasks, all four checkbox states, and h1
`# Backlog (not yet phased)` with both flat-bullet notes and nested descoped
subtrees.

## Phase 1 - Foundational phase
- [x] 1.1 - done task
- [-] 1.2 - won't-do task
- [>] 1.3 - backlog task

## Phase AI - Studio dogfood

Intro paragraph for Phase AI — context that belongs to the phase as a whole,
not to any particular task.

- [x] AI.0 - Lock decisions
- [ ] AI.1 - Implement driver
  - [x] AI.1.0 - protocol
  - [ ] AI.1.1 - transport
    Indented prose under the subtask — task-level, stays with AI.1.1.
- [-] AI.2 - abandoned subtask

Prose between tasks — also phase-level.

- [ ] AI.3 - more work

Trailing prose for Phase AI — sweeps with the phase to PLAN_ARCHIVE.md.

## Phase AQ - frame rollout *(depends on: AP)*

- [ ] AQ.1 - promote frame primitive
- [ ] AQ.2 - thread frame through generator

## Phase AM - Standardize on tailwind *(prefer after: AI)*

Soft sequencing: AM benefits from AI landing first but isn't gated on it.

- [ ] AM.0 - audit + spike

## Phase AS - invariant spine *(depends on: AR, AQ)* *(prefer after: AM)*

Both marker kinds in one header.

- [ ] AS.0 - plan rollout

# Backlog (not yet phased)

- **Plain note** — added 2026-05-19.
- **Deferred from a phase** — deferred from 1.2 on 2026-05-19.
- X.1 - Descoped item from an archived phase
  - X.1.1 - Subtask carried over with structure intact
    Prose continuation under the descoped subtask.
- Y.7 - Another descoped subtree
