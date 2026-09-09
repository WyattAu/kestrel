//! Server-push queue (cache.db, migration 0005): flag/move mutations that
//! must be applied to the IMAP server (UID STORE / UID MOVE), drained at the
//! start of each sync cycle. Rows are per-message with the local folder/uid
//! snapshot captured at enqueue time, so the drain can target the exact
//! server messages and reconcile the new UIDs afterwards.
//!
//! Without this queue the local apply alone never reaches the server: a
//! locally archived message would reappear on the next delta sync (server
//! state is authoritative). This is distinct from `pending_ops` (0003),
//! which journals *offline-mode* commands for later local replay.
//!
//! Payload types live in `kestrel-core::store_model` (the `MailStore` seam).

use kestrel_core::{
    ids::AccountId,
    store_model::{PushOp, PushOpPayload, PushOpType},
};

use crate::{
    error::StorageResult,
    ops::{Store, parse_id},
};

/// Row shape for `SQLx` hydration.
#[derive(sqlx::FromRow)]
struct PushOpRow {
    id: i64,
    account_id: String,
    op_type: String,
    payload_json: String,
    created_at: i64,
    retry_count: i64,
    last_error: Option<String>,
}

/// Server-push queue extension.
pub(crate) trait StorePushQueueExt {
    /// Enqueues one mutation for server-side application.
    fn enqueue_push(
        &self,
        account: AccountId,
        op_type: PushOpType,
        payload: &PushOpPayload,
    ) -> impl Future<Output = StorageResult<i64>>;

    /// Drains all queued push ops for an account, ordered FIFO.
    fn drain_push_queue(
        &self,
        account: AccountId,
    ) -> impl Future<Output = StorageResult<Vec<PushOp>>>;

    /// Marks a push op as failed (increments retry, records error).
    fn mark_push_failed(&self, id: i64, error: &str) -> impl Future<Output = StorageResult<()>>;

    /// Removes a push op after successful application.
    fn remove_push(&self, id: i64) -> impl Future<Output = StorageResult<()>>;
}

impl StorePushQueueExt for Store {
    async fn enqueue_push(
        &self,
        account: AccountId,
        op_type: PushOpType,
        payload: &PushOpPayload,
    ) -> StorageResult<i64> {
        let now = self.clock.now_unix_ms();
        let payload_json = serde_json::to_string(payload)?;
        let op_type_str = op_type.to_string();
        let row = sqlx::query!(
            "INSERT INTO push_queue (account_id, op_type, payload_json, created_at, retry_count, last_error)
             VALUES (?1, ?2, ?3, ?4, 0, NULL)",
            account.to_string(),
            op_type_str,
            payload_json,
            now
        )
        .execute(&self.db.cache.write)
        .await?;
        Ok(row.last_insert_rowid())
    }

    async fn drain_push_queue(&self, account: AccountId) -> StorageResult<Vec<PushOp>> {
        let rows = sqlx::query_as!(
            PushOpRow,
            "SELECT id, account_id, op_type, payload_json, created_at, retry_count, last_error
             FROM push_queue
             WHERE account_id = ?1
             ORDER BY created_at ASC, id ASC",
            account.to_string()
        )
        .fetch_all(&self.db.cache.read)
        .await?;
        rows.into_iter()
            .map(|r| {
                let account_id = parse_id::<AccountId>(&r.account_id)?;
                let op_type: PushOpType = match r.op_type.as_str() {
                    "flag" => PushOpType::Flag,
                    "move" => PushOpType::Move,
                    other => {
                        return Err(crate::error::StorageError::Row(format!(
                            "push_queue op_type: {other}"
                        )));
                    }
                };
                let payload: PushOpPayload = serde_json::from_str(&r.payload_json)?;
                Ok(PushOp {
                    id: r.id,
                    account_id,
                    op_type,
                    payload,
                    created_at: r.created_at,
                    retry_count: u32::try_from(r.retry_count.max(0)).unwrap_or(0),
                    last_error: r.last_error,
                })
            })
            .collect()
    }

    async fn mark_push_failed(&self, id: i64, error: &str) -> StorageResult<()> {
        sqlx::query!(
            "UPDATE push_queue
             SET retry_count = retry_count + 1, last_error = ?2
             WHERE id = ?1",
            id,
            error
        )
        .execute(&self.db.cache.write)
        .await?;
        Ok(())
    }

    async fn remove_push(&self, id: i64) -> StorageResult<()> {
        sqlx::query!("DELETE FROM push_queue WHERE id = ?1", id)
            .execute(&self.db.cache.write)
            .await?;
        Ok(())
    }
}
