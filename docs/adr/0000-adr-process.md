# ADR 0000: Architecture Decision Record Process

- **Status:** Accepted
- **Date:** 2026-08-28
- **Deciders:** Kestrel team

## Context

Kestrel is a long-lived, multi-crate system with hard performance budgets and
security requirements. Architectural choices (frameworks, crates, concurrency
patterns) have outsized consequences and must not be made implicitly in code
review comments or lost in chat history. We need a lightweight, durable record
of every significant decision: what was chosen, why, what was rejected, and
what would make us revisit it.

## Decision

We adopt **Architecture Decision Records (ADRs)** as the single source of truth
for architectural decisions.

### Scope — when an ADR is required

An ADR is REQUIRED for any decision that meets one or more of:

1. Selecting or replacing a dependency that crosses a crate boundary or is
   security- or performance-critical (parsers, network stacks, UI frameworks,
   database layers).
2. Changing the concurrency model, message protocol, or supervision semantics.
3. Changing the storage schema strategy or on-disk format (SQLite, Tantivy,
   blob store).
4. Changing a security posture (threat model, sandboxing, credential handling).
5. Anything a new team member would ask "why is it done this way?" about.

An ADR is NOT required for: internal implementation details of a single module,
refactors that preserve behavior, or dependency patch bumps.

### Format

Every ADR file lives in `docs/adr/NNNN-short-title.md` where `NNNN` is a
monotonically increasing zero-padded number. Sections:

```markdown
# ADR NNNN: Title

- **Status:** Proposed | Accepted | Deprecated | Superseded by ADR NNNN
- **Date:** YYYY-MM-DD
- **Deciders:** names or roles

## Context
The forces at play: technical, organizational. The problem being solved.

## Decision
The change we are making, stated in the present tense ("We use ...").

## Consequences
What becomes easier, what becomes harder, what we must now do.

## Alternatives Considered
Each serious alternative with the reason it was rejected.
```

### Lifecycle

1. **Proposed** — author opens the ADR alongside a discussion issue or PR.
2. **Accepted** — after review; an accepted ADR is binding. Code that
   contradicts an accepted ADR must not be merged.
3. **Deprecated** — no longer relevant, but historically accurate.
4. **Superseded by ADR NNNN** — replaced; the superseding ADR links back.

ADRs are immutable once Accepted. To change a decision, write a new ADR that
supersedes the old one; never edit the history out of an accepted record.

## Consequences

- Every architectural choice is auditable; onboarding question "why?" has a
  written answer.
- Adds a small documentation tax on major decisions; the process is deliberately
  minimal (one file, five sections) to keep the tax low.
- `docs/engineering-standards.md` references ADRs; CI-relevant rules derive from
  them, not the reverse.

## Alternatives Considered

- **RFC process (Rust-style)** — heavier, multi-phase; disproportionate for a
  2–5 person team.
- **Wiki / design docs folder without format** — decays quickly, no lifecycle,
  hard to link from code review.
- **Decisions in commit messages only** — unsearchable, buried in history.
