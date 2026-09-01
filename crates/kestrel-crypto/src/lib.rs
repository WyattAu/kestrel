//! `kestrel-crypto` — credentials and cryptography for Kestrel.
//!
//! OS keyring credential storage (plaintext refused — threat model §4.8),
//! SASL mechanisms (PLAIN/LOGIN/SCRAM-SHA-256/XOAUTH2), `OAuth2` loopback +
//! PKCE flows (requirements §2.3), rustls TLS configuration (TLS 1.3
//! default / 1.2 minimum), and `OpenPGP` via Sequoia (ADR 0012).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, missing_docs))]

pub mod credentials;
pub mod error;
pub mod oauth;
pub mod openpgp;
pub mod sasl;
pub mod smime;
pub mod tls;

pub use credentials::{
    CredentialService, CredentialStore, InMemoryStore, KeyringStore, resolve_credential_store,
};
pub use error::{CryptoError, CryptoResult};
pub use kestrel_core::{
    sasl::{SaslMechanism, SaslSession},
    secrets::SecretString,
};
pub use sasl::start;
pub use tls::tls_config;
