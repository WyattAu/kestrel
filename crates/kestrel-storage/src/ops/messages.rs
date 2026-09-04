//! Message operations (cache.db): ingest, listing, views, mutations.

use kestrel_core::{
    ids::{BlobHash, FolderId, MessageId, MessageId as Mid, PartId, ThreadId},
    links::{LinkRisk, classify_link},
    mime::{MimeParser, PartContent, StalwartParser},
    protocol::{
        Address, Flag, FlagOp, MessagePage, MessagePartView, MessageSummary, MessageView,
        PartIdView, SortDir, SortField, SortSpec, SuspiciousLinkInfo, SuspiciousLinkReason,
        ThreadIdLite, Window,
    },
    sanitizer::sanitize_html_body,
    threading::{ThreadInput, thread_messages},
};
use tracing::instrument;

use crate::{
    error::{StorageError, StorageResult},
    ops::{IdOnly, Store, parse_id},
    store::{IngestBatch, IngestStats, MessageLoad, PendingDoc},
};

/// Per-part pre-computed write (CAS done before the ingest transaction).
struct PartWrite {
    part_id: PartId,
    seq: u32,
    mime_type: String,
    content_id: Option<String>,
    disposition: Option<String>,
    filename: Option<String>,
    encoding: String,
    byte_size: u64,
    blob: BlobHash,
}

/// COUNT(*) row shape.
#[derive(sqlx::FromRow)]
struct FlagRow {
    /// Message id.
    id: String,
    /// Flags JSON.
    flags: String,
}

/// COUNT(*) row shape.
#[derive(sqlx::FromRow)]
struct CountRow {
    n: i64,
}

/// `raw_blob` row shape.
#[derive(sqlx::FromRow)]
struct RawBlobRow {
    raw_blob: Option<String>,
}

/// Part row hydration shape.
#[derive(sqlx::FromRow)]
struct PartRow {
    id: String,
    seq: i64,
    mime_type: String,
    content_id: Option<String>,
    disposition: Option<String>,
    filename: Option<String>,
    byte_size: i64,
    /// CAS hash: read by the `kestrel-cid://` viewport path (GUI).
    #[allow(dead_code)]
    blob_sha256: String,
}

/// Message operations extension.
pub(crate) trait StoreMessagesExt {
    /// Ingests a parsed batch atomically.
    fn ingest_batch(&self, batch: IngestBatch) -> impl Future<Output = StorageResult<IngestStats>>;
    fn list_messages(
        &self,
        folder: FolderId,
        window: Window,
        sort: &SortSpec,
    ) -> impl Future<Output = StorageResult<MessagePage>>;
    /// Lists messages from all folders with role = 'inbox' (unified inbox).
    fn list_unified_inbox(
        &self,
        window: Window,
        sort: &SortSpec,
    ) -> impl Future<Output = StorageResult<MessagePage>>;
    fn get_message(&self, id: MessageId) -> impl Future<Output = StorageResult<MessageLoad>>;
    /// Returns decoded bytes for a specific MIME part.
    fn get_attachment_data(
        &self,
        message: MessageId,
        part_key: &str,
    ) -> impl Future<Output = StorageResult<Vec<u8>>>;
    fn set_flags(
        &self,
        messages: &[MessageId],
        op: &FlagOp,
    ) -> impl Future<Output = StorageResult<Vec<MessageId>>>;
    fn delete_messages(&self, messages: &[MessageId]) -> impl Future<Output = StorageResult<u64>>;
    fn move_messages(
        &self,
        moves: &[(MessageId, FolderId, u32)],
    ) -> impl Future<Output = StorageResult<u64>>;
    fn purge_folder(&self, folder: FolderId) -> impl Future<Output = StorageResult<u64>>;
    fn pending_index(&self, limit: u64) -> impl Future<Output = StorageResult<Vec<PendingDoc>>>;
    fn feed_all_for_index(
        &self,
        limit: u64,
        offset: u64,
    ) -> impl Future<Output = StorageResult<Vec<PendingDoc>>>;
    fn mark_indexed(&self, ids: &[MessageId], at: i64) -> impl Future<Output = StorageResult<()>>;
}

pub(crate) fn flags_json(flags: &[Flag]) -> String {
    serde_json::to_string(flags).unwrap_or_else(|_| "[]".to_string())
}

pub(crate) fn parse_flags(s: &str) -> Vec<Flag> {
    serde_json::from_str(s).unwrap_or_default()
}

pub(crate) fn addrs_json(addrs: &[Address]) -> String {
    serde_json::to_string(addrs).unwrap_or_else(|_| "[]".to_string())
}

pub(crate) fn parse_addrs(s: &str) -> Vec<Address> {
    serde_json::from_str(s).unwrap_or_default()
}

/// Row shape shared by list/pending/get queries.
#[derive(sqlx::FromRow)]
pub(crate) struct MsgRow {
    id: String,
    folder_id: String,
    uid: i64,
    internal_date: i64,
    flags: String,
    message_id: Option<String>,
    in_reply_to: Option<String>,
    subject: Option<String>,
    from_addr: Option<String>,
    to_addrs: String,
    cc_addrs: String,
    size: i64,
    is_read: i64,
    is_flagged: i64,
    is_answered: i64,
    has_attachments: i64,
    thread_id: String,
}

fn row_to_summary(row: MsgRow) -> StorageResult<MessageSummary> {
    let MsgRow {
        id,
        folder_id,
        uid,
        internal_date,
        flags,
        message_id,
        in_reply_to,
        subject,
        from_addr,
        to_addrs,
        cc_addrs,
        size,
        is_read,
        is_flagged,
        is_answered,
        has_attachments,
        thread_id,
    } = row;
    Ok(MessageSummary {
        id: parse_id::<MessageId>(&id)?,
        folder: parse_id::<FolderId>(&folder_id)?,
        uid: u32::try_from(uid.max(0)).unwrap_or(0),
        internal_date,
        flags: parse_flags(&flags),
        message_id,
        in_reply_to,
        subject,
        from: from_addr.as_deref().map(parse_addrs).unwrap_or_default(),
        to: parse_addrs(&to_addrs),
        cc: parse_addrs(&cc_addrs),
        size: u64::try_from(size.max(0)).unwrap_or(0),
        is_read: is_read != 0,
        is_flagged: is_flagged != 0,
        is_answered: is_answered != 0,
        has_attachments: has_attachments != 0,
        thread: ThreadIdLite { key: thread_id },
    })
}

impl StoreMessagesExt for Store {
    /// Ingests a parsed batch atomically: threading (pure) → threads upsert
    /// (data.db) → part blobs (CAS) → messages + parts (one cache.db tx).
    #[instrument(skip_all)]
    #[allow(clippy::too_many_lines)]
    async fn ingest_batch(&self, batch: IngestBatch) -> StorageResult<IngestStats> {
        if batch.messages.is_empty() {
            return Ok(IngestStats::default());
        }

        // 1) Threading (pure, core).
        let inputs: Vec<ThreadInput> = batch
            .messages
            .iter()
            .map(|m| ThreadInput {
                id: Mid::from_uuid(self.ids.next_id()).to_string(),
                message_id: m.parsed.message_id.clone(),
                in_reply_to: m.parsed.in_reply_to.clone(),
                references: m.parsed.references.clone(),
                subject: m.parsed.subject.clone(),
                timestamp: m.internal_date,
            })
            .collect();
        let assignments = thread_messages(&inputs);

        // 2) Threads upsert (data.db).
        let mut thread_ids: Vec<ThreadId> = Vec::with_capacity(assignments.len());
        let mut tx_data = self.db.data.write.begin().await?;
        for (i, assignment) in assignments.iter().enumerate() {
            let subject_norm = kestrel_core::threading::normalize_subject(
                batch.messages[i]
                    .parsed
                    .subject
                    .as_deref()
                    .unwrap_or_default(),
            );
            let existing: Option<(String,)> =
                sqlx::query_as("SELECT id FROM threads WHERE thread_key = ?1")
                    .bind(&assignment.thread_key)
                    .fetch_optional(&mut *tx_data)
                    .await?;
            let tid = if let Some((tid,)) = existing {
                parse_id::<ThreadId>(&tid)?
            } else {
                let tid = ThreadId::from_uuid(self.ids.next_id());
                sqlx::query(
                    "INSERT INTO threads (id, thread_key, subject_norm, first_seen)
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .bind(tid.to_string())
                .bind(&assignment.thread_key)
                .bind(&subject_norm)
                .bind(batch.messages[i].internal_date)
                .execute(&mut *tx_data)
                .await?;
                tid
            };
            thread_ids.push(tid);
        }
        tx_data.commit().await?;

        // 3) Part blobs into the CAS (before the cache transaction; orphans
        //    on failure are GC'd).
        let mut all_parts: Vec<Vec<PartWrite>> = Vec::with_capacity(batch.messages.len());
        for msg in &batch.messages {
            let mut parts = Vec::with_capacity(msg.parsed.parts.len());
            for p in &msg.parsed.parts {
                let content_bytes: Vec<u8> = match &p.content {
                    PartContent::Text(t) => t.as_bytes().to_vec(),
                    PartContent::Binary(b) => b.clone(),
                    PartContent::Nested(nested) => {
                        nested.text_body.clone().unwrap_or_default().into_bytes()
                    }
                };
                let blob = self.blobs.write(&content_bytes).await?;
                parts.push(PartWrite {
                    part_id: PartId::from_uuid(self.ids.next_id()),
                    seq: p.seq,
                    mime_type: p.mime_type.clone(),
                    content_id: p.content_id.clone(),
                    disposition: p.disposition.clone(),
                    filename: p.filename.clone(),
                    encoding: p.encoding.clone(),
                    byte_size: p.decoded_size,
                    blob,
                });
            }
            all_parts.push(parts);
        }

        // 4) One cache.db transaction for messages + parts. Part rows are
        //    rewritten per (folder, uid) slot; the surviving row id is
        //    selected after the upsert.
        let mut stats = IngestStats::default();
        let mut tx = self.db.cache.write.begin().await?;
        for (i, msg) in batch.messages.iter().enumerate() {
            let existed: Option<(String,)> =
                sqlx::query_as("SELECT id FROM messages WHERE folder_id = ?1 AND uid = ?2")
                    .bind(msg.folder.to_string())
                    .bind(i64::from(msg.uid))
                    .fetch_optional(&mut *tx)
                    .await?;
            let flags = flags_json(&msg.flags);
            let from_json = addrs_json(&msg.parsed.from);
            let to_json = addrs_json(&msg.parsed.to);
            let cc_json = addrs_json(&msg.parsed.cc);
            let has_attachments = msg.parsed.parts.iter().any(|p| {
                p.disposition.as_deref() == Some("attachment")
                    || (p.filename.is_some()
                        && p.disposition.is_none()
                        && !p.mime_type.starts_with("text/"))
            });
            sqlx::query(
                "INSERT INTO messages
                   (id, folder_id, uid, internal_date, flags, message_id, in_reply_to, subject,
                    from_addr, to_addrs, cc_addrs, size, is_read, is_flagged, is_answered,
                    has_attachments, thread_id, raw_blob, indexed_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,NULL)
                 ON CONFLICT(folder_id, uid) DO UPDATE SET
                   internal_date = excluded.internal_date,
                   flags = excluded.flags,
                   message_id = excluded.message_id,
                   in_reply_to = excluded.in_reply_to,
                   subject = excluded.subject,
                   from_addr = excluded.from_addr,
                   to_addrs = excluded.to_addrs,
                   cc_addrs = excluded.cc_addrs,
                   size = excluded.size,
                   is_read = excluded.is_read,
                   is_flagged = excluded.is_flagged,
                   is_answered = excluded.is_answered,
                   has_attachments = excluded.has_attachments,
                   thread_id = excluded.thread_id,
                   raw_blob = COALESCE(excluded.raw_blob, messages.raw_blob),
                   indexed_at = NULL",
            )
            .bind(Mid::from_uuid(self.ids.next_id()).to_string())
            .bind(msg.folder.to_string())
            .bind(i64::from(msg.uid))
            .bind(msg.internal_date)
            .bind(&flags)
            .bind(&msg.parsed.message_id)
            .bind(&msg.parsed.in_reply_to)
            .bind(&msg.parsed.subject)
            .bind(&from_json)
            .bind(&to_json)
            .bind(&cc_json)
            .bind(i64::try_from(msg.raw_size).unwrap_or(0))
            .bind(i32::from(msg.flags.contains(&Flag::Seen)))
            .bind(i32::from(msg.flags.contains(&Flag::Flagged)))
            .bind(i32::from(msg.flags.contains(&Flag::Answered)))
            .bind(i32::from(has_attachments))
            .bind(thread_ids[i].to_string())
            .bind(msg.raw_blob.as_ref().map(BlobHash::to_hex))
            .execute(&mut *tx)
            .await?;

            if existed.is_some() {
                stats.updated += 1;
            } else {
                stats.inserted += 1;
            }

            // The surviving row id (old on update, new on insert).
            let live = sqlx::query_as!(
                IdOnly,
                "SELECT id FROM messages WHERE folder_id = ?1 AND uid = ?2",
                msg.folder.to_string(),
                i64::from(msg.uid)
            )
            .fetch_one(&mut *tx)
            .await?;

            // Rewrite parts for this slot.
            sqlx::query!("DELETE FROM parts WHERE message_id = ?1", live.id)
                .execute(&mut *tx)
                .await?;
            for p in &all_parts[i] {
                sqlx::query!(
                    "INSERT INTO parts
                       (id, message_id, seq, mime_type, content_id, disposition, filename, encoding, byte_size, blob_sha256)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    p.part_id.to_string(),
                    live.id,
                    i64::from(p.seq),
                    p.mime_type,
                    p.content_id,
                    p.disposition,
                    p.filename,
                    p.encoding,
                    i64::try_from(p.byte_size).unwrap_or(0),
                    p.blob.to_hex()
                )
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        Ok(stats)
    }

    /// Windowed, sorted listing.
    #[instrument(skip_all, fields(folder = %folder))]
    async fn list_messages(
        &self,
        folder: FolderId,
        window: Window,
        sort: &SortSpec,
    ) -> StorageResult<MessagePage> {
        let counted = sqlx::query_as!(
            CountRow,
            "SELECT COUNT(*) AS n FROM messages WHERE folder_id = ?1",
            folder.to_string()
        )
        .fetch_one(&self.db.cache.read)
        .await?;
        let total = counted.n;
        let order = match (sort.field, sort.dir) {
            (SortField::Date, SortDir::Asc) => "internal_date ASC",
            (SortField::Date, SortDir::Desc) => "internal_date DESC",
            (SortField::Subject, SortDir::Asc) => "subject ASC",
            (SortField::Subject, SortDir::Desc) => "subject DESC",
            (SortField::Sender, SortDir::Asc) => "from_addr ASC",
            (SortField::Sender, SortDir::Desc) => "from_addr DESC",
            (SortField::Uid, SortDir::Asc) => "uid ASC",
            (SortField::Uid, SortDir::Desc) => "uid DESC",
        };
        // Only the ORDER BY term varies, from the closed set validated
        // above; QueryBuilder is the sqlx-sanctioned dynamic-SQL path (the
        // single non-macro query — reviewed: no user input reaches `order`).
        let mut qb = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
            "SELECT id, folder_id, uid, internal_date, flags, message_id, in_reply_to, subject,
                    from_addr, to_addrs, cc_addrs, size, is_read, is_flagged, is_answered,
                    has_attachments, thread_id
             FROM messages WHERE folder_id = ",
        );
        qb.push_bind(folder.to_string());
        qb.push(" ORDER BY ");
        qb.push(order);
        qb.push(" LIMIT ");
        qb.push_bind(i64::try_from(window.limit).unwrap_or(50));
        qb.push(" OFFSET ");
        qb.push_bind(i64::try_from(window.offset).unwrap_or(0));
        let rows: Vec<MsgRow> = qb
            .build_query_as::<MsgRow>()
            .fetch_all(&self.db.cache.read)
            .await?;
        Ok(MessagePage {
            items: rows
                .into_iter()
                .map(row_to_summary)
                .collect::<StorageResult<Vec<_>>>()?,
            total: u64::try_from(total.max(0)).unwrap_or(0),
        })
    }

    /// Windowed, sorted listing across all folders with role = 'inbox'.
    #[instrument(skip_all)]
    async fn list_unified_inbox(
        &self,
        window: Window,
        sort: &SortSpec,
    ) -> StorageResult<MessagePage> {
        let counted: CountRow = sqlx::query_as(
            "SELECT COUNT(*) AS n FROM messages m
             JOIN folders f ON f.id = m.folder_id
             WHERE f.role = 'inbox'",
        )
        .fetch_one(&self.db.cache.read)
        .await?;
        let total = counted.n;
        let order = match (sort.field, sort.dir) {
            (SortField::Date, SortDir::Asc) => "m.internal_date ASC",
            (SortField::Date, SortDir::Desc) => "m.internal_date DESC",
            (SortField::Subject, SortDir::Asc) => "m.subject ASC",
            (SortField::Subject, SortDir::Desc) => "m.subject DESC",
            (SortField::Sender, SortDir::Asc) => "m.from_addr ASC",
            (SortField::Sender, SortDir::Desc) => "m.from_addr DESC",
            (SortField::Uid, SortDir::Asc) => "m.uid ASC",
            (SortField::Uid, SortDir::Desc) => "m.uid DESC",
        };
        let mut qb = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
            "SELECT m.id, m.folder_id, m.uid, m.internal_date, m.flags, m.message_id, m.in_reply_to,
                    m.subject, m.from_addr, m.to_addrs, m.cc_addrs, m.size, m.is_read, m.is_flagged,
                    m.is_answered, m.has_attachments, m.thread_id
             FROM messages m
             JOIN folders f ON f.id = m.folder_id
             WHERE f.role = 'inbox'
             ORDER BY ",
        );
        qb.push(order);
        qb.push(" LIMIT ");
        qb.push_bind(i64::try_from(window.limit).unwrap_or(50));
        qb.push(" OFFSET ");
        qb.push_bind(i64::try_from(window.offset).unwrap_or(0));
        let rows: Vec<MsgRow> = qb
            .build_query_as::<MsgRow>()
            .fetch_all(&self.db.cache.read)
            .await?;
        Ok(MessagePage {
            items: rows
                .into_iter()
                .map(row_to_summary)
                .collect::<StorageResult<Vec<_>>>()?,
            total: u64::try_from(total.max(0)).unwrap_or(0),
        })
    }

    /// Loads a message with resolved body (re-parses the raw blob).
    #[instrument(skip_all, fields(message = %id))]
    async fn get_message(&self, id: MessageId) -> StorageResult<MessageLoad> {
        let row = sqlx::query_as!(
            RawBlobRow,
            "SELECT raw_blob FROM messages WHERE id = ?1",
            id.to_string()
        )
        .fetch_optional(&self.db.cache.read)
        .await?;
        let Some(RawBlobRow { raw_blob }) = row else {
            return Err(StorageError::Row(format!("message {id} not found")));
        };
        let summary_row = sqlx::query_as!(
            MsgRow,
            "SELECT id, folder_id, uid, internal_date, flags, message_id, in_reply_to, subject,
                    from_addr, to_addrs, cc_addrs, size, is_read, is_flagged, is_answered,
                    has_attachments, thread_id
             FROM messages WHERE id = ?1",
            id.to_string()
        )
        .fetch_one(&self.db.cache.read)
        .await?;
        let summary = row_to_summary(summary_row)?;

        let part_rows = sqlx::query_as!(
            PartRow,
            "SELECT id, seq, mime_type, content_id, disposition, filename, byte_size, blob_sha256
             FROM parts WHERE message_id = ?1 ORDER BY seq",
            id.to_string()
        )
        .fetch_all(&self.db.cache.read)
        .await?;
        let parts: Vec<MessagePartView> = part_rows
            .into_iter()
            .map(|r| MessagePartView {
                id: PartIdView { key: r.id },
                seq: u32::try_from(r.seq.max(0)).unwrap_or(0),
                mime_type: r.mime_type,
                content_id: r.content_id,
                disposition: r.disposition,
                filename: r.filename,
                byte_size: u64::try_from(r.byte_size.max(0)).unwrap_or(0),
            })
            .collect();

        // Resolve bodies from the raw blob (single source of truth).
        let raw = match raw_blob.as_deref().and_then(BlobHash::parse_hex) {
            Some(hash) => self.blobs.read(&hash).await.ok(),
            None => None,
        };
        let mut view = MessageView {
            summary,
            parts,
            body_plain: None,
            body_html: None,
            remote_blocked: 0,
            warnings: Vec::new(),
            suspicious_links: Vec::new(),
        };
        if let Some(raw_bytes) = &raw
            && let Ok(parsed) = StalwartParser::parse(raw_bytes)
        {
            view.warnings.clone_from(&parsed.warnings);
            view.body_plain.clone_from(&parsed.text_body);
            if let Some(html) = &parsed.html_body {
                let sanitized = sanitize_html_body(html);
                view.body_html = Some(sanitized.html);
                view.remote_blocked = sanitized.remote_blocked;
                for part in &parsed.parts {
                    if part.mime_type == "text/html"
                        && let Some(text) = match &part.content {
                            PartContent::Text(t) => Some(t.clone()),
                            _ => None,
                        }
                    {
                        collect_links(&mut view, &text, id);
                    }
                }
            }
        }
        Ok(MessageLoad { view, raw })
    }

    #[instrument(skip_all, fields(message = %message, part_key))]
    async fn get_attachment_data(
        &self,
        message: MessageId,
        part_key: &str,
    ) -> StorageResult<Vec<u8>> {
        use crate::ops::blobs_gc::StoreBlobsExt;
        let row: Option<(String,)> =
            sqlx::query_as("SELECT blob_sha256 FROM parts WHERE id = ?1 AND message_id = ?2")
                .bind(part_key)
                .bind(message.to_string())
                .fetch_optional(&self.db.cache.read)
                .await?;
        let Some((hash_hex,)) = row else {
            return Err(StorageError::Row(format!(
                "part {part_key} not found in message {message}"
            )));
        };
        let hash = BlobHash::parse_hex(&hash_hex).ok_or_else(|| {
            StorageError::Row(format!("invalid blob hash {hash_hex} for part {part_key}"))
        })?;
        self.read_blob(&hash).await
    }

    /// Applies a flag mutation; returns the affected ids.
    #[instrument(skip_all, fields(uid = messages.len()))]
    async fn set_flags(
        &self,
        messages: &[MessageId],
        op: &FlagOp,
    ) -> StorageResult<Vec<MessageId>> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }
        let ids_json =
            serde_json::to_string(&messages.iter().map(ToString::to_string).collect::<Vec<_>>())?;
        let tx = &self.db.cache.write;
        let rows = sqlx::query_as!(
            FlagRow,
            "SELECT id, flags FROM messages WHERE id IN (SELECT value FROM json_each(?1))",
            ids_json
        )
        .fetch_all(tx)
        .await?;
        let mut affected = Vec::with_capacity(rows.len());
        for FlagRow { id, flags } in rows {
            let mut current = parse_flags(&flags);
            match op {
                FlagOp::Set(next) => current = next.clone(),
                FlagOp::Add(add) => {
                    for f in add {
                        if !current.contains(f) {
                            current.push(f.clone());
                        }
                    }
                }
                FlagOp::Remove(rm) => current.retain(|f| !rm.contains(f)),
            }
            let next_json = flags_json(&current);
            sqlx::query!(
                "UPDATE messages SET flags = ?2,
                    is_read = ?3, is_flagged = ?4, is_answered = ?5,
                    indexed_at = NULL
                 WHERE id = ?1",
                id,
                next_json,
                i32::from(current.contains(&Flag::Seen)),
                i32::from(current.contains(&Flag::Flagged)),
                i32::from(current.contains(&Flag::Answered))
            )
            .execute(tx)
            .await?;
            if let Some(mid) = MessageId::parse(&id) {
                affected.push(mid);
            }
        }
        Ok(affected)
    }

    /// Deletes messages locally (parts cascade; triggers adjust refcounts).
    #[instrument(skip_all, fields(uid = messages.len()))]
    async fn delete_messages(&self, messages: &[MessageId]) -> StorageResult<u64> {
        if messages.is_empty() {
            return Ok(0);
        }
        let ids_json =
            serde_json::to_string(&messages.iter().map(ToString::to_string).collect::<Vec<_>>())?;
        let result =
            sqlx::query("DELETE FROM messages WHERE id IN (SELECT value FROM json_each(?1))")
                .bind(&ids_json)
                .execute(&self.db.cache.write)
                .await?;
        Ok(result.rows_affected())
    }

    /// Re-assigns messages to another folder with fresh UIDs.
    #[instrument(skip_all, fields(uid = moves.len()))]
    async fn move_messages(&self, moves: &[(MessageId, FolderId, u32)]) -> StorageResult<u64> {
        let mut count = 0u64;
        let tx = &self.db.cache.write;
        for (id, folder, uid) in moves {
            let result = sqlx::query!(
                "UPDATE messages SET folder_id = ?2, uid = ?3, indexed_at = NULL WHERE id = ?1",
                id.to_string(),
                folder.to_string(),
                i64::from(*uid)
            )
            .execute(tx)
            .await?;
            count += result.rows_affected();
        }
        Ok(count)
    }

    /// Purges a folder's message rows (UIDVALIDITY reconciliation step 1).
    #[instrument(skip_all, fields(folder = %folder))]
    async fn purge_folder(&self, folder: FolderId) -> StorageResult<u64> {
        let result = sqlx::query!(
            "DELETE FROM messages WHERE folder_id = ?1",
            folder.to_string()
        )
        .execute(&self.db.cache.write)
        .await?;
        Ok(result.rows_affected())
    }

    /// Messages pending index (catch-up cursor), re-parsed for body text.
    #[instrument(skip_all)]
    async fn pending_index(&self, limit: u64) -> StorageResult<Vec<PendingDoc>> {
        let rows = sqlx::query_as!(
            MsgRow,
            "SELECT id, folder_id, uid, internal_date, flags, message_id, in_reply_to, subject,
                    from_addr, to_addrs, cc_addrs, size, is_read, is_flagged, is_answered,
                    has_attachments, thread_id
             FROM messages WHERE indexed_at IS NULL ORDER BY internal_date DESC LIMIT ?1",
            i64::try_from(limit).unwrap_or(100)
        )
        .fetch_all(&self.db.cache.read)
        .await?;
        let mut docs = Vec::with_capacity(rows.len());
        for row in rows {
            let summary = row_to_summary(row)?;
            let account: Option<(String,)> =
                sqlx::query_as("SELECT f.account_id FROM folders f WHERE f.id = ?1")
                    .bind(summary.folder.to_string())
                    .fetch_optional(&self.db.cache.read)
                    .await?;
            let account = match account {
                Some((a,)) => parse_id::<kestrel_core::ids::AccountId>(&a)?,
                None => continue, // folder vanished mid-catch-up
            };
            let raw = sqlx::query_as!(
                RawBlobRow,
                "SELECT raw_blob FROM messages WHERE id = ?1",
                summary.id.to_string()
            )
            .fetch_one(&self.db.cache.read)
            .await?;
            let raw_blob = raw.raw_blob;
            let (body_text, attachment_names) =
                match raw_blob.as_deref().and_then(BlobHash::parse_hex) {
                    Some(hash) => match self.blobs.read(&hash).await {
                        Ok(bytes) => match StalwartParser::parse(&bytes) {
                            Ok(parsed) => {
                                let names = parsed
                                    .parts
                                    .iter()
                                    .filter(|p| p.disposition.as_deref() == Some("attachment"))
                                    .filter_map(|p| p.filename.clone())
                                    .collect::<Vec<_>>();
                                (kestrel_core::mime::text_for_index(&parsed), names)
                            }
                            Err(_) => (String::new(), Vec::new()),
                        },
                        Err(_) => (String::new(), Vec::new()),
                    },
                    None => (String::new(), Vec::new()),
                };
            docs.push(PendingDoc {
                id: summary.id,
                folder: summary.folder,
                account,
                summary,
                body_text,
                attachment_names,
            });
        }
        Ok(docs)
    }

    /// Full-feed variant for rebuild (ignores the `indexed_at` cursor).
    #[instrument(skip_all)]
    async fn feed_all_for_index(&self, limit: u64, offset: u64) -> StorageResult<Vec<PendingDoc>> {
        let rows = sqlx::query_as!(
            MsgRow,
            "SELECT id, folder_id, uid, internal_date, flags, message_id, in_reply_to, subject,
                    from_addr, to_addrs, cc_addrs, size, is_read, is_flagged, is_answered,
                    has_attachments, thread_id
             FROM messages ORDER BY internal_date DESC LIMIT ?1 OFFSET ?2",
            i64::try_from(limit).unwrap_or(100),
            i64::try_from(offset).unwrap_or(0)
        )
        .fetch_all(&self.db.cache.read)
        .await?;
        let mut docs = Vec::with_capacity(rows.len());
        for row in rows {
            let summary = row_to_summary(row)?;
            let account: Option<(String,)> =
                sqlx::query_as("SELECT f.account_id FROM folders f WHERE f.id = ?1")
                    .bind(summary.folder.to_string())
                    .fetch_optional(&self.db.cache.read)
                    .await?;
            let account = match account {
                Some((a,)) => parse_id::<kestrel_core::ids::AccountId>(&a)?,
                None => continue,
            };
            let raw = sqlx::query_as!(
                RawBlobRow,
                "SELECT raw_blob FROM messages WHERE id = ?1",
                summary.id.to_string()
            )
            .fetch_one(&self.db.cache.read)
            .await?;
            let raw_blob = raw.raw_blob;
            let (body_text, attachment_names) =
                match raw_blob.as_deref().and_then(BlobHash::parse_hex) {
                    Some(hash) => match self.blobs.read(&hash).await {
                        Ok(bytes) => match StalwartParser::parse(&bytes) {
                            Ok(parsed) => {
                                let names = parsed
                                    .parts
                                    .iter()
                                    .filter(|p| p.disposition.as_deref() == Some("attachment"))
                                    .filter_map(|p| p.filename.clone())
                                    .collect::<Vec<_>>();
                                (kestrel_core::mime::text_for_index(&parsed), names)
                            }
                            Err(_) => (String::new(), Vec::new()),
                        },
                        Err(_) => (String::new(), Vec::new()),
                    },
                    None => (String::new(), Vec::new()),
                };
            docs.push(PendingDoc {
                id: summary.id,
                folder: summary.folder,
                account,
                summary,
                body_text,
                attachment_names,
            });
        }
        Ok(docs)
    }

    /// Marks messages indexed.
    #[instrument(skip_all, fields(uid = ids.len()))]
    async fn mark_indexed(&self, ids: &[MessageId], at: i64) -> StorageResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let ids_json =
            serde_json::to_string(&ids.iter().map(ToString::to_string).collect::<Vec<_>>())?;
        sqlx::query(
            "UPDATE messages SET indexed_at = ?2 WHERE id IN (SELECT value FROM json_each(?1))",
        )
        .bind(&ids_json)
        .bind(at)
        .execute(&self.db.cache.write)
        .await?;
        Ok(())
    }
}

/// Extracts `<a href>` links from HTML and classifies them (M19/M20).
fn collect_links(view: &mut MessageView, html: &str, id: MessageId) {
    let lower = html.to_lowercase();
    let mut start = 0usize;
    while let Some(pos) = lower[start..].find("href=\"") {
        let val_start = start + pos + 6;
        let Some(end_rel) = lower[val_start..].find('"') else {
            break;
        };
        let href = &html[val_start..val_start + end_rel];
        if classify_link(href, "") != LinkRisk::Safe {
            view.suspicious_links.push(SuspiciousLinkInfo {
                href: href.to_owned(),
                reason: match classify_link(href, "") {
                    LinkRisk::Punycode => SuspiciousLinkReason::Punycode,
                    LinkRisk::MixedScript => SuspiciousLinkReason::MixedScript,
                    LinkRisk::DisplayMismatch => SuspiciousLinkReason::DisplayMismatch,
                    LinkRisk::Safe => continue,
                },
            });
            tracing::debug!(message = %id, href, "suspicious link");
        }
        start = val_start + end_rel;
    }
}
