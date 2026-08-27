# ADR 0007: Error Strategy — `thiserror` Taxonomy in `kestrel-core`

- **Status:** Accepted
- **Date:** 2026-08-28
- **Deciders:** Kestrel team

## Context

`requirements.md` places an "error taxonomy" in `kestrel-core`. The client has
distinct failure domains (protocol, network, storage, crypto, MIME, UI) with
different recovery semantics; errors cross service boundaries as messages and
must be presentable to users, loggable with context, and exhaustive at match
sites. Panics are unacceptable on any untrusted-input path.

## Decision

- Every crate defines its error enums with **`thiserror`**; the complete
  taxonomy (error kinds, sources, recovery class) is specified in
  `docs/error-taxonomy.md` and owned by `kestrel-core` as the cross-crate
  vocabulary (`KestrelError` wraps domain errors via `#[from]`).
- Errors are **values, not control flow**: services return `Result`; the actor
  protocol carries `ServiceError` payloads in `Err` replies and
  `ServiceDegraded` events (ADR 0004).
- Each variant maps to exactly one **recovery class**:
  `Retryable { backoff }`, `UserAction` (e.g., re-authenticate),
  `Permanent`, `Bug`.
- **Context is attached, not formatted**: `tracing` spans carry structured
  context (account id, folder, uid); error display stays stable for tests.
- Panic policy: `panic = "abort"` is NOT used — panics in service tasks are
  contained by the supervisor; `unwrap`/`expect` are banned outside tests and
  documented invariant sites (clippy lint enforced, see
  `docs/engineering-standards.md`).
- No `anyhow` in library crates; `anyhow` is permitted only at binary
  top-level `main` for final reporting.

## Consequences

- Exhaustive matching: adding a variant forces every handler to consider it —
  compile-time driven error handling.
- User-facing messages: each variant has a stable `Display` plus a separate
  user-action table in the UI, so wording can change without touching logic.
- **Cost:** taxonomy discipline required — new variants need a recovery class
  and (if user-visible) a UI message; `docs/error-taxonomy.md` review is part
  of the PR checklist.

## Alternatives Considered

- **`anyhow` everywhere** — fast to write, but erases variant exhaustiveness
  and pushes error semantics to string matching; unacceptable for a system
  with automated retry/reconciliation logic.
- **`snafu`** — comparable power with context selectors; `thiserror` is more
  widespread and lighter; context needs are met by tracing spans.
- **Numeric error codes / custom trait** — ceremony without ecosystem support.
