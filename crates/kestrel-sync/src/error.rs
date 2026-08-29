//! Sync-domain errors (ADR 0007): internal enum wrapping [`KestrelError`]
//! taxonomy payloads; converted back at the service boundary.

use kestrel_core::error::KestrelError;

/// Errors internal to `kestrel-sync`.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// Transport/auth/storage failure carried from the taxonomy.
    #[error("{0}")]
    Kestrel(#[from] KestrelError),
    /// Protocol-level misuse detected locally.
    #[error("protocol: {0}")]
    Protocol(String),
}

impl From<SyncError> for KestrelError {
    fn from(err: SyncError) -> Self {
        match err {
            SyncError::Kestrel(e) => e,
            SyncError::Protocol(detail) => Self::MalformedCommand { detail },
        }
    }
}

/// Result alias for sync internals.
pub type SyncResult<T> = Result<T, SyncError>;
