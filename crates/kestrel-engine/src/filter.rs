//! Filter service: subscribes to `MailArrived` events, evaluates filter
//! rules against new messages, and executes matching actions through the
//! storage handle. Rules are loaded from the event bus (`ConfigUpdated`)
//! and kept in-memory for fast evaluation.
//!
//! The service follows ADR 0004: it is a long-running task owned by the
//! supervisor, with graceful shutdown via `CancellationToken`.

use std::sync::Arc;

use kestrel_core::{
    clock::Clock,
    error::KestrelError,
    ids::{AccountId, FolderId},
    protocol::{EngineEvent, Flag, FlagOp},
};
use kestrel_filter::{
    eval::{self, RegexCache},
    types::{Action, FieldValues, FilterRule},
};
use kestrel_storage::StorageHandle;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

/// The filter service: evaluates rules on incoming mail.
pub struct FilterService {
    storage: StorageHandle,
    _clock: Arc<dyn Clock>,
    bus: broadcast::Sender<EngineEvent>,
}

impl FilterService {
    /// Creates a new filter service.
    #[must_use]
    pub fn new(
        storage: StorageHandle,
        clock: Arc<dyn Clock>,
        bus: broadcast::Sender<EngineEvent>,
    ) -> Self {
        Self {
            storage,
            _clock: clock,
            bus,
        }
    }

    /// Runs the filter service until cancelled.
    ///
    /// Listens for `MailArrived` events and evaluates rules against new
    /// messages. Rules are reloaded when `ConfigUpdated` is received.
    #[instrument(skip_all)]
    pub async fn run(self, cancel: CancellationToken) {
        let mut events = self.bus.subscribe();
        let mut rules: Vec<FilterRule> = Vec::new();
        let regex_cache = RegexCache::default();

        tracing::info!("filter service started");

        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                event = events.recv() => {
                    match event {
                        Ok(EngineEvent::MailArrived { account, folder, .. }) => {
                            self.handle_mail_arrived(account, folder, &rules, &regex_cache).await;
                        }
                        Ok(EngineEvent::ConfigUpdated { .. }) => {
                            // Reload rules from storage on config change.
                            match self.load_rules().await {
                                Ok(loaded) => {
                                    rules = loaded;
                                    tracing::debug!(count = rules.len(), "filter rules reloaded");
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "failed to reload filter rules");
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(missed = n, "filter service lagged on event bus");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }

        tracing::info!("filter service stopped");
    }

    /// Load filter rules from storage.
    async fn load_rules(&self) -> Result<Vec<FilterRule>, KestrelError> {
        // Rules are stored as JSON in the settings table under the key
        // "filter_rules". This avoids adding new StoreCommand variants
        // while still keeping rules durable in data.db.
        match self.storage.get_setting("filter_rules").await? {
            Some(json) => {
                let rules: Vec<FilterRule> =
                    serde_json::from_str(&json).map_err(|e| KestrelError::Bug {
                        detail: format!("invalid filter_rules JSON: {e}"),
                    })?;
                Ok(rules)
            }
            None => Ok(Vec::new()),
        }
    }

    /// Handle a `MailArrived` event: fetch message summaries and evaluate
    /// rules.
    #[instrument(skip_all, fields(account = %account, folder = %folder))]
    async fn handle_mail_arrived(
        &self,
        account: AccountId,
        folder: FolderId,
        rules: &[FilterRule],
        regex_cache: &RegexCache,
    ) {
        if rules.is_empty() {
            return;
        }

        // List recent messages in the folder to evaluate rules against.
        let window = kestrel_core::protocol::Window {
            offset: 0,
            limit: 50,
        };
        let sort = kestrel_core::protocol::SortSpec::default();

        let messages = match self.storage.list_messages(folder, window, sort).await {
            Ok(page) => page.items,
            Err(e) => {
                tracing::warn!(error = %e, "failed to list messages for filter evaluation");
                return;
            }
        };

        // Filter rules for this account (or rules with no account restriction).
        let applicable: Vec<&FilterRule> = rules
            .iter()
            .filter(|r| {
                r.enabled
                    && (r.account_id == account
                        || r.account_id == AccountId::from_uuid(uuid::Uuid::nil()))
            })
            .collect();

        if applicable.is_empty() {
            return;
        }

        for msg in &messages {
            let field_values = FieldValues::from_message(msg, "");
            for rule in &applicable {
                if eval::evaluate_rule(rule, &field_values, regex_cache) {
                    tracing::info!(
                        rule = %rule.name,
                        message = %msg.id,
                        "filter rule matched"
                    );
                    self.execute_actions(&rule.actions, msg.id, account, folder)
                        .await;
                    // First matching rule wins (priority order).
                    break;
                }
            }
        }
    }

    /// Execute a set of actions for a matched message.
    #[instrument(skip_all, fields(message = %message))]
    async fn execute_actions(
        &self,
        actions: &[Action],
        message: kestrel_core::ids::MessageId,
        _account: AccountId,
        _source_folder: FolderId,
    ) {
        for action in actions {
            match action {
                Action::MoveTo(dest) => {
                    if let Err(e) = self
                        .storage
                        .move_messages(vec![(message, *dest, u32::MAX)])
                        .await
                    {
                        tracing::warn!(error = %e, "filter action move failed");
                    }
                }
                Action::CopyTo(_dest) => {
                    // Copy is not directly supported by the storage API;
                    // would require IMAP COPY + local ingest. Log for now.
                    tracing::debug!("filter action copy_to not yet implemented");
                }
                Action::Flag(flags) => {
                    let flag_op = FlagOp::Add(flags.clone());
                    if let Err(e) = self.storage.set_flags(vec![message], flag_op).await {
                        tracing::warn!(error = %e, "filter action flag failed");
                    }
                }
                Action::MarkRead => {
                    let flag_op = FlagOp::Add(vec![Flag::Seen]);
                    if let Err(e) = self.storage.set_flags(vec![message], flag_op).await {
                        tracing::warn!(error = %e, "filter action mark_read failed");
                    }
                }
                Action::Delete => {
                    if let Err(e) = self.storage.delete_messages(vec![message]).await {
                        tracing::warn!(error = %e, "filter action delete failed");
                    }
                }
                Action::Forward(addr) => {
                    // Forward requires building a new message and enqueuing
                    // to the outbox. Log for now; Phase 2 integration.
                    tracing::debug!(
                        to = %addr,
                        "filter action forward not yet implemented"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use kestrel_core::{
        ids::{FolderId, MessageId},
        protocol::{Address, MessageSummary, ThreadIdLite},
    };

    use super::*;

    fn make_summary(subject: &str, from_email: &str) -> MessageSummary {
        MessageSummary {
            id: MessageId::from_uuid(uuid::Uuid::now_v7()),
            folder: FolderId::from_uuid(uuid::Uuid::now_v7()),
            uid: 1,
            internal_date: 0,
            flags: vec![],
            message_id: None,
            in_reply_to: None,
            subject: Some(subject.to_string()),
            from: vec![Address {
                name: None,
                email: from_email.to_string(),
            }],
            to: vec![],
            cc: vec![],
            size: 100,
            is_read: false,
            is_flagged: false,
            is_answered: false,
            has_attachments: false,
            thread: ThreadIdLite {
                key: "test".to_string(),
            },
        }
    }

    #[test]
    fn field_values_from_message() {
        let msg = make_summary("Hello", "alice@example.com");
        let fv = FieldValues::from_message(&msg, "body text");
        assert_eq!(fv.from, "alice@example.com");
        assert_eq!(fv.subject, "Hello");
        assert_eq!(fv.body, "body text");
        assert!(!fv.has_attachment);
    }
}
