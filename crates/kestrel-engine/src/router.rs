//! Command router (message-protocol §1-2): bounded frontend inbox → service
//! dispatch → replies; events published on the bus. Ordered shutdown per
//! architecture §3.3.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use kestrel_core::{
    clock::Clock,
    compose::{build_rfc5322, build_rfc5322_pgp},
    config::Config,
    error::KestrelError,
    ids::{AccountId, IdGenerator},
    protocol::{CommandPayload, EngineEvent, FlagOp, Provider, Reply, ShutdownStage, Window},
};
use kestrel_storage::{
    FlagPayload, OpType, OutboxEnvelope, PendingOpPayload, SearchHandle, StorageHandle,
};
use tokio::sync::mpsc;
use tracing::instrument;

use crate::{
    accounts::{self, AccountRegistry, AccountServicesSpec},
    bus::EventBus,
};

/// The engine's frontend-facing router.
pub struct EngineRouter {
    config: Arc<tokio::sync::RwLock<Arc<Config>>>,
    storage: StorageHandle,
    search: SearchHandle,
    bus: EventBus,
    ids: Arc<dyn IdGenerator>,
    clock: Arc<dyn Clock>,
    creds: std::sync::Arc<kestrel_crypto::CredentialService>,
    /// Shared per-account service registry (in-session adds and startup
    /// resumes alike; accounts.rs).
    registry: Arc<AccountRegistry>,
    /// Engine-wide cancellation; per-account tokens are its children, so the
    /// shutdown epilogue stops every account's services.
    engine_cancel: tokio_util::sync::CancellationToken,
    /// Offline mode flag (sync-engine.md §6).
    offline: Arc<AtomicBool>,
    /// Whether the shutdown epilogue waits (≤ 5 s) for the outbox final
    /// flush; set by `Command::Shutdown { drain }`, defaults to drain.
    drain_outbox: Arc<AtomicBool>,
    /// Pending `OAuth2` browser flows keyed by the single-use `state` the
    /// authorization URL carries (#28). Each entry owns the loopback
    /// capture handle; the spawned completer removes it when the flow
    /// resolves. Entries self-expire with the capture timeout, so the map
    /// cannot grow unbounded.
    oauth_flows: Arc<tokio::sync::Mutex<HashMap<String, PendingOAuthFlow>>>,
}

/// A started but not-yet-retrieved `OAuth2` browser flow (threat model
/// §4.8: single-use `state`, loopback-only capture, codes never logged).
enum PendingOAuthFlow {
    /// Redirect not yet captured: the loopback handle resolves with the
    /// code + PKCE verifier + redirect port, or a typed error (timeout,
    /// state mismatch, provider error). Resolved exactly once.
    Capturing {
        /// Provider the flow authenticates with (re-derived preset at
        /// completion so env overrides apply uniformly).
        provider: Provider,
        /// Loopback capture handle.
        capture: tokio::task::JoinHandle<Result<kestrel_crypto::oauth::CapturedCode, KestrelError>>,
    },
    /// Code exchanged; serialized credential set awaiting its single
    /// retrieval via `CompleteOAuth2Flow`.
    Completed { tokens: String },
}

impl EngineRouter {
    /// Assembles the router over live services.
    #[must_use]
    #[allow(private_interfaces)] // the account registry is an in-crate handle
    #[allow(clippy::too_many_arguments)] // one per live service the router wires
    pub fn new(
        config: Arc<Config>,
        storage: StorageHandle,
        search: SearchHandle,
        bus: EventBus,
        ids: Arc<dyn IdGenerator>,
        clock: Arc<dyn Clock>,
        creds: std::sync::Arc<kestrel_crypto::CredentialService>,
        registry: Arc<AccountRegistry>,
        engine_cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            config: Arc::new(tokio::sync::RwLock::new(config)),
            storage,
            search,
            bus,
            ids,
            clock,
            creds,
            registry,
            engine_cancel,
            offline: Arc::new(AtomicBool::new(false)),
            drain_outbox: Arc::new(AtomicBool::new(true)),
            oauth_flows: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Returns a reference to the credential service.
    #[must_use]
    pub fn credentials(&self) -> &std::sync::Arc<kestrel_crypto::CredentialService> {
        &self.creds
    }

    /// Main loop: drain commands until cancellation, then perform the
    /// ordered shutdown (architecture §3.3).
    #[instrument(skip_all)]
    pub async fn run(
        self,
        mut commands: mpsc::Receiver<kestrel_core::protocol::Command>,
        cancel: tokio_util::sync::CancellationToken,
        engine_cancel: tokio_util::sync::CancellationToken,
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

        // Ordered shutdown: frontends detached (channel closing or cancel)
        // → background services cancel → bounded outbox flush (≤ 5 s) →
        // storage checkpoint → done (architecture §3.3).
        self.bus.publish(EngineEvent::EngineShutdownProgress {
            stage: ShutdownStage::DetachFrontends,
        });
        // Cancel the supervised background services (config watcher, GC,
        // snooze, filter, startup outbox/JMAP sync). Outbox instances run
        // their final flush in the cancellation branch of `run`.
        engine_cancel.cancel();
        // Cancel per-account sync/outbox services too (they are engine-cancel
        // children, but cancel explicitly so the drain order is deterministic).
        let account_handles: Vec<_> = {
            let map = self.registry.lock().await;
            map.values().cloned().collect()
        };
        for handle in account_handles {
            handle.cancel.cancel();
        }
        self.bus.publish(EngineEvent::EngineShutdownProgress {
            stage: ShutdownStage::CancelServices,
        });
        self.bus.publish(EngineEvent::EngineShutdownProgress {
            stage: ShutdownStage::FlushOutbox,
        });
        // Bounded final outbox flush: storage must stay open while the
        // flushers drain, so poll until the queue is empty (≤ 5 s cap)
        // before closing it. Skipped when the caller requested no drain.
        if self.drain_outbox.load(Ordering::Relaxed) {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                match self.storage.outbox_due().await {
                    Ok(due) if due.is_empty() => break,
                    Ok(_) | Err(_) => {
                        // Still flushing (or storage busy); poll again.
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }
            }
        }
        self.bus.publish(EngineEvent::EngineShutdownProgress {
            stage: ShutdownStage::StorageCheckpoint,
        });
        storage_cancel.cancel();
        // Storage closes when its task observes the cancellation; give the
        // loop a bounded moment to quiesce.
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.bus.publish(EngineEvent::EngineShutdownProgress {
            stage: ShutdownStage::Done,
        });
    }

    /// Dispatches one command payload.
    #[instrument(skip_all)]
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
            P::ListUnifiedInbox {
                window,
                sort,
                reply,
            } => {
                let result = self.storage.list_unified_inbox(window, sort).await;
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
            P::GetAttachment {
                message,
                part,
                reply,
            } => {
                let result = self
                    .storage
                    .get_attachment_data(message, part.key)
                    .await
                    .map(Reply::AttachmentData);
                Self::answer(Some(reply), result);
            }
            P::SaveAttachment {
                message,
                part,
                path,
                reply,
            } => {
                let result = self.storage.get_attachment_data(message, part.key).await;
                match result {
                    Ok(data) => match std::fs::write(&path, &data) {
                        Ok(()) => Self::answer(Some(reply), Ok(Reply::Accepted)),
                        Err(e) => Self::answer(
                            Some(reply),
                            Err(kestrel_core::error::KestrelError::StorageIo {
                                detail: format!("failed to write attachment: {e}"),
                            }),
                        ),
                    },
                    Err(e) => Self::answer(Some(reply), Err(e)),
                }
            }

            // ---- calendar / contacts (placeholder — CalDAV/CardDAV not yet wired) ----
            P::ListCalendars { account: _, reply } => {
                Self::answer(Some(reply), Ok(Reply::Calendars(vec![])));
            }
            P::ListEvents {
                calendar: _,
                window: _,
                reply,
            } => {
                Self::answer(Some(reply), Ok(Reply::Events(vec![])));
            }
            P::CreateEvent {
                calendar_id,
                uid,
                summary,
                description,
                location,
                start_time,
                end_time,
                all_day,
                reply,
            } => {
                let event = kestrel_calcard::CalendarEvent {
                    id: String::new(),
                    calendar_id,
                    account_id: self.first_account().await,
                    uid,
                    summary,
                    description,
                    location,
                    start_time,
                    end_time,
                    all_day,
                    recurrence: None,
                    attendees: vec![],
                    alarms: vec![],
                    ical_data: None,
                    created_at: self.clock.now_unix_ms(),
                    updated_at: self.clock.now_unix_ms(),
                };
                let ical_data = kestrel_calcard::serialize_ical(&event);
                // For now, return Accepted; CalDAV PUT will be wired in Phase 2.
                tracing::info!(
                    uid = %event.uid,
                    summary = %event.summary,
                    "CreateEvent received (CalDAV PUT pending Phase 2)"
                );
                let _ = ical_data;
                Self::answer(Some(reply), Ok(Reply::Accepted));
            }
            P::ListContacts { account: _, reply } => {
                Self::answer(Some(reply), Ok(Reply::Contacts(vec![])));
            }

            // ---- mutations ----
            P::SetFlags {
                messages,
                flags,
                reply,
            } => {
                // Server-push queue: flag mutations must reach the IMAP
                // server (UID STORE) or the next delta sync reverts them.
                // Enqueued in BOTH branches — offline mode additionally
                // journals the local replay (pending_ops).
                self.enqueue_flag_push(&messages, &flags).await;
                if self.offline.load(Ordering::Relaxed) {
                    let account = self.first_account().await;
                    let payload = PendingOpPayload::Flag {
                        messages,
                        flags: FlagPayload::from(&flags),
                    };
                    let result = self
                        .storage
                        .pending_ops_enqueue(account, OpType::Flag, payload)
                        .await
                        .map(|_| Reply::Accepted);
                    Self::answer(Some(reply), result);
                } else {
                    let result = self.storage.set_flags(messages, flags).await;
                    if let Ok(affected) = &result {
                        self.bus.publish(EngineEvent::FlagsChanged {
                            messages: affected.clone(),
                        });
                    }
                    Self::answer(Some(reply), result.map(|_| Reply::Accepted));
                }
            }
            P::MoveMessages {
                messages,
                to,
                reply,
            } => {
                // Server-push queue: moves must reach the server (UID MOVE)
                // or the next delta sync restores the message to its source
                // folder (see SetFlags above for the offline note).
                self.enqueue_move_push(&messages, to).await;
                if self.offline.load(Ordering::Relaxed) {
                    let account = self.first_account().await;
                    let payload = PendingOpPayload::Move { messages, to };
                    let result = self
                        .storage
                        .pending_ops_enqueue(account, OpType::Move, payload)
                        .await
                        .map(|_| Reply::Accepted);
                    Self::answer(Some(reply), result);
                } else {
                    let moves: Vec<_> = messages
                        .into_iter()
                        .map(|id| (id, to, u32::MAX)) // placeholder uid; sync replaces
                        .collect();
                    let result = self.storage.move_messages(moves).await;
                    Self::answer(Some(reply), result.map(|_| Reply::Accepted));
                }
            }
            P::DeleteMessages {
                messages,
                expunge,
                reply,
            } => {
                if self.offline.load(Ordering::Relaxed) {
                    let account = self.first_account().await;
                    let payload = PendingOpPayload::Delete { messages, expunge };
                    let result = self
                        .storage
                        .pending_ops_enqueue(account, OpType::Delete, payload)
                        .await
                        .map(|_| Reply::Accepted);
                    Self::answer(Some(reply), result);
                } else {
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
            }

            // ---- composition ----
            P::ComposeSubmit { draft, reply } => {
                if self.offline.load(Ordering::Relaxed) {
                    let account = draft.account;
                    let payload = PendingOpPayload::Compose {
                        draft: Box::new(draft),
                    };
                    let result = self
                        .storage
                        .pending_ops_enqueue(account, OpType::Compose, payload)
                        .await
                        .map(|_| Reply::Accepted);
                    Self::answer(Some(reply), result);
                } else {
                    let result = self.compose_submit(draft).await.map(|id| {
                        self.bus.publish(EngineEvent::OutboxEnqueued { id });
                        Reply::Accepted
                    });
                    Self::answer(Some(reply), result);
                }
            }
            P::CancelOutbox { id, reply } => {
                let result = self
                    .storage
                    .outbox_cancel(id)
                    .await
                    .map(|()| Reply::Accepted);
                Self::answer(Some(reply), result);
            }

            // ---- snooze ----
            P::SnoozeMessage {
                message,
                account,
                folder,
                until,
                reply,
            } => {
                let result = self
                    .storage
                    .enqueue_snooze(message, account, folder, until)
                    .await
                    .map(|_| Reply::Accepted);
                Self::answer(Some(reply), result);
            }
            P::UnsnoozeMessage { message, reply } => {
                let result = self
                    .storage
                    .remove_snooze(message)
                    .await
                    .map(|()| Reply::Accepted);
                Self::answer(Some(reply), result);
            }

            // ---- onboarding ----
            P::AddAccount {
                config,
                password,
                reply,
            } => {
                let result = self.add_account(config, password).await;
                Self::answer(Some(reply), result.map(Reply::Accounts));
            }
            P::TestConnection {
                config,
                password,
                reply,
            } => {
                let result = self.test_connection(&config, &password).await;
                Self::answer(Some(reply), result.map(|()| Reply::Accepted));
            }
            P::StartOAuth2Flow { provider, reply } => {
                let result = self.start_oauth2_flow(&provider).await;
                Self::answer(Some(reply), result.map(Reply::OAuthUrl));
            }
            P::RemoveAccount { account, reply } => {
                // Cancel the account's supervised services (IMAP/JMAP sync
                // and outbox share one token).
                accounts::stop_account(&self.registry, account).await;
                let result = self
                    .storage
                    .delete_account(account)
                    .await
                    .map(|()| Reply::Accepted);
                Self::answer(Some(reply), result);
            }
            P::UpdateAccount {
                config,
                password,
                reply,
            } => {
                let result = self.update_account(config, password).await;
                Self::answer(Some(reply), result.map(Reply::Accounts));
            }
            P::CompleteOAuth2Flow { state, reply } => {
                let result = self.complete_oauth2_flow(&state).await;
                Self::answer(
                    Some(reply),
                    result.map(|tokens| {
                        Reply::OAuthTokens(kestrel_core::secrets::SecretString::new(tokens))
                    }),
                );
            }

            // ---- sync control ----
            // Fire-and-forget by construction (message-protocol §6.3): wake
            // the account's sync service so it ends its current IDLE/poll
            // wait and starts a cycle now. Unknown accounts are ignored.
            P::TriggerSync { account, kind } => {
                let registry = self.registry.lock().await;
                if let Some(handle) = registry.get(&account) {
                    tracing::info!(account = %account, ?kind, "TriggerSync: waking sync service");
                    handle.trigger.notify_one();
                } else {
                    tracing::debug!(account = %account, "TriggerSync for unknown account ignored");
                }
            }
            P::GoOffline => {
                self.offline.store(true, Ordering::Relaxed);
                tracing::info!("entering offline mode");
            }
            P::GoOnline => {
                self.offline.store(false, Ordering::Relaxed);
                tracing::info!("leaving offline mode; replaying pending ops");
                self.replay_pending_ops().await;
            }
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
                // Whether the epilogue waits for the outbox final flush.
                self.drain_outbox.store(drain, Ordering::Relaxed);
                // The run loop exits on engine cancellation or channel
                // close; the epilogue performs the ordered shutdown.
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
    /// Adds an account: store config → keyring credentials → start sync.
    #[instrument(skip_all, fields(account = %config.email))]
    #[allow(clippy::too_many_lines)]
    async fn add_account(
        &self,
        config: kestrel_core::provider::AccountConfig,
        password: kestrel_core::secrets::SecretString,
    ) -> Result<Vec<kestrel_core::protocol::AccountSummary>, KestrelError> {
        // Validate before storing.
        let errors = kestrel_core::provider::validate_account_config(&config);
        if !errors.is_empty() {
            return Err(KestrelError::DraftInvalid {
                detail: errors.join("; "),
            });
        }

        // 1. Create the account row.
        let account_id = self
            .storage
            .upsert_account(kestrel_storage::store::NewAccount {
                name: config.display_name.clone(),
                email: config.email.clone(),
                provider: config.provider.clone(),
                protocol: if config.provider == kestrel_core::protocol::Provider::Jmap {
                    kestrel_core::protocol::MailProtocol::Jmap
                } else {
                    kestrel_core::protocol::MailProtocol::Imap
                },
                auth_kind: config.auth_kind.clone(),
                host: config.imap_host.clone(),
            })
            .await?;

        // 2. Store credentials in the OS keyring (threat model §4.8).
        self.creds
            .set_password(account_id, &password)
            .map_err(KestrelError::from)?;
        // OAuth2 accounts: the same secret seeds the refresh-token slot
        // the unattended worker reads (#26). After a `CompleteOAuth2Flow`
        // exchange, that flow's real refresh token replaces it.
        if config.auth_kind == "oauth2" {
            self.creds
                .set_refresh_token(account_id, &password)
                .map_err(KestrelError::from)?;
        }

        // 3. Start the per-account services under the supervisor (shared
        // lifecycle with UpdateAccount and startup resume; accounts.rs).
        self.start_account_services(config, account_id, password)
            .await?;

        // Return the updated account list.
        self.storage.list_accounts().await
    }

    /// Updates an existing account: cancel old sync, upsert config, update
    /// credentials, and restart sync.
    #[instrument(skip_all, fields(account = %config.email))]
    #[allow(clippy::too_many_lines)]
    async fn update_account(
        &self,
        config: kestrel_core::provider::AccountConfig,
        password: kestrel_core::secrets::SecretString,
    ) -> Result<Vec<kestrel_core::protocol::AccountSummary>, KestrelError> {
        // Validate before storing.
        let errors = kestrel_core::provider::validate_account_config(&config);
        if !errors.is_empty() {
            return Err(KestrelError::DraftInvalid {
                detail: errors.join("; "),
            });
        }

        // Find existing account by email.
        let accounts = self.storage.list_accounts().await.unwrap_or_default();
        let existing = accounts.iter().find(|a| a.email == config.email);

        // If an existing account is found, stop its supervised services so
        // the restart below cannot overlap the old ones.
        if let Some(acct) = existing {
            accounts::stop_account(&self.registry, acct.id).await;
        }

        // Upsert the account row (same as add_account).
        let account_id = self
            .storage
            .upsert_account(kestrel_storage::store::NewAccount {
                name: config.display_name.clone(),
                email: config.email.clone(),
                provider: config.provider.clone(),
                protocol: if config.provider == kestrel_core::protocol::Provider::Jmap {
                    kestrel_core::protocol::MailProtocol::Jmap
                } else {
                    kestrel_core::protocol::MailProtocol::Imap
                },
                auth_kind: config.auth_kind.clone(),
                host: config.imap_host.clone(),
            })
            .await?;

        // Update credentials in the OS keyring.
        self.creds
            .set_password(account_id, &password)
            .map_err(KestrelError::from)?;

        // Start the per-account services under the supervisor (shared
        // lifecycle with AddAccount and startup resume; accounts.rs).
        self.start_account_services(config, account_id, password)
            .await?;

        // Return the updated account list.
        self.storage.list_accounts().await
    }

    /// Probes IMAP connectivity without storing anything.
    #[instrument(skip_all)]
    async fn test_connection(
        &self,
        config: &kestrel_core::provider::AccountConfig,
        password: &kestrel_core::secrets::SecretString,
    ) -> Result<(), KestrelError> {
        let security = Self::parse_security(&config.imap_security);
        let params = kestrel_sync::ConnectParams {
            host: config.imap_host.clone(),
            port: config.imap_port,
            security,
            username: config
                .username
                .clone()
                .unwrap_or_else(|| config.email.clone()),
            secret: password.clone(),
            secret_override: None,
            mechanisms: vec![kestrel_core::sasl::SaslMechanism::Plain],
            tls: tokio_rustls::TlsConnector::from(
                kestrel_crypto::tls_config(None).map_err(KestrelError::from)?,
            ),
            sasl_factory: std::sync::Arc::new(|mech, user, secret| {
                kestrel_crypto::sasl::start(mech, user, secret)
            }),
        };
        match kestrel_sync::ImapSession::connect_and_authenticate(&params).await {
            Ok(mut session) => {
                session.logout().await;
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Parses a security string ("tls" | "starttls" | "insecure") into the
    /// enum. `"insecure"` (cleartext) exists for the integration fixtures
    /// and self-hosted LAN servers; frontends must label it clearly.
    fn parse_security(s: &str) -> kestrel_sync::Security {
        match s {
            "starttls" => kestrel_sync::Security::StartTls,
            "insecure" => kestrel_sync::Security::Insecure,
            _ => kestrel_sync::Security::Tls,
        }
    }

    /// Builds and starts one account's supervised services from a validated
    /// `AccountConfig` (shared by `AddAccount` / `UpdateAccount`; backlog
    /// #2). The account row and keyring entry must already exist.
    #[instrument(skip_all, fields(account = %account_id))]
    async fn start_account_services(
        &self,
        config: kestrel_core::provider::AccountConfig,
        account_id: AccountId,
        password: kestrel_core::secrets::SecretString,
    ) -> Result<(), KestrelError> {
        let store: std::sync::Arc<dyn kestrel_core::store_model::MailStore> =
            std::sync::Arc::new(self.storage.clone());
        let tls = tokio_rustls::TlsConnector::from(
            kestrel_crypto::tls_config(None).map_err(KestrelError::from)?,
        );
        let sasl_factory: kestrel_sync::SaslFactory = std::sync::Arc::new(|mech, user, secret| {
            kestrel_crypto::sasl::start(mech, user, secret)
        });

        let (jmap, imap, outbox) = if config.provider == kestrel_core::protocol::Provider::Jmap {
            // JMAP sync only (JMAP SMTP submit is not wired yet).
            (Some((config.imap_host.clone(), password)), None, None)
        } else {
            let security = Self::parse_security(&config.imap_security);
            let mechanisms = match &config.provider {
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
            let username = config
                .username
                .clone()
                .unwrap_or_else(|| config.email.clone());
            let imap = kestrel_sync::ConnectParams {
                host: config.imap_host.clone(),
                port: config.imap_port,
                security,
                username: username.clone(),
                secret: password.clone(),
                secret_override: None,
                mechanisms,
                tls: tls.clone(),
                sasl_factory: sasl_factory.clone(),
            };
            let smtp = kestrel_sync::SmtpParams {
                host: config.smtp_host.clone(),
                port: config.smtp_port,
                username: username.clone(),
                secret: password.clone(),
                secret_override: None,
                oauth2: config.auth_kind == "oauth2",
                security: match config.smtp_security.as_str() {
                    "starttls" => kestrel_sync::SmtpSecurity::StartTls,
                    "insecure" => kestrel_sync::SmtpSecurity::Insecure,
                    _ => kestrel_sync::SmtpSecurity::ImplicitTls,
                },
            };
            // Outbox health checks reuse the IMAP connection params.
            let outbox_imap = kestrel_sync::ConnectParams {
                host: config.imap_host.clone(),
                port: config.imap_port,
                security,
                username,
                secret: password,
                secret_override: None,
                mechanisms: vec![kestrel_core::sasl::SaslMechanism::Plain],
                tls,
                sasl_factory,
            };
            (None, Some(imap), Some((smtp, outbox_imap)))
        };

        let spec = AccountServicesSpec {
            account: account_id,
            provider: config.provider.clone(),
            is_oauth2: config.auth_kind == "oauth2",
            creds: Arc::clone(&self.creds),
            jmap,
            imap,
            outbox,
            store,
            clock: Arc::clone(&self.clock),
            cfg: Arc::clone(&*self.config.read().await),
        };
        accounts::start_account_services(
            &self.registry,
            self.bus.clone(),
            self.engine_cancel.clone(),
            spec,
        )
        .await;
        Ok(())
    }

    /// Starts an `OAuth2` browser flow: builds the authorization URL
    /// (PKCE S256 + single-use `state`) and parks the loopback capture in
    /// the pending-flow map. The flow completes server-side when the
    /// browser redirect lands — either autonomously (the spawned completer
    /// exchanges the code and emits
    /// [`EngineEvent::OAuth2FlowCompleted`]) or when the frontend calls
    /// [`Command::CompleteOAuth2Flow`] with the same `state`.
    #[instrument(skip_all)]
    async fn start_oauth2_flow(&self, provider: &Provider) -> Result<String, KestrelError> {
        let preset = kestrel_crypto::oauth::provider_from_env(provider).map_err(|e| {
            KestrelError::DraftInvalid {
                detail: e.to_string(),
            }
        })?;
        let flow = kestrel_crypto::oauth::start_flow_captured(
            &preset,
            None,
            std::time::Duration::from_mins(5),
        )
        .await
        .map_err(|e| KestrelError::OAuthFlowFailed {
            detail: e.to_string(),
        })?;
        let capture = tokio::spawn(async move {
            flow.handle
                .await
                .map_err(|e| KestrelError::OAuthFlowFailed {
                    detail: format!("capture join: {e}"),
                })?
                .map_err(|e| KestrelError::OAuthFlowFailed {
                    detail: e.to_string(),
                })
        });
        let pending = PendingOAuthFlow::Capturing {
            provider: provider.clone(),
            capture,
        };
        self.oauth_flows
            .lock()
            .await
            .insert(flow.state.clone(), pending);
        self.spawn_flow_completer(flow.state);
        Ok(flow.url)
    }

    /// Spawns the autonomous flow completer: awaits the loopback capture,
    /// exchanges the code off the router task, and reparks the serialized
    /// credential set (or the failure) for retrieval. Resolution removes
    /// the capturing entry (single-use) and publishes exactly one
    /// [`EngineEvent::OAuth2FlowCompleted`].
    fn spawn_flow_completer(&self, state: String) {
        let flows = Arc::clone(&self.oauth_flows);
        let bus = self.bus.clone();
        let http = kestrel_crypto::oauth::shared_http_client();
        tokio::spawn(async move {
            // Take the capturing entry out: from here on the flow is
            // resolving — a racing `CompleteOAuth2Flow` must observe it as
            // gone (single-use, threat model §4.8).
            let Some(PendingOAuthFlow::Capturing { provider, capture }) =
                flows.lock().await.remove(&state)
            else {
                return; // already retrieved or never started
            };
            let captured = match capture.await {
                Ok(Ok(captured)) => captured,
                Ok(Err(e)) => {
                    bus.publish(EngineEvent::OAuth2FlowCompleted {
                        state,
                        result: Err(e),
                    });
                    return;
                }
                Err(e) => {
                    bus.publish(EngineEvent::OAuth2FlowCompleted {
                        state,
                        result: Err(KestrelError::OAuthFlowFailed {
                            detail: format!("capture join: {e}"),
                        }),
                    });
                    return;
                }
            };
            let http = match http {
                Ok(http) => http,
                Err(e) => {
                    bus.publish(EngineEvent::OAuth2FlowCompleted {
                        state,
                        result: Err(KestrelError::OAuthFlowFailed {
                            detail: e.to_string(),
                        }),
                    });
                    return;
                }
            };
            let preset = match kestrel_crypto::oauth::provider_from_env(&provider) {
                Ok(preset) => preset,
                Err(e) => {
                    bus.publish(EngineEvent::OAuth2FlowCompleted {
                        state,
                        result: Err(KestrelError::OAuthFlowFailed {
                            detail: e.to_string(),
                        }),
                    });
                    return;
                }
            };
            let tokens = match kestrel_crypto::oauth::exchange_code(
                &http,
                &preset,
                None,
                &captured.code,
                captured.port,
                &captured.verifier,
            )
            .await
            {
                Ok(tokens) => tokens,
                Err(e) => {
                    bus.publish(EngineEvent::OAuth2FlowCompleted {
                        state,
                        result: Err(KestrelError::OAuthFlowFailed {
                            detail: e.to_string(),
                        }),
                    });
                    return;
                }
            };
            match kestrel_crypto::oauth::serialize_token_set(&tokens) {
                Ok(tokens) => {
                    // Park the credential set for the single retrieval
                    // via `CompleteOAuth2Flow`; if the frontend never
                    // claims it this entry stays resident — bounded by
                    // one slot per successful flow.
                    flows
                        .lock()
                        .await
                        .insert(state.clone(), PendingOAuthFlow::Completed { tokens });
                    bus.publish(EngineEvent::OAuth2FlowCompleted {
                        state,
                        result: Ok(()),
                    });
                }
                Err(e) => {
                    bus.publish(EngineEvent::OAuth2FlowCompleted {
                        state,
                        result: Err(KestrelError::OAuthFlowFailed {
                            detail: e.to_string(),
                        }),
                    });
                }
            }
        });
    }

    /// Completes a pending `OAuth2` flow by `state` and returns the
    /// serialized credential set (single-use: a second call with the same
    /// `state` fails). If the autonomous completer already exchanged the
    /// code, this retrieves the parked result; otherwise it awaits the
    /// capture and performs the exchange here.
    #[instrument(skip_all)]
    async fn complete_oauth2_flow(&self, state: &str) -> Result<String, KestrelError> {
        let Some(pending) = self.oauth_flows.lock().await.remove(state) else {
            return Err(KestrelError::OAuthFlowFailed {
                detail: "unknown, expired, or already-completed OAuth2 flow".into(),
            });
        };
        match pending {
            PendingOAuthFlow::Completed { tokens } => Ok(tokens),
            PendingOAuthFlow::Capturing { provider, capture } => {
                let captured = capture.await.map_err(|e| KestrelError::OAuthFlowFailed {
                    detail: format!("capture join: {e}"),
                })??;
                let preset = kestrel_crypto::oauth::provider_from_env(&provider).map_err(|e| {
                    KestrelError::OAuthFlowFailed {
                        detail: e.to_string(),
                    }
                })?;
                let http = kestrel_crypto::oauth::shared_http_client().map_err(|e| {
                    KestrelError::OAuthFlowFailed {
                        detail: e.to_string(),
                    }
                })?;
                let tokens = kestrel_crypto::oauth::exchange_code(
                    &http,
                    &preset,
                    None,
                    &captured.code,
                    captured.port,
                    &captured.verifier,
                )
                .await
                .map_err(|e| KestrelError::OAuthFlowFailed {
                    detail: e.to_string(),
                })?;
                kestrel_crypto::oauth::serialize_token_set(&tokens).map_err(|e| {
                    KestrelError::OAuthFlowFailed {
                        detail: e.to_string(),
                    }
                })
            }
        }
    }

    #[instrument(skip_all, fields(account = %draft.account))]
    async fn compose_submit(
        &self,
        draft: kestrel_core::protocol::Draft,
    ) -> Result<kestrel_core::ids::OutboxId, KestrelError> {
        let raw = if draft.pgp_sign || draft.pgp_encrypt {
            let sign_cert = if draft.pgp_sign {
                self.creds
                    .pgp_secret_cert(draft.account)
                    .map_err(KestrelError::from)?
            } else {
                None
            };
            let sign_password = if draft.pgp_sign {
                self.creds
                    .pgp_secret_password(draft.account)
                    .map_err(KestrelError::from)?
            } else {
                None
            };
            let encrypt_certs = if draft.pgp_encrypt {
                self.creds
                    .pgp_recipient_certs(&draft.to, &draft.cc)
                    .map_err(KestrelError::from)?
            } else {
                vec![]
            };

            if draft.pgp_sign && sign_cert.is_none() {
                return Err(KestrelError::OpenPgpFailed {
                    detail: "no OpenPGP signing key configured for this account".into(),
                });
            }
            if draft.pgp_encrypt && encrypt_certs.is_empty() {
                return Err(KestrelError::OpenPgpFailed {
                    detail: "no OpenPGP public keys found for recipients".into(),
                });
            }

            let sign_fn = sign_cert.clone().map(|cert| {
                let pw = sign_password
                    .clone()
                    .unwrap_or_else(|| kestrel_core::secrets::SecretString::new(String::new()));
                move |data: &[u8]| -> Result<Vec<u8>, KestrelError> {
                    kestrel_crypto::openpgp::sign(&cert, &pw, data).map_err(KestrelError::from)
                }
            });
            let encrypt_fn = if encrypt_certs.is_empty() {
                None
            } else {
                let sign_ctx = sign_cert.map(|cert| {
                    let pw = sign_password
                        .unwrap_or_else(|| kestrel_core::secrets::SecretString::new(String::new()));
                    (cert, pw)
                });
                Some(move |data: &[u8]| -> Result<Vec<u8>, KestrelError> {
                    let sign_ref = sign_ctx.as_ref().map(|(c, p)| (c, p));
                    kestrel_crypto::openpgp::encrypt(&encrypt_certs, sign_ref, data)
                        .map_err(KestrelError::from)
                })
            };

            build_rfc5322_pgp(
                &draft,
                self.ids.as_ref(),
                self.clock.as_ref(),
                sign_fn,
                encrypt_fn,
            )?
        } else {
            build_rfc5322(&draft, self.ids.as_ref(), self.clock.as_ref())?
        };

        let envelope = OutboxEnvelope {
            from: draft.from,
            to: draft.to,
            cc: draft.cc,
            bcc: draft.bcc,
            subject: draft.subject,
        };

        // Apply undo-send delay: if config delay > 0 and the draft has no
        // explicit send_after, schedule the outbox entry for later so the
        // user can cancel within the window.
        let send_after = if draft.send_after.is_none() {
            let delay_secs = self.config.read().await.send_delay_seconds;
            if delay_secs > 0 {
                Some(self.clock.now_unix_ms() + i64::from(delay_secs) * 1000)
            } else {
                None
            }
        } else {
            draft.send_after
        };

        self.storage
            .outbox_enqueue(draft.account, envelope, raw, send_after)
            .await
    }

    /// Returns the first account id (used for offline enqueue when account
    /// context is unavailable from the mutation command).
    /// Queues per-message flag push ops (server-side UID STORE) for the
    /// given messages. Best-effort: a storage failure is logged and
    /// swallowed — the local apply still succeeded, and the next full
    /// reconciliation can re-converge. Locating the (folder, uid)
    /// coordinates first keeps payloads stable even if the user mutates
    /// again before the sync drain runs.
    async fn enqueue_flag_push(&self, messages: &[kestrel_core::ids::MessageId], flags: &FlagOp) {
        use kestrel_core::store_model::{PushOpPayload, PushOpType};
        let (add, remove) = match flags {
            FlagOp::Add(v) => (v.clone(), Vec::new()),
            FlagOp::Remove(v) => (Vec::new(), v.clone()),
            // A full replace cannot be expressed as add/remove deltas; the
            // delta sync reconciles the remaining difference (and the next
            // set of the same flags re-enqueues). Rare path: the TUI only
            // issues Add/Remove today.
            FlagOp::Set(_) => return,
        };
        if add.is_empty() && remove.is_empty() {
            return;
        }
        let Ok(locations) = self.storage.message_locations(messages.to_vec()).await else {
            return;
        };
        let account = self.current_account().await;
        for (message, folder, uid) in locations {
            let payload = PushOpPayload::Flag {
                message,
                folder,
                uid,
                add: add.clone(),
                remove: remove.clone(),
            };
            if let Err(e) = self
                .storage
                .push_enqueue(account, PushOpType::Flag, payload)
                .await
            {
                tracing::warn!(error = %e, "failed to enqueue flag push op");
            }
        }
        // Wake the sync service so the drain runs now, not at the next
        // natural cycle (which could be 29 minutes away while IDLE — the
        // delta pass would then revert the user's mutation server-side).
        self.wake_sync(account).await;
    }

    /// Wakes one account's sync service via its registry handle (the same
    /// mechanism as the `TriggerSync` command). Unknown accounts are ignored.
    async fn wake_sync(&self, account: kestrel_core::ids::AccountId) {
        let registry = self.registry.lock().await;
        if let Some(handle) = registry.get(&account) {
            handle.trigger.notify_one();
        }
    }

    /// Queues a per-message move push op (server-side UID MOVE) for the
    /// given messages into the destination folder. Best-effort; see
    /// [`Self::enqueue_flag_push`].
    async fn enqueue_move_push(
        &self,
        messages: &[kestrel_core::ids::MessageId],
        to: kestrel_core::ids::FolderId,
    ) {
        use kestrel_core::store_model::{PushOpPayload, PushOpType};
        let Ok(locations) = self.storage.message_locations(messages.to_vec()).await else {
            return;
        };
        let account = self.current_account().await;
        for (message, from_folder, uid) in locations {
            if from_folder == to {
                continue; // no-op move
            }
            let payload = PushOpPayload::Move {
                message,
                from_folder,
                uid,
                to_folder: to,
            };
            if let Err(e) = self
                .storage
                .push_enqueue(account, PushOpType::Move, payload)
                .await
            {
                tracing::warn!(error = %e, "failed to enqueue move push op");
            }
        }
        // Drain now (see wake_sync).
        self.wake_sync(account).await;
    }

    /// The account the queued push ops belong to. Single-account today:
    /// `first_account` covers the current data model and matches the
    /// offline-journal behavior.
    async fn current_account(&self) -> AccountId {
        self.first_account().await
    }

    async fn first_account(&self) -> AccountId {
        self.storage
            .list_accounts()
            .await
            .ok()
            .and_then(|a| a.into_iter().next().map(|a| a.id))
            .unwrap_or_else(|| AccountId::from_uuid(uuid::Uuid::now_v7()))
    }

    /// Drains all pending ops and replays them FIFO (sync-engine.md §6).
    /// Failed replays are marked for retry; successful ones are removed.
    #[instrument(skip_all)]
    async fn replay_pending_ops(&self) {
        let accounts = match self.storage.list_accounts().await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e, "failed to list accounts for pending ops replay");
                return;
            }
        };
        for acct in &accounts {
            let ops = match self.storage.pending_ops_drain(acct.id).await {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!(
                        account = %acct.id,
                        error = %e,
                        "failed to drain pending ops"
                    );
                    continue;
                }
            };
            if ops.is_empty() {
                continue;
            }
            tracing::info!(
                account = %acct.id,
                count = ops.len(),
                "replaying pending offline ops"
            );
            for op in ops {
                let result = self.replay_one_op(&op).await;
                match result {
                    Ok(()) => {
                        if let Err(e) = self.storage.pending_ops_remove(op.id).await {
                            tracing::warn!(
                                op_id = op.id,
                                error = %e,
                                "failed to remove replayed pending op"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            op_id = op.id,
                            op_type = %op.op_type,
                            error = %e,
                            "pending op replay failed"
                        );
                        let _ = self
                            .storage
                            .pending_ops_mark_failed(op.id, e.to_string())
                            .await;
                    }
                }
            }
        }
    }

    /// Replays a single pending op by re-dispatching the equivalent mutation.
    #[instrument(skip_all, fields(uid = op.id))]
    async fn replay_one_op(&self, op: &kestrel_storage::PendingOp) -> Result<(), KestrelError> {
        use kestrel_storage::OpType as T;
        match &op.op_type {
            T::Flag => {
                if let PendingOpPayload::Flag { messages, flags } = &op.payload {
                    let flag_op = flags.to_flag_op();
                    let affected = self.storage.set_flags(messages.clone(), flag_op).await?;
                    self.bus
                        .publish(EngineEvent::FlagsChanged { messages: affected });
                }
            }
            T::Move => {
                if let PendingOpPayload::Move { messages, to } = &op.payload {
                    let moves: Vec<_> = messages.iter().map(|id| (*id, *to, u32::MAX)).collect();
                    self.storage.move_messages(moves).await?;
                }
            }
            T::Delete => {
                if let PendingOpPayload::Delete { messages, .. } = &op.payload {
                    let removed = self.storage.delete_messages(messages.clone()).await?;
                    self.bus.publish(EngineEvent::MessagesChanged {
                        folder: kestrel_core::ids::FolderId::from_uuid(self.ids.next_id()),
                        changed: 0,
                        removed: u32::try_from(removed).unwrap_or(u32::MAX),
                    });
                }
            }
            T::Compose => {
                if let PendingOpPayload::Compose { draft } = &op.payload {
                    let id = self.compose_submit(draft.as_ref().clone()).await?;
                    self.bus.publish(EngineEvent::OutboxEnqueued { id });
                }
            }
        }
        Ok(())
    }
}

/// Window default helper for the router's list command (protocol §2 allows
/// implementations to default).
#[must_use]
pub fn default_window() -> Window {
    Window::default()
}
