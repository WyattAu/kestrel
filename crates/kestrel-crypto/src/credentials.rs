//! Credential storage (threat model §4.8): OS keyring mandatory where
//! available; **plaintext fallback is refused, not warned about**.
//! Secrets live in [`SecretString`] (zeroized on drop) and are never
//! logged, never in `SQLite`, never in config (ADR 0008).

use std::{collections::HashMap, sync::RwLock};

use kestrel_core::{ids::AccountId, secrets::SecretString};

use crate::error::{CryptoError, CryptoResult};

/// Keyring service name.
const SERVICE: &str = "kestrel";

/// Credential kinds stored per account.
#[derive(Clone, Debug, PartialEq)]
pub enum Credential {
    /// Account password (for SASL PLAIN/LOGIN/SCRAM).
    Password(SecretString),
    /// `OAuth2` refresh token (token endpoint exchange handled by [`crate::oauth`]).
    RefreshToken(SecretString),
}

/// Storage backend seam (tests inject [`InMemoryStore`]).
pub trait CredentialStore: Send + Sync {
    /// Persists a credential under the account's key.
    ///
    /// # Errors
    /// [`CryptoError::KeyringUnavailable`] when the backend is unusable.
    fn save(&self, account: AccountId, kind: &str, secret: &SecretString) -> CryptoResult<()>;

    /// Loads a credential.
    ///
    /// # Errors
    /// [`CryptoError::KeyringUnavailable`] when the backend is unusable;
    /// `Ok(None)` when absent.
    fn load(&self, account: AccountId, kind: &str) -> CryptoResult<Option<SecretString>>;

    /// Deletes a credential (idempotent).
    ///
    /// # Errors
    /// [`CryptoError::KeyringUnavailable`] when the backend is unusable.
    fn delete(&self, account: AccountId, kind: &str) -> CryptoResult<()>;
}

/// OS keyring backend (Secret Service / Keychain / Credential Manager).
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore;

impl CredentialStore for KeyringStore {
    fn save(&self, account: AccountId, kind: &str, secret: &SecretString) -> CryptoResult<()> {
        let entry = keyring::Entry::new(SERVICE, &entry_user(account, kind))
            .map_err(|e| CryptoError::KeyringUnavailable(e.to_string()))?;
        entry
            .set_password(secret.expose())
            .map_err(|e| CryptoError::KeyringUnavailable(e.to_string()))
    }

    fn load(&self, account: AccountId, kind: &str) -> CryptoResult<Option<SecretString>> {
        let entry = keyring::Entry::new(SERVICE, &entry_user(account, kind))
            .map_err(|e| CryptoError::KeyringUnavailable(e.to_string()))?;
        match entry.get_password() {
            Ok(value) => Ok(Some(SecretString::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CryptoError::KeyringUnavailable(e.to_string())),
        }
    }

    fn delete(&self, account: AccountId, kind: &str) -> CryptoResult<()> {
        let entry = keyring::Entry::new(SERVICE, &entry_user(account, kind))
            .map_err(|e| CryptoError::KeyringUnavailable(e.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CryptoError::KeyringUnavailable(e.to_string())),
        }
    }
}

fn entry_user(account: AccountId, kind: &str) -> String {
    format!("{kind}:{account}")
}

/// In-memory backend for tests and sandboxed environments.
#[derive(Default)]
pub struct InMemoryStore {
    map: RwLock<HashMap<String, String>>,
}

impl InMemoryStore {
    /// New empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored secrets (test assertions).
    pub fn len(&self) -> usize {
        self.map.read().map_or(0, |m| m.len())
    }

    /// Emptiness (test assertions).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl CredentialStore for InMemoryStore {
    fn save(&self, account: AccountId, kind: &str, secret: &SecretString) -> CryptoResult<()> {
        if let Ok(mut map) = self.map.write() {
            map.insert(entry_user(account, kind), secret.expose().to_owned());
        }
        Ok(())
    }

    fn load(&self, account: AccountId, kind: &str) -> CryptoResult<Option<SecretString>> {
        Ok(self
            .map
            .read()
            .ok()
            .and_then(|m| m.get(&entry_user(account, kind)).cloned())
            .map(SecretString::new))
    }

    fn delete(&self, account: AccountId, kind: &str) -> CryptoResult<()> {
        if let Ok(mut map) = self.map.write() {
            map.remove(&entry_user(account, kind));
        }
        Ok(())
    }
}

/// Credential service facade over a store.
pub struct CredentialService<S: CredentialStore> {
    store: S,
}

impl<S: CredentialStore> CredentialService<S> {
    /// Wraps a store.
    #[must_use]
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Stores an account password.
    ///
    /// # Errors
    /// Backend failure.
    pub fn set_password(&self, account: AccountId, password: &SecretString) -> CryptoResult<()> {
        self.store.save(account, "password", password)
    }

    /// Loads the account password.
    ///
    /// # Errors
    /// Backend failure.
    pub fn password(&self, account: AccountId) -> CryptoResult<Option<SecretString>> {
        self.store.load(account, "password")
    }

    /// Stores an `OAuth2` refresh token.
    ///
    /// # Errors
    /// Backend failure.
    pub fn set_refresh_token(&self, account: AccountId, token: &SecretString) -> CryptoResult<()> {
        self.store.save(account, "oauth_refresh", token)
    }

    /// Loads the `OAuth2` refresh token.
    ///
    /// # Errors
    /// Backend failure.
    pub fn refresh_token(&self, account: AccountId) -> CryptoResult<Option<SecretString>> {
        self.store.load(account, "oauth_refresh")
    }

    /// Deletes every credential of an account (account removal path).
    ///
    /// # Errors
    /// Backend failure.
    pub fn purge(&self, account: AccountId) -> CryptoResult<()> {
        self.store.delete(account, "password")?;
        self.store.delete(account, "oauth_refresh")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use uuid::Uuid;

    use super::*;

    fn acct() -> AccountId {
        AccountId::from_uuid(Uuid::now_v7())
    }

    #[test]
    fn in_memory_roundtrip_and_purge() {
        let svc = CredentialService::new(InMemoryStore::new());
        let a = acct();
        svc.set_password(a, &SecretString::new("hunter2".into()))
            .unwrap();
        assert_eq!(
            svc.password(a).unwrap(),
            Some(SecretString::new("hunter2".into()))
        );
        assert!(svc.refresh_token(a).unwrap().is_none());
        svc.set_refresh_token(a, &SecretString::new("rt".into()))
            .unwrap();
        svc.purge(a).unwrap();
        assert!(svc.password(a).unwrap().is_none());
        assert!(svc.refresh_token(a).unwrap().is_none());
    }

    #[test]
    fn secrets_are_masked_in_debug() {
        let s = SecretString::new("super-secret".into());
        assert_eq!(format!("{s:?}"), "SecretString(***)");
    }

    #[test]
    fn kinds_are_namespaced() {
        let a = acct();
        let svc = CredentialService::new(InMemoryStore::new());
        svc.set_password(a, &SecretString::new("p".into())).unwrap();
        svc.set_refresh_token(a, &SecretString::new("t".into()))
            .unwrap();
        assert_eq!(svc.password(a).unwrap().unwrap().expose(), "p");
        assert_eq!(svc.refresh_token(a).unwrap().unwrap().expose(), "t");
    }

    #[test]
    fn keyring_store_reports_unavailable_without_dbus() {
        // In environments without Secret Service, every op maps to the
        // typed KeyringUnavailable error (never a panic, never plaintext).
        let a = acct();
        let result = KeyringStore.save(a, "password", &SecretString::new("x".into()));
        match result {
            Ok(()) => {
                // A keyring backend IS available in this environment; then
                // load must round-trip.
                assert!(KeyringStore.load(a, "password").unwrap().is_some());
                KeyringStore.delete(a, "password").unwrap();
            }
            Err(CryptoError::KeyringUnavailable(_)) => {}
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
}
