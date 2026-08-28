//! `kestrel-sync` — network engines for Kestrel.
//!
//! IMAP state machine on `imap-flow` (ADR 0005), SMTP submission via
//! `lettre`, QRESYNC/CONDSTORE delta sync, `UIDVALIDITY` reconciliation,
//! and the outbox queue (`docs/sync-engine.md`).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, missing_docs))]
