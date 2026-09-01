//! `SnoozeService`: polls for due snoozes and emits `SnoozeExpired` events
//! so frontends can re-show the message.

use std::{sync::Arc, time::Duration};

use kestrel_core::{protocol::EngineEvent, store_model::MailStore};
use tokio_util::sync::CancellationToken;

/// Poll interval for checking due snoozes.
const SNOOZE_POLL_INTERVAL: Duration = Duration::from_mins(1);

/// The snooze expiry service.
pub struct SnoozeService {
    storage: Arc<dyn MailStore>,
    bus: tokio::sync::broadcast::Sender<EngineEvent>,
}

impl SnoozeService {
    /// Creates the service.
    #[must_use]
    pub fn new(
        storage: Arc<dyn MailStore>,
        _clock: Arc<dyn kestrel_core::clock::Clock>,
        bus: tokio::sync::broadcast::Sender<EngineEvent>,
    ) -> Self {
        Self { storage, bus }
    }

    /// Poll loop until cancellation. Each pass drains all due snoozes.
    pub async fn run(&self, cancel: CancellationToken) {
        let mut ticker = tokio::time::interval(SNOOZE_POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                _ = ticker.tick() => {
                    if let Err(e) = self.check_due().await {
                        tracing::warn!(error = %e, "snooze check failed");
                    }
                }
            }
        }
    }

    async fn check_due(&self) -> Result<(), kestrel_core::error::KestrelError> {
        let due = self.storage.get_due_snoozes().await?;
        for entry in &due {
            let _ = self.bus.send(EngineEvent::SnoozeExpired {
                message: entry.message,
                account: entry.account,
                folder: entry.folder,
            });
            // Remove the snooze after emitting the event.
            if let Err(e) = self.storage.remove_snooze(entry.message).await {
                tracing::warn!(
                    message = %entry.message,
                    error = %e,
                    "failed to remove expired snooze"
                );
            }
        }
        Ok(())
    }
}
