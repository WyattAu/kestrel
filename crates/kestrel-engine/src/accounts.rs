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
    protocol::{EngineEvent, ServiceId},
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
    let account = spec.account;
    let cancel = engine_stop.child_token();
    let handle = AccountHandle {
        account,
        cancel: cancel.clone(),
        trigger: Arc::new(Notify::new()),
    };
    replace_account(registry, handle.clone()).await;

    let clock = Arc::clone(&spec.clock);

    if let Some((host, token)) = spec.jmap {
        let service = Arc::new(
            JmapSyncService::new(
                account,
                host,
                token,
                Arc::clone(&spec.store),
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
                async move {
                    service.run(attempt).await;
                    Ok(())
                }
            },
        );
    }

    if let Some(connect) = spec.imap {
        let service = Arc::new(
            SyncService::new(
                account,
                connect,
                Arc::clone(&spec.store),
                Arc::clone(&spec.cfg),
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

    if let Some((smtp, imap)) = spec.outbox {
        let online = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let outbox = Arc::new(OutboxService::new(
            Arc::clone(&spec.store),
            smtp,
            imap,
            Arc::clone(&clock),
            bus_forwarder(&bus),
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

    handle
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
