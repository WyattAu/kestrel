//! [`MailStore`] implementation for the storage handle (core §`store_model`
//! seam; consumed by `kestrel-sync` via dependency inversion).

use kestrel_core::{
    error::KestrelError,
    ids::{AccountId, BlobHash, FolderId, MessageId, OutboxId},
    protocol::{ConnectionState, MessageView, SortSpec, Window},
    store_model::{FolderRow, IngestBatch, IngestStats, MailStore, NewFolder, OutboxRow},
};

use crate::store::StorageHandle;

#[async_trait::async_trait]
impl MailStore for StorageHandle {
    async fn upsert_folder(&self, folder: &NewFolder) -> Result<FolderId, KestrelError> {
        StorageHandle::upsert_folder(self, folder.clone()).await
    }

    async fn list_folders(
        &self,
        account: AccountId,
    ) -> Result<Vec<kestrel_core::protocol::FolderSummary>, KestrelError> {
        StorageHandle::list_folders(self, account).await
    }

    async fn get_folder(&self, id: FolderId) -> Result<FolderRow, KestrelError> {
        StorageHandle::get_folder(self, id).await
    }

    async fn ingest_batch(&self, batch: IngestBatch) -> Result<IngestStats, KestrelError> {
        StorageHandle::ingest_batch(self, batch).await
    }

    async fn list_messages(
        &self,
        folder: FolderId,
        window: Window,
        sort: SortSpec,
    ) -> Result<kestrel_core::protocol::MessagePage, KestrelError> {
        StorageHandle::list_messages(self, folder, window, sort).await
    }

    async fn purge_folder(&self, folder: FolderId) -> Result<u64, KestrelError> {
        StorageHandle::purge_folder(self, folder).await
    }

    async fn update_sync_cursors(
        &self,
        folder: FolderId,
        uid_validity: u32,
        highest_modseq: Option<u64>,
    ) -> Result<(), KestrelError> {
        StorageHandle::update_sync_cursors(self, folder, uid_validity, highest_modseq).await
    }

    async fn max_uid(&self, folder: FolderId) -> Result<Option<u32>, KestrelError> {
        StorageHandle::max_uid(self, folder).await
    }

    async fn outbox_due(&self) -> Result<Vec<OutboxRow>, KestrelError> {
        StorageHandle::outbox_due(self).await
    }

    async fn outbox_mark_retry(
        &self,
        id: OutboxId,
        retry_count: u32,
        next_attempt_at: i64,
        last_error: &str,
    ) -> Result<(), KestrelError> {
        StorageHandle::outbox_mark_retry(
            self,
            id,
            retry_count,
            next_attempt_at,
            last_error.to_owned(),
        )
        .await
    }

    async fn outbox_mark_sent(&self, id: OutboxId, sent_at: i64) -> Result<(), KestrelError> {
        StorageHandle::outbox_mark_sent(self, id, sent_at).await
    }

    async fn read_blob(&self, hash: &BlobHash) -> Result<Vec<u8>, KestrelError> {
        StorageHandle::read_blob(self, hash.clone()).await
    }

    async fn write_blob(&self, bytes: Vec<u8>) -> Result<BlobHash, KestrelError> {
        StorageHandle::write_blob(self, bytes).await
    }

    async fn set_account_state(
        &self,
        id: AccountId,
        state: ConnectionState,
    ) -> Result<(), KestrelError> {
        StorageHandle::set_account_state(self, id, state).await
    }

    async fn get_message_view(&self, id: MessageId) -> Result<MessageView, KestrelError> {
        StorageHandle::get_message(self, id)
            .await
            .map(|load| load.view)
    }
}
