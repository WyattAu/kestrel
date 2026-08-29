//! Zeroized secret carrier (threat model §4.8): secrets never appear in
//! Debug output, logs, or `SQLite`. Implementation detail of
//! `kestrel-crypto`, vocabulary lives here so all crates can carry secrets
//! without lateral imports.

use zeroize::{Zeroize, ZeroizeOnDrop};

/// A zeroized-on-drop secret string.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SecretString(String);

impl SecretString {
    /// Wraps an owned secret.
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Reveals the secret.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretString(***)")
    }
}

impl PartialEq for SecretString {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
