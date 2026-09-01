//! `kestrel-storage` — persistence for Kestrel.
//!
//! Dual `SQLite` databases (ADR 0009: rebuildable `cache.db` + durable
//! `data.db`), sqlx access with compile-time discipline (ADR 0003), the
//! content-addressed blob store with refcount GC (`docs/schema.md` §4), and
//! the Tantivy full-text index (§5).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, missing_docs))]

pub mod blob;
pub mod db;
pub mod error;
pub mod index;
pub mod mail_store_impl;
pub mod ops;
pub mod search;
pub mod store;

pub use blob::BlobStore;
pub use error::{StorageError, StorageResult};
pub use index::{IndexDoc, IndexHandle, IndexService};
pub use ops::{FlagPayload, OpType, PendingOp, PendingOpPayload, SnoozeRow};
pub use search::{SearchHandle, SearchService};
pub use store::{
    FolderRow, IngestBatch, IngestMessage, IngestStats, MessageLoad, NewAccount, NewFolder,
    OutboxEnvelope, OutboxRow, PendingDoc, StorageHandle, StorageService, StoreCommand,
};
