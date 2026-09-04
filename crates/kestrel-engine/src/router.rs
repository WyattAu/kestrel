//! Command router (message-protocol §1-2): bounded frontend inbox → service
//! dispatch → replies; events published on the bus. Ordered shutdown per
//! architecture §3.3.

use std::{
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
    protocol::{CommandPayload, EngineEvent, Reply, ShutdownStage, Window},
};
use kestrel_storage::{
    FlagPayload, OpType, OutboxEnvelope, PendingOpPayload, SearchHandle, StorageHandle,
};
use tokio::sync::mpsc;
use tracing::{Instrument, instrument};

use crate::bus::EventBus;

/// The engine's frontend-facing router.
pub struct EngineRouter {
    config: Arc<tokio::sync::RwLock<Arc<Config>>>,
    storage: StorageHandle,
    search: SearchHandle,
    bus: EventBus,
    ids: Arc<dyn IdGenerator>,
    clock: Arc<dyn Clock>,
    creds: std::sync::Arc<kestrel_crypto::CredentialService>,
    /// Per-account `SyncService` cancellation tokens (started on `AddAccount`).
    sync_tasks: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<AccountId, tokio_util::sync::CancellationToken>,
        >,
    >,
    /// Offline mode flag (sync-engine.md §6).
    offline: Arc<AtomicBool>,
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
        creds: std::sync::Arc<kestrel_crypto::CredentialService>,
    ) -> Self {
        Self {
            config: Arc::new(tokio::sync::RwLock::new(config)),
            storage,
            search,
            bus,
            ids,
            clock,
            creds,
            sync_tasks: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            offline: Arc::new(AtomicBool::new(false)),
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
                // Cancel the sync service.
                if let Some(token) = self.sync_tasks.lock().await.remove(&account) {
                    token.cancel();
                }
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

            // ---- sync control ----
            // Phase 2 attaches sync-mode handling (fire-and-forget per
            // the protocol; no reply by construction).
            P::TriggerSync { .. } => {}
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

        // 3. Start the per-account sync service.
        let cancel = tokio_util::sync::CancellationToken::new();
        self.sync_tasks
            .lock()
            .await
            .insert(account_id, cancel.clone());

        let store: std::sync::Arc<dyn kestrel_core::store_model::MailStore> =
            std::sync::Arc::new(self.storage.clone());

        let token = self
            .creds
            .password(account_id)
            .map_err(KestrelError::from)?
            .unwrap_or_else(|| kestrel_core::secrets::SecretString::new(String::new()));

        if config.provider == kestrel_core::protocol::Provider::Jmap {
            let jmap_host = config.imap_host.clone();
            kestrel_sync::JmapSyncService::spawn(
                account_id,
                jmap_host,
                token,
                store,
                Arc::clone(&self.clock),
                self.bus_forwarder(),
                cancel,
            );
        } else {
            let security = Self::parse_security(&config.imap_security);
            let connect = kestrel_sync::ConnectParams {
                host: config.imap_host.clone(),
                port: config.imap_port,
                security,
                username: config
                    .username
                    .clone()
                    .unwrap_or_else(|| config.email.clone()),
                secret: token,
                mechanisms: match &config.provider {
                    kestrel_core::protocol::Provider::Gmail
                    | kestrel_core::protocol::Provider::Yahoo
                    | kestrel_core::protocol::Provider::Aol => {
                        vec![
                            kestrel_core::sasl::SaslMechanism::Xoauth2,
                            kestrel_core::sasl::SaslMechanism::Plain,
                        ]
                    }
                    kestrel_core::protocol::Provider::Outlook
                    | kestrel_core::protocol::Provider::Fastmail => {
                        vec![kestrel_core::sasl::SaslMechanism::Plain]
                    }
                    _ => vec![
                        kestrel_core::sasl::SaslMechanism::Plain,
                        kestrel_core::sasl::SaslMechanism::Login,
                        kestrel_core::sasl::SaslMechanism::ScramSha256,
                    ],
                },
                tls: tokio_rustls::TlsConnector::from(
                    kestrel_crypto::tls_config(None).map_err(KestrelError::from)?,
                ),
                sasl_factory: std::sync::Arc::new(|mech, user, secret| {
                    kestrel_crypto::sasl::start(mech, user, secret)
                }),
            };
            let service = kestrel_sync::SyncService::new(
                account_id,
                connect,
                store.clone(),
                Arc::clone(&*self.config.read().await),
                Arc::clone(&self.clock),
                self.bus_forwarder(),
            );
            tokio::spawn(async move { service.run(cancel).await });

            // Spawn the outbox flush service for this account.
            let smtp_params = kestrel_sync::SmtpParams {
                host: config.smtp_host.clone(),
                port: config.smtp_port,
                username: config
                    .username
                    .clone()
                    .unwrap_or_else(|| config.email.clone()),
                secret: password.clone(),
                oauth2: config.auth_kind == "oauth2",
                security: match config.smtp_security.as_str() {
                    "starttls" => kestrel_sync::SmtpSecurity::StartTls,
                    _ => kestrel_sync::SmtpSecurity::ImplicitTls,
                },
            };
            let imap_connect = kestrel_sync::ConnectParams {
                host: config.imap_host.clone(),
                port: config.imap_port,
                security,
                username: config
                    .username
                    .clone()
                    .unwrap_or_else(|| config.email.clone()),
                secret: password,
                mechanisms: vec![kestrel_core::sasl::SaslMechanism::Plain],
                tls: tokio_rustls::TlsConnector::from(
                    kestrel_crypto::tls_config(None).map_err(KestrelError::from)?,
                ),
                sasl_factory: std::sync::Arc::new(|mech, user, secret| {
                    kestrel_crypto::sasl::start(mech, user, secret)
                }),
            };
            let online = std::sync::Arc::new(AtomicBool::new(true));
            let outbox = kestrel_sync::OutboxService::new(
                store,
                smtp_params,
                imap_connect,
                Arc::clone(&self.clock),
                self.bus_forwarder(),
                online,
            );
            let outbox_cancel = tokio_util::sync::CancellationToken::new();
            let outbox_span = tracing::info_span!("outbox", account = %account_id);
            tokio::spawn(async move { outbox.run(outbox_cancel).await }.instrument(outbox_span));
        }

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

        // If an existing account is found, cancel its sync service.
        if let Some(acct) = existing
            && let Some(token) = self.sync_tasks.lock().await.remove(&acct.id)
        {
            token.cancel();
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

        // Start the per-account sync service.
        let cancel = tokio_util::sync::CancellationToken::new();
        self.sync_tasks
            .lock()
            .await
            .insert(account_id, cancel.clone());

        let store: std::sync::Arc<dyn kestrel_core::store_model::MailStore> =
            std::sync::Arc::new(self.storage.clone());

        let token = self
            .creds
            .password(account_id)
            .map_err(KestrelError::from)?
            .unwrap_or_else(|| kestrel_core::secrets::SecretString::new(String::new()));

        if config.provider == kestrel_core::protocol::Provider::Jmap {
            let jmap_host = config.imap_host.clone();
            kestrel_sync::JmapSyncService::spawn(
                account_id,
                jmap_host,
                token,
                store,
                Arc::clone(&self.clock),
                self.bus_forwarder(),
                cancel,
            );
        } else {
            let security = Self::parse_security(&config.imap_security);
            let connect = kestrel_sync::ConnectParams {
                host: config.imap_host.clone(),
                port: config.imap_port,
                security,
                username: config
                    .username
                    .clone()
                    .unwrap_or_else(|| config.email.clone()),
                secret: token,
                mechanisms: match &config.provider {
                    kestrel_core::protocol::Provider::Gmail
                    | kestrel_core::protocol::Provider::Yahoo
                    | kestrel_core::protocol::Provider::Aol => {
                        vec![
                            kestrel_core::sasl::SaslMechanism::Xoauth2,
                            kestrel_core::sasl::SaslMechanism::Plain,
                        ]
                    }
                    kestrel_core::protocol::Provider::Outlook
                    | kestrel_core::protocol::Provider::Fastmail => {
                        vec![kestrel_core::sasl::SaslMechanism::Plain]
                    }
                    _ => vec![
                        kestrel_core::sasl::SaslMechanism::Plain,
                        kestrel_core::sasl::SaslMechanism::Login,
                        kestrel_core::sasl::SaslMechanism::ScramSha256,
                    ],
                },
                tls: tokio_rustls::TlsConnector::from(
                    kestrel_crypto::tls_config(None).map_err(KestrelError::from)?,
                ),
                sasl_factory: std::sync::Arc::new(|mech, user, secret| {
                    kestrel_crypto::sasl::start(mech, user, secret)
                }),
            };
            let service = kestrel_sync::SyncService::new(
                account_id,
                connect,
                store.clone(),
                Arc::clone(&*self.config.read().await),
                Arc::clone(&self.clock),
                self.bus_forwarder(),
            );
            tokio::spawn(async move { service.run(cancel).await });

            // Spawn the outbox flush service for this account.
            let smtp_params = kestrel_sync::SmtpParams {
                host: config.smtp_host.clone(),
                port: config.smtp_port,
                username: config
                    .username
                    .clone()
                    .unwrap_or_else(|| config.email.clone()),
                secret: password.clone(),
                oauth2: config.auth_kind == "oauth2",
                security: match config.smtp_security.as_str() {
                    "starttls" => kestrel_sync::SmtpSecurity::StartTls,
                    _ => kestrel_sync::SmtpSecurity::ImplicitTls,
                },
            };
            let imap_connect = kestrel_sync::ConnectParams {
                host: config.imap_host.clone(),
                port: config.imap_port,
                security,
                username: config
                    .username
                    .clone()
                    .unwrap_or_else(|| config.email.clone()),
                secret: password,
                mechanisms: vec![kestrel_core::sasl::SaslMechanism::Plain],
                tls: tokio_rustls::TlsConnector::from(
                    kestrel_crypto::tls_config(None).map_err(KestrelError::from)?,
                ),
                sasl_factory: std::sync::Arc::new(|mech, user, secret| {
                    kestrel_crypto::sasl::start(mech, user, secret)
                }),
            };
            let online = std::sync::Arc::new(AtomicBool::new(true));
            let outbox = kestrel_sync::OutboxService::new(
                store,
                smtp_params,
                imap_connect,
                Arc::clone(&self.clock),
                self.bus_forwarder(),
                online,
            );
            let outbox_cancel = tokio_util::sync::CancellationToken::new();
            let outbox_span = tracing::info_span!("outbox", account = %account_id);
            tokio::spawn(async move { outbox.run(outbox_cancel).await }.instrument(outbox_span));
        }

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

    /// Parses a security string ("tls" | "starttls") into the enum.
    fn parse_security(s: &str) -> kestrel_sync::Security {
        match s {
            "starttls" => kestrel_sync::Security::StartTls,
            _ => kestrel_sync::Security::Tls,
        }
    }

    /// Builds the `OAuth2` authorization URL for a provider.
    /// The frontend opens this URL in the browser; the engine will listen
    /// on a loopback server for the callback.
    #[instrument(skip_all)]
    async fn start_oauth2_flow(
        &self,
        provider: &kestrel_core::protocol::Provider,
    ) -> Result<String, KestrelError> {
        let client_id =
            std::env::var("KESTREL_OAUTH2_CLIENT_ID").unwrap_or_else(|_| "kestrel-desktop".into());
        let oauth_provider = match provider {
            kestrel_core::protocol::Provider::Gmail => {
                kestrel_crypto::oauth::MailProvider::gmail(&client_id)
            }
            kestrel_core::protocol::Provider::Outlook => {
                let tenant =
                    std::env::var("KESTREL_OAUTH2_TENANT").unwrap_or_else(|_| "common".into());
                kestrel_crypto::oauth::MailProvider::outlook(&client_id, &tenant)
            }
            kestrel_core::protocol::Provider::Yahoo => {
                kestrel_crypto::oauth::MailProvider::yahoo(&client_id)
            }
            kestrel_core::protocol::Provider::Fastmail => {
                kestrel_crypto::oauth::MailProvider::fastmail(&client_id)
            }
            _ => {
                return Err(KestrelError::DraftInvalid {
                    detail: "provider does not support OAuth2".into(),
                });
            }
        };
        let (flow, _handle) = kestrel_crypto::oauth::start_flow(
            &oauth_provider,
            None,
            std::time::Duration::from_mins(5),
        )
        .await
        .map_err(|e| KestrelError::OAuthFlowFailed {
            detail: e.to_string(),
        })?;
        Ok(flow.url)
    }

    /// Returns a channel sender that publishes events on the bus.
    fn bus_forwarder(&self) -> tokio::sync::mpsc::Sender<kestrel_core::protocol::EngineEvent> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let bus = self.bus.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                bus.publish(ev);
            }
        });
        tx
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
