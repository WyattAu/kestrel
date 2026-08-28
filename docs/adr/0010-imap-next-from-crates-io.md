# ADR 0010: Consume `imap-flow` as `imap-next` from crates.io

- **Status:** Accepted
- **Date:** 2026-08-28
- **Deciders:** Kestrel team

## Context

ADR 0005 selected DeltaChat's `imap-flow` as the IMAP transport. During
Phase 2 implementation we verified that `imap-flow` is not — and never was —
published to crates.io under that name: the project was renamed upstream to
**`imap-next`** (same authors, same sans-I/O design: strict
command/response correlation, surfaced literals, integrated IDLE handling)
and is published at `>= 0.3` with the companion crates `imap-codec` /
`imap-types`. ADR 0005's decision — use the DeltaChat-lineage transport with
raw QRESYNC/HIGHESTMODSEQ access rather than a higher-level client — is
unchanged; only the crate's registry identity moved.

## Decision

We consume **`imap-next` from crates.io** (version-pinned in `Cargo.lock`,
feature-gated: `starttls`, `ext_condstore_qresync`, `ext_namespace`,
`ext_utf8`), treat it as the continuation of ADR 0005's chosen transport, and
keep ownership of the sync state machine ourselves exactly as ADR 0005
specifies.

## Consequences

- Registry dependency with semver guarantees replaces what would otherwise
  have been a git-rev pin; no Cargo `[patch]` or git source entries needed
  (supply-chain check stays registry-only).
- The sans-I/O `State` trait means we drive I/O ourselves; STARTTLS upgrade
  wraps our transport without resetting protocol state (see
  `docs/sync-engine.md`).
- Upstream is pre-1.0; minor-version bumps are isolated in `kestrel-sync`
  behind our session wrapper.

## Alternatives Considered

- **Git pin to the renamed repository** — possible, but adds a git source to
  the supply chain for no benefit now that the crate is published.
- **Supersede to the `imap` crate v3 alpha** — same lineage, less direct
  access to the primitives ADR 0005 wanted; unnecessary indirection.
