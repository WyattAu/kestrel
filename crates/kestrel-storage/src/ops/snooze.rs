//! Snooze operations (cache.db) for deferred message visibility.

use kestrel_core::ids::{AccountId, FolderId, MessageId};
use tracing::instrument;

use super::parse_id;
use crate::error::StorageResult;

/// One snooze entry.
#[derive(Clone, Debug)]
pub struct SnoozeRow {
    /// Snooze id.
    pub id: String,
    /// Snoozed message.
    pub message_id: MessageId,
    /// Account that owns the message.
    pub account_id: AccountId,
    /// Folder containing the message.
    pub folder_id: FolderId,
    /// When the snooze expires (unix ms).
    pub snoozed_until: i64,
    /// When the snooze was created (unix ms).
    pub created_at: i64,
}

/// Snooze operations extension.
pub(crate) trait StoreSnoozeExt {
    /// Enqueue a snooze entry.
    fn enqueue_snooze(
        &self,
        message: MessageId,
        account: AccountId,
        folder: FolderId,
        until: i64,
    ) -> impl Future<Output = StorageResult<String>>;

    /// Get all snoozes that have expired (due).
    fn get_due_snoozes(&self) -> impl Future<Output = StorageResult<Vec<SnoozeRow>>>;

    /// Remove a snooze by message id.
    fn remove_snooze(&self, message: MessageId) -> impl Future<Output = StorageResult<()>>;
}

impl StoreSnoozeExt for super::Store {
    #[instrument(skip_all, fields(message = %message))]
    async fn enqueue_snooze(
        &self,
        message: MessageId,
        account: AccountId,
        folder: FolderId,
        until: i64,
    ) -> StorageResult<String> {
        let id = format!("{}", self.ids.next_id());
        let now = self.clock.now_unix_ms();
        sqlx::query!(
            "INSERT INTO snooze (id, message_id, account_id, folder_id, snoozed_until, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            id,
            message.to_string(),
            account.to_string(),
            folder.to_string(),
            until,
            now
        )
        .execute(&self.db.cache.write)
        .await?;
        Ok(id)
    }

    #[instrument(skip_all)]
    async fn get_due_snoozes(&self) -> StorageResult<Vec<SnoozeRow>> {
        let now = self.clock.now_unix_ms();
        #[derive(sqlx::FromRow)]
        struct SnoozeDbRow {
            id: String,
            message_id: String,
            account_id: String,
            folder_id: String,
            snoozed_until: i64,
            created_at: i64,
        }
        let rows = sqlx::query_as!(
            SnoozeDbRow,
            "SELECT id, message_id, account_id, folder_id, snoozed_until, created_at
             FROM snooze
             WHERE snoozed_until <= ?1
             ORDER BY snoozed_until",
            now
        )
        .fetch_all(&self.db.cache.read)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok(SnoozeRow {
                    id: r.id,
                    message_id: parse_id::<MessageId>(&r.message_id)?,
                    account_id: parse_id::<AccountId>(&r.account_id)?,
                    folder_id: parse_id::<FolderId>(&r.folder_id)?,
                    snoozed_until: r.snoozed_until,
                    created_at: r.created_at,
                })
            })
            .collect()
    }

    #[instrument(skip_all, fields(message = %message))]
    async fn remove_snooze(&self, message: MessageId) -> StorageResult<()> {
        sqlx::query!(
            "DELETE FROM snooze WHERE message_id = ?1",
            message.to_string()
        )
        .execute(&self.db.cache.write)
        .await?;
        Ok(())
    }
}
