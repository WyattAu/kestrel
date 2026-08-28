//! Error taxonomy (ADR 0007, `docs/error-taxonomy.md`).
//!
//! [`KestrelError`] is the cross-crate vocabulary: it is the **only** error
//! type that crosses the message protocol (`Reply::Err`, `ServiceDegraded`)
//! and the only type frontends match. Domain crates keep internal
//! `thiserror` enums and convert into `KestrelError` at the service boundary.
//!
//! `Display` strings are stable identifiers (tests assert on them); user
//! wording lives in frontends keyed by variant. Every variant maps to exactly
//! one [`RecoveryClass`].

use std::time::Duration;

/// How the engine (and UI) should recover from an error
/// (`docs/error-taxonomy.md` §1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryClass {
    /// Transient; retry with backoff is safe. Status line / passive note.
    Retryable,
    /// Requires user input (auth, certificate, quota). Blocking prompt.
    UserAction,
    /// Will not succeed by retrying. Inline failure state, logged once.
    Permanent,
    /// Invariant violated; owning service is restarted (ADR 0004).
    Bug,
    /// Triggers a reconciliation pipeline instead of retry semantics
    /// (e.g. `UIDVALIDITY` change, requirements §2.2).
    Reconciliation,
}

impl RecoveryClass {
    /// Suggested initial backoff for [`RecoveryClass::Retryable`] errors.
    #[must_use]
    pub fn default_backoff(self) -> Duration {
        match self {
            Self::Retryable => Duration::from_millis(250),
            _ => Duration::ZERO,
        }
    }
}

/// Cross-crate error vocabulary (ADR 0007). Variants are grouped by owning
/// domain; see `docs/error-taxonomy.md` §2 for the recovery-class table.
///
/// Variant payloads are stable wire identifiers; their semantics are
/// specified in `docs/error-taxonomy.md` (standards §8: docs are the source
/// of truth, code does not restate them).
#[allow(missing_docs)]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KestrelError {
    // ---- config (core) -------------------------------------------------
    /// Config file is invalid TOML or fails validation.
    #[error("config.invalid_toml")]
    InvalidToml { path: String, detail: String },
    /// Unknown configuration key (warning-class).
    #[error("config.unknown_key")]
    UnknownKey { key: String },

    // ---- message protocol (core) ---------------------------------------
    /// Engine command queue is full; retry later, never spin.
    #[error("protocol.busy")]
    Busy,
    /// Request was cancelled before completion (default oneshot reply).
    #[error("protocol.cancelled")]
    Cancelled,
    /// A command was malformed at the protocol level (invariant).
    #[error("protocol.malformed_command")]
    MalformedCommand { detail: String },

    // ---- auth (crypto domain) -------------------------------------------
    /// Server rejected credentials.
    #[error("auth.credentials_rejected")]
    CredentialsRejected,
    /// `OAuth2` refresh failed (expired/revoked token).
    #[error("auth.oauth_refresh_failed")]
    OAuthRefreshFailed { detail: String },
    /// OS keyring unavailable / GPG fallback unusable.
    #[error("auth.keyring_unavailable")]
    KeyringUnavailable { detail: String },

    // ---- transport (sync) ------------------------------------------------
    /// TLS handshake failure.
    #[error("transport.tls_handshake")]
    TlsHandshake { detail: String },
    /// Connection lost / IO error / timeout.
    #[error("transport.connection_lost")]
    ConnectionLost { detail: String },
    /// Server lacks a capability we require for the operation.
    #[error("transport.capability_missing")]
    CapabilityMissing { capability: String },

    // ---- imap (sync) ------------------------------------------------------
    /// `UIDVALIDITY` changed for a folder: reconciliation required.
    #[error("imap.uidvalidity_changed")]
    UidValidityChanged { folder: String },
    /// A fetch batch aborted mid-way; safe to retry from last cursor.
    #[error("imap.fetch_aborted")]
    FetchAborted,

    // ---- smtp (sync) ------------------------------------------------------
    /// Relay refused the connection (permanent).
    #[error("smtp.relay_refused")]
    RelayRefused { detail: String },
    /// Transient 4xx SMTP failure; retry with backoff.
    #[error("smtp.transient_4xx")]
    SmtpTransient { code: u16 },
    /// Server permanently rejected the message.
    #[error("smtp.message_rejected")]
    MessageRejected { detail: String },

    // ---- storage -----------------------------------------------------------
    /// Database file corrupt; user action (rebuild prompt).
    #[error("storage.db_corrupt")]
    DbCorrupt { db: String },
    /// Migration failed; user action.
    #[error("storage.migration_failed")]
    MigrationFailed { db: String, detail: String },
    /// Blob referenced but absent in CAS; re-fetchable.
    #[error("storage.blob_missing")]
    BlobMissing { hash: String },
    /// Storage-level IO failure (disk full, permissions); retryable.
    #[error("storage.io")]
    StorageIo { detail: String },
    /// Requested entity does not exist.
    #[error("storage.not_found")]
    NotFound { entity: String },

    // ---- index ---------------------------------------------------------------
    /// Tantivy index corrupt; rebuild from cache.db + blobs.
    #[error("index.corrupt")]
    IndexCorrupt,
    /// Index commit failed; retryable (batched commits).
    #[error("index.commit_failed")]
    IndexCommitFailed,

    // ---- mime parsing (ADR 0002, threat model §4.2) ---------------------------
    /// Malformed MIME; message still listable, degraded view.
    #[error("parse.malformed")]
    ParseMalformed { detail: String },
    /// A parser hard limit tripped (threat model §4.2).
    #[error("parse.limit")]
    ParseLimit { kind: LimitKind, actual: String },

    // ---- crypto -----------------------------------------------------------------
    /// `OpenPGP` operation unsupported for this key/algorithm.
    #[error("crypto.openpgp_unsupported")]
    OpenPgpUnsupported { detail: String },
    /// Signing failed (retryable: key available, operation failed).
    #[error("crypto.signing_failed")]
    SigningFailed { detail: String },
    /// `OpenPGP` operation failed (decrypt/verify/import).
    #[error("crypto.openpgp_failed")]
    OpenPgpFailed { detail: String },

    // ---- outbox -------------------------------------------------------------------
    /// Retry budget exhausted; draft preserved.
    #[error("outbox.retry_exhausted")]
    RetryExhausted { attempts: u32 },
    /// Draft failed validation; user must fix.
    #[error("outbox.draft_invalid")]
    DraftInvalid { detail: String },

    // ---- engine --------------------------------------------------------------------
    /// Invariant violated; owning service restarts (ADR 0004).
    #[error("engine.bug")]
    Bug { detail: String },
}

/// Kinds of parser hard limits (threat model §4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    /// MIME tree nesting depth exceeded 64.
    NestingDepth,
    /// A single decoded part exceeded 128 MiB.
    PartSize,
    /// Total decoded message exceeded 512 MiB.
    TotalSize,
    /// Header count exceeded the cap.
    HeaderCount,
    /// A single header exceeded the size cap.
    HeaderSize,
    /// Decoded/encoded expansion ratio exceeded 100×.
    DecompressionRatio,
}

impl KestrelError {
    /// The single recovery class governing this error
    /// (`docs/error-taxonomy.md` §2).
    #[must_use]
    pub fn recovery_class(&self) -> RecoveryClass {
        match self {
            Self::InvalidToml { .. }
            | Self::UnknownKey { .. }
            | Self::CredentialsRejected
            | Self::OAuthRefreshFailed { .. }
            | Self::KeyringUnavailable { .. }
            | Self::DbCorrupt { .. }
            | Self::MigrationFailed { .. }
            | Self::DraftInvalid { .. } => RecoveryClass::UserAction,
            Self::Busy
            | Self::TlsHandshake { .. }
            | Self::ConnectionLost { .. }
            | Self::FetchAborted
            | Self::SmtpTransient { .. }
            | Self::StorageIo { .. }
            | Self::IndexCorrupt
            | Self::IndexCommitFailed
            | Self::SigningFailed { .. } => RecoveryClass::Retryable,
            Self::Cancelled
            | Self::CapabilityMissing { .. }
            | Self::RelayRefused { .. }
            | Self::MessageRejected { .. }
            | Self::BlobMissing { .. }
            | Self::NotFound { .. }
            | Self::ParseMalformed { .. }
            | Self::ParseLimit { .. }
            | Self::RetryExhausted { .. } => RecoveryClass::Permanent,
            Self::OpenPgpUnsupported { .. } | Self::OpenPgpFailed { .. } => {
                RecoveryClass::Permanent
            }
            Self::MalformedCommand { .. } | Self::Bug { .. } => RecoveryClass::Bug,
            Self::UidValidityChanged { .. } => RecoveryClass::Reconciliation,
        }
    }

    /// Stable identifier (the `Display` form), for frontend message tables
    /// and test assertions.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        // `Display` is already the stable identifier; expose it typed.
        match self {
            Self::InvalidToml { .. } => "config.invalid_toml",
            Self::UnknownKey { .. } => "config.unknown_key",
            Self::Busy => "protocol.busy",
            Self::Cancelled => "protocol.cancelled",
            Self::MalformedCommand { .. } => "protocol.malformed_command",
            Self::CredentialsRejected => "auth.credentials_rejected",
            Self::OAuthRefreshFailed { .. } => "auth.oauth_refresh_failed",
            Self::KeyringUnavailable { .. } => "auth.keyring_unavailable",
            Self::TlsHandshake { .. } => "transport.tls_handshake",
            Self::ConnectionLost { .. } => "transport.connection_lost",
            Self::CapabilityMissing { .. } => "transport.capability_missing",
            Self::UidValidityChanged { .. } => "imap.uidvalidity_changed",
            Self::FetchAborted => "imap.fetch_aborted",
            Self::RelayRefused { .. } => "smtp.relay_refused",
            Self::SmtpTransient { .. } => "smtp.transient_4xx",
            Self::MessageRejected { .. } => "smtp.message_rejected",
            Self::DbCorrupt { .. } => "storage.db_corrupt",
            Self::MigrationFailed { .. } => "storage.migration_failed",
            Self::BlobMissing { .. } => "storage.blob_missing",
            Self::StorageIo { .. } => "storage.io",
            Self::NotFound { .. } => "storage.not_found",
            Self::IndexCorrupt => "index.corrupt",
            Self::IndexCommitFailed => "index.commit_failed",
            Self::ParseMalformed { .. } => "parse.malformed",
            Self::ParseLimit { .. } => "parse.limit",
            Self::OpenPgpUnsupported { .. } => "crypto.openpgp_unsupported",
            Self::OpenPgpFailed { .. } => "crypto.openpgp_failed",
            Self::SigningFailed { .. } => "crypto.signing_failed",
            Self::RetryExhausted { .. } => "outbox.retry_exhausted",
            Self::DraftInvalid { .. } => "outbox.draft_invalid",
            Self::Bug { .. } => "engine.bug",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_maps_to_a_recovery_class() {
        // Exhaustive match: adding a variant without a class breaks this test.
        let samples = [
            KestrelError::InvalidToml {
                path: "x".into(),
                detail: "y".into(),
            },
            KestrelError::UnknownKey { key: "k".into() },
            KestrelError::Busy,
            KestrelError::Cancelled,
            KestrelError::MalformedCommand { detail: "d".into() },
            KestrelError::CredentialsRejected,
            KestrelError::OAuthRefreshFailed { detail: "d".into() },
            KestrelError::KeyringUnavailable { detail: "d".into() },
            KestrelError::TlsHandshake { detail: "d".into() },
            KestrelError::ConnectionLost { detail: "d".into() },
            KestrelError::CapabilityMissing {
                capability: "QRESYNC".into(),
            },
            KestrelError::UidValidityChanged {
                folder: "INBOX".into(),
            },
            KestrelError::FetchAborted,
            KestrelError::RelayRefused { detail: "d".into() },
            KestrelError::SmtpTransient { code: 421 },
            KestrelError::MessageRejected { detail: "d".into() },
            KestrelError::DbCorrupt { db: "cache".into() },
            KestrelError::MigrationFailed {
                db: "data".into(),
                detail: "d".into(),
            },
            KestrelError::BlobMissing { hash: "ab".into() },
            KestrelError::StorageIo { detail: "d".into() },
            KestrelError::NotFound {
                entity: "message".into(),
            },
            KestrelError::IndexCorrupt,
            KestrelError::IndexCommitFailed,
            KestrelError::ParseMalformed { detail: "d".into() },
            KestrelError::ParseLimit {
                kind: LimitKind::NestingDepth,
                actual: "65".into(),
            },
            KestrelError::OpenPgpUnsupported { detail: "d".into() },
            KestrelError::OpenPgpFailed { detail: "d".into() },
            KestrelError::SigningFailed { detail: "d".into() },
            KestrelError::RetryExhausted { attempts: 12 },
            KestrelError::DraftInvalid { detail: "d".into() },
            KestrelError::Bug {
                detail: "invariant".into(),
            },
        ];
        for err in &samples {
            assert_eq!(err.kind(), err.to_string(), "kind() must equal Display");
            // Every class is one of the five defined values by construction.
            let _class = err.recovery_class();
        }
        // Spot-check the table from docs/error-taxonomy.md §2.
        assert_eq!(
            KestrelError::UidValidityChanged {
                folder: String::new()
            }
            .recovery_class(),
            RecoveryClass::Reconciliation
        );
        assert_eq!(
            KestrelError::Busy.recovery_class(),
            RecoveryClass::Retryable
        );
        assert_eq!(
            KestrelError::CredentialsRejected.recovery_class(),
            RecoveryClass::UserAction
        );
        assert_eq!(
            KestrelError::MalformedCommand {
                detail: String::new()
            }
            .recovery_class(),
            RecoveryClass::Bug
        );
        assert_eq!(
            KestrelError::BlobMissing {
                hash: String::new()
            }
            .recovery_class(),
            RecoveryClass::Permanent
        );
    }
}
