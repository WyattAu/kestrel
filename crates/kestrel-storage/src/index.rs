//! Tantivy full-text index service (docs/schema.md §5, requirements §3.3).
//!
//! Single writer (`IndexService`), batched commits (≤ 1 per 500 ms or 500
//! docs), `messages.indexed_at` is the freshness truth: after every
//! successful commit the affected message ids are marked indexed through
//! the storage handle (crash between commit and mark ⇒ harmless redo).
//! The index is always **rebuildable** from cache.db + blobs.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use kestrel_core::ids::{AccountId, FolderId, MessageId};
use tantivy::{
    Index, IndexReader, IndexWriter, ReloadPolicy, Term,
    collector::TopDocs,
    query::QueryParser,
    schema::{
        FAST, Field, INDEXED, IndexRecordOption, STORED, Schema, SchemaBuilder, TextFieldIndexing,
        TextOptions,
    },
};
use tokio::sync::{RwLock, mpsc, oneshot};

use crate::{
    error::{StorageError, StorageResult},
    store::{PendingDoc, StorageHandle},
};

/// Batch commit interval (schema.md §5).
const COMMIT_INTERVAL: Duration = Duration::from_millis(500);
/// Documents that force an immediate commit.
const COMMIT_DOC_THRESHOLD: usize = 500;
/// Index writer heap budget.
const WRITER_HEAP: usize = 64 * 1024 * 1024;

/// A document ready for the index.
#[derive(Clone, Debug)]
pub struct IndexDoc {
    /// Message id (join key back to `SQLite`).
    pub id: MessageId,
    /// Owning folder (facet).
    pub folder: FolderId,
    /// Owning account (facet).
    pub account: AccountId,
    /// Subject.
    pub subject: Option<String>,
    /// Extracted plain text (never HTML, schema.md §5).
    pub body: String,
    /// From addresses.
    pub from: Vec<String>,
    /// To addresses.
    pub to: Vec<String>,
    /// Cc addresses.
    pub cc: Vec<String>,
    /// Attachment file names.
    pub attachment_names: Vec<String>,
    /// `internal_date` (unix ms).
    pub date: i64,
    /// Attachment presence.
    pub has_attachment: bool,
}

impl IndexDoc {
    /// Builds from a storage pending doc.
    #[must_use]
    pub fn from_pending(p: &PendingDoc) -> Self {
        let addr_strings = |a: &kestrel_core::protocol::Address| -> String { a.email.clone() };
        Self {
            id: p.id,
            folder: p.folder,
            account: p.account,
            subject: p.summary.subject.clone(),
            body: p.body_text.clone(),
            from: p.summary.from.iter().map(addr_strings).collect(),
            to: p.summary.to.iter().map(addr_strings).collect(),
            cc: p.summary.cc.iter().map(addr_strings).collect(),
            attachment_names: p.attachment_names.clone(),
            date: p.summary.internal_date,
            has_attachment: p.summary.has_attachments,
        }
    }
}

/// Tantivy field handles (built once from the fixed schema).
#[derive(Clone)]
pub(crate) struct Fields {
    pub(crate) msg_id: Field,
    pub(crate) account: Field,
    pub(crate) folder: Field,
    pub(crate) subject: Field,
    pub(crate) body: Field,
    pub(crate) from: Field,
    pub(crate) from_exact: Field,
    pub(crate) to: Field,
    pub(crate) to_exact: Field,
    pub(crate) cc: Field,
    pub(crate) cc_exact: Field,
    pub(crate) attachment_names: Field,
    pub(crate) date: Field,
    pub(crate) has_attachment: Field,
}

fn en_stem() -> TextOptions {
    TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("en_stem")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    )
}

fn tokenized() -> TextOptions {
    TextOptions::default().set_indexing_options(
        TextFieldIndexing::default().set_index_option(IndexRecordOption::WithFreqsAndPositions),
    )
}

fn raw_str() -> TextOptions {
    TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("raw")
            .set_index_option(IndexRecordOption::Basic),
    )
}

/// Builds the fixed schema (schema.md §5).
#[must_use]
pub(crate) fn build_schema() -> (Schema, Fields) {
    let mut b = SchemaBuilder::new();
    let msg_id = b.add_text_field("msg_id", STORED);
    let account = b.add_u64_field("account", INDEXED | FAST);
    let folder = b.add_u64_field("folder", INDEXED | FAST);
    let subject = b.add_text_field("subject", en_stem().set_stored());
    let body = b.add_text_field("body", en_stem());
    let from = b.add_text_field("from", tokenized());
    let from_exact = b.add_text_field("from_exact", raw_str());
    let to = b.add_text_field("to", tokenized());
    let to_exact = b.add_text_field("to_exact", raw_str());
    let cc = b.add_text_field("cc", tokenized());
    let cc_exact = b.add_text_field("cc_exact", raw_str());
    let attachment_names = b.add_text_field("attachment_names", tokenized());
    let date = b.add_i64_field("date", FAST);
    let has_attachment = b.add_u64_field("has_attachment", INDEXED | FAST);
    let schema = b.build();
    (
        schema,
        Fields {
            msg_id,
            account,
            folder,
            subject,
            body,
            from,
            from_exact,
            to,
            to_exact,
            cc,
            cc_exact,
            attachment_names,
            date,
            has_attachment,
        },
    )
}

/// uuid-string → u64 facet map (persisted beside the index; rebuildable).
#[derive(Default)]
pub(crate) struct IdMap {
    pub(crate) folders: HashMap<String, u64>,
    pub(crate) accounts: HashMap<String, u64>,
    next: u64,
}

#[derive(serde::Deserialize)]
struct Persisted {
    folders: HashMap<String, u64>,
    accounts: HashMap<String, u64>,
    next: u64,
}

impl IdMap {
    fn folder(&mut self, id: FolderId) -> u64 {
        alloc_facet(id.to_string(), &mut self.folders, &mut self.next)
    }

    fn account(&mut self, id: AccountId) -> u64 {
        alloc_facet(id.to_string(), &mut self.accounts, &mut self.next)
    }

    fn load(path: &std::path::Path) -> Self {
        let mut map = Self {
            next: 1,
            ..Self::default()
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return map;
        };
        if let Ok(p) = serde_json::from_str::<Persisted>(&text) {
            map.folders = p.folders;
            map.accounts = p.accounts;
            map.next = p.next.max(1);
        }
        map
    }

    fn save(&self, path: &std::path::Path) {
        #[derive(serde::Serialize)]
        struct PersistedOut<'a> {
            folders: &'a HashMap<String, u64>,
            accounts: &'a HashMap<String, u64>,
            next: u64,
        }
        let payload = PersistedOut {
            folders: &self.folders,
            accounts: &self.accounts,
            next: self.next,
        };
        if let Ok(json) = serde_json::to_string_pretty(&payload)
            && let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_ok()
        {
            let _ = std::fs::write(path, json);
        }
    }
}

/// The shared index: tantivy handle + schema + facet map + reader.
/// Writer-side mutation is confined to `IndexService`.
pub(crate) struct SharedIndex {
    pub(crate) index: Index,
    pub(crate) fields: Fields,
    pub(crate) map: RwLock<IdMap>,
    pub(crate) reader: IndexReader,
    map_path: PathBuf,
}

fn alloc_facet(key: String, map: &mut HashMap<String, u64>, next: &mut u64) -> u64 {
    if let Some(v) = map.get(&key) {
        return *v;
    }
    *next += 1;
    map.insert(key, *next);
    *next
}

impl SharedIndex {
    /// Opens (creating) the index directory.
    pub(crate) fn open(dir: &std::path::Path) -> StorageResult<Self> {
        std::fs::create_dir_all(dir)
            .map_err(|e| StorageError::Index(format!("mkdir {}: {e}", dir.display())))?;
        let (schema, fields) = build_schema();
        let index = Index::create_in_dir(dir, schema)
            .or_else(|_| Index::open_in_dir(dir))
            .map_err(|e| StorageError::Index(format!("open {}: {e}", dir.display())))?;
        // "en_stem" is among tantivy's pre-registered tokenizers; nothing
        // to register manually.
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|e| StorageError::Index(format!("reader: {e}")))?;
        let map_path = dir.join("idmap.json");
        let map = IdMap::load(&map_path);
        Ok(Self {
            index,
            fields,
            map: RwLock::new(map),
            reader,
            map_path,
        })
    }

    /// Registers a folder id, returning its u64 facet (allocating on first
    /// sight). Persisted immediately — map files are tiny.
    pub(crate) async fn folder_facet(&self, id: FolderId) -> u64 {
        {
            let mut map = self.map.write().await;
            let f = map.folder(id);
            map.save(&self.map_path);
            f
        }
    }

    /// Registers an account id, returning its u64 facet.
    pub(crate) async fn account_facet(&self, id: AccountId) -> u64 {
        {
            let mut map = self.map.write().await;
            let a = map.account(id);
            map.save(&self.map_path);
            a
        }
    }
}

impl SharedIndex {
    /// Cheap integrity pass: a match-nothing search that forces every
    /// segment reader open; decode failures surface as corrupt
    /// (schema.md §7).
    fn validate(&self) -> StorageResult<()> {
        use tantivy::query::TermQuery;
        let searcher = self.reader.searcher();
        let query = TermQuery::new(
            Term::from_field_text(self.fields.msg_id, "\u{0}impossible-id"),
            IndexRecordOption::Basic,
        );
        searcher
            .search(&query, &TopDocs::with_limit(1).order_by_score())
            .map(|_| ())
            .map_err(|e| StorageError::Index(format!("validate: {e}")))
    }
}

/// Internal accessor for the search service.
pub(crate) fn shared_of(handle: &IndexHandle) -> Arc<SharedIndex> {
    Arc::clone(&handle.shared)
}

enum IndexCommand {
    Add {
        docs: Vec<IndexDoc>,
        reply: Option<oneshot::Sender<StorageResult<()>>>,
    },
    Delete {
        ids: Vec<MessageId>,
        reply: Option<oneshot::Sender<StorageResult<()>>>,
    },
    Commit {
        reply: Option<oneshot::Sender<StorageResult<()>>>,
    },
    Rebuild {
        reply: oneshot::Sender<StorageResult<u64>>,
    },
    Validate {
        reply: oneshot::Sender<StorageResult<()>>,
    },
}

/// Cloneable handle to the `IndexService`.
#[derive(Clone)]
pub struct IndexHandle {
    tx: mpsc::Sender<IndexCommand>,
    shared: Arc<SharedIndex>,
}

impl IndexHandle {
    /// Queues documents for indexing (delete-then-add per message id).
    ///
    /// # Errors
    /// [`kestrel_core::error::KestrelError`] on index failure.
    pub async fn add(&self, docs: Vec<IndexDoc>) -> Result<(), kestrel_core::error::KestrelError> {
        self.call(|reply| IndexCommand::Add {
            docs,
            reply: Some(reply),
        })
        .await
    }

    /// Queues fire-and-forget adds (bulk path).
    pub async fn add_fire_and_forget(&self, docs: Vec<IndexDoc>) {
        let _ = self.tx.send(IndexCommand::Add { docs, reply: None }).await;
    }

    /// Removes messages from the index.
    ///
    /// # Errors
    /// [`kestrel_core::error::KestrelError`] on index failure.
    pub async fn delete(
        &self,
        ids: Vec<MessageId>,
    ) -> Result<(), kestrel_core::error::KestrelError> {
        self.call(|reply| IndexCommand::Delete {
            ids,
            reply: Some(reply),
        })
        .await
    }

    /// Forces a commit (shutdown path).
    ///
    /// # Errors
    /// [`kestrel_core::error::KestrelError`] on index failure.
    pub async fn commit(&self) -> Result<(), kestrel_core::error::KestrelError> {
        self.call(|reply| IndexCommand::Commit { reply: Some(reply) })
            .await
    }

    /// Rebuilds the whole index from storage (schema.md §7 recovery).
    ///
    /// # Errors
    /// [`kestrel_core::error::KestrelError`] on failure.
    pub async fn rebuild(&self) -> Result<u64, kestrel_core::error::KestrelError> {
        self.call(|reply| IndexCommand::Rebuild { reply }).await
    }

    /// Validates index integrity (startup pass, schema.md §7).
    ///
    /// # Errors
    /// [`kestrel_core::error::KestrelError`] when corrupt.
    pub async fn validate(&self) -> Result<(), kestrel_core::error::KestrelError> {
        self.call(|reply| IndexCommand::Validate { reply }).await
    }

    async fn call<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<StorageResult<T>>) -> IndexCommand,
    ) -> Result<T, kestrel_core::error::KestrelError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(make(tx))
            .await
            .map_err(|_| kestrel_core::error::KestrelError::Cancelled)?;
        rx.await
            .map_err(|_| kestrel_core::error::KestrelError::Cancelled)?
            .map_err(kestrel_core::error::KestrelError::from)
    }
}

/// The single-writer index service.
pub struct IndexService;

impl IndexService {
    /// Spawns the service over `index_dir`.
    ///
    /// # Errors
    /// Fails when the index directory cannot be opened/created.
    pub fn spawn(
        index_dir: &std::path::Path,
        storage: StorageHandle,
        clock: Arc<dyn kestrel_core::clock::Clock>,
    ) -> Result<IndexHandle, StorageError> {
        let shared = Arc::new(SharedIndex::open(index_dir)?);
        let (tx, rx) = mpsc::channel(64);
        let handle = IndexHandle {
            tx: tx.clone(),
            shared: Arc::clone(&shared),
        };
        tokio::spawn(Self::run(shared, storage, clock, rx));
        Ok(handle)
    }

    async fn run(
        shared: Arc<SharedIndex>,
        storage: StorageHandle,
        clock: Arc<dyn kestrel_core::clock::Clock>,
        mut rx: mpsc::Receiver<IndexCommand>,
    ) {
        let mut writer = match shared.index.writer(WRITER_HEAP) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(error = %e, "index writer open failed");
                // Drain commands with the failure.
                while let Some(cmd) = rx.recv().await {
                    reply_index_error(cmd, &e.to_string());
                }
                return;
            }
        };
        let mut pending: Vec<MessageId> = Vec::new();
        let mut dirty = false;
        let mut ticker = tokio::time::interval(COMMIT_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tracing::info!("service.index started");
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if dirty
                        && let Err(e) = Self::commit(&shared, &mut writer, &mut pending, &storage, &clock).await {
                            tracing::error!(error = %e, "index commit failed");
                        }
                }
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else {
                        let _ = Self::commit(&shared, &mut writer, &mut pending, &storage, &clock).await;
                        break;
                    };
                    match cmd {
                        IndexCommand::Add { docs, reply } => {
                            let count = docs.len();
                            let result =
                                Self::add(&shared, &mut writer, &mut pending, &mut dirty, docs).await;
                            // Awaited adds imply durability: commit before
                            // replying (fire-and-forget batches on the
                            // 500 ms ticker instead).
                            if reply.is_some() {
                                let commit =
                                    Self::commit(&shared, &mut writer, &mut pending, &storage, &clock).await;
                                let result = result.and(commit);
                                if let Some(reply) = reply {
                                    let _ = reply.send(result);
                                }
                            } else if let Some(reply) = reply {
                                let _ = reply.send(result);
                            } else if count >= COMMIT_DOC_THRESHOLD && dirty {
                                let _ = Self::commit(&shared, &mut writer, &mut pending, &storage, &clock).await;
                            }
                        }
                        IndexCommand::Delete { ids, reply } => {
                            Self::delete(&shared, &mut writer, &mut dirty, &ids);
                            if let Some(reply) = reply {
                                let _ = reply.send(Ok(()));
                            }
                        }
                        IndexCommand::Commit { reply } => {
                            let result = Self::commit(&shared, &mut writer, &mut pending, &storage, &clock).await;
                            if let Some(reply) = reply {
                                let _ = reply.send(result);
                            }
                        }
                        IndexCommand::Rebuild { reply } => {
                            let result = Self::rebuild(&shared, &mut writer, &storage, &clock).await;
                            let _ = reply.send(result);
                        }
                        IndexCommand::Validate { reply } => {
                            let result: StorageResult<()> = SharedIndex::validate(&shared);
                            let _ = reply.send(result);
                        }
                    }
                }
            }
        }
        tracing::info!("service.index stopped");
    }

    async fn add(
        shared: &SharedIndex,
        writer: &mut IndexWriter,
        pending: &mut Vec<MessageId>,
        dirty: &mut bool,
        docs: Vec<IndexDoc>,
    ) -> StorageResult<()> {
        for doc in docs {
            // delete-then-add: one live doc per message id.
            writer.delete_term(Term::from_field_text(
                shared.fields.msg_id,
                &doc.id.to_string(),
            ));
            let mut d = tantivy::TantivyDocument::new();
            d.add_text(shared.fields.msg_id, doc.id.to_string());
            let folder_facet = shared.folder_facet(doc.folder).await;
            let account_facet = shared.account_facet(doc.account).await;
            d.add_u64(shared.fields.account, account_facet);
            d.add_u64(shared.fields.folder, folder_facet);
            if let Some(subject) = &doc.subject {
                d.add_text(shared.fields.subject, subject);
            }
            d.add_text(shared.fields.body, &doc.body);
            for a in &doc.from {
                d.add_text(shared.fields.from, a);
                d.add_text(shared.fields.from_exact, a.to_lowercase());
            }
            for a in &doc.to {
                d.add_text(shared.fields.to, a);
                d.add_text(shared.fields.to_exact, a.to_lowercase());
            }
            for a in &doc.cc {
                d.add_text(shared.fields.cc, a);
                d.add_text(shared.fields.cc_exact, a.to_lowercase());
            }
            for n in &doc.attachment_names {
                d.add_text(shared.fields.attachment_names, n);
            }
            d.add_i64(shared.fields.date, doc.date);
            d.add_u64(shared.fields.has_attachment, u64::from(doc.has_attachment));
            writer
                .add_document(d)
                .map_err(|e| StorageError::Index(e.to_string()))?;
            pending.push(doc.id);
            *dirty = true;
        }
        Ok(())
    }

    fn delete(shared: &SharedIndex, writer: &mut IndexWriter, dirty: &mut bool, ids: &[MessageId]) {
        for id in ids {
            writer.delete_term(Term::from_field_text(shared.fields.msg_id, &id.to_string()));
            *dirty = true;
        }
    }

    async fn commit(
        shared: &SharedIndex,
        writer: &mut IndexWriter,
        pending: &mut Vec<MessageId>,
        storage: &StorageHandle,
        clock: &Arc<dyn kestrel_core::clock::Clock>,
    ) -> StorageResult<()> {
        if pending.is_empty() {
            return Ok(());
        }
        writer
            .commit()
            .map_err(|e| StorageError::Index(e.to_string()))?;
        let _ = shared.reader.reload();
        // Freshness cursor: messages.indexed_at is the truth (schema.md §5).
        let ids = std::mem::take(pending);
        if storage
            .mark_indexed(ids, clock.now_unix_ms())
            .await
            .is_err()
        {
            tracing::warn!("mark_indexed failed after index commit; catch-up will redo");
        }
        Ok(())
    }

    async fn rebuild(
        shared: &SharedIndex,
        writer: &mut IndexWriter,
        storage: &StorageHandle,
        clock: &Arc<dyn kestrel_core::clock::Clock>,
    ) -> StorageResult<u64> {
        // Wipe and re-feed from cache.db + blobs.
        writer
            .delete_all_documents()
            .map_err(|e| StorageError::Index(e.to_string()))?;
        let mut total = 0u64;
        let mut offset = 0u64;
        loop {
            let batch = storage
                .feed_all_for_index(256, offset)
                .await
                .map_err(|_| StorageError::Index("storage unavailable during rebuild".into()))?;
            if batch.is_empty() {
                break;
            }
            offset += batch.len() as u64;
            let docs: Vec<IndexDoc> = batch.iter().map(IndexDoc::from_pending).collect();
            let count = docs.len();
            Self::add(shared, writer, &mut Vec::new(), &mut true, docs).await?;
            writer
                .commit()
                .map_err(|e| StorageError::Index(e.to_string()))?;
            let ids: Vec<MessageId> = batch.into_iter().map(|d| d.id).collect();
            let _ = storage.mark_indexed(ids, clock.now_unix_ms()).await;
            total += count as u64;
        }
        let _ = shared.reader.reload();
        Ok(total)
    }
}

fn reply_index_error(cmd: IndexCommand, detail: &str) {
    let err = StorageError::Index(detail.to_string());
    match cmd {
        IndexCommand::Add { reply: Some(r), .. }
        | IndexCommand::Delete { reply: Some(r), .. }
        | IndexCommand::Commit { reply: Some(r) } => {
            let _ = r.send(Err(err));
        }
        IndexCommand::Rebuild { reply } => {
            let _ = reply.send(Err(err));
        }
        IndexCommand::Validate { reply } => {
            let _ = reply.send(Err(err));
        }
        _ => {}
    }
}

/// Search-side helpers shared with the `SearchService`.
pub(crate) fn query_parser(index: &Index, fields: &Fields) -> QueryParser {
    QueryParser::for_index(
        index,
        vec![
            fields.subject,
            fields.body,
            fields.from,
            fields.to,
            fields.cc,
            fields.attachment_names,
        ],
    )
}
