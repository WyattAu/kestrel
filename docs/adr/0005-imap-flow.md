# ADR 0005: Use `imap-flow` for IMAP Transport

- **Status:** Accepted
- **Date:** 2026-08-28
- **Deciders:** Kestrel team

## Context

`requirements.md` §2.1–2.2 require IMAP4rev1/rev2 with `IDLE`, `CONDSTORE`/
`QRESYNC`, `UIDPLUS`, `NAMESPACE`, `MOVE`; a delta-sync state machine;
`UIDVALIDITY` reconciliation; and offline-first behavior. IMAP is a notoriously
trap-laden protocol (literal continuations, unsolicited responses, IDLE
interruption, server quirks). The transport library is the most
battlefield-sensitive dependency in the client.

## Decision

We use **`imap-flow`** (from the DeltaChat project) as the IMAP client
transport inside `kestrel-sync`:

- `imap-flow` provides a session-oriented, strictly sequential command/literal
  API with integrated `IDLE` handling and fine-grained control over
  unsolicited data — exactly the primitives the sync state machine needs.
- Server capability negotiation (`QRESYNC`, `CONDSTORE`, `UIDPLUS`, `MOVE`,
  `NAMESPACE`, `IMAP4rev2`) is layered by us on top of capability responses.
- TLS via `rustls` (TLS 1.3 default, 1.2 minimum) with per-provider
  certificate settings; SASL (PLAIN, LOGIN, SCRAM-SHA-256, XOAUTH2) is handled
  in `kestrel-crypto` and injected as authentication callbacks.
- A fake/recorded-server test harness exercises the full state machine offline
  (see `docs/testing-strategy.md`).

## Consequences

- **Proven robustness:** `imap-flow` powers DeltaChat's sync, one of the
  largest deployed Rust IMAP clients; its design (every response matched to a
  command, literals surfaced explicitly) eliminates the classic
  response-misattribution bugs.
- **QRESYNC-friendly:** raw access to `HIGHESTMODSEQ`/`vanished` data lets us
  implement the mandated delta-sync semantics without a library hiding them.
- **Cost:** `imap-flow` is a lower-level building block than the `imap` crate
  — we own more of the state machine. That is deliberate: the state machine
  IS our product (see `docs/sync-engine.md`).
- **Version risk:** active development upstream; we pin minor versions and
  vendor fuzz-tested integration tests.

## Alternatives Considered

- **`async-imap`** — flexible streaming API, but response/command correlation
  and literal edge cases are left to the caller — historically the source of
  hard bugs; effectively lower-level than `imap-flow`.
- **`imap` (sync crate)** — mature but synchronous; would force
  `spawn_blocking` across the entire sync engine and complicates IDLE
  cancellation.
- **JMAP-first with IMAP shim** — JMAP is Phase 5; leading with it contradicts
  the roadmap.
