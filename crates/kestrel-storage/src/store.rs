//! `StorageService`: the single writer over both databases and the blob CAS
//! (ADR 0004 service pattern; ADR 0003 access discipline; ADR 0009 split).
//!
//! The handle is a thin async RPC client; the service task owns all state.

use std::sync::Arc;

pub use kestrel_core::store_model::{
    FolderRow, IngestBatch, IngestMessage, IngestStats, NewFolder, OutboxEnvelope, OutboxRow,
};
use kestrel_core::{
    clock::Clock,
    error::KestrelError,
    ids::{AccountId, BlobHash, FolderId, IdGenerator, MessageId, OutboxId},
    protocol::{
        AccountSummary, FlagOp, FolderSummary, MailProtocol, MessagePage, MessageSummary,
        MessageView, Provider, SortSpec, Window,
    },
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, instrument};

use crate::{
    error::StorageError,
    ops::{
        OpType, PendingOp, PendingOpPayload, StoreBlobsExt, StoreMessagesExt, StoreOutboxExt,
        StorePendingOpsExt, StoreSnoozeExt,
    },
};

/// Inbound storage command (internal protocol; the engine router translates
/// frontend protocol commands into these).
pub enum StoreCommand {
    /// Insert or refresh an account (data.db).
    UpsertAccount {
        /// Account fields.
        account: NewAccount,
        /// Reply channel.
        reply: oneshot::Sender<Result<AccountId, StorageError>>,
    },
    /// List accounts.
    ListAccounts {
        /// Reply channel.
        reply: oneshot::Sender<Result<Vec<AccountSummary>, StorageError>>,
    },
    /// Delete account (cascades outbox; cache purge is caller-driven).
    DeleteAccount {
        /// Account to delete.
        id: AccountId,
        /// Reply channel.
        reply: oneshot::Sender<Result<(), StorageError>>,
    },
    /// Mirror connection state into the account row.
    SetAccountState {
        /// Account.
        id: AccountId,
        /// New state string (`ConnectionState` wire form).
        state: String,
        /// Reply channel.
        reply: oneshot::Sender<Result<(), StorageError>>,
    },

    /// Insert/refresh a folder (cache.db; account FK code-enforced).
    UpsertFolder {
        /// Folder fields.
        folder: NewFolder,
        /// Reply channel.
        reply: oneshot::Sender<Result<FolderId, StorageError>>,
    },
    /// List folders of an account with local counts.
    ListFolders {
        /// Owner account.
        account: AccountId,
        /// Reply channel.
        reply: oneshot::Sender<Result<Vec<FolderSummary>, StorageError>>,
    },
    /// Fetch one folder row.
    GetFolder {
        /// Folder id.
        id: FolderId,
        /// Reply channel.
        reply: oneshot::Sender<Result<FolderRow, StorageError>>,
    },

    /// Atomically ingest a parsed batch into a folder (threading + parts +
    /// CAS references in one cache.db transaction).
    IngestBatch {
        /// Batch payload.
        batch: IngestBatch,
        /// Reply channel.
        reply: oneshot::Sender<Result<IngestStats, StorageError>>,
    },
    /// Windowed listing.
    ListMessages {
        /// Folder.
        folder: FolderId,
        /// Window.
        window: Window,
        /// Sort spec.
        sort: SortSpec,
        /// Reply channel.
        reply: oneshot::Sender<Result<MessagePage, StorageError>>,
    },
    /// Windowed listing across all folders with role = 'inbox'.
    ListUnifiedInbox {
        /// Window.
        window: Window,
        /// Sort spec.
        sort: SortSpec,
        /// Reply channel.
        reply: oneshot::Sender<Result<MessagePage, StorageError>>,
    },
    /// Full message view (re-parses raw when present).
    GetMessage {
        /// Message id.
        id: MessageId,
        /// Reply channel.
        reply: oneshot::Sender<Result<MessageLoad, StorageError>>,
    },
    /// Decoded bytes for a specific MIME part.
    GetAttachmentData {
        /// Message containing the part.
        message: MessageId,
        /// Part key (from `PartIdView.key`).
        part_key: String,
        /// Reply channel.
        reply: oneshot::Sender<Result<Vec<u8>, StorageError>>,
    },
    /// Apply a flag mutation to a set of messages.
    SetFlags {
        /// Targets.
        messages: Vec<MessageId>,
        /// Operation.
        op: FlagOp,
        /// Reply channel.
        reply: oneshot::Sender<Result<Vec<MessageId>, StorageError>>,
    },
    /// Remove messages locally (server mutation is the sync engine's job).
    DeleteMessages {
        /// Targets.
        messages: Vec<MessageId>,
        /// Reply channel.
        reply: oneshot::Sender<Result<u64, StorageError>>,
    },
    /// Re-assign messages to another folder with fresh UIDs (post-server
    /// MOVE mirror).
    MoveMessages {
        /// (message, destination folder, new uid) triples.
        moves: Vec<(MessageId, FolderId, u32)>,
        /// Reply channel.
        reply: oneshot::Sender<Result<u64, StorageError>>,
    },
    /// Purge all messages of a folder (UIDVALIDITY reconciliation step 1).
    PurgeFolder {
        /// Folder.
        folder: FolderId,
        /// Reply channel.
        reply: oneshot::Sender<Result<u64, StorageError>>,
    },
    /// Messages pending index (catch-up cursor).
    PendingIndex {
        /// Maximum rows.
        limit: u64,
        /// Reply channel.
        reply: oneshot::Sender<Result<Vec<PendingDoc>, StorageError>>,
    },
    /// Full-feed index list (rebuild path; ignores the cursor).
    FeedAllForIndex {
        /// Maximum rows.
        limit: u64,
        /// Offset.
        offset: u64,
        /// Reply channel.
        reply: oneshot::Sender<Result<Vec<PendingDoc>, StorageError>>,
    },
    /// Mark messages indexed (index commit confirmed).
    MarkIndexed {
        /// Message ids.
        ids: Vec<MessageId>,
        /// Completion time (unix ms).
        at: i64,
        /// Reply channel.
        reply: oneshot::Sender<Result<(), StorageError>>,
    },

    /// Persist a draft into the outbox (raw goes to CAS + inline copy).
    OutboxEnqueue {
        /// Sending account.
        account: AccountId,
        /// Envelope (JSON payload).
        envelope: OutboxEnvelope,
        /// Raw RFC 5322 bytes.
        raw: Vec<u8>,
        /// Optional scheduled send time (unix ms).
        send_after: Option<i64>,
        /// Reply channel.
        reply: oneshot::Sender<Result<OutboxId, StorageError>>,
    },
    /// Due (unsent, attempt time reached) outbox rows.
    OutboxDue {
        /// Reply channel.
        reply: oneshot::Sender<Result<Vec<OutboxRow>, StorageError>>,
    },
    /// Record a retry.
    OutboxMarkRetry {
        /// Entry.
        id: OutboxId,
        /// New retry count.
        retry_count: u32,
        /// Next attempt time (unix ms).
        next_attempt_at: i64,
        /// Last error summary (no secrets).
        last_error: String,
        /// Reply channel.
        reply: oneshot::Sender<Result<(), StorageError>>,
    },
    /// Record successful send.
    OutboxMarkSent {
        /// Entry.
        id: OutboxId,
        /// Sent time (unix ms).
        sent_at: i64,
        /// Reply channel.
        reply: oneshot::Sender<Result<(), StorageError>>,
    },
    /// Cancel (keep as draft reference) or remove an entry.
    OutboxCancel {
        /// Entry.
        id: OutboxId,
        /// Reply channel.
        reply: oneshot::Sender<Result<(), StorageError>>,
    },

    // ---- snooze (cache.db) ----
    /// Enqueue a snooze entry.
    SnoozeEnqueue {
        /// Message to snooze.
        message: MessageId,
        /// Owning account.
        account: AccountId,
        /// Folder containing the message.
        folder: FolderId,
        /// When the snooze expires (unix ms).
        until: i64,
        /// Reply channel.
        reply: oneshot::Sender<Result<String, StorageError>>,
    },
    /// Get all snoozes that have expired (due).
    SnoozeGetDue {
        /// Reply channel.
        reply: oneshot::Sender<Result<Vec<crate::ops::SnoozeRow>, StorageError>>,
    },
    /// Remove a snooze by message id.
    SnoozeRemove {
        /// Message to unsnooze.
        message: MessageId,
        /// Reply channel.
        reply: oneshot::Sender<Result<(), StorageError>>,
    },

    // ---- offline mutation journal (sync-engine.md §6) ----
    /// Enqueue a mutation for later replay.
    PendingOpsEnqueue {
        /// Owning account.
        account: AccountId,
        /// Operation type.
        op_type: OpType,
        /// Serialized payload.
        payload: PendingOpPayload,
        /// Reply channel.
        reply: oneshot::Sender<Result<i64, StorageError>>,
    },
    /// Drain all pending ops for an account (FIFO order).
    PendingOpsDrain {
        /// Account.
        account: AccountId,
        /// Reply channel.
        reply: oneshot::Sender<Result<Vec<PendingOp>, StorageError>>,
    },
    /// Mark a pending op as failed.
    PendingOpsMarkFailed {
        /// Row id.
        id: i64,
        /// Error message.
        error: String,
        /// Reply channel.
        reply: oneshot::Sender<Result<(), StorageError>>,
    },
    /// Remove a pending op after successful replay.
    PendingOpsRemove {
        /// Row id.
        id: i64,
        /// Reply channel.
        reply: oneshot::Sender<Result<(), StorageError>>,
    },

    /// Write bytes into the CAS.
    WriteBlob {
        /// Payload.
        bytes: Vec<u8>,
        /// Reply channel.
        reply: oneshot::Sender<Result<BlobHash, StorageError>>,
    },
    /// Read a blob from the CAS.
    ReadBlob {
        /// Hash.
        hash: BlobHash,
        /// Reply channel.
        reply: oneshot::Sender<Result<Vec<u8>, StorageError>>,
    },
    /// Adjust the CAS refcount for an outbox reference (cross-DB, in code).
    AdjustOutboxRef {
        /// Hash.
        hash: BlobHash,
        /// +1 / -1.
        delta: i64,
        /// Reply channel.
        reply: oneshot::Sender<Result<(), StorageError>>,
    },
    /// Two-phase GC: mark unreferenced blobs.
    GcMark {
        /// Now (unix ms).
        now: i64,
        /// Reply channel.
        reply: oneshot::Sender<Result<u64, StorageError>>,
    },
    /// Two-phase GC: sweep expired marks.
    GcSweep {
        /// Now (unix ms).
        now: i64,
        /// Grace period (ms).
        grace_ms: i64,
        /// Reply channel.
        reply: oneshot::Sender<Result<u64, StorageError>>,
    },

    /// Highest stored UID in a folder (delta sync start point).
    MaxUid {
        /// Folder.
        folder: FolderId,
        /// Reply channel.
        reply: oneshot::Sender<Result<Option<u32>, StorageError>>,
    },
    /// Persist sync cursors (UIDVALIDITY / HIGHESTMODSEQ).
    UpdateSyncCursors {
        /// Folder.
        folder: FolderId,
        /// New UIDVALIDITY (0 = keep).
        uid_validity: u32,
        /// New HIGHESTMODSEQ (None = keep).
        highest_modseq: Option<u64>,
        /// Reply channel.
        reply: oneshot::Sender<Result<(), StorageError>>,
    },

    /// Read a data.db setting.
    GetSetting {
        /// Key.
        key: String,
        /// Reply channel.
        reply: oneshot::Sender<Result<Option<String>, StorageError>>,
    },
    /// Write a data.db setting.
    SetSetting {
        /// Key.
        key: String,
        /// Value.
        value: String,
        /// Reply channel.
        reply: oneshot::Sender<Result<(), StorageError>>,
    },
}

/// New-account payload.
#[derive(Clone, Debug)]
pub struct NewAccount {
    /// Display name.
    pub name: String,
    /// Email address.
    pub email: String,
    /// Provider family.
    pub provider: Provider,
    /// Mail protocol.
    pub protocol: MailProtocol,
    /// `password` | `oauth2`.
    pub auth_kind: String,
    /// IMAP/JMAP host.
    pub host: String,
}

/// Full message load returned by `GetMessage`.
#[derive(Clone, Debug)]
pub struct MessageLoad {
    /// View (sanitized bodies, links, parts).
    pub view: MessageView,
    /// Raw RFC 5322 bytes when cached locally.
    pub raw: Option<Vec<u8>>,
}

/// A document pending index (index catch-up cursor).
#[derive(Clone, Debug)]
pub struct PendingDoc {
    /// Message id.
    pub id: MessageId,
    /// Folder.
    pub folder: FolderId,
    /// Account.
    pub account: AccountId,
    /// Summary.
    pub summary: MessageSummary,
    /// Extracted body text for `body_plain`.
    pub body_text: String,
    /// Attachment names.
    pub attachment_names: Vec<String>,
}

/// Cloneable RPC handle to the `StorageService`.
#[derive(Clone)]
pub struct StorageHandle {
    tx: mpsc::Sender<StoreCommand>,
}

impl StorageHandle {
    /// Sends a command with the standard bounded-send policy.
    async fn call<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, StorageError>>) -> StoreCommand,
    ) -> Result<T, KestrelError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        // Bounded send: the router-level Busy policy is enforced upstream;
        // here a closed channel means shutdown.
        self.tx
            .send(make(reply_tx))
            .await
            .map_err(|_| KestrelError::Cancelled)?;
        reply_rx
            .await
            .map_err(|_| KestrelError::Cancelled)?
            .map_err(KestrelError::from)
    }

    /// Upserts an account, returning its id.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn upsert_account(&self, account: NewAccount) -> Result<AccountId, KestrelError> {
        self.call(|reply| StoreCommand::UpsertAccount { account, reply })
            .await
    }

    /// Lists all accounts.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn list_accounts(&self) -> Result<Vec<AccountSummary>, KestrelError> {
        self.call(|reply| StoreCommand::ListAccounts { reply })
            .await
    }

    /// Deletes an account.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn delete_account(&self, id: AccountId) -> Result<(), KestrelError> {
        self.call(|reply| StoreCommand::DeleteAccount { id, reply })
            .await
    }

    /// Mirrors a connection state into the account row.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn set_account_state(
        &self,
        id: AccountId,
        state: kestrel_core::protocol::ConnectionState,
    ) -> Result<(), KestrelError> {
        self.call(move |reply| StoreCommand::SetAccountState {
            id,
            state: state_wire(state),
            reply,
        })
        .await
    }

    /// Upserts a folder, returning its id.
    ///
    /// # Errors
    /// [`KestrelError`] (incl. cross-DB invariant if account missing).
    pub async fn upsert_folder(&self, folder: NewFolder) -> Result<FolderId, KestrelError> {
        self.call(|reply| StoreCommand::UpsertFolder { folder, reply })
            .await
    }

    /// Lists folders with unread/total counts.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn list_folders(
        &self,
        account: AccountId,
    ) -> Result<Vec<FolderSummary>, KestrelError> {
        self.call(move |reply| StoreCommand::ListFolders { account, reply })
            .await
    }

    /// Fetches one folder row.
    ///
    /// # Errors
    /// [`KestrelError`] when missing.
    pub async fn get_folder(&self, id: FolderId) -> Result<FolderRow, KestrelError> {
        self.call(move |reply| StoreCommand::GetFolder { id, reply })
            .await
    }

    /// Ingests a parsed batch atomically.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn ingest_batch(&self, batch: IngestBatch) -> Result<IngestStats, KestrelError> {
        self.call(|reply| StoreCommand::IngestBatch { batch, reply })
            .await
    }

    /// Windowed message listing.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn list_messages(
        &self,
        folder: FolderId,
        window: Window,
        sort: SortSpec,
    ) -> Result<MessagePage, KestrelError> {
        self.call(move |reply| StoreCommand::ListMessages {
            folder,
            window,
            sort,
            reply,
        })
        .await
    }

    /// Windowed message listing across all folders with role = 'inbox'.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn list_unified_inbox(
        &self,
        window: Window,
        sort: SortSpec,
    ) -> Result<MessagePage, KestrelError> {
        self.call(move |reply| StoreCommand::ListUnifiedInbox {
            window,
            sort,
            reply,
        })
        .await
    }

    /// Loads a message with resolved bodies.
    ///
    /// # Errors
    /// [`KestrelError`] when missing.
    pub async fn get_message(&self, id: MessageId) -> Result<MessageLoad, KestrelError> {
        self.call(move |reply| StoreCommand::GetMessage { id, reply })
            .await
    }

    /// Returns decoded bytes for a specific MIME part.
    ///
    /// # Errors
    /// [`KestrelError`] when the part is missing.
    pub async fn get_attachment_data(
        &self,
        message: MessageId,
        part_key: String,
    ) -> Result<Vec<u8>, KestrelError> {
        self.call(move |reply| StoreCommand::GetAttachmentData {
            message,
            part_key,
            reply,
        })
        .await
    }

    /// Applies a flag operation.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn set_flags(
        &self,
        messages: Vec<MessageId>,
        op: FlagOp,
    ) -> Result<Vec<MessageId>, KestrelError> {
        self.call(move |reply| StoreCommand::SetFlags {
            messages,
            op,
            reply,
        })
        .await
    }

    /// Deletes messages locally.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn delete_messages(&self, messages: Vec<MessageId>) -> Result<u64, KestrelError> {
        self.call(move |reply| StoreCommand::DeleteMessages { messages, reply })
            .await
    }

    /// Re-assigns messages to another folder with fresh UIDs.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn move_messages(
        &self,
        moves: Vec<(MessageId, FolderId, u32)>,
    ) -> Result<u64, KestrelError> {
        self.call(move |reply| StoreCommand::MoveMessages { moves, reply })
            .await
    }

    /// Purges a folder's messages (UIDVALIDITY reconciliation).
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn purge_folder(&self, folder: FolderId) -> Result<u64, KestrelError> {
        self.call(move |reply| StoreCommand::PurgeFolder { folder, reply })
            .await
    }

    /// Fetches messages pending index.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn pending_index(&self, limit: u64) -> Result<Vec<PendingDoc>, KestrelError> {
        self.call(move |reply| StoreCommand::PendingIndex { limit, reply })
            .await
    }

    /// Fetches all messages for a full index rebuild.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn feed_all_for_index(
        &self,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<PendingDoc>, KestrelError> {
        self.call(move |reply| StoreCommand::FeedAllForIndex {
            limit,
            offset,
            reply,
        })
        .await
    }

    /// Marks messages indexed.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn mark_indexed(
        &self,
        ids: Vec<MessageId>,
        at: kestrel_core::clock::UnixMillis,
    ) -> Result<(), KestrelError> {
        self.call(move |reply| StoreCommand::MarkIndexed { ids, at, reply })
            .await
    }

    /// Persists a draft into the outbox.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn outbox_enqueue(
        &self,
        account: AccountId,
        envelope: OutboxEnvelope,
        raw: Vec<u8>,
        send_after: Option<i64>,
    ) -> Result<OutboxId, KestrelError> {
        self.call(move |reply| StoreCommand::OutboxEnqueue {
            account,
            envelope,
            raw,
            send_after,
            reply,
        })
        .await
    }

    /// Lists due outbox entries.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn outbox_due(&self) -> Result<Vec<OutboxRow>, KestrelError> {
        self.call(|reply| StoreCommand::OutboxDue { reply }).await
    }

    /// Records a retry.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn outbox_mark_retry(
        &self,
        id: OutboxId,
        retry_count: u32,
        next_attempt_at: i64,
        last_error: String,
    ) -> Result<(), KestrelError> {
        self.call(move |reply| StoreCommand::OutboxMarkRetry {
            id,
            retry_count,
            next_attempt_at,
            last_error,
            reply,
        })
        .await
    }

    /// Records a send.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn outbox_mark_sent(&self, id: OutboxId, sent_at: i64) -> Result<(), KestrelError> {
        self.call(move |reply| StoreCommand::OutboxMarkSent { id, sent_at, reply })
            .await
    }

    /// Cancels an outbox entry.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn outbox_cancel(&self, id: OutboxId) -> Result<(), KestrelError> {
        self.call(move |reply| StoreCommand::OutboxCancel { id, reply })
            .await
    }

    // ---- snooze ----

    /// Enqueues a snooze entry.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn enqueue_snooze(
        &self,
        message: MessageId,
        account: AccountId,
        folder: FolderId,
        until: i64,
    ) -> Result<String, KestrelError> {
        self.call(move |reply| StoreCommand::SnoozeEnqueue {
            message,
            account,
            folder,
            until,
            reply,
        })
        .await
    }

    /// Returns all snoozes that have expired (due).
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn get_due_snoozes(&self) -> Result<Vec<crate::ops::SnoozeRow>, KestrelError> {
        self.call(|reply| StoreCommand::SnoozeGetDue { reply })
            .await
    }

    /// Removes a snooze by message id.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn remove_snooze(&self, message: MessageId) -> Result<(), KestrelError> {
        self.call(move |reply| StoreCommand::SnoozeRemove { message, reply })
            .await
    }

    // ---- offline mutation journal ----

    /// Enqueues a mutation for later replay.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn pending_ops_enqueue(
        &self,
        account: AccountId,
        op_type: OpType,
        payload: PendingOpPayload,
    ) -> Result<i64, KestrelError> {
        self.call(move |reply| StoreCommand::PendingOpsEnqueue {
            account,
            op_type,
            payload,
            reply,
        })
        .await
    }

    /// Drains all pending ops for an account in FIFO order.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn pending_ops_drain(
        &self,
        account: AccountId,
    ) -> Result<Vec<PendingOp>, KestrelError> {
        self.call(move |reply| StoreCommand::PendingOpsDrain { account, reply })
            .await
    }

    /// Marks a pending op as failed.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn pending_ops_mark_failed(
        &self,
        id: i64,
        error: String,
    ) -> Result<(), KestrelError> {
        self.call(move |reply| StoreCommand::PendingOpsMarkFailed { id, error, reply })
            .await
    }

    /// Removes a pending op after successful replay.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn pending_ops_remove(&self, id: i64) -> Result<(), KestrelError> {
        self.call(move |reply| StoreCommand::PendingOpsRemove { id, reply })
            .await
    }

    /// Writes bytes to the CAS.
    ///
    /// # Errors
    /// [`KestrelError`] on IO failure.
    pub async fn write_blob(&self, bytes: Vec<u8>) -> Result<BlobHash, KestrelError> {
        self.call(move |reply| StoreCommand::WriteBlob { bytes, reply })
            .await
    }

    /// Reads a blob.
    ///
    /// # Errors
    /// [`KestrelError`] when missing.
    pub async fn read_blob(&self, hash: BlobHash) -> Result<Vec<u8>, KestrelError> {
        self.call(move |reply| StoreCommand::ReadBlob { hash, reply })
            .await
    }

    /// Adjusts an outbox CAS reference.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn adjust_outbox_ref(&self, hash: BlobHash, delta: i64) -> Result<(), KestrelError> {
        self.call(move |reply| StoreCommand::AdjustOutboxRef { hash, delta, reply })
            .await
    }

    /// GC mark phase.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn gc_mark(&self, now: i64) -> Result<u64, KestrelError> {
        self.call(move |reply| StoreCommand::GcMark { now, reply })
            .await
    }

    /// GC sweep phase.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn gc_sweep(&self, now: i64, grace_ms: i64) -> Result<u64, KestrelError> {
        self.call(move |reply| StoreCommand::GcSweep {
            now,
            grace_ms,
            reply,
        })
        .await
    }

    /// Highest stored UID in a folder (`None` when empty).
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn max_uid(&self, folder: FolderId) -> Result<Option<u32>, KestrelError> {
        self.call(move |reply| StoreCommand::MaxUid { folder, reply })
            .await
    }

    /// Persists sync cursors for a folder.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn update_sync_cursors(
        &self,
        folder: FolderId,
        uid_validity: u32,
        highest_modseq: Option<u64>,
    ) -> Result<(), KestrelError> {
        self.call(move |reply| StoreCommand::UpdateSyncCursors {
            folder,
            uid_validity,
            highest_modseq,
            reply,
        })
        .await
    }

    /// Reads a data.db setting.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn get_setting(&self, key: &str) -> Result<Option<String>, KestrelError> {
        self.call(|reply| StoreCommand::GetSetting {
            key: key.to_owned(),
            reply,
        })
        .await
    }

    /// Writes a data.db setting.
    ///
    /// # Errors
    /// [`KestrelError`] on storage failure.
    pub async fn set_setting(&self, key: &str, value: &str) -> Result<(), KestrelError> {
        self.call(|reply| StoreCommand::SetSetting {
            key: key.to_owned(),
            value: value.to_owned(),
            reply,
        })
        .await
    }
}

/// `ConnectionState` wire form for the `accounts.sync_state` column.
fn state_wire(state: kestrel_core::protocol::ConnectionState) -> String {
    use kestrel_core::protocol::ConnectionState as C;
    match state {
        C::Disconnected => "disconnected",
        C::Connecting => "connecting",
        C::Authenticating => "authenticating",
        C::Syncing => "syncing",
        C::Idle => "idle",
        C::OfflineMode => "offline",
    }
    .to_string()
}

/// Service supervisor hook: runs the loop until cancellation. The open
/// happens inside the task; commands queued meanwhile are answered with the
/// open error if it fails (never silently dropped).
pub struct StorageService;

impl StorageService {
    /// Spawns the service on the current runtime.
    #[must_use]
    pub fn spawn(
        paths: kestrel_core::paths::Paths,
        ids: Arc<dyn IdGenerator>,
        clock: Arc<dyn Clock>,
    ) -> (StorageHandle, CancellationToken) {
        let (tx, mut rx) = mpsc::channel(64);
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let span = tracing::info_span!("service.storage");
        tokio::spawn(
            async move {
                let store = match crate::ops::Store::open(&paths, ids, clock).await {
                    Ok(store) => store,
                    Err(e) => {
                        tracing::error!(error = %e, "service.storage failed to open");
                        let mut rx = rx;
                        while let Some(cmd) = rx.recv().await {
                            reply_open_error(cmd, &e);
                        }
                        return;
                    }
                };
                let store = Arc::new(store);
                tracing::info!("service.storage started");
                loop {
                    tokio::select! {
                        () = token.cancelled() => break,
                        maybe = rx.recv() => {
                            let Some(cmd) = maybe else { break };
                            dispatch(&store, cmd).await;
                        }
                    }
                }
                store.db.close().await;
                tracing::info!("service.storage stopped");
            }
            .instrument(span),
        );
        (StorageHandle { tx }, cancel)
    }
}

// clippy::same_item_push / match-arms / too-many-lines: every variant carries a
// differently typed oneshot; the table must enumerate all of them.
#[allow(clippy::match_same_arms, clippy::too_many_lines)]
fn reply_open_error(cmd: StoreCommand, err: &StorageError) {
    use StoreCommand as C;
    let err = err.clone();
    match cmd {
        C::UpsertAccount { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::ListAccounts { reply } => {
            let _ = reply.send(Err(err));
        }
        C::DeleteAccount { reply, .. } | C::SetAccountState { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::UpsertFolder { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::ListFolders { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::GetFolder { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::IngestBatch { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::ListMessages { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::ListUnifiedInbox { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::GetMessage { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::GetAttachmentData { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::SetFlags { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::DeleteMessages { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::MoveMessages { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::PurgeFolder { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::PendingIndex { reply, .. } | C::FeedAllForIndex { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::MarkIndexed { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::OutboxEnqueue { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::OutboxDue { reply } => {
            let _ = reply.send(Err(err));
        }
        C::OutboxMarkRetry { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::OutboxMarkSent { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::OutboxCancel { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::SnoozeEnqueue { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::SnoozeGetDue { reply } => {
            let _ = reply.send(Err(err));
        }
        C::SnoozeRemove { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::PendingOpsEnqueue { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::PendingOpsDrain { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::PendingOpsMarkFailed { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::PendingOpsRemove { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::WriteBlob { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::ReadBlob { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::AdjustOutboxRef { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::GcMark { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::GcSweep { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::MaxUid { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::UpdateSyncCursors { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::GetSetting { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        C::SetSetting { reply, .. } => {
            let _ = reply.send(Err(err));
        }
    }
}

// clippy::too_many_lines: a flat dispatch table — one arm per command is
// the clearest form here.
#[instrument(skip_all)]
#[allow(clippy::too_many_lines)]
async fn dispatch(store: &Arc<crate::ops::Store>, cmd: StoreCommand) {
    use StoreCommand as C;
    match cmd {
        C::UpsertAccount { account, reply } => {
            let _ = reply.send(store.upsert_account(&account).await);
        }
        C::ListAccounts { reply } => {
            let _ = reply.send(store.list_accounts().await);
        }
        C::DeleteAccount { id, reply } => {
            let _ = reply.send(store.delete_account(id).await);
        }
        C::SetAccountState { id, state, reply } => {
            let _ = reply.send(store.set_account_state(id, &state).await);
        }
        C::UpsertFolder { folder, reply } => {
            let _ = reply.send(store.upsert_folder(&folder).await);
        }
        C::ListFolders { account, reply } => {
            let _ = reply.send(store.list_folders(account).await);
        }
        C::GetFolder { id, reply } => {
            let _ = reply.send(store.get_folder(id).await);
        }
        C::IngestBatch { batch, reply } => {
            let _ = reply.send(store.ingest_batch(batch).await);
        }
        C::ListMessages {
            folder,
            window,
            sort,
            reply,
        } => {
            let _ = reply.send(store.list_messages(folder, window, &sort).await);
        }
        C::ListUnifiedInbox {
            window,
            sort,
            reply,
        } => {
            let _ = reply.send(store.list_unified_inbox(window, &sort).await);
        }
        C::GetMessage { id, reply } => {
            let _ = reply.send(store.get_message(id).await);
        }
        C::GetAttachmentData {
            message,
            part_key,
            reply,
        } => {
            let _ = reply.send(store.get_attachment_data(message, &part_key).await);
        }
        C::SetFlags {
            messages,
            op,
            reply,
        } => {
            let _ = reply.send(store.set_flags(&messages, &op).await);
        }
        C::DeleteMessages { messages, reply } => {
            let _ = reply.send(store.delete_messages(&messages).await);
        }
        C::MoveMessages { moves, reply } => {
            let _ = reply.send(store.move_messages(&moves).await);
        }
        C::PurgeFolder { folder, reply } => {
            let _ = reply.send(store.purge_folder(folder).await);
        }
        C::PendingIndex { limit, reply } => {
            let _ = reply.send(store.pending_index(limit).await);
        }
        C::FeedAllForIndex {
            limit,
            offset,
            reply,
        } => {
            let _ = reply.send(store.feed_all_for_index(limit, offset).await);
        }
        C::MarkIndexed { ids, at, reply } => {
            let _ = reply.send(store.mark_indexed(&ids, at).await);
        }
        C::OutboxEnqueue {
            account,
            envelope,
            raw,
            send_after,
            reply,
        } => {
            let _ = reply.send(
                store
                    .outbox_enqueue(account, &envelope, &raw, send_after)
                    .await,
            );
        }
        C::OutboxDue { reply } => {
            let _ = reply.send(store.outbox_due().await);
        }
        C::OutboxMarkRetry {
            id,
            retry_count,
            next_attempt_at,
            last_error,
            reply,
        } => {
            let _ = reply.send(
                store
                    .outbox_mark_retry(id, retry_count, next_attempt_at, &last_error)
                    .await,
            );
        }
        C::OutboxMarkSent { id, sent_at, reply } => {
            let _ = reply.send(store.outbox_mark_sent(id, sent_at).await);
        }
        C::OutboxCancel { id, reply } => {
            let _ = reply.send(store.outbox_cancel(id).await);
        }
        C::SnoozeEnqueue {
            message,
            account,
            folder,
            until,
            reply,
        } => {
            let _ = reply.send(store.enqueue_snooze(message, account, folder, until).await);
        }
        C::SnoozeGetDue { reply } => {
            let _ = reply.send(store.get_due_snoozes().await);
        }
        C::SnoozeRemove { message, reply } => {
            let _ = reply.send(store.remove_snooze(message).await);
        }
        C::PendingOpsEnqueue {
            account,
            op_type,
            payload,
            reply,
        } => {
            let _ = reply.send(store.enqueue_pending_op(account, op_type, &payload).await);
        }
        C::PendingOpsDrain { account, reply } => {
            let _ = reply.send(store.drain_pending_ops(account).await);
        }
        C::PendingOpsMarkFailed { id, error, reply } => {
            let _ = reply.send(store.mark_pending_op_failed(id, &error).await);
        }
        C::PendingOpsRemove { id, reply } => {
            let _ = reply.send(store.remove_pending_op(id).await);
        }
        C::WriteBlob { bytes, reply } => {
            // CAS write + registry row (refcount 0) so unreferenced blobs
            // are visible to the two-phase GC (schema.md §4.3).
            let result = match store.write_blob(&bytes).await {
                Ok(hash) => store.adjust_outbox_ref_inner(&hash, 0).await.map(|()| hash),
                Err(e) => Err(e),
            };
            let _ = reply.send(result);
        }
        C::ReadBlob { hash, reply } => {
            let _ = reply.send(store.read_blob(&hash).await);
        }
        C::AdjustOutboxRef { hash, delta, reply } => {
            let _ = reply.send(store.adjust_outbox_ref_inner(&hash, delta).await);
        }
        C::GcMark { now, reply } => {
            let _ = reply.send(store.gc_mark(now).await);
        }
        C::GcSweep {
            now,
            grace_ms,
            reply,
        } => {
            let _ = reply.send(store.gc_sweep(now, grace_ms).await);
        }
        C::MaxUid { folder, reply } => {
            let _ = reply.send(store.max_uid(folder).await);
        }
        C::UpdateSyncCursors {
            folder,
            uid_validity,
            highest_modseq,
            reply,
        } => {
            let _ = reply.send(
                store
                    .update_sync_cursors(folder, uid_validity, highest_modseq)
                    .await,
            );
        }
        C::GetSetting { key, reply } => {
            let _ = reply.send(store.get_setting(&key).await);
        }
        C::SetSetting { key, value, reply } => {
            let _ = reply.send(store.set_setting(&key, &value).await);
        }
    }
}
