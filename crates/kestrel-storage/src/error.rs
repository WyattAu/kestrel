//! Internal storage errors (ADR 0007): domain crate keeps its own
//! `thiserror` enum and converts to [`KestrelError`] at the service
//! boundary — the only error type crossing crates.

use kestrel_core::error::KestrelError;

/// Errors internal to `kestrel-storage`.
#[derive(Clone, Debug, thiserror::Error)]
pub enum StorageError {
    /// Database IO/SQL failure (retryable). Stored as text to keep the
    /// error `Clone` (it crosses service replies).
    #[error("sql error: {0}")]
    Sql(String),
    /// Migration application failed (user action).
    #[error("migration failed: {0}")]
    Migration(String),
    /// Blob store IO failure.
    #[error("blob io: {0}")]
    BlobIo(String),
    /// Blob missing from CAS (permanent; re-fetchable).
    #[error("blob missing: {0}")]
    BlobMissing(String),
    /// Search/index failure.
    #[error("index error: {0}")]
    Index(String),
    /// Serialization failure of a stored JSON column (invariant).
    #[error("json error: {0}")]
    Json(String),
    /// Cross-database invariant violated (Bug class, ADR 0009).
    #[error("cross-db invariant violated: {0}")]
    Invariant(String),
    /// Row->type mapping failed (invariant).
    #[error("row mapping: {0}")]
    Row(String),
}

impl From<StorageError> for KestrelError {
    fn from(err: StorageError) -> Self {
        match err {
            StorageError::Sql(detail) | StorageError::Json(detail) => Self::StorageIo { detail },
            StorageError::Migration(detail) => Self::MigrationFailed {
                db: "cache-or-data".to_string(),
                detail,
            },
            StorageError::BlobIo(detail) | StorageError::Index(detail) => {
                Self::StorageIo { detail }
            }
            StorageError::BlobMissing(hash) => Self::BlobMissing { hash },
            StorageError::Invariant(detail) | StorageError::Row(detail) => Self::Bug { detail },
        }
    }
}

impl From<StorageError> for String {
    fn from(err: StorageError) -> Self {
        err.to_string()
    }
}

impl From<sqlx::Error> for StorageError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sql(e.to_string())
    }
}

impl From<serde_json::Error> for StorageError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e.to_string())
    }
}

/// Result alias for storage internals.
pub type StorageResult<T> = Result<T, StorageError>;
