//! Crypto-domain errors (ADR 0007): internal enum, converted to
//! [`KestrelError`] at the service boundary.

use kestrel_core::error::KestrelError;

/// Errors internal to `kestrel-crypto`.
#[derive(Clone, Debug, thiserror::Error)]
pub enum CryptoError {
    /// Credentials rejected by the server (`UserAction`).
    #[error("credentials rejected")]
    CredentialsRejected,
    /// `OAuth2` flow failure (refresh/expiry/rejection).
    #[error("oauth: {0}")]
    OAuth(String),
    /// The OS keyring is unavailable (`UserAction`).
    #[error("keyring unavailable: {0}")]
    KeyringUnavailable(String),
    /// SASL protocol violation.
    #[error("sasl: {0}")]
    Sasl(String),
    /// TLS configuration/handshake failure.
    #[error("tls: {0}")]
    Tls(String),
    /// `OpenPGP` operation failure.
    #[error("openpgp: {0}")]
    OpenPgp(String),
    /// Unexpected internal state (Bug).
    #[error("bug: {0}")]
    Bug(String),
}

impl From<CryptoError> for KestrelError {
    fn from(err: CryptoError) -> Self {
        match err {
            CryptoError::CredentialsRejected | CryptoError::Sasl(_) => Self::CredentialsRejected,
            CryptoError::OAuth(detail) => Self::OAuthRefreshFailed { detail },
            CryptoError::KeyringUnavailable(detail) => Self::KeyringUnavailable { detail },
            CryptoError::Tls(detail) => Self::TlsHandshake { detail },
            CryptoError::OpenPgp(detail) => Self::OpenPgpFailed { detail },
            CryptoError::Bug(detail) => Self::Bug { detail },
        }
    }
}

/// Result alias for crypto internals.
pub type CryptoResult<T> = Result<T, CryptoError>;
