//! Storage vocabulary (architecture §2 DIP): the DTOs and the
//! [`MailStore`] trait that network engines consume. Implementation lives
//! in `kestrel-storage`; the engine injects it — no lateral crate imports.

use serde::{Deserialize, Serialize};

use crate::{
    error::KestrelError,
    ids::{AccountId, BlobHash, FolderId, MessageId, OutboxId},
    mime::ParsedMessage,
    protocol::{
        Address, Flag, FlagOp, FolderRole, FolderSummary, MessagePage, MessageView, SortSpec,
        Window,
    },
};

/// New-folder payload.
#[derive(Clone, Debug)]
pub struct NewFolder {
    /// Owning account (must exist — cross-DB FK, ADR 0009).
    pub account: AccountId,
    /// Server name.
    pub remote_name: String,
    /// Attributes (e.g. `\\HasNoChildren`).
    pub attributes: Vec<String>,
    /// Canonical role, if recognized.
    pub role: Option<FolderRole>,
    /// Hierarchy delimiter.
    pub delimiter: String,
    /// `UIDVALIDITY` (0 when not yet selected).
    pub uid_validity: u32,
    /// `HIGHESTMODSEQ` (0 when unknown).
    pub highest_modseq: u64,
}

/// Folder row as stored.
#[derive(Clone, Debug)]
pub struct FolderRow {
    /// Folder id.
    pub id: FolderId,
    /// Owning account.
    pub account: AccountId,
    /// Server name.
    pub remote_name: String,
    /// Attributes.
    pub attributes: Vec<String>,
    /// Canonical role.
    pub role: Option<FolderRole>,
    /// Hierarchy delimiter.
    pub delimiter: String,
    /// IMAP `UIDVALIDITY` cursor.
    pub uid_validity: u32,
    /// CONDSTORE `HIGHESTMODSEQ` cursor.
    pub highest_modseq: u64,
}

/// One message ready for ingestion.
#[derive(Clone, Debug)]
pub struct IngestMessage {
    /// Destination folder.
    pub folder: FolderId,
    /// IMAP UID.
    pub uid: u32,
    /// `INTERNALDATE` (unix ms).
    pub internal_date: i64,
    /// Server flags.
    pub flags: Vec<crate::protocol::Flag>,
    /// Parsed MIME tree (ADR 0002).
    pub parsed: ParsedMessage,
    /// Raw message blob (already in CAS; hash only).
    pub raw_blob: Option<BlobHash>,
    /// Raw size in bytes.
    pub raw_size: u64,
}

/// Ingestion batch: applied in one transaction.
#[derive(Clone, Debug, Default)]
pub struct IngestBatch {
    /// Messages.
    pub messages: Vec<IngestMessage>,
}

/// Ingestion outcome counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IngestStats {
    /// New rows.
    pub inserted: u64,
    /// Updated rows.
    pub updated: u64,
}

/// Envelope persisted beside the outbox raw blob.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OutboxEnvelope {
    /// From.
    pub from: Address,
    /// To.
    pub to: Vec<Address>,
    /// Cc.
    pub cc: Vec<Address>,
    /// Bcc.
    pub bcc: Vec<Address>,
    /// Subject.
    pub subject: String,
}

/// Outbox row as returned to the outbox service.
#[derive(Clone, Debug)]
pub struct OutboxRow {
    /// Entry id.
    pub id: OutboxId,
    /// Owning account.
    pub account: AccountId,
    /// CAS hash of the raw RFC 5322.
    pub raw_blob: BlobHash,
    /// Envelope.
    pub envelope: OutboxEnvelope,
    /// Retry counter.
    pub retry_count: u32,
    /// Last error summary.
    pub last_error: Option<String>,
    /// Creation time.
    pub created_at: i64,
}

/// The storage seam consumed by network engines (sync/outbox); implemented
/// by `kestrel-storage` and injected by the engine.
#[async_trait::async_trait]
pub trait MailStore: Send + Sync {
    /// Mirrors `upsert_folder`.
    /// # Errors
    /// Storage failure.
    async fn upsert_folder(&self, folder: &NewFolder) -> Result<FolderId, KestrelError>;
    /// Mirrors `list_folders`.
    /// # Errors
    /// Storage failure.
    async fn list_folders(&self, account: AccountId) -> Result<Vec<FolderSummary>, KestrelError>;
    /// Mirrors `get_folder`.
    /// # Errors
    /// Storage failure.
    async fn get_folder(&self, id: FolderId) -> Result<FolderRow, KestrelError>;
    /// Mirrors `ingest_batch`.
    /// # Errors
    /// Storage failure.
    async fn ingest_batch(&self, batch: IngestBatch) -> Result<IngestStats, KestrelError>;
    /// Mirrors `set_flags`: applies a flag mutation to a set of messages
    /// (used by the sync engine to persist CHANGEDSINCE flag deltas).
    /// # Errors
    /// Storage failure.
    async fn set_flags(
        &self,
        messages: Vec<MessageId>,
        op: FlagOp,
    ) -> Result<Vec<MessageId>, KestrelError>;
    /// Mirrors `list_messages`.
    /// # Errors
    /// Storage failure.
    async fn list_messages(
        &self,
        folder: FolderId,
        window: Window,
        sort: SortSpec,
    ) -> Result<MessagePage, KestrelError>;
    /// Mirrors `purge_folder`.
    /// # Errors
    /// Storage failure.
    async fn purge_folder(&self, folder: FolderId) -> Result<u64, KestrelError>;
    /// Mirrors `update_sync_cursors`.
    /// # Errors
    /// Storage failure.
    async fn update_sync_cursors(
        &self,
        folder: FolderId,
        uid_validity: u32,
        highest_modseq: Option<u64>,
    ) -> Result<(), KestrelError>;
    /// Mirrors `max_uid`.
    /// # Errors
    /// Storage failure.
    async fn max_uid(&self, folder: FolderId) -> Result<Option<u32>, KestrelError>;
    /// Mirrors `outbox_due`.
    /// # Errors
    /// Storage failure.
    async fn outbox_due(&self) -> Result<Vec<OutboxRow>, KestrelError>;
    /// Mirrors `outbox_mark_retry`.
    /// # Errors
    /// Storage failure.
    async fn outbox_mark_retry(
        &self,
        id: OutboxId,
        retry_count: u32,
        next_attempt_at: i64,
        last_error: &str,
    ) -> Result<(), KestrelError>;
    /// Mirrors `outbox_mark_sent`.
    /// # Errors
    /// Storage failure.
    async fn outbox_mark_sent(&self, id: OutboxId, sent_at: i64) -> Result<(), KestrelError>;
    /// Mirrors `read_blob`.
    /// # Errors
    /// Storage failure.
    async fn read_blob(&self, hash: &BlobHash) -> Result<Vec<u8>, KestrelError>;
    /// Mirrors `write_blob`.
    /// # Errors
    /// Storage failure.
    async fn write_blob(&self, bytes: Vec<u8>) -> Result<BlobHash, KestrelError>;
    /// Returns decoded bytes for a specific MIME part of a message.
    /// # Errors
    /// Storage failure or part not found.
    async fn get_attachment_data(
        &self,
        message: MessageId,
        part_key: &str,
    ) -> Result<Vec<u8>, KestrelError>;
    /// Mirrors `set_account_state`.
    /// # Errors
    /// Storage failure.
    async fn set_account_state(
        &self,
        id: AccountId,
        state: crate::protocol::ConnectionState,
    ) -> Result<(), KestrelError>;
    /// Mirrors `get_message` (view only; raw handled via blobs).
    /// # Errors
    /// Storage failure.
    async fn get_message_view(&self, id: MessageId) -> Result<MessageView, KestrelError>;

    // ---- snooze ----
    /// Enqueue a snooze entry.
    /// # Errors
    /// Storage failure.
    async fn enqueue_snooze(
        &self,
        message: MessageId,
        account: AccountId,
        folder: FolderId,
        until: i64,
    ) -> Result<(), KestrelError>;
    /// Get all snoozes that have expired (due).
    /// # Errors
    /// Storage failure.
    async fn get_due_snoozes(&self) -> Result<Vec<SnoozeEntry>, KestrelError>;
    /// Remove a snooze by message id.
    /// # Errors
    /// Storage failure.
    async fn remove_snooze(&self, message: MessageId) -> Result<(), KestrelError>;

    // ---- server-push queue (mutation push to the IMAP server) ----
    /// Queues a mutation for server-side application (UID STORE / MOVE).
    /// The sync engine drains it at the start of each sync cycle; without
    /// it a locally-applied mutation never reaches the authoritative server
    /// state and is silently reverted by the next delta sync.
    /// # Errors
    /// Storage failure.
    async fn enqueue_push_op(
        &self,
        account: AccountId,
        op_type: PushOpType,
        payload: PushOpPayload,
    ) -> Result<(), KestrelError>;
    /// Looks up the local (folder, uid) snapshot for the given messages
    /// (the coordinates a push op needs to target the server message).
    /// # Errors
    /// Storage failure.
    async fn message_locations(
        &self,
        messages: Vec<MessageId>,
    ) -> Result<Vec<(MessageId, FolderId, u32)>, KestrelError>;
    /// Drains all queued push ops for an account, ordered FIFO.
    /// # Errors
    /// Storage failure.
    async fn drain_push_queue(&self, account: AccountId) -> Result<Vec<PushOp>, KestrelError>;
    /// Records a failed push attempt (retry counter + error note).
    /// # Errors
    /// Storage failure.
    async fn mark_push_op_failed(&self, id: i64, error: &str) -> Result<(), KestrelError>;
    /// Removes a push op after successful server application.
    /// # Errors
    /// Storage failure.
    async fn remove_push_op(&self, id: i64) -> Result<(), KestrelError>;
}

/// A snooze entry returned by `get_due_snoozes`.
#[derive(Clone, Debug)]
pub struct SnoozeEntry {
    /// Snooze id (for removal after processing).
    pub id: String,
    /// Snoozed message.
    pub message: MessageId,
    /// Account that owns the message.
    pub account: AccountId,
    /// Folder containing the message.
    pub folder: FolderId,
    /// When the snooze expires.
    pub snoozed_until: i64,
}

// ---- server-push queue (sync-engine.md §6 mutation push) --------------------

/// Operation category of a queued server-push mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushOpType {
    /// Flag mutation (UID STORE).
    Flag,
    /// Move messages between folders (UID MOVE; COPY+EXPUNGE fallback).
    Move,
}

impl std::fmt::Display for PushOpType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Flag => write!(f, "flag"),
            Self::Move => write!(f, "move"),
        }
    }
}

/// Serializable per-message payload for a server-push mutation. Rows carry
/// the local folder/UID snapshot captured at enqueue time so the drain can
/// target the exact server messages; per-message rows keep UID
/// reconciliation 1:1 and stop one bad row from poisoning a batch.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PushOpPayload {
    /// Flag operation on one message.
    Flag {
        /// Target message.
        message: MessageId,
        /// Folder the message lived in at enqueue time (server mailbox).
        folder: FolderId,
        /// Server UID at enqueue time.
        uid: u32,
        /// Flags to add.
        add: Vec<Flag>,
        /// Flags to remove.
        remove: Vec<Flag>,
    },
    /// Move one message to another folder.
    Move {
        /// Target message.
        message: MessageId,
        /// Source folder (the server mailbox to SELECT).
        from_folder: FolderId,
        /// Server UID at enqueue time.
        uid: u32,
        /// Destination folder (the server mailbox to move into).
        to_folder: FolderId,
    },
}

/// A queued server-push operation.
#[derive(Clone, Debug)]
pub struct PushOp {
    /// Row id.
    pub id: i64,
    /// Owning account.
    pub account_id: AccountId,
    /// Operation category.
    pub op_type: PushOpType,
    /// Serialized operation payload.
    pub payload: PushOpPayload,
    /// Creation timestamp (unix ms).
    pub created_at: i64,
    /// Drain attempts so far.
    pub retry_count: u32,
    /// Last drain error, if any.
    pub last_error: Option<String>,
}
