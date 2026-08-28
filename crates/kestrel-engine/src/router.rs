//! Command router (message-protocol §1-2): bounded frontend inbox → service
//! dispatch → replies; events published on the bus. Ordered shutdown per
//! architecture §3.3.

use std::{sync::Arc, time::Duration};

use kestrel_core::{
    clock::Clock,
    compose::build_rfc5322,
    config::Config,
    error::KestrelError,
    ids::IdGenerator,
    protocol::{CommandPayload, EngineEvent, Reply, ShutdownStage, Window},
};
use kestrel_storage::{OutboxEnvelope, SearchHandle, StorageHandle};
use tokio::sync::mpsc;

use crate::bus::EventBus;

/// The engine's frontend-facing router.
pub struct EngineRouter {
    config: Arc<tokio::sync::RwLock<Arc<Config>>>,
    storage: StorageHandle,
    search: SearchHandle,
    bus: EventBus,
    ids: Arc<dyn IdGenerator>,
    clock: Arc<dyn Clock>,
}

impl EngineRouter {
    /// Assembles the router over live services.
    #[must_use]
    pub fn new(
        config: Arc<Config>,
        storage: StorageHandle,
        search: SearchHandle,
        bus: EventBus,
        ids: Arc<dyn IdGenerator>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            config: Arc::new(tokio::sync::RwLock::new(config)),
            storage,
            search,
            bus,
            ids,
            clock,
        }
    }

    /// Main loop: drain commands until cancellation, then perform the
    /// ordered shutdown (architecture §3.3).
    pub async fn run(
        self,
        mut commands: mpsc::Receiver<kestrel_core::protocol::Command>,
        cancel: tokio_util::sync::CancellationToken,
        storage_cancel: tokio_util::sync::CancellationToken,
    ) {
        let accounts = self.storage.list_accounts().await.unwrap_or_default();
        self.bus.publish(EngineEvent::EngineStarted {
            version: kestrel_core::protocol::PROTOCOL_VERSION,
            accounts,
        });
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                maybe = commands.recv() => {
                    let Some(cmd) = maybe else { break };
                    self.dispatch(cmd.payload).await;
                }
            }
        }

        // Ordered shutdown: frontends detached (channel closing) → services
        // cancel → outbox flush (Phase 2 attaches here) → storage
        // checkpoint → done.
        self.bus.publish(EngineEvent::EngineShutdownProgress {
            stage: ShutdownStage::DetachFrontends,
        });
        self.bus.publish(EngineEvent::EngineShutdownProgress {
            stage: ShutdownStage::CancelServices,
        });
        storage_cancel.cancel();
        self.bus.publish(EngineEvent::EngineShutdownProgress {
            stage: ShutdownStage::FlushOutbox,
        });
        self.bus.publish(EngineEvent::EngineShutdownProgress {
            stage: ShutdownStage::StorageCheckpoint,
        });
        // Storage closes when its task observes the cancellation; give the
        // loop a bounded moment to quiesce.
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.bus.publish(EngineEvent::EngineShutdownProgress {
            stage: ShutdownStage::Done,
        });
    }

    /// Dispatches one command payload.
    #[allow(clippy::too_many_lines)]
    async fn dispatch(&self, payload: CommandPayload) {
        use CommandPayload as P;

        match payload {
            // ---- reads ----
            P::ListAccounts { reply } => {
                let result = self.storage.list_accounts().await;
                Self::answer(Some(reply), result.map(Reply::Accounts));
            }
            P::ListFolders { account, reply } => {
                let result = self.storage.list_folders(account).await;
                Self::answer(Some(reply), result.map(Reply::Folders));
            }
            P::ListMessages {
                folder,
                window,
                sort,
                reply,
            } => {
                let result = self.storage.list_messages(folder, window, sort).await;
                Self::answer(Some(reply), result.map(Reply::Messages));
            }
            P::GetMessage {
                message,
                body: _,
                reply,
            } => {
                // BodyPreference::Full triggers a sync-side lazy fetch in
                // Phase 2; Phase 1 serves the cached raw.
                match self.storage.get_message(message).await {
                    Ok(load) => {
                        if load.view.remote_blocked > 0 {
                            self.bus.publish(EngineEvent::RemoteContentBlocked {
                                message: load.view.summary.id,
                                count: load.view.remote_blocked,
                            });
                        }
                        for link in &load.view.suspicious_links {
                            self.bus.publish(EngineEvent::SuspiciousLink {
                                message: load.view.summary.id,
                                href: link.href.clone(),
                            });
                        }
                        Self::answer(Some(reply), Ok(Reply::Message(load.view)));
                    }
                    Err(e) => Self::answer(Some(reply), Err(e)),
                }
            }
            P::Search { query, reply } => {
                let limit = query.limit;
                let result = self.search.search(&query).await;
                Self::answer(
                    Some(reply),
                    result.map(|mut hits| {
                        if let Some(limit) = limit {
                            hits.truncate(usize::try_from(limit).unwrap_or(hits.len()));
                        }
                        Reply::SearchResults(hits)
                    }),
                );
            }

            // ---- mutations ----
            P::SetFlags {
                messages,
                flags,
                reply,
            } => {
                let result = self.storage.set_flags(messages, flags).await;
                if let Ok(affected) = &result {
                    self.bus.publish(EngineEvent::FlagsChanged {
                        messages: affected.clone(),
                    });
                }
                Self::answer(Some(reply), result.map(|_| Reply::Accepted));
            }
            P::MoveMessages {
                messages,
                to,
                reply,
            } => {
                let moves: Vec<_> = messages
                    .into_iter()
                    .map(|id| (id, to, u32::MAX)) // placeholder uid; sync replaces
                    .collect();
                let result = self.storage.move_messages(moves).await;
                Self::answer(Some(reply), result.map(|_| Reply::Accepted));
            }
            P::DeleteMessages {
                messages,
                expunge,
                reply,
            } => {
                let _ = expunge; // server-side expunge is the sync engine's Phase 2 path
                let result = self.storage.delete_messages(messages).await;
                if let Ok(removed) = &result {
                    self.bus.publish(EngineEvent::MessagesChanged {
                        folder: kestrel_core::ids::FolderId::from_uuid(self.ids.next_id()),
                        changed: 0,
                        removed: u32::try_from(*removed).unwrap_or(u32::MAX),
                    });
                }
                Self::answer(Some(reply), result.map(|_| Reply::Accepted));
            }

            // ---- composition ----
            P::ComposeSubmit { draft, reply } => {
                let result = self.compose_submit(draft).await.map(|id| {
                    self.bus.publish(EngineEvent::OutboxEnqueued { id });
                    Reply::Accepted
                });
                Self::answer(Some(reply), result);
            }
            P::CancelOutbox { id, reply } => {
                let result = self
                    .storage
                    .outbox_cancel(id)
                    .await
                    .map(|()| Reply::Accepted);
                Self::answer(Some(reply), result);
            }

            // ---- sync control ----
            // Phase 2 attaches sync-mode handling (fire-and-forget per
            // the protocol; no reply by construction).
            P::TriggerSync { .. } | P::GoOffline | P::GoOnline => {}
            P::ResyncState { reply } => {
                // Authoritative state is command-reply based; the frontend
                // re-issues its list commands. Acknowledge.
                Self::answer(Some(reply), Ok(Reply::Accepted));
            }

            // ---- config & lifecycle ----
            P::ConfigUpdated { snapshot } => {
                *self.config.write().await = Arc::clone(&snapshot);
                self.bus.publish(EngineEvent::ConfigUpdated { snapshot });
            }
            P::Shutdown { drain } => {
                let _ = drain; // outbox drain attaches with the Phase 2 flusher
                // Router exits via its run loop cancellation; the engine
                // cancels itself on shutdown commands.
                self.bus.publish(EngineEvent::EngineShutdownProgress {
                    stage: ShutdownStage::DetachFrontends,
                });
            }
        }
    }

    /// Sends exactly one reply; `Err` payloads map to `Reply::Err`
    /// (message-protocol §4 rule 2).
    fn answer(
        reply: Option<tokio::sync::oneshot::Sender<Reply>>,
        result: Result<Reply, KestrelError>,
    ) {
        if let Some(tx) = reply {
            let _ = tx.send(result.unwrap_or_else(Reply::err));
        }
    }

    /// Builds RFC 5322 from the draft and enqueues it into the outbox
    /// (architecture §4.2).
    async fn compose_submit(
        &self,
        draft: kestrel_core::protocol::Draft,
    ) -> Result<kestrel_core::ids::OutboxId, KestrelError> {
        let raw = build_rfc5322(&draft, self.ids.as_ref(), self.clock.as_ref())?;
        let envelope = OutboxEnvelope {
            from: draft.from,
            to: draft.to,
            cc: draft.cc,
            bcc: draft.bcc,
            subject: draft.subject,
        };
        self.storage
            .outbox_enqueue(draft.account, envelope, raw)
            .await
    }
}

/// Window default helper for the router's list command (protocol §2 allows
/// implementations to default).
#[must_use]
pub fn default_window() -> Window {
    Window::default()
}
