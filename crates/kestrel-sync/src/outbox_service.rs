//! `OutboxService` (sync-engine.md §6): drains due rows through SMTP with
//! exponential backoff + jitter, files sent mail to the Sent folder via
//! IMAP APPEND (UIDPLUS), and emits `OutboxRetry` / `MailSent` /
//! `MailFailed` events. Offline mode defers flushing entirely.

use std::{sync::Arc, time::Duration};

use kestrel_core::{
    clock::Clock,
    error::KestrelError,
    protocol::EngineEvent,
    store_model::{MailStore, OutboxRow},
};
use tokio_util::sync::CancellationToken;

use crate::{
    error::{SyncError, SyncResult},
    session::{ConnectParams, ImapSession},
    smtp::{self, SmtpParams},
};

/// Backoff schedule (sync-engine.md §6).
const BACKOFF_SCHEDULE_MS: [u64; 12] = [
    30_000, 120_000, 480_000, 1_800_000, 7_200_000, 21_600_000, 21_600_000, 21_600_000, 21_600_000,
    21_600_000, 21_600_000, 21_600_000,
];

/// The outbox flush service.
pub struct OutboxService {
    storage: std::sync::Arc<dyn MailStore>,
    smtp: SmtpParams,
    imap: ConnectParams,
    clock: Arc<dyn Clock>,
    bus: tokio::sync::mpsc::Sender<EngineEvent>,
    /// Offline gate (GoOffline/GoOnline).
    online: Arc<std::sync::atomic::AtomicBool>,
}

impl OutboxService {
    /// Creates the service.
    #[must_use]
    pub fn new(
        storage: std::sync::Arc<dyn MailStore>,
        smtp: SmtpParams,
        imap: ConnectParams,
        clock: Arc<dyn Clock>,
        bus: tokio::sync::mpsc::Sender<EngineEvent>,
        online: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            storage,
            smtp,
            imap,
            clock,
            bus,
            online,
        }
    }

    /// Backoff for a retry count, with deterministic jitter (±20%).
    #[must_use]
    pub fn backoff_for(retry_count: u32) -> Duration {
        // clippy::cast_precision_loss: schedule values are far below 2^52.
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        {
            let idx = (retry_count.saturating_sub(1)) as usize % BACKOFF_SCHEDULE_MS.len();
            let base = BACKOFF_SCHEDULE_MS[idx];
            // Jitter derived from the retry counter: stable for tests.
            let jitter = 1.0 + (f64::from(retry_count % 10) / 25.0) - 0.2;
            Duration::from_millis((base as f64 * jitter) as u64)
        }
    }

    /// Flush loop until cancellation. Each pass drains all due entries.
    pub async fn run(&self, cancel: CancellationToken) {
        let mut ticker = tokio::time::interval(Duration::from_secs(5));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    // Final bounded flush (≤5s budget per entry).
                    let _ = Box::pin(self.flush_pass()).await;
                    return;
                }
                _ = ticker.tick() => {
                    if self.online.load(std::sync::atomic::Ordering::Relaxed)
                        && let Err(e) = Box::pin(self.flush_pass()).await {
                            tracing::warn!(error = %e, "outbox flush pass failed");
                        }
                }
            }
        }
    }

    async fn flush_pass(&self) -> SyncResult<()> {
        let due = self.storage.outbox_due().await.map_err(SyncError::from)?;
        for row in due {
            match Box::pin(self.try_send(&row)).await {
                SendOutcome::Sent => {
                    let _ = self
                        .storage
                        .outbox_mark_sent(row.id, self.clock.now_unix_ms())
                        .await;
                    let _ = self
                        .bus
                        .send(EngineEvent::MailSent {
                            id: row.id,
                            message: sent_marker(),
                        })
                        .await;
                }
                SendOutcome::Transient(detail) => {
                    let retry = row.retry_count + 1;
                    let next = self.clock.now_unix_ms()
                        + i64::try_from(Self::backoff_for(retry).as_millis()).unwrap_or(0);
                    let _ = self
                        .storage
                        .outbox_mark_retry(row.id, retry, next, &detail)
                        .await;
                    let _ = self
                        .bus
                        .send(EngineEvent::OutboxRetry {
                            id: row.id,
                            attempt: retry,
                            next_in: Self::backoff_for(retry),
                            last_error: detail,
                        })
                        .await;
                    if retry >= 12 {
                        let _ = self
                            .bus
                            .send(EngineEvent::MailFailed {
                                id: row.id,
                                error: KestrelError::RetryExhausted { attempts: retry },
                                permanent: true,
                            })
                            .await;
                    }
                }
                SendOutcome::Permanent(detail) => {
                    let _ = self
                        .bus
                        .send(EngineEvent::MailFailed {
                            id: row.id,
                            error: KestrelError::MessageRejected {
                                detail: detail.clone(),
                            },
                            permanent: true,
                        })
                        .await;
                }
            }
        }
        Ok(())
    }

    async fn try_send(&self, row: &OutboxRow) -> SendOutcome {
        // Load the raw from the CAS.
        let Ok(raw) = self.storage.read_blob(&row.raw_blob).await else {
            return SendOutcome::Permanent("raw blob missing from CAS".into());
        };
        let mut recipients: Vec<String> = row
            .envelope
            .to
            .iter()
            .chain(&row.envelope.cc)
            .chain(&row.envelope.bcc)
            .map(|a| a.email.clone())
            .collect();
        recipients.dedup();
        match smtp::submit_envelope(&self.smtp, &row.envelope.from.email, &recipients, &raw).await {
            Ok(()) => {
                // Sent APPEND (best-effort; a failure here still counts as
                // sent — the server copy exists).
                let _ = Box::pin(self.append_to_sent(&raw)).await;
                SendOutcome::Sent
            }
            Err(KestrelError::SmtpTransient { code }) => {
                SendOutcome::Transient(format!("smtp {code}"))
            }
            Err(
                KestrelError::ConnectionLost { detail } | KestrelError::TlsHandshake { detail },
            ) => SendOutcome::Transient(detail),
            Err(e) => SendOutcome::Permanent(e.to_string()),
        }
    }

    /// APPEND the sent raw to the account's Sent folder (best-effort).
    async fn append_to_sent(&self, raw: &[u8]) -> SyncResult<()> {
        let mut session = Box::pin(ImapSession::connect_and_authenticate(&self.imap)).await?;
        let sent_name = "Sent";
        let flags: Vec<imap_next::imap_types::flag::Flag<'static>> = vec![];
        let date = None;
        let mailbox = imap_next::imap_types::mailbox::Mailbox::try_from(sent_name.to_owned())
            .map_err(|e| SyncError::Protocol(format!("sent mailbox: {e:?}")))?;
        let outcome = Box::pin(
            session.execute(
                imap_next::imap_types::command::CommandBody::Append {
                    mailbox,
                    flags,
                    date,
                    message: imap_next::imap_types::extensions::binary::LiteralOrLiteral8::Literal(
                        imap_next::imap_types::core::Literal::try_from(raw.to_vec())
                            .map_err(|e| SyncError::Protocol(format!("append literal: {e:?}")))?,
                    ),
                },
                Duration::from_mins(1),
            ),
        )
        .await?;
        Box::pin(session.logout()).await;
        if outcome.is_ok() {
            Ok(())
        } else {
            // Non-fatal: mail is delivered; Sent filing failed.
            tracing::warn!("Sent APPEND failed: {}", outcome.status_summary());
            Ok(())
        }
    }
}

enum SendOutcome {
    Sent,
    Transient(String),
    Permanent(String),
}

fn sent_marker() -> kestrel_core::ids::MessageId {
    // The outbox row is the durable record; the engine emits MailSent with
    // the outbox id mapped onto a fresh message id for frontend lists.
    kestrel_core::ids::MessageId::from_uuid(uuid::Uuid::now_v7())
}
