//! Storage operations (SQL bodies behind [`crate::store`]).
//!
//! Every statement is compile-time checked (ADR 0003) against the combined
//! prepare schema (`scripts/sqlx-prepare-db.sh`). Id-list queries bind a
//! JSON array and join through `json_each`, keeping the SQL static.

mod blobs_gc;
mod messages;
mod outbox;
mod pending_ops;
mod snooze;

use std::sync::Arc;

/// Folder row hydration shape.
#[derive(sqlx::FromRow)]
struct FolderDbRow {
    account_id: String,
    remote_name: String,
    attributes: String,
    role: Option<String>,
    delimiter: String,
    uid_validity: i64,
    highest_modseq: i64,
}

/// MAX(uid) row shape.
#[derive(sqlx::FromRow)]
struct MaxUidRow {
    uid: Option<i64>,
}

/// Single-id row shape for lookup queries.
#[derive(sqlx::FromRow)]
pub(crate) struct IdOnly {
    /// The id column.
    id: String,
}

pub(crate) use blobs_gc::StoreBlobsExt;
use kestrel_core::{
    clock::Clock,
    ids::{AccountId, FolderId, IdGenerator},
    protocol::{ConnectionState, FolderRole, MailProtocol, Provider},
};
pub(crate) use messages::StoreMessagesExt;
pub(crate) use outbox::StoreOutboxExt;
pub(crate) use pending_ops::StorePendingOpsExt;
pub use pending_ops::{FlagPayload, OpType, PendingOp, PendingOpPayload};
pub use snooze::SnoozeRow;
pub(crate) use snooze::StoreSnoozeExt;

use crate::{
    blob::BlobStore,
    db::Databases,
    error::{StorageError, StorageResult},
    store::{NewAccount, NewFolder},
};

/// Shared storage state (databases + CAS + injectables).
pub struct Store {
    /// Dual databases (ADR 0009).
    pub(crate) db: Databases,
    /// Content-addressed blob store.
    pub(crate) blobs: BlobStore,
    /// Injected id source (determinism).
    pub(crate) ids: Arc<dyn IdGenerator>,
    /// Injected clock.
    pub(crate) clock: Arc<dyn Clock>,
}

impl Store {
    /// Opens databases, runs migrations + startup passes (integrity check,
    /// temp sweep).
    ///
    /// # Errors
    /// Migration/integrity failures per `docs/error-taxonomy.md`.
    pub async fn open(
        paths: &kestrel_core::paths::Paths,
        ids: Arc<dyn IdGenerator>,
        clock: Arc<dyn Clock>,
    ) -> StorageResult<Self> {
        let db = Databases::open(&paths.cache_db(), &paths.data_db()).await?;
        db.integrity_check().await?;
        let blobs = BlobStore::new(paths.blob_root(), paths.blob_tmp());
        blobs.sweep_tmp().await;
        Ok(Self {
            db,
            blobs,
            ids,
            clock,
        })
    }
}

impl Store {
    // ---- accounts (data.db) ------------------------------------------------

    /// Inserts or refreshes an account keyed by email.
    pub(crate) async fn upsert_account(&self, account: &NewAccount) -> StorageResult<AccountId> {
        let now = self.clock.now_unix_ms();
        let existing = sqlx::query_as!(
            IdOnly,
            "SELECT id FROM accounts WHERE email = ?1",
            account.email
        )
        .fetch_optional(&self.db.data.write)
        .await?;
        let id = match existing {
            Some(row) => AccountId::parse(&row.id).ok_or_else(|| {
                StorageError::Row(format!("stored account id not a uuid: {}", row.id))
            })?,
            None => AccountId::from_uuid(self.ids.next_id()),
        };
        sqlx::query!(
            "INSERT INTO accounts (id, name, email, provider, protocol, auth_kind, host, sync_state, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'disconnected', ?8, ?8)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name,
               email = excluded.email,
               provider = excluded.provider,
               protocol = excluded.protocol,
               auth_kind = excluded.auth_kind,
               host = excluded.host,
               updated_at = excluded.updated_at",
            id.to_string(),
            account.name,
            account.email,
            provider_wire(&account.provider),
            protocol_wire(account.protocol),
            account.auth_kind,
            account.host,
            now
        )
        .execute(&self.db.data.write)
        .await?;
        Ok(id)
    }

    /// Lists all accounts with their mirrored state.
    pub(crate) async fn list_accounts(
        &self,
    ) -> StorageResult<Vec<kestrel_core::protocol::AccountSummary>> {
        #[derive(sqlx::FromRow)]
        struct AccountRow {
            id: String,
            name: String,
            email: String,
            provider: String,
            protocol: String,
            sync_state: String,
            host: String,
        }
        let rows = sqlx::query_as!(
            AccountRow,
            "SELECT id, name, email, provider, protocol, sync_state, host FROM accounts ORDER BY created_at"
        )
        .fetch_all(&self.db.data.read)
        .await?;
        rows.into_iter()
            .map(
                |AccountRow {
                     id,
                     name,
                     email,
                     provider,
                     protocol,
                     sync_state,
                     host,
                 }| {
                    Ok(kestrel_core::protocol::AccountSummary {
                        id: parse_id::<AccountId>(&id)?,
                        name,
                        email,
                        provider: provider_parse(&provider),
                        protocol: protocol_parse(&protocol),
                        state: state_parse(&sync_state),
                        host,
                        color: None,
                    })
                },
            )
            .collect()
    }

    /// Deletes an account (cascades outbox rows; folder purge is separate).
    pub(crate) async fn delete_account(&self, id: AccountId) -> StorageResult<()> {
        sqlx::query!("DELETE FROM accounts WHERE id = ?1", id.to_string())
            .execute(&self.db.data.write)
            .await?;
        Ok(())
    }

    /// Mirrors the connection state into the account row.
    pub(crate) async fn set_account_state(&self, id: AccountId, state: &str) -> StorageResult<()> {
        sqlx::query!(
            "UPDATE accounts SET sync_state = ?2, updated_at = ?3 WHERE id = ?1",
            id.to_string(),
            state,
            self.clock.now_unix_ms()
        )
        .execute(&self.db.data.write)
        .await?;
        Ok(())
    }

    // ---- sync cursors (cache.db) ---------------------------------------------

    /// Highest stored UID in a folder.
    pub(crate) async fn max_uid(&self, folder: FolderId) -> StorageResult<Option<u32>> {
        let row = sqlx::query_as!(
            MaxUidRow,
            "SELECT MAX(uid) AS uid FROM messages WHERE folder_id = ?1",
            folder.to_string()
        )
        .fetch_one(&self.db.cache.read)
        .await?;
        Ok(row.uid.map(|u| u32::try_from(u.max(0)).unwrap_or(0)))
    }

    /// Persists sync cursors for a folder.
    pub(crate) async fn update_sync_cursors(
        &self,
        folder: FolderId,
        uid_validity: u32,
        highest_modseq: Option<u64>,
    ) -> StorageResult<()> {
        sqlx::query!(
            "UPDATE folders SET uid_validity = ?2,
                    highest_modseq = COALESCE(?3, highest_modseq)
             WHERE id = ?1",
            folder.to_string(),
            i64::from(uid_validity),
            highest_modseq.map(i64::try_from).transpose().ok().flatten()
        )
        .execute(&self.db.cache.write)
        .await?;
        Ok(())
    }

    // ---- folders (cache.db) --------------------------------------------------

    /// Upserts a folder; enforces the cross-DB account FK in code (ADR 0009).
    pub(crate) async fn upsert_folder(&self, folder: &NewFolder) -> StorageResult<FolderId> {
        let exists = sqlx::query_as!(
            IdOnly,
            "SELECT id FROM accounts WHERE id = ?1",
            folder.account.to_string()
        )
        .fetch_optional(&self.db.data.read)
        .await?;
        if exists.is_none() {
            return Err(StorageError::Invariant(format!(
                "upsert_folder: account {} does not exist in data.db (cross-DB FK)",
                folder.account
            )));
        }
        let now = self.clock.now_unix_ms();
        let existing = sqlx::query_as!(
            IdOnly,
            "SELECT id FROM folders WHERE account_id = ?1 AND remote_name = ?2",
            folder.account.to_string(),
            folder.remote_name
        )
        .fetch_optional(&self.db.cache.write)
        .await?;
        let id = match existing {
            Some(row) => FolderId::parse(&row.id).ok_or_else(|| {
                StorageError::Row(format!("stored folder id not a uuid: {}", row.id))
            })?,
            None => FolderId::from_uuid(self.ids.next_id()),
        };
        let attrs = serde_json::to_string(&folder.attributes)?;
        sqlx::query!(
            "INSERT INTO folders (id, account_id, remote_name, attributes, role, delimiter, uid_validity, highest_modseq, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(account_id, remote_name) DO UPDATE SET
               attributes = excluded.attributes,
               role = excluded.role,
               delimiter = excluded.delimiter,
               last_seen = excluded.last_seen",
            id.to_string(),
            folder.account.to_string(),
            folder.remote_name,
            attrs,
            folder.role.map(role_wire),
            folder.delimiter,
            i64::from(folder.uid_validity),
            i64::try_from(folder.highest_modseq).unwrap_or(0),
            now
        )
        .execute(&self.db.cache.write)
        .await?;
        Ok(id)
    }

    /// Lists folders with unread/total counts.
    pub(crate) async fn list_folders(
        &self,
        account: AccountId,
    ) -> StorageResult<Vec<kestrel_core::protocol::FolderSummary>> {
        #[derive(sqlx::FromRow)]
        struct FolderListRow {
            id: String,
            remote_name: String,
            delimiter: String,
            role: Option<String>,
            total: i64,
            unread: i64,
        }
        let rows = sqlx::query_as!(
            FolderListRow,
            "SELECT f.id, f.remote_name, f.delimiter, f.role,
                    (SELECT COUNT(*) FROM messages m WHERE m.folder_id = f.id) AS total,
                    (SELECT COUNT(*) FROM messages m WHERE m.folder_id = f.id AND m.is_read = 0) AS unread
             FROM folders f WHERE f.account_id = ?1 ORDER BY f.remote_name",
            account.to_string()
        )
        .fetch_all(&self.db.cache.read)
        .await?;
        rows.into_iter()
            .map(
                |FolderListRow {
                     id,
                     remote_name,
                     delimiter,
                     role,
                     total,
                     unread,
                 }| {
                    Ok(kestrel_core::protocol::FolderSummary {
                        id: parse_id::<FolderId>(&id)?,
                        account,
                        remote_name,
                        role: role.and_then(|r| role_parse(&r)),
                        delimiter,
                        unread: u64::try_from(unread.max(0)).unwrap_or(0),
                        total: u64::try_from(total.max(0)).unwrap_or(0),
                    })
                },
            )
            .collect()
    }

    /// Fetches one folder row.
    pub(crate) async fn get_folder(&self, id: FolderId) -> StorageResult<crate::store::FolderRow> {
        let row = sqlx::query_as!(
            FolderDbRow,
            "SELECT account_id, remote_name, attributes, role, delimiter, uid_validity, highest_modseq
             FROM folders WHERE id = ?1",
            id.to_string()
        )
        .fetch_optional(&self.db.cache.read)
        .await?;
        match row {
            None => Err(StorageError::Row(format!("folder {id} not found"))),
            Some(FolderDbRow {
                account_id: account,
                remote_name,
                attributes: attrs,
                role,
                delimiter,
                uid_validity: uidv,
                highest_modseq: modseq,
            }) => {
                let attributes: Vec<String> = serde_json::from_str(&attrs)
                    .map_err(|e| StorageError::Row(format!("folder attributes JSON: {e}")))?;
                Ok(crate::store::FolderRow {
                    id,
                    account: parse_id::<AccountId>(&account)?,
                    remote_name,
                    attributes,
                    role: role.and_then(|r| role_parse(&r)),
                    delimiter,
                    uid_validity: u32::try_from(uidv.max(0)).unwrap_or(0),
                    highest_modseq: u64::try_from(modseq.max(0)).unwrap_or(0),
                })
            }
        }
    }
}

// ---- wire helpers ----------------------------------------------------------

pub(crate) fn parse_id<T: kestrel_core::ids::IdParse>(s: &str) -> StorageResult<T> {
    T::parse_id(s).ok_or_else(|| StorageError::Row(format!("stored id not a uuid: {s}")))
}

pub(crate) fn provider_wire(p: &Provider) -> &'static str {
    match p {
        Provider::Generic => "generic",
        Provider::Gmail => "gmail",
        Provider::Outlook => "outlook",
        Provider::Fastmail => "fastmail",
        Provider::Jmap => "jmap",
        Provider::Yahoo => "yahoo",
        Provider::Aol => "aol",
        Provider::Icloud => "icloud",
        Provider::Proton => "proton",
        Provider::Zoho => "zoho",
        Provider::Gmx => "gmx",
        Provider::Webde => "webde",
        Provider::Mailru => "mailru",
        Provider::Yandex => "yandex",
        Provider::Comcast => "comcast",
        Provider::Att => "att",
        Provider::Verizon => "verizon",
        Provider::Tonline => "tonline",
        Provider::Ionos => "ionos",
        Provider::Rackspace => "rackspace",
        Provider::Mailbox => "mailbox",
    }
}

pub(crate) fn provider_parse(s: &str) -> Provider {
    match s {
        "gmail" => Provider::Gmail,
        "outlook" => Provider::Outlook,
        "fastmail" => Provider::Fastmail,
        "jmap" => Provider::Jmap,
        "yahoo" => Provider::Yahoo,
        "aol" => Provider::Aol,
        "icloud" => Provider::Icloud,
        "proton" => Provider::Proton,
        "zoho" => Provider::Zoho,
        "gmx" => Provider::Gmx,
        "webde" => Provider::Webde,
        "mailru" => Provider::Mailru,
        "yandex" => Provider::Yandex,
        "comcast" => Provider::Comcast,
        "att" => Provider::Att,
        "verizon" => Provider::Verizon,
        "tonline" => Provider::Tonline,
        "ionos" => Provider::Ionos,
        "rackspace" => Provider::Rackspace,
        "mailbox" => Provider::Mailbox,
        _ => Provider::Generic,
    }
}

pub(crate) fn protocol_wire(p: MailProtocol) -> &'static str {
    match p {
        MailProtocol::Imap => "imap",
        MailProtocol::Jmap => "jmap",
    }
}

pub(crate) fn protocol_parse(s: &str) -> MailProtocol {
    match s {
        "jmap" => MailProtocol::Jmap,
        _ => MailProtocol::Imap,
    }
}

pub(crate) fn role_wire(r: FolderRole) -> &'static str {
    match r {
        FolderRole::Inbox => "inbox",
        FolderRole::Sent => "sent",
        FolderRole::Drafts => "drafts",
        FolderRole::Trash => "trash",
        FolderRole::Archive => "archive",
        FolderRole::Junk => "junk",
        FolderRole::UnifiedInbox => "unified_inbox",
    }
}

pub(crate) fn role_parse(s: &str) -> Option<FolderRole> {
    match s {
        "inbox" => Some(FolderRole::Inbox),
        "sent" => Some(FolderRole::Sent),
        "drafts" => Some(FolderRole::Drafts),
        "trash" => Some(FolderRole::Trash),
        "archive" => Some(FolderRole::Archive),
        "junk" => Some(FolderRole::Junk),
        _ => None,
    }
}

pub(crate) fn state_parse(s: &str) -> ConnectionState {
    match s {
        "connecting" => ConnectionState::Connecting,
        "authenticating" => ConnectionState::Authenticating,
        "syncing" => ConnectionState::Syncing,
        "idle" => ConnectionState::Idle,
        "offline" => ConnectionState::OfflineMode,
        _ => ConnectionState::Disconnected,
    }
}
