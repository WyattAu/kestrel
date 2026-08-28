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
    protocol::{Command, CommandPayload, EngineEvent, FrontendKind, Reply},
};
use kestrel_storage::{IndexService, SearchService, StorageService};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub mod bus;
pub mod router;
pub mod supervisor;

pub use bus::EventBus;
pub use router::EngineRouter;

/// Command channel capacity (message-protocol §4).
pub const COMMAND_CAPACITY: usize = 256;

/// A spawned engine: the frontend's only handles.
pub struct EngineHandle {
    /// Bounded command sender (one per frontend).
    pub commands: mpsc::Sender<Command>,
    /// Event subscription (cloned receivers per frontend task).
    pub events: tokio::sync::broadcast::Receiver<EngineEvent>,
    /// Signals full shutdown completion.
    pub done: oneshot::Receiver<()>,
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
        Self::spawn_with(
            config,
            paths,
            Arc::new(SystemIdGenerator),
            Arc::new(SystemClock),
        )
        .await
    }

    /// [`Engine::spawn`] with injected id/clock sources (tests, embedded).
    ///
    /// # Errors
    /// [`KestrelError`] when storage cannot open.
    #[allow(clippy::too_many_lines)]
    pub async fn spawn_with(
        config: Arc<Config>,
        paths: Arc<Paths>,
        ids: Arc<dyn kestrel_core::ids::IdGenerator>,
        clock: Arc<dyn Clock>,
    ) -> Result<EngineHandle, KestrelError> {
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

        // Router.
        let router = EngineRouter::new(
            config,
            storage,
            search,
            bus.clone(),
            Arc::clone(&ids),
            Arc::clone(&clock),
        );
        let router_cancel = engine_cancel.child_token();
        let router_done = done_tx;
        tokio::spawn(async move {
            router.run(command_rx, router_cancel, storage_cancel).await;
            let _ = router_done.send(());
        });

        Ok(EngineHandle {
            commands: command_tx,
            events: bus.subscribe(),
            done: done_rx,
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
