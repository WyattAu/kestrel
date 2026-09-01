//! Outbox operations (data.db) + cross-DB blob ref adjustments (ADR 0009).

use kestrel_core::ids::{AccountId, BlobHash, OutboxId};
use tracing::instrument;

use crate::{
    error::{StorageError, StorageResult},
    ops::{Store, StoreBlobsExt, parse_id},
    store::OutboxEnvelope,
};

/// `raw_rfc822_blob` row shape.
#[derive(sqlx::FromRow)]
struct RawBlobRow {
    raw_rfc822_blob: String,
}

/// Due-outbox row hydration shape.
#[derive(sqlx::FromRow)]
struct DueRow {
    id: String,
    account_id: String,
    raw_rfc822_blob: String,
    envelope: String,
    retry_count: i64,
    last_error: Option<String>,
    created_at: i64,
}

/// Outbox operations extension.
pub(crate) trait StoreOutboxExt {
    /// Persists a draft: raw → CAS, envelope + inline copy → outbox row.
    fn outbox_enqueue(
        &self,
        account: AccountId,
        envelope: &OutboxEnvelope,
        raw: &[u8],
        send_after: Option<i64>,
    ) -> impl Future<Output = StorageResult<OutboxId>>;
    /// Due entries (unsent, retry time reached), oldest first.
    fn outbox_due(&self) -> impl Future<Output = StorageResult<Vec<crate::store::OutboxRow>>>;
    /// Records a retry.
    fn outbox_mark_retry(
        &self,
        id: OutboxId,
        retry_count: u32,
        next_attempt_at: i64,
        last_error: &str,
    ) -> impl Future<Output = StorageResult<()>>;
    /// Records a successful send (drops the raw blob reference).
    fn outbox_mark_sent(
        &self,
        id: OutboxId,
        sent_at: i64,
    ) -> impl Future<Output = StorageResult<()>>;
    /// Cancels an entry (drops the raw blob reference).
    fn outbox_cancel(&self, id: OutboxId) -> impl Future<Output = StorageResult<()>>;
}

impl StoreOutboxExt for Store {
    #[instrument(skip_all, fields(account = %account))]
    async fn outbox_enqueue(
        &self,
        account: AccountId,
        envelope: &OutboxEnvelope,
        raw: &[u8],
        send_after: Option<i64>,
    ) -> StorageResult<OutboxId> {
        let hash = self.blobs.write(raw).await?;
        let id = OutboxId::from_uuid(self.ids.next_id());
        let envelope_json = serde_json::to_string(envelope)?;
        let now = self.clock.now_unix_ms();
        // Raw inline durability copy for messages under 64 KiB
        // (docs/schema.md §3.1 note).
        let inline: Option<&[u8]> = (raw.len() < 64 * 1024).then_some(raw);
        let tx = &self.db.data.write;
        sqlx::query!(
            "INSERT INTO outbox (id, account_id, raw_rfc822_blob, raw_inline, envelope, retry_count, next_attempt_at, send_after, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, NULL, ?6, ?7)",
            id.to_string(),
            account.to_string(),
            hash.to_hex(),
            inline,
            envelope_json,
            send_after,
            now
        )
        .execute(tx)
        .await?;
        // Cross-DB blob reference (outbox lives in data.db, registry in
        // cache.db — adjusted in code per ADR 0009).
        self.adjust_outbox_ref_inner(&hash, 1).await?;
        Ok(id)
    }

    #[instrument(skip_all)]
    async fn outbox_due(&self) -> StorageResult<Vec<crate::store::OutboxRow>> {
        let now = self.clock.now_unix_ms();
        let rows = sqlx::query_as!(
            DueRow,
            "SELECT id, account_id, raw_rfc822_blob, envelope, retry_count, last_error, created_at
             FROM outbox
             WHERE sent_at IS NULL
               AND (next_attempt_at IS NULL OR next_attempt_at <= ?1)
               AND (send_after IS NULL OR send_after <= ?1)
             ORDER BY created_at",
            now
        )
        .fetch_all(&self.db.data.read)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok(crate::store::OutboxRow {
                    id: parse_id::<OutboxId>(&r.id)?,
                    account: parse_id::<AccountId>(&r.account_id)?,
                    raw_blob: BlobHash::parse_hex(&r.raw_rfc822_blob).ok_or_else(|| {
                        StorageError::Row(format!(
                            "outbox blob hash invalid: {}",
                            r.raw_rfc822_blob
                        ))
                    })?,
                    envelope: serde_json::from_str(&r.envelope)?,
                    retry_count: u32::try_from(r.retry_count.max(0)).unwrap_or(0),
                    last_error: r.last_error,
                    created_at: r.created_at,
                })
            })
            .collect()
    }

    #[instrument(skip_all, fields(uid = %id))]
    async fn outbox_mark_retry(
        &self,
        id: OutboxId,
        retry_count: u32,
        next_attempt_at: i64,
        last_error: &str,
    ) -> StorageResult<()> {
        sqlx::query!(
            "UPDATE outbox SET retry_count = ?2, next_attempt_at = ?3, last_error = ?4 WHERE id = ?1 AND sent_at IS NULL",
            id.to_string(),
            i64::from(retry_count),
            next_attempt_at,
            last_error
        )
        .execute(&self.db.data.write)
        .await?;
        Ok(())
    }

    #[instrument(skip_all, fields(uid = %id))]
    async fn outbox_mark_sent(&self, id: OutboxId, sent_at: i64) -> StorageResult<()> {
        let row = sqlx::query_as!(
            RawBlobRow,
            "SELECT raw_rfc822_blob FROM outbox WHERE id = ?1 AND sent_at IS NULL",
            id.to_string()
        )
        .fetch_optional(&self.db.data.write)
        .await?;
        let Some(RawBlobRow { raw_rfc822_blob }) = row else {
            return Ok(()); // already sent/cancelled — idempotent
        };
        sqlx::query!(
            "UPDATE outbox SET sent_at = ?2 WHERE id = ?1",
            id.to_string(),
            sent_at
        )
        .execute(&self.db.data.write)
        .await?;
        if let Some(hash) = BlobHash::parse_hex(&raw_rfc822_blob) {
            self.adjust_outbox_ref_inner(&hash, -1).await?;
        }
        Ok(())
    }

    #[instrument(skip_all, fields(uid = %id))]
    async fn outbox_cancel(&self, id: OutboxId) -> StorageResult<()> {
        let row = sqlx::query_as!(
            RawBlobRow,
            "SELECT raw_rfc822_blob FROM outbox WHERE id = ?1 AND sent_at IS NULL",
            id.to_string()
        )
        .fetch_optional(&self.db.data.write)
        .await?;
        sqlx::query!(
            "DELETE FROM outbox WHERE id = ?1 AND sent_at IS NULL",
            id.to_string()
        )
        .execute(&self.db.data.write)
        .await?;
        if let Some(RawBlobRow { raw_rfc822_blob }) = row
            && let Some(hash) = BlobHash::parse_hex(&raw_rfc822_blob)
        {
            self.adjust_outbox_ref_inner(&hash, -1).await?;
        }
        Ok(())
    }
}
