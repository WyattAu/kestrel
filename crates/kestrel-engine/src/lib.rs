//! `kestrel-engine` — assembles the core engine.
//!
//! Service supervisor (ADR 0004), the command router, the event bus, and
//! lifecycle wiring of `StorageService`, `IndexService`, `SearchService`,
//! `OutboxService`, `CredentialService`, and per-account `SyncService`s.
//! Frontends spawn the engine in-process and interact
//! with it exclusively through the typed message protocol.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, missing_docs))]
