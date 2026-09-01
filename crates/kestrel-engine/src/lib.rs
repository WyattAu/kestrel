//! `kestrel-engine` — assembles the core engine.
//!
//! Service supervisor (ADR 0004), the command router, the event bus, and
//! lifecycle wiring of `StorageService`, `IndexService`, `SearchService`,
//! `OutboxService`, `CredentialService`, and per-account `SyncService`s.
//! Frontends spawn the engine in-process and interact with it exclusively
//! through the typed message protocol.
//!
//! Phase 1 composition: storage/index/search/config-watcher/GC-scheduler.
//! Sync + outbox + credential services attach in Phase 2 without
//! changing this crate's seams.

use std::{sync::Arc, time::Duration};

use kestrel_core::{
    clock::{Clock, SystemClock},
    config::Config,
    error::KestrelError,
    ids::SystemIdGenerator,
    paths::Paths,
    protocol::{Command, CommandPayload, EngineEvent, FrontendKind, MailProtocol, Reply},
};
use kestrel_storage::{IndexService, SearchService, StorageService};
use kestrel_sync::{OutboxService, SmtpParams, SmtpSecurity};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

pub mod bus;
pub mod filter;
pub mod router;
pub mod snooze_service;
pub mod supervisor;

pub use bus::EventBus;
pub use router::EngineRouter;

/// Command channel capacity (message-protocol §4).
pub const COMMAND_CAPACITY: usize = 256;

/// A spawned engine: the frontend's only handles. `events` is a fresh
/// broadcast receiver per clone (each frontend tracks its own lag).
pub struct EngineHandle {
    /// Bounded command sender (clones share the bounded queue).
    pub commands: mpsc::Sender<Command>,
    /// Event subscription (this instance's receiver).
    events_rx: tokio::sync::broadcast::Receiver<EngineEvent>,
    /// Event sender for cloning fresh receivers.
    events_tx: tokio::sync::broadcast::Sender<EngineEvent>,
    /// Signals full shutdown completion (single-consumption).
    pub done: std::sync::Arc<tokio::sync::Mutex<Option<oneshot::Receiver<()>>>>,
}

impl Clone for EngineHandle {
    fn clone(&self) -> Self {
        Self {
            commands: self.commands.clone(),
            events_rx: self.events_tx.subscribe(),
            events_tx: self.events_tx.clone(),
            done: std::sync::Arc::clone(&self.done),
        }
    }
}

impl EngineHandle {
    /// This handle's event receiver.
    #[must_use]
    pub fn events(&self) -> tokio::sync::broadcast::Receiver<EngineEvent> {
        self.events_rx.resubscribe()
    }
}

/// Engine assembly over the Phase 1 services (ADR 0011).
pub struct Engine;

impl Engine {
    /// Spawns the engine: opens storage, starts index/search, config
    /// watcher, GC scheduler, and the command router.
    ///
    /// # Errors
    /// [`KestrelError`] when storage cannot open (fail fast at startup).
    pub async fn spawn(
        config: Arc<Config>,
        paths: Arc<Paths>,
    ) -> Result<EngineHandle, KestrelError> {
        let store =
            kestrel_crypto::resolve_credential_store().map_err(|e| KestrelError::StorageIo {
                detail: e.to_string(),
            })?;
        Self::spawn_with(
            config,
            paths,
            Arc::new(SystemIdGenerator),
            Arc::new(SystemClock),
            store,
        )
        .await
    }

    /// [`Engine::spawn`] with injected id/clock/sources (tests, embedded).
    ///
    /// # Errors
    /// [`KestrelError`] when storage cannot open.
    #[allow(clippy::too_many_lines)]
    pub async fn spawn_with(
        config: Arc<Config>,
        paths: Arc<Paths>,
        ids: Arc<dyn kestrel_core::ids::IdGenerator>,
        clock: Arc<dyn Clock>,
        store: Arc<dyn kestrel_crypto::CredentialStore>,
    ) -> Result<EngineHandle, KestrelError> {
        let _span = tracing::info_span!("engine").entered();
        paths.ensure().map_err(|e| KestrelError::StorageIo {
            detail: e.to_string(),
        })?;

        let (storage, storage_cancel) =
            StorageService::spawn((*paths).clone(), Arc::clone(&ids), Arc::clone(&clock));
        // Fail fast if the databases cannot open.
        storage.list_accounts().await?;

        let index = IndexService::spawn(&paths.index_dir(), storage.clone(), Arc::clone(&clock))
            .map_err(KestrelError::from)?;
        let search = SearchService::from_index(&index, storage.clone());
        if let Err(e) = index.validate().await {
            tracing::warn!(error = %e, "index failed validation; rebuilding");
            let _ = index.rebuild().await;
        }

        let bus = EventBus::new();
        let events_tx = bus.inner_sender();
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (done_tx, done_rx) = oneshot::channel();

        let engine_cancel = CancellationToken::new();

        // Config watcher (ADR 0006): publishes ConfigUpdated on the bus.
        let watcher_sink = {
            let (tx, mut rx) = mpsc::channel(16);
            let bus_clone = bus.clone();
            tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    bus_clone.publish(event);
                }
            });
            tx
        };
        let watcher_paths = Arc::clone(&paths);
        let watcher_cancel = engine_cancel.child_token();
        tokio::spawn(async move {
            if let Err(e) =
                kestrel_core::config::watch_config(watcher_paths, watcher_cancel, watcher_sink)
                    .await
            {
                tracing::warn!(error = %e, "config watcher stopped");
            }
        });

        // Catch-up: index anything left pending (crash between DB write and
        // index commit — schema.md §5).
        {
            let storage = storage.clone();
            let index = index.clone();
            tokio::spawn(async move {
                loop {
                    let Ok(pending) = storage.pending_index(256).await else {
                        break;
                    };
                    if pending.is_empty() {
                        break;
                    }
                    let docs: Vec<_> = pending
                        .iter()
                        .map(kestrel_storage::IndexDoc::from_pending)
                        .collect();
                    index.add_fire_and_forget(docs).await;
                }
            });
        }

        // GC scheduler: mark hourly; sweep with the configured grace
        // (schema.md §4.3). Silent by design (tracing only).
        {
            let storage = storage.clone();
            let clock = Arc::clone(&clock);
            let cancel = engine_cancel.child_token();
            let grace = Duration::from_secs(3600 * config.storage.blob_gc_grace_hours);
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(Duration::from_hours(1));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                // First sweep shortly after start (startup residue).
                tokio::time::sleep(Duration::from_secs(30)).await;
                loop {
                    tokio::select! {
                        () = cancel.cancelled() => break,
                        _ = ticker.tick() => {
                            let now = clock.now_unix_ms();
                            match storage.gc_mark(now).await {
                                Ok(marked) if marked > 0 => {
                                    tracing::debug!(marked, "gc mark");
                                    if let Ok(swept) =
                                        storage.gc_sweep(now, i64::try_from(grace.as_millis()).unwrap_or(i64::MAX)).await
                                    {
                                        tracing::debug!(swept, "gc sweep");
                                    }
                                }
                                Ok(_) => {}
                                Err(e) => tracing::warn!(error = %e, "gc mark failed"),
                            }
                        }
                    }
                }
            });
        }

        // Snooze expiry service: polls for due snoozes every 60s.
        {
            let storage_for_snooze: std::sync::Arc<dyn kestrel_core::store_model::MailStore> =
                std::sync::Arc::new(storage.clone());
            let snooze_cancel = engine_cancel.child_token();
            let snooze_service = snooze_service::SnoozeService::new(
                storage_for_snooze,
                Arc::clone(&clock),
                bus.inner_sender(),
            );
            let snooze_span = tracing::info_span!("snooze");
            tokio::spawn(
                async move { snooze_service.run(snooze_cancel).await }.instrument(snooze_span),
            );
        }

        // Filter service: evaluates rules on incoming mail.
        {
            let filter_cancel = engine_cancel.child_token();
            let filter_service =
                filter::FilterService::new(storage.clone(), Arc::clone(&clock), bus.inner_sender());
            let filter_span = tracing::info_span!("filter");
            tokio::spawn(
                async move { filter_service.run(filter_cancel).await }.instrument(filter_span),
            );
        }

        // Router.
        let creds = std::sync::Arc::new(kestrel_crypto::CredentialService::new(store));
        let router = EngineRouter::new(
            config,
            storage.clone(),
            search,
            bus.clone(),
            Arc::clone(&ids),
            Arc::clone(&clock),
            std::sync::Arc::clone(&creds),
        );

        // Spawn JMAP sync + outbox services for existing accounts.
        if let Ok(accounts) = storage.list_accounts().await {
            for acct in &accounts {
                if acct.protocol == MailProtocol::Jmap {
                    let token = creds
                        .password(acct.id)
                        .unwrap_or_default()
                        .unwrap_or_else(|| kestrel_core::secrets::SecretString::new(String::new()));
                    let host = acct.host.clone();
                    let store: std::sync::Arc<dyn kestrel_core::store_model::MailStore> =
                        std::sync::Arc::new(storage.clone());
                    let (ev_tx, mut ev_rx) = mpsc::channel(64);
                    let bus_clone = bus.clone();
                    tokio::spawn(async move {
                        while let Some(ev) = ev_rx.recv().await {
                            bus_clone.publish(ev);
                        }
                    });
                    let cancel = engine_cancel.child_token();
                    kestrel_sync::JmapSyncService::spawn(
                        acct.id,
                        host,
                        token,
                        store,
                        Arc::clone(&clock),
                        ev_tx,
                        cancel,
                    );
                }

                // Spawn outbox service for accounts with known provider presets.
                if !matches!(
                    acct.provider,
                    kestrel_core::protocol::Provider::Generic
                        | kestrel_core::protocol::Provider::Jmap
                ) {
                    let preset =
                        kestrel_core::provider::provider_preset(&acct.provider, &acct.email);
                    let secret = creds
                        .password(acct.id)
                        .unwrap_or_default()
                        .unwrap_or_else(|| kestrel_core::secrets::SecretString::new(String::new()));
                    let smtp_params = SmtpParams {
                        host: preset.smtp_host.clone(),
                        port: preset.smtp_port,
                        username: preset
                            .username
                            .clone()
                            .unwrap_or_else(|| preset.email.clone()),
                        secret: secret.clone(),
                        oauth2: preset.auth_kind == "oauth2",
                        security: match preset.smtp_security.as_str() {
                            "starttls" => SmtpSecurity::StartTls,
                            _ => SmtpSecurity::ImplicitTls,
                        },
                    };
                    let imap_security = match preset.imap_security.as_str() {
                        "starttls" => kestrel_sync::Security::StartTls,
                        _ => kestrel_sync::Security::Tls,
                    };
                    let imap_connect = kestrel_sync::ConnectParams {
                        host: preset.imap_host.clone(),
                        port: preset.imap_port,
                        security: imap_security,
                        username: preset
                            .username
                            .clone()
                            .unwrap_or_else(|| preset.email.clone()),
                        secret,
                        mechanisms: vec![kestrel_core::sasl::SaslMechanism::Plain],
                        tls: tokio_rustls::TlsConnector::from(
                            kestrel_crypto::tls_config(None).map_err(KestrelError::from)?,
                        ),
                        sasl_factory: std::sync::Arc::new(|mech, user, secret| {
                            kestrel_crypto::sasl::start(mech, user, secret)
                        }),
                    };
                    let online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
                    let (outbox_ev_tx, mut outbox_ev_rx) = mpsc::channel(64);
                    let outbox_bus_clone = bus.clone();
                    tokio::spawn(async move {
                        while let Some(ev) = outbox_ev_rx.recv().await {
                            outbox_bus_clone.publish(ev);
                        }
                    });
                    let outbox_store: std::sync::Arc<dyn kestrel_core::store_model::MailStore> =
                        std::sync::Arc::new(storage.clone());
                    let outbox_cancel = engine_cancel.child_token();
                    let outbox = OutboxService::new(
                        outbox_store,
                        smtp_params,
                        imap_connect,
                        Arc::clone(&clock),
                        outbox_ev_tx,
                        online,
                    );
                    let outbox_span = tracing::info_span!("outbox", account = %acct.id);
                    tokio::spawn(
                        async move { outbox.run(outbox_cancel).await }.instrument(outbox_span),
                    );
                }
            }
        }
        let router_cancel = engine_cancel.child_token();
        let router_done = done_tx;
        tokio::spawn(async move {
            router.run(command_rx, router_cancel, storage_cancel).await;
            let _ = router_done.send(());
        });

        Ok(EngineHandle {
            commands: command_tx,
            events_rx: events_tx.subscribe(),
            events_tx,
            done: std::sync::Arc::new(tokio::sync::Mutex::new(Some(done_rx))),
        })
    }
}

/// Convenience: builds a [`Command`] with a fresh request id.
#[must_use]
pub fn command(origin: FrontendKind, payload: CommandPayload) -> Command {
    Command {
        id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
        origin,
        payload,
    }
}

/// Convenience: a reply oneshot pair.
#[must_use]
pub fn reply_channel<T>() -> (oneshot::Sender<Reply>, oneshot::Receiver<Reply>) {
    oneshot::channel()
}
