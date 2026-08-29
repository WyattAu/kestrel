//! Storage vocabulary (architecture §2 DIP): the DTOs and the
//! [`MailStore`] trait that network engines consume. Implementation lives
//! in `kestrel-storage`; the engine injects it — no lateral crate imports.

use crate::{
    error::KestrelError,
    ids::{AccountId, BlobHash, FolderId, MessageId, OutboxId},
    mime::ParsedMessage,
    protocol::{Address, FolderRole, FolderSummary, MessagePage, MessageView, SortSpec, Window},
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
}
