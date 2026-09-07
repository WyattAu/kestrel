//! `kestrel-engine` — assembles the core engine.
//!
//! Service supervisor (ADR 0004), the command router, the event bus, and
//! lifecycle wiring of `StorageService`, `IndexService`, `SearchService`,
//! `OutboxService`, `CredentialService`, per-account IMAP/JMAP sync
//! services, `FilterService`, the snooze poller, and the GC scheduler.
//! Every long-running task runs under [`supervisor::spawn_supervised`], so
//! a panic or `Err` emits `ServiceDegraded` and restarts with backoff.
//! Frontends spawn the engine in-process and interact with it exclusively
//! through the typed message protocol.
//!
//! Per-account sync/outbox attach for accounts added via the router, and at
//! startup for stored JMAP accounts, generic-IMAP accounts (when the stored
//! account carries an IMAP host) and preset-provider outboxes. Only
//! `Command::TriggerSync` remains a documented no-op
//! (`docs/roadmap.md` “Known gaps”).

use std::{sync::Arc, time::Duration};

use kestrel_core::{
    clock::{Clock, SystemClock},
    config::Config,
    error::KestrelError,
    ids::SystemIdGenerator,
    paths::Paths,
    protocol::{
        Command, CommandPayload, EngineEvent, FrontendKind, MailProtocol, Reply, ServiceId,
    },
};
use kestrel_storage::{IndexService, SearchService, StorageHandle, StorageService};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

pub mod accounts;
pub mod bus;
pub mod filter;
pub mod router;
pub mod snooze_service;
pub mod supervisor;

pub use bus::EventBus;
pub use router::EngineRouter;

use crate::supervisor::spawn_supervised;

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
    /// Engine-scope cancellation token; cancelling it stops every
    /// supervised background service (architecture §3.3).
    engine_cancel: CancellationToken,
    /// Signals full shutdown completion (single-consumption).
    pub done: std::sync::Arc<tokio::sync::Mutex<Option<oneshot::Receiver<()>>>>,
}

impl Clone for EngineHandle {
    fn clone(&self) -> Self {
        Self {
            commands: self.commands.clone(),
            events_rx: self.events_tx.subscribe(),
            events_tx: self.events_tx.clone(),
            engine_cancel: self.engine_cancel.clone(),
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

    /// Requests an ordered engine shutdown (architecture §3.3): the router
    /// stops accepting commands, supervised background services cancel,
    /// the outbox performs a bounded final flush when `drain` is set, and
    /// storage checkpoints. Returns once the engine publishes `Done`.
    ///
    /// Idempotent: the completion signal is consumed once; later calls
    /// return immediately.
    pub async fn shutdown(&self, drain: bool) {
        let _ = self
            .commands
            .send(Command {
                id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                // Cosmetic for lifecycle commands (never echoed).
                origin: FrontendKind::Gui,
                payload: CommandPayload::Shutdown { drain },
            })
            .await;
        // Ensure the router's run loop exits even if the command channel
        // is already closed (frontend dropped its sender).
        self.engine_cancel.cancel();
        if let Some(rx) = self.done.lock().await.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(12), rx).await;
        }
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
        let watcher_stop = engine_cancel.child_token();
        spawn_supervised(
            ServiceId::Config,
            bus.clone(),
            watcher_stop,
            move |attempt| {
                let paths = Arc::clone(&watcher_paths);
                let sink = watcher_sink.clone();
                async move { kestrel_core::config::watch_config(paths, attempt, sink).await }
            },
        );

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
        // (schema.md §4.3). Supervised so a panic restarts the scheduler
        // and surfaces as `ServiceDegraded(Maintenance)`; runtime failures
        // are logged inside the loop and never fatal.
        {
            let storage = storage.clone();
            let clock = Arc::clone(&clock);
            let stop = engine_cancel.child_token();
            let grace = Duration::from_secs(3600 * config.storage.blob_gc_grace_hours);
            spawn_supervised(ServiceId::Maintenance, bus.clone(), stop, move |attempt| {
                let storage = storage.clone();
                let clock = Arc::clone(&clock);
                async move { gc_scheduler(storage, clock, grace, attempt).await }
            });
        }

        // Snooze expiry service: polls for due snoozes every 60s.
        {
            let storage_for_snooze: Arc<dyn kestrel_core::store_model::MailStore> =
                std::sync::Arc::new(storage.clone());
            let snooze_stop = engine_cancel.child_token();
            let snooze_service = std::sync::Arc::new(snooze_service::SnoozeService::new(
                storage_for_snooze,
                Arc::clone(&clock),
                bus.inner_sender(),
            ));
            spawn_supervised(
                ServiceId::Snooze,
                bus.clone(),
                snooze_stop,
                move |attempt| {
                    let service = Arc::clone(&snooze_service);
                    let span = tracing::info_span!("snooze");
                    async move {
                        service.run(attempt).await;
                        Ok(())
                    }
                    .instrument(span)
                },
            );
        }

        // Filter service: evaluates rules on incoming mail.
        {
            let filter_stop = engine_cancel.child_token();
            let filter_service = std::sync::Arc::new(filter::FilterService::new(
                storage.clone(),
                Arc::clone(&clock),
                bus.inner_sender(),
            ));
            spawn_supervised(
                ServiceId::Filter,
                bus.clone(),
                filter_stop,
                move |attempt| {
                    let service = Arc::clone(&filter_service);
                    let span = tracing::info_span!("filter");
                    async move {
                        service.run(attempt).await;
                        Ok(())
                    }
                    .instrument(span)
                },
            );
        }

        // Router.
        let creds = std::sync::Arc::new(kestrel_crypto::CredentialService::new(store));
        let startup_config = Arc::clone(&config);
        // Shared per-account service registry: the startup resume loop below
        // and the router's add/update/remove/TriggerSync all use it, so every
        // account behaves the same however it came to life.
        let registry = accounts::new_registry();
        let router = EngineRouter::new(
            config,
            storage.clone(),
            search,
            bus.clone(),
            Arc::clone(&ids),
            Arc::clone(&clock),
            std::sync::Arc::clone(&creds),
            Arc::clone(&registry),
            engine_cancel.clone(),
        );

        // Resume per-account services for stored accounts (JMAP sync, IMAP
        // sync, preset-provider outbox) through the same supervised
        // lifecycle the router uses for in-session adds (accounts.rs; #2).
        if let Ok(accounts) = storage.list_accounts().await {
            for acct in &accounts {
                let Some(spec) =
                    startup_spec_for(acct, &creds, &storage, &clock, Arc::clone(&startup_config))
                else {
                    continue;
                };
                accounts::start_account_services(
                    &registry,
                    bus.clone(),
                    engine_cancel.clone(),
                    spec,
                )
                .await;
            }
        }
        let router_cancel = engine_cancel.child_token();
        let router_done = done_tx;
        let handle_cancel = engine_cancel.clone();
        tokio::spawn(async move {
            router
                .run(command_rx, router_cancel, engine_cancel, storage_cancel)
                .await;
            let _ = router_done.send(());
        });

        Ok(EngineHandle {
            commands: command_tx,
            events_rx: events_tx.subscribe(),
            events_tx,
            engine_cancel: handle_cancel,
            done: std::sync::Arc::new(tokio::sync::Mutex::new(Some(done_rx))),
        })
    }
}

/// Resolves the services to resume for one stored account (startup path).
///
/// Stored account rows keep only provider/email/host, so the provider
/// preset fills in the connection details (the in-session router path uses
/// the full `AccountConfig` instead). Returns `None` when the account has
/// neither an IMAP host to sync nor a preset SMTP route to flush.
#[allow(clippy::too_many_lines)]
fn startup_spec_for(
    acct: &kestrel_core::protocol::AccountSummary,
    creds: &kestrel_crypto::CredentialService,
    storage: &StorageHandle,
    clock: &Arc<dyn Clock>,
    cfg: Arc<Config>,
) -> Option<accounts::AccountServicesSpec> {
    let secret = creds
        .password(acct.id)
        .unwrap_or_default()
        .unwrap_or_else(|| kestrel_core::secrets::SecretString::new(String::new()));
    let store: Arc<dyn kestrel_core::store_model::MailStore> = Arc::new(storage.clone());
    let tls = tokio_rustls::TlsConnector::from(kestrel_crypto::tls_config(None).ok()?);
    let sasl_factory: kestrel_sync::SaslFactory =
        std::sync::Arc::new(|mech, user, secret| kestrel_crypto::sasl::start(mech, user, secret));

    let is_jmap = acct.protocol == MailProtocol::Jmap;
    let preset = kestrel_core::provider::provider_preset(&acct.provider, &acct.email);
    let host = if preset.imap_host.is_empty() {
        acct.host.clone()
    } else {
        preset.imap_host.clone()
    };
    let has_outbox = !matches!(
        acct.provider,
        kestrel_core::protocol::Provider::Generic | kestrel_core::protocol::Provider::Jmap
    );
    // Nothing to resume: not JMAP, no IMAP host to sync, and no preset
    // SMTP route to flush.
    if !is_jmap && host.is_empty() && !has_outbox {
        return None;
    }

    let imap_security = match preset.imap_security.as_str() {
        "starttls" => kestrel_sync::Security::StartTls,
        _ => kestrel_sync::Security::Tls,
    };
    let imap = if is_jmap || host.is_empty() {
        None
    } else {
        let mechanisms = match &acct.provider {
            kestrel_core::protocol::Provider::Gmail
            | kestrel_core::protocol::Provider::Yahoo
            | kestrel_core::protocol::Provider::Aol => vec![
                kestrel_core::sasl::SaslMechanism::Xoauth2,
                kestrel_core::sasl::SaslMechanism::Plain,
            ],
            kestrel_core::protocol::Provider::Outlook
            | kestrel_core::protocol::Provider::Fastmail => {
                vec![kestrel_core::sasl::SaslMechanism::Plain]
            }
            _ => vec![
                kestrel_core::sasl::SaslMechanism::Plain,
                kestrel_core::sasl::SaslMechanism::Login,
                kestrel_core::sasl::SaslMechanism::ScramSha256,
            ],
        };
        Some(kestrel_sync::ConnectParams {
            host: host.clone(),
            port: preset.imap_port,
            security: imap_security,
            username: preset
                .username
                .clone()
                .unwrap_or_else(|| preset.email.clone()),
            secret: secret.clone(),
            mechanisms,
            tls: tls.clone(),
            sasl_factory: sasl_factory.clone(),
        })
    };

    let outbox = if has_outbox {
        let smtp = kestrel_sync::SmtpParams {
            host: preset.smtp_host.clone(),
            port: preset.smtp_port,
            username: preset
                .username
                .clone()
                .unwrap_or_else(|| preset.email.clone()),
            secret: secret.clone(),
            oauth2: preset.auth_kind == "oauth2",
            security: match preset.smtp_security.as_str() {
                "starttls" => kestrel_sync::SmtpSecurity::StartTls,
                _ => kestrel_sync::SmtpSecurity::ImplicitTls,
            },
        };
        let health = kestrel_sync::ConnectParams {
            host: preset.imap_host.clone(),
            port: preset.imap_port,
            security: imap_security,
            username: preset
                .username
                .clone()
                .unwrap_or_else(|| preset.email.clone()),
            secret: secret.clone(),
            mechanisms: vec![kestrel_core::sasl::SaslMechanism::Plain],
            tls,
            sasl_factory,
        };
        Some((smtp, health))
    } else {
        None
    };

    Some(accounts::AccountServicesSpec {
        account: acct.id,
        jmap: if is_jmap {
            Some((acct.host.clone(), secret.clone()))
        } else {
            None
        },
        imap,
        outbox,
        store,
        clock: Arc::clone(clock),
        cfg,
    })
}

/// Blob GC scheduler (schema.md §4.3): mark hourly, sweep after the
/// configured grace period, first pass shortly after startup to clear
/// residue. Runs until cancelled; per-pass failures are logged and never
/// fatal (the supervisor restarts the whole scheduler if it panics).
async fn gc_scheduler(
    storage: StorageHandle,
    clock: Arc<dyn Clock>,
    grace: Duration,
    cancel: CancellationToken,
) -> Result<(), KestrelError> {
    let mut ticker = tokio::time::interval(Duration::from_hours(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // First sweep shortly after start (startup residue), cancellable.
    tokio::select! {
        () = cancel.cancelled() => return Ok(()),
        () = tokio::time::sleep(Duration::from_secs(30)) => {}
    }
    loop {
        tokio::select! {
            () = cancel.cancelled() => return Ok(()),
            _ = ticker.tick() => {
                let now = clock.now_unix_ms();
                match storage.gc_mark(now).await {
                    Ok(marked) if marked > 0 => {
                        tracing::debug!(marked, "gc mark");
                        if let Ok(swept) = storage
                            .gc_sweep(now, i64::try_from(grace.as_millis()).unwrap_or(i64::MAX))
                            .await
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
