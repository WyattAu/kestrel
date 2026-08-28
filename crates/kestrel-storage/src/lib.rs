//! `kestrel-storage` — persistence for Kestrel.
//!
//! Dual `SQLite` databases (ADR 0009: rebuildable `cache.db` + durable
//! `data.db`), sqlx compile-time-checked queries (ADR 0003), the
//! content-addressed blob store with refcount GC (`docs/schema.md` §4), and
//! the Tantivy full-text index service (§5).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, missing_docs))]
