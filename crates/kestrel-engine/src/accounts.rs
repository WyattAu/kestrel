//! Per-account background-service lifecycle (ADR 0004; backlog #1/#2).
//!
//! One spawn path (`start_account_services`) starts JMAP sync, IMAP sync,
//! and the outbox flusher under the supervisor for an account, and one
//! shared registry tracks every account's services regardless of whether it
//! was added in-session (`AddAccount`) or resumed at startup. The router
//! uses the registry to stop services (`RemoveAccount`, `UpdateAccount`,
//! shutdown drain) and to trigger an immediate sync (`TriggerSync`), so all
//! accounts behave the same no matter how they came to life.

use std::{collections::HashMap, sync::Arc};

use kestrel_core::{
    clock::Clock,
    config::Config,
    ids::AccountId,
    protocol::{ConnectionState, EngineEvent, ServiceId},
    secrets::SecretString,
    store_model::MailStore,
};
use kestrel_sync::{ConnectParams, JmapSyncService, OutboxService, SmtpParams, SyncService};
use tokio::sync::{Mutex, Notify, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::{bus::EventBus, supervisor::spawn_supervised};

/// Shared per-account service registry (router + startup resume loop).
pub(crate) type AccountRegistry = Mutex<HashMap<AccountId, AccountHandle>>;

/// Handle to one account's supervised background services.
#[derive(Clone)]
pub(crate) struct AccountHandle {
    pub account: AccountId,
    /// Stops the account's services (`RemoveAccount` / `UpdateAccount` /
    /// shutdown drain).
    pub cancel: CancellationToken,
    /// Fired by `Command::TriggerSync` to force an immediate sync cycle.
    pub trigger: Arc<Notify>,
}

/// Creates an empty registry.
#[must_use]
pub(crate) fn new_registry() -> Arc<AccountRegistry> {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Registers a handle, cancelling any previous handle for the same account
/// (restart semantics: a replacement must not leave the old services
/// running alongside the new ones).
pub(crate) async fn replace_account(registry: &AccountRegistry, handle: AccountHandle) {
    if let Some(previous) = registry.lock().await.insert(handle.account, handle) {
        previous.cancel.cancel();
    }
}

/// Removes and cancels an account's services, returning its handle (if any).
pub(crate) async fn stop_account(
    registry: &AccountRegistry,
    account: AccountId,
) -> Option<AccountHandle> {
    let handle = registry.lock().await.remove(&account);
    if let Some(h) = &handle {
        h.cancel.cancel();
    }
    handle
}

/// Everything needed to start one account's background services.
pub(crate) struct AccountServicesSpec {
    pub account: AccountId,
    /// Provider family (resolves the `OAuth2` refresh-worker preset).
    pub provider: kestrel_core::protocol::Provider,
    /// `"oauth2"` when the account authenticates with `OAuth2` (drives the
    /// unattended refresh worker).
    pub is_oauth2: bool,
    /// Credential store (shared with the refresh worker).
    pub creds: Arc<kestrel_crypto::CredentialService>,
    /// JMAP sync (session URL host + bearer token) — `Some` for JMAP
    /// accounts.
    pub jmap: Option<(String, SecretString)>,
    /// IMAP sync connect params — `Some` for IMAP accounts with a host.
    pub imap: Option<ConnectParams>,
    /// Outbox flusher (SMTP params + IMAP connect for server-presence
    /// checks) — `Some` when the account can send.
    pub outbox: Option<(SmtpParams, ConnectParams)>,
    /// Storage RPC handle (single-writer actor; ADR 0009).
    pub store: Arc<dyn MailStore>,
    pub clock: Arc<dyn Clock>,
    pub cfg: Arc<Config>,
}

/// Starts (or restarts) one account's supervised services and registers its
/// handle. Creates the account cancellation token as a child of
/// `engine_stop` and spawns JMAP sync / IMAP sync / outbox as needed under
/// the supervisor. Callers that need to trigger or stop the account read
/// the handle back from the registry.
pub(crate) async fn start_account_services(
    registry: &AccountRegistry,
    bus: EventBus,
    engine_stop: CancellationToken,
    spec: AccountServicesSpec,
) -> AccountHandle {
    let AccountServicesSpec {
        account,
        provider,
        is_oauth2,
        creds,
        jmap,
        imap,
        outbox,
        store,
        clock,
        cfg,
    } = spec;
    let cancel = engine_stop.child_token();
    let handle = AccountHandle {
        account,
        cancel: cancel.clone(),
        trigger: Arc::new(Notify::new()),
    };
    replace_account(registry, handle.clone()).await;

    if let Some((host, token)) = jmap {
        start_jmap_sync(
            account,
            host,
            token,
            Arc::clone(&store),
            Arc::clone(&clock),
            &bus,
            &cancel,
            &handle,
        );
    }

    let mut smtp_cell: Option<Arc<std::sync::RwLock<Option<SecretString>>>> = None;
    if let Some(connect) = imap {
        // OAuth2 accounts get the unattended refresh worker (#26): it
        // refreshes before expiry and publishes tokens into a shared cell
        // both IMAP and SMTP read per connect/submit.
        let (connect, secret_cell) = connect.with_shared_secret();
        if is_oauth2 {
            spawn_refresh_worker(
                account,
                provider,
                secret_cell.clone(),
                Arc::clone(&creds),
                bus.clone(),
                Arc::clone(&clock),
                cancel.clone(),
            );
            smtp_cell = Some(secret_cell);
        }
        let service = Arc::new(
            SyncService::new(
                account,
                connect,
                Arc::clone(&store),
                Arc::clone(&cfg),
                Arc::clone(&clock),
                bus_forwarder(&bus),
            )
            .with_trigger(Arc::clone(&handle.trigger)),
        );
        spawn_supervised(
            ServiceId::Sync(account),
            bus.clone(),
            cancel.clone(),
            move |attempt| {
                let service = Arc::clone(&service);
                let span = tracing::info_span!("sync", account = %account);
                async move {
                    service.run(attempt).await;
                    Ok(())
                }
                .instrument(span)
            },
        );
    }

    if let Some((smtp, imap)) = outbox {
        start_outbox_flusher(account, smtp, imap, smtp_cell, store, clock, &bus, cancel);
    }

    handle
}

/// Starts the JMAP sync service for one account.
#[allow(clippy::too_many_arguments)]
fn start_jmap_sync(
    account: AccountId,
    host: String,
    token: SecretString,
    store: Arc<dyn MailStore>,
    clock: Arc<dyn Clock>,
    bus: &EventBus,
    cancel: &CancellationToken,
    handle: &AccountHandle,
) {
    let service = Arc::new(
        JmapSyncService::new(account, host, token, store, clock, bus_forwarder(bus))
            .with_trigger(Arc::clone(&handle.trigger)),
    );
    spawn_supervised(
        ServiceId::Sync(account),
        bus.clone(),
        cancel.clone(),
        move |attempt| {
            let service = Arc::clone(&service);
            async move {
                service.run(attempt).await;
                Ok(())
            }
        },
    );
}

/// Starts the outbox flusher for one account. A shared secret cell (only
/// present for `OAuth2` accounts, where the refresh worker publishes fresh
/// tokens) is attached to the SMTP params so submissions authenticate with
/// the current access token.
#[allow(clippy::too_many_arguments)]
fn start_outbox_flusher(
    account: AccountId,
    smtp: SmtpParams,
    imap: ConnectParams,
    smtp_cell: Option<Arc<std::sync::RwLock<Option<SecretString>>>>,
    store: Arc<dyn MailStore>,
    clock: Arc<dyn Clock>,
    bus: &EventBus,
    cancel: CancellationToken,
) {
    let smtp = match smtp_cell {
        Some(cell) => smtp.with_secret_cell(cell),
        None => smtp,
    };
    let online = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let outbox = Arc::new(OutboxService::new(
        store,
        smtp,
        imap,
        clock,
        bus_forwarder(bus),
        online,
    ));
    spawn_supervised(ServiceId::Outbox, bus.clone(), cancel, move |attempt| {
        let service = Arc::clone(&outbox);
        let span = tracing::info_span!("outbox", account = %account);
        async move {
            service.run(attempt).await;
            Ok(())
        }
        .instrument(span)
    });
}

/// Spawns the unattended `OAuth2` refresh worker for one account (#26).
///
/// Supervised like the other services, except a `Rejected` refresh is
/// terminal-by-design: the worker emits `NeedsReauth` and stops cleanly
/// (`Ok(())`) instead of being restarted into a guaranteed-failure loop.
fn spawn_refresh_worker(
    account: AccountId,
    provider: kestrel_core::protocol::Provider,
    secret_cell: Arc<std::sync::RwLock<Option<SecretString>>>,
    creds: Arc<kestrel_crypto::CredentialService>,
    bus: EventBus,
    clock: Arc<dyn Clock>,
    stop: CancellationToken,
) {
    let spawned = tokio::spawn(async move {
        // The worker's own token: a child of the account token, so
        // RemoveAccount/shutdown stop it too.
        let cancel = stop.child_token();
        let spec = kestrel_crypto::oauth::RefreshWorkerSpec {
            http: match kestrel_crypto::oauth::shared_http_client() {
                Ok(http) => http,
                Err(e) => {
                    tracing::error!(account = %account, error = %e, "refresh worker: http client");
                    return;
                }
            },
            provider: match kestrel_crypto::oauth::provider_from_env(&provider) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(account = %account, error = %e, "refresh worker: provider");
                    return;
                }
            },
            creds,
            account,
            clock,
            secret_cell,
            initial_expires_at: None,
            cfg: kestrel_crypto::oauth::RefreshWorkerConfig::default(),
        };
        let outcome = kestrel_crypto::oauth::refresh_worker(spec, cancel).await;
        if outcome == kestrel_crypto::oauth::RefreshWorkerOutcome::Rejected {
            tracing::warn!(
                account = %account,
                "OAuth2 refresh token rejected: account needs re-authentication"
            );
            bus.publish(EngineEvent::AccountConnection {
                account,
                state: ConnectionState::NeedsReauth,
            });
        }
    });
    drop(spawned); // detached by design; supervision is via NeedsReauth, not restart
}

/// One mpsc→broadcast bridge task per spawned service. Services take an
/// `mpsc::Sender<EngineEvent>` so `kestrel-sync` stays decoupled from the
/// engine's broadcast bus type.
fn bus_forwarder(bus: &EventBus) -> mpsc::Sender<EngineEvent> {
    let (tx, mut rx) = mpsc::channel(64);
    let bus = bus.clone();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            bus.publish(event);
        }
    });
    tx
}

#[cfg(test)]
mod tests {
    use kestrel_core::ids::AccountId;
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn registry_replace_cancels_the_previous_handle() {
        let registry = new_registry();
        let account = AccountId::from_uuid(Uuid::now_v7());

        let first = AccountHandle {
            account,
            cancel: CancellationToken::new(),
            trigger: Arc::new(Notify::new()),
        };
        replace_account(&registry, first.clone()).await;
        assert!(!first.cancel.is_cancelled());

        // A restart (UpdateAccount) replaces the handle and cancels the old
        // services so they cannot overlap the new ones.
        let second = AccountHandle {
            account,
            cancel: CancellationToken::new(),
            trigger: Arc::new(Notify::new()),
        };
        replace_account(&registry, second.clone()).await;

        assert!(first.cancel.is_cancelled());
        assert!(!second.cancel.is_cancelled());
        assert_eq!(registry.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn stop_account_removes_and_cancels() {
        let registry = new_registry();
        let account = AccountId::from_uuid(Uuid::now_v7());
        let handle = AccountHandle {
            account,
            cancel: CancellationToken::new(),
            trigger: Arc::new(Notify::new()),
        };
        replace_account(&registry, handle).await;

        let removed = stop_account(&registry, account).await;
        assert!(removed.is_some());
        assert!(registry.lock().await.is_empty());
        assert!(stop_account(&registry, account).await.is_none());
    }
}
