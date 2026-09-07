# ADR 0015: Adopt `kestrel-filter` and `kestrel-calcard` as Engine-Adjacent Crates

- **Status:** Accepted
- **Date:** 2026-09-07
- **Deciders:** Kestrel team

## Context

Two crates shipped in-tree ahead of any decision record: `kestrel-filter`
(an automated filter-rule engine with `regex`-based matching) and
`kestrel-calcard` (CalDAV/CardDAV types and client stubs per RFC 4791 /
RFC 6352, using `reqwest`). `kestrel-engine` composes both (ADR 0011 lists
only the four core-side crates), and `docs/engineering-standards.md` §1
requires an ADR-level justification for every dependency that crosses a
crate boundary. Neither crate was covered by `requirements.md` at the time
it landed; both are now reflected in the workspace tree, `requirements.md`
§1, and `docs/architecture.md` §2.

## Decision

1. **Adopt `kestrel-filter`** as a first-class engine-adjacent crate: a
   pure evaluation engine over `kestrel-core` message summaries. Rules are
   stored as JSON in `data.db` (`settings` key `filter_rules`) and evaluated
   by the engine's supervised `FilterService` on `MailArrived` events
   (`ServiceId::Filter`). `regex` is accepted as its one rule-matching
   dependency (bounded, compiled once per rule through `RegexCache`).
2. **Adopt `kestrel-calcard` as types-and-stubs only.** Calendar/contact
   product scope is not yet defined; the crate documents the wire formats
   (iCal/vCard parsing is implemented and tested; CalDAV/CardDAV HTTP
   clients are stubs that return `FeatureNotYetAvailable`). `reqwest` is
   accepted now because the sync/crypto crates already carry it, but the
   HTTP client surface must not be expanded until the calendar/contacts
   product decision is made (see roadmap).
3. `kestrel-engine` may compose both crates, matching how it composes
   `kestrel-sync`/`kestrel-storage`/`kestrel-crypto`; neither crate may
   depend on the engine or on frontends.

## Consequences

- The crate graph and dependency justifications in `docs/architecture.md`
  and `requirements.md` now match the workspace.
- Filter rules are a shipped, supervised feature; any further product scope
  (rule UI, server-side evaluation) rides on this decision.
- CalDAV/CardDAV remains non-functional-by-design until a product decision
  — reviewers should treat expanding the stubs as out of scope unless a new
  ADR supersedes this one.

## Alternatives Considered

- **Fold filter evaluation into `kestrel-sync`** — sync is already the
  largest crate; a separate rule engine keeps the mailbox/network concerns
  from growing rule DSLs.
- **Fold CalDAV/CardDAV into `kestrel-sync`** — same reason; the formats are
  self-contained (iCal/vCard) and independent of mail sync cadence.
- **Defer both until product scope is written** — they already shipped and
  are exercised by tests; retroactive adoption documents reality rather than
  deleting working, reviewed code.
