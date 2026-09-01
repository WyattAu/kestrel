//! Offline mutation journal (sync-engine.md §6): ops enqueued while offline,
//! replayed FIFO on reconnect. Stored in cache.db (ephemeral, rebuildable).

use kestrel_core::{
    ids::{AccountId, MessageId},
    protocol::FlagOp,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::StorageResult,
    ops::{Store, parse_id},
};

/// Operation type tag stored in the `op_type` column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpType {
    /// Flag mutation (add/remove/set flags on messages).
    Flag,
    /// Move messages between folders.
    Move,
    /// Delete messages.
    Delete,
    /// Compose/submit a draft.
    Compose,
}

impl std::fmt::Display for OpType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Flag => write!(f, "flag"),
            Self::Move => write!(f, "move"),
            Self::Delete => write!(f, "delete"),
            Self::Compose => write!(f, "compose"),
        }
    }
}

impl std::str::FromStr for OpType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "flag" => Ok(Self::Flag),
            "move" => Ok(Self::Move),
            "delete" => Ok(Self::Delete),
            "compose" => Ok(Self::Compose),
            _ => Err(format!("unknown op_type: {s}")),
        }
    }
}

/// Serializable payload for a single pending mutation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PendingOpPayload {
    /// Flag operation on messages.
    Flag {
        /// Target messages.
        messages: Vec<MessageId>,
        /// Flag operation.
        flags: FlagPayload,
    },
    /// Move messages to another folder.
    Move {
        /// Target messages.
        messages: Vec<MessageId>,
        /// Destination folder.
        to: kestrel_core::ids::FolderId,
    },
    /// Delete messages.
    Delete {
        /// Target messages.
        messages: Vec<MessageId>,
        /// Whether to expunge immediately.
        expunge: bool,
    },
    /// Compose and submit a draft.
    Compose {
        /// The draft to submit.
        draft: Box<kestrel_core::protocol::Draft>,
    },
}

/// Serializable flag operation (mirrors [`FlagOp`] for serde).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FlagPayload {
    /// Replace the whole flag set.
    Set(Vec<String>),
    /// Add flags.
    Add(Vec<String>),
    /// Remove flags.
    Remove(Vec<String>),
}

impl FlagPayload {
    /// Converts to the protocol [`FlagOp`].
    #[must_use]
    pub fn to_flag_op(&self) -> FlagOp {
        use kestrel_core::protocol::Flag;
        let flags: Vec<Flag> = self
            .as_strings()
            .iter()
            .map(|s| match s.as_str() {
                "\\Seen" => Flag::Seen,
                "\\Answered" => Flag::Answered,
                "\\Flagged" => Flag::Flagged,
                "\\Deleted" => Flag::Deleted,
                "\\Draft" => Flag::Draft,
                other => Flag::Custom(other.to_string()),
            })
            .collect();
        match self {
            FlagPayload::Set(_) => FlagOp::Set(flags),
            FlagPayload::Add(_) => FlagOp::Add(flags),
            FlagPayload::Remove(_) => FlagOp::Remove(flags),
        }
    }

    /// Returns the raw flag strings.
    #[must_use]
    pub fn as_strings(&self) -> &[String] {
        match self {
            FlagPayload::Set(s) | FlagPayload::Add(s) | FlagPayload::Remove(s) => s,
        }
    }
}

impl From<&FlagOp> for FlagPayload {
    fn from(op: &FlagOp) -> Self {
        use kestrel_core::protocol::Flag;
        let wire = |flags: &[Flag]| -> Vec<String> {
            flags
                .iter()
                .map(|f| match f {
                    Flag::Seen => "\\Seen".to_string(),
                    Flag::Answered => "\\Answered".to_string(),
                    Flag::Flagged => "\\Flagged".to_string(),
                    Flag::Deleted => "\\Deleted".to_string(),
                    Flag::Draft => "\\Draft".to_string(),
                    Flag::Custom(s) => s.clone(),
                })
                .collect()
        };
        match op {
            FlagOp::Set(f) => FlagPayload::Set(wire(f)),
            FlagOp::Add(f) => FlagPayload::Add(wire(f)),
            FlagOp::Remove(f) => FlagPayload::Remove(wire(f)),
        }
    }
}

/// A pending offline operation.
#[derive(Clone, Debug)]
pub struct PendingOp {
    /// Row id.
    pub id: i64,
    /// Owning account.
    pub account_id: AccountId,
    /// Operation category.
    pub op_type: OpType,
    /// Serialized operation payload.
    pub payload: PendingOpPayload,
    /// Creation timestamp (unix ms).
    pub created_at: i64,
    /// Number of replay attempts.
    pub retry_count: u32,
    /// Last replay error, if any.
    pub last_error: Option<String>,
}

/// Row shape for `SQLx` hydration.
#[derive(sqlx::FromRow)]
struct PendingOpRow {
    id: i64,
    account_id: String,
    op_type: String,
    payload_json: String,
    created_at: i64,
    retry_count: i64,
    last_error: Option<String>,
}

/// Offline mutation journal extension.
pub(crate) trait StorePendingOpsExt {
    /// Enqueues a mutation for later replay.
    fn enqueue_pending_op(
        &self,
        account: AccountId,
        op_type: OpType,
        payload: &PendingOpPayload,
    ) -> impl Future<Output = StorageResult<i64>>;

    /// Drains all pending ops for an account, ordered FIFO.
    fn drain_pending_ops(
        &self,
        account: AccountId,
    ) -> impl Future<Output = StorageResult<Vec<PendingOp>>>;

    /// Marks a pending op as failed (increments retry, records error).
    fn mark_pending_op_failed(
        &self,
        id: i64,
        error: &str,
    ) -> impl Future<Output = StorageResult<()>>;

    /// Removes a pending op after successful replay.
    fn remove_pending_op(&self, id: i64) -> impl Future<Output = StorageResult<()>>;
}

impl StorePendingOpsExt for Store {
    async fn enqueue_pending_op(
        &self,
        account: AccountId,
        op_type: OpType,
        payload: &PendingOpPayload,
    ) -> StorageResult<i64> {
        let now = self.clock.now_unix_ms();
        let payload_json = serde_json::to_string(payload)?;
        let op_type_str = op_type.to_string();
        let row = sqlx::query!(
            "INSERT INTO pending_ops (account_id, op_type, payload_json, created_at, retry_count, last_error)
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

    async fn drain_pending_ops(&self, account: AccountId) -> StorageResult<Vec<PendingOp>> {
        let rows = sqlx::query_as!(
            PendingOpRow,
            "SELECT id, account_id, op_type, payload_json, created_at, retry_count, last_error
             FROM pending_ops
             WHERE account_id = ?1
             ORDER BY created_at ASC, id ASC",
            account.to_string()
        )
        .fetch_all(&self.db.cache.read)
        .await?;
        rows.into_iter()
            .map(|r| {
                let account_id = parse_id::<AccountId>(&r.account_id)?;
                let op_type: OpType = r.op_type.parse().map_err(|e: String| {
                    crate::error::StorageError::Row(format!("pending_ops op_type: {e}"))
                })?;
                let payload: PendingOpPayload = serde_json::from_str(&r.payload_json)?;
                Ok(PendingOp {
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

    async fn mark_pending_op_failed(&self, id: i64, error: &str) -> StorageResult<()> {
        sqlx::query!(
            "UPDATE pending_ops
             SET retry_count = retry_count + 1, last_error = ?2
             WHERE id = ?1",
            id,
            error
        )
        .execute(&self.db.cache.write)
        .await?;
        Ok(())
    }

    async fn remove_pending_op(&self, id: i64) -> StorageResult<()> {
        sqlx::query!("DELETE FROM pending_ops WHERE id = ?1", id)
            .execute(&self.db.cache.write)
            .await?;
        Ok(())
    }
}
