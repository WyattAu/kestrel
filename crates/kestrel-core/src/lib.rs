//! `kestrel-core` — the shared vocabulary of the Kestrel email client.
//!
//! Domain types, typed IDs, the error taxonomy (ADR 0007), the frozen
//! core↔frontend message protocol (`docs/message-protocol.md`), configuration
//! (ADR 0006), XDG path resolution, the clock abstraction (architecture §8),
//! the `MimeParser` trait and its `mail-parser` adapter (ADR 0002), the TUI
//! escape sanitizer and link classifier (threat model §4.5–4.6), and the
//! pure JWZ-lite threading algorithm (`docs/schema.md` §3.4).
//!
//! This crate is dependency-light by design: no UI, no storage backends, no
//! network clients (architecture §2).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, missing_docs))]
