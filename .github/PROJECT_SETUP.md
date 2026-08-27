# GitHub Project Setup (one-time)

Create once, then maintain via issues/PRs. This file documents the intended
shape so it can be recreated.

## Project

- Name: `Kestrel`
- Type: Board (`Backlog` → `In progress` → `In review` → `Done`)
- Field `Phase` (single-select): `P1`…`P5`, `Cross-phase`
- Field `Type` (single-select): `epic`, `task`, `bug`, `chore`, `spike`
- Field `SLA` (single-select, optional): `perf`, `memory`, `startup`
- Views:
  - **Roadmap** — grouped by `Phase`, epics only
  - **Active** — `Status != Done AND Phase = current phase`
  - **SLA watch** — `SLA is not empty`

## Milestones

One per phase (`phase-1` … `phase-5`) — issues get a milestone matching their
phase; exit criteria live in `docs/roadmap.md`.

## Labels

`epic`, `phase:1`…`phase:5`, `adr`, `security`, `perf`, `blocked`, plus
crate-scoped labels `crates/core`, `crates/sync`, `crates/storage`,
`crates/crypto`, `crates/tui`, `crates/gui`.

## Automation

- Issues opened from templates auto-attach to the project.
- Linked PRs (`Closes #N`) move the issue to `In review` on PR open, `Done`
  on merge.
