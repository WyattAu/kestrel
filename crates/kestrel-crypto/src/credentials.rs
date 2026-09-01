//! Credential storage (threat model §4.8): OS keyring mandatory where
//! available; **plaintext fallback is refused, not warned about**.
//! Secrets live in [`SecretString`] (zeroized on drop) and are never
//! logged, never in `SQLite`, never in config (ADR 0008).

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use kestrel_core::{ids::AccountId, secrets::SecretString};

use crate::{
    error::{CryptoError, CryptoResult},
    openpgp,
};

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
pub struct CredentialService {
    store: Arc<dyn CredentialStore>,
}

impl CredentialService {
    /// Wraps a store.
    #[must_use]
    pub fn new(store: Arc<dyn CredentialStore>) -> Self {
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

    /// Stores an `OAuth2` refresh token from a plain string.
    ///
    /// # Errors
    /// Backend failure.
    pub fn store_refresh_token(&self, account: AccountId, token: &str) -> CryptoResult<()> {
        self.store.save(
            account,
            "oauth_refresh",
            &SecretString::new(token.to_owned()),
        )
    }

    /// Loads the `OAuth2` refresh token as a plain string.
    ///
    /// # Errors
    /// Backend failure.
    pub fn get_refresh_token(&self, account: AccountId) -> CryptoResult<Option<String>> {
        self.store
            .load(account, "oauth_refresh")
            .map(|opt| opt.map(|s| s.expose().to_owned()))
    }

    /// Deletes every credential of an account (account removal path).
    ///
    /// # Errors
    /// Backend failure.
    pub fn purge(&self, account: AccountId) -> CryptoResult<()> {
        self.store.delete(account, "password")?;
        self.store.delete(account, "oauth_refresh")?;
        self.store.delete(account, "pgp_secret_key")?;
        self.store.delete(account, "pgp_secret_password")?;
        self.store.delete(account, "pgp_public_keys")
    }

    /// Stores the user's secret PGP cert (armored) and optional key password.
    ///
    /// # Errors
    /// Backend failure.
    pub fn set_pgp_secret_cert(
        &self,
        account: AccountId,
        armored_cert: &str,
        password: Option<&SecretString>,
    ) -> CryptoResult<()> {
        self.store.save(
            account,
            "pgp_secret_key",
            &SecretString::new(armored_cert.to_owned()),
        )?;
        if let Some(pw) = password {
            self.store.save(account, "pgp_secret_password", pw)?;
        }
        Ok(())
    }

    /// Loads and parses the user's secret PGP cert for the given account.
    ///
    /// Returns `Ok(None)` when no key is configured.
    ///
    /// # Errors
    /// Backend failure or invalid cert data.
    pub fn pgp_secret_cert(
        &self,
        account: AccountId,
    ) -> CryptoResult<Option<sequoia_openpgp::Cert>> {
        let Some(armored) = self.store.load(account, "pgp_secret_key")? else {
            return Ok(None);
        };
        let cert = openpgp::parse_cert(armored.expose())
            .map_err(|e| CryptoError::OpenPgp(format!("pgp secret cert: {e}")))?;
        Ok(Some(cert))
    }

    /// Returns the password for the account's PGP secret key, if stored.
    ///
    /// # Errors
    /// Backend failure.
    pub fn pgp_secret_password(&self, account: AccountId) -> CryptoResult<Option<SecretString>> {
        self.store.load(account, "pgp_secret_password")
    }

    /// Stores a public PGP key (armored cert) for a recipient address.
    ///
    /// Multiple keys per address are supported (appended with a separator).
    ///
    /// # Errors
    /// Backend failure.
    pub fn add_pgp_public_key(&self, address: &str, armored_cert: &str) -> CryptoResult<()> {
        let key = format!("pgp_pub:{address}");
        let existing = self
            .store
            .load(AccountId::from_uuid(uuid::Uuid::nil()), &key)?;
        let value = match existing {
            Some(prev) => format!("{}|||{}", prev.expose(), armored_cert),
            None => armored_cert.to_owned(),
        };
        self.store.save(
            AccountId::from_uuid(uuid::Uuid::nil()),
            &key,
            &SecretString::new(value),
        )
    }

    /// Loads all public PGP certs for the given recipient addresses.
    ///
    /// # Errors
    /// Backend failure or invalid cert data.
    pub fn pgp_recipient_certs(
        &self,
        to: &[kestrel_core::protocol::Address],
        cc: &[kestrel_core::protocol::Address],
    ) -> CryptoResult<Vec<sequoia_openpgp::Cert>> {
        let nil_account = AccountId::from_uuid(uuid::Uuid::nil());
        let mut certs = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for addr in to.iter().chain(cc.iter()) {
            if !seen.insert(addr.email.as_str()) {
                continue;
            }
            let key = format!("pgp_pub:{}", addr.email);
            if let Some(data) = self.store.load(nil_account, &key)? {
                for part in data.expose().split("|||") {
                    let trimmed = part.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let cert = openpgp::parse_cert(trimmed).map_err(|e| {
                        CryptoError::OpenPgp(format!("pgp public key for {}: {e}", addr.email))
                    })?;
                    certs.push(cert);
                }
            }
        }
        Ok(certs)
    }
}

/// Resolves the credential store for the current environment.
///
/// Returns a `KeyringStore` backed by the OS secret service.
///
/// # Errors
/// Currently infallible.
pub fn resolve_credential_store() -> CryptoResult<Arc<dyn CredentialStore>> {
    Ok(Arc::new(KeyringStore))
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
        let svc = CredentialService::new(Arc::new(InMemoryStore::new()));
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
        let svc = CredentialService::new(Arc::new(InMemoryStore::new()));
        svc.set_password(a, &SecretString::new("p".into())).unwrap();
        svc.set_refresh_token(a, &SecretString::new("t".into()))
            .unwrap();
        assert_eq!(svc.password(a).unwrap().unwrap().expose(), "p");
        assert_eq!(svc.refresh_token(a).unwrap().unwrap().expose(), "t");
    }

    #[test]
    fn store_and_get_refresh_token_roundtrip() {
        let svc = CredentialService::new(Arc::new(InMemoryStore::new()));
        let a = acct();
        assert!(svc.get_refresh_token(a).unwrap().is_none());
        svc.store_refresh_token(a, "refresh-abc").unwrap();
        assert_eq!(
            svc.get_refresh_token(a).unwrap(),
            Some("refresh-abc".to_owned())
        );
    }

    #[test]
    fn store_refresh_token_replaces_previous() {
        let svc = CredentialService::new(Arc::new(InMemoryStore::new()));
        let a = acct();
        svc.store_refresh_token(a, "old-rt").unwrap();
        svc.store_refresh_token(a, "new-rt").unwrap();
        assert_eq!(svc.get_refresh_token(a).unwrap(), Some("new-rt".to_owned()));
    }

    #[test]
    fn refresh_token_isolation_between_accounts() {
        let svc = CredentialService::new(Arc::new(InMemoryStore::new()));
        let a1 = acct();
        let a2 = acct();
        svc.store_refresh_token(a1, "rt-1").unwrap();
        svc.store_refresh_token(a2, "rt-2").unwrap();
        assert_eq!(svc.get_refresh_token(a1).unwrap(), Some("rt-1".to_owned()));
        assert_eq!(svc.get_refresh_token(a2).unwrap(), Some("rt-2".to_owned()));
    }

    #[test]
    fn credential_service_works_with_trait_object() {
        let store: Arc<dyn CredentialStore> = Arc::new(InMemoryStore::new());
        let svc = CredentialService::new(store);
        let a = acct();
        svc.set_password(a, &SecretString::new("trait-object-pw".into()))
            .unwrap();
        assert_eq!(
            svc.password(a).unwrap(),
            Some(SecretString::new("trait-object-pw".into()))
        );
        svc.purge(a).unwrap();
        assert!(svc.password(a).unwrap().is_none());
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
