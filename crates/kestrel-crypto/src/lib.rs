//! `kestrel-crypto` — credentials and cryptography for Kestrel.
//!
//! OS keyring credential storage (plaintext refused — threat model §4.8),
//! SASL mechanisms, `OAuth2` loopback + PKCE flows (requirements §2.3), rustls
//! TLS configuration, and `OpenPGP` via Sequoia (Phase 5).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, missing_docs))]
