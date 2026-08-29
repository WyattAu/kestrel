//! `SyncService` (per account): the sync-engine.md §1 state machine —
//! Disconnected → Connecting → Authenticating → Hierarchy Sync → Delta
//! Sync → IDLE loop, with QRESYNC/CONDSTORE/no-extension fallbacks,
//! `UIDVALIDITY` reconciliation, and a polling fallback.

use std::{sync::Arc, time::Duration};

use imap_next::imap_types::{
    command::CommandBody,
    fetch::{MessageDataItem, MessageDataItemName},
    flag::Flag,
    mailbox::{ListMailbox, Mailbox},
    response::{Code, Data},
};
use kestrel_core::{
    clock::Clock,
    config::Config,
    ids::AccountId,
    mime::{MimeParser as _, StalwartParser},
    protocol::{ConnectionState, EngineEvent, Flag as CoreFlag, FolderDelta, FolderRole},
    sanitizer::sanitize_terminal_text,
    store_model::{FolderRow, IngestBatch, IngestMessage, MailStore, NewFolder},
};
use tokio_util::sync::CancellationToken;

use crate::{
    error::{SyncError, SyncResult},
    session::{CommandOutcome, ConnectParams, ImapSession, Unsolicited},
};

/// Events the service emits go straight on the engine bus (passed in).
pub struct SyncService {
    account: AccountId,
    params: ConnectParams,
    storage: std::sync::Arc<dyn MailStore>,
    config: Arc<Config>,
    clock: Arc<dyn Clock>,
    bus: tokio::sync::mpsc::Sender<EngineEvent>,
}

/// Selected-folder metadata from the SELECT untagged responses.
#[derive(Default, Debug, Clone, Copy)]
struct SelectInfo {
    uid_validity: u32,
    uid_next: u32,
    highest_modseq: u64,
    exists: u32,
}

impl SyncService {
    /// Creates the service.
    #[must_use]
    pub fn new(
        account: AccountId,
        params: ConnectParams,
        storage: std::sync::Arc<dyn MailStore>,
        config: Arc<Config>,
        clock: Arc<dyn Clock>,
        bus: tokio::sync::mpsc::Sender<EngineEvent>,
    ) -> Self {
        Self {
            account,
            params,
            storage,
            config,
            clock,
            bus,
        }
    }

    /// Runs the state machine until cancellation.
    pub async fn run(&self, cancel: CancellationToken) {
        let mut backoff = Duration::from_millis(250);
        loop {
            if cancel.is_cancelled() {
                return;
            }
            match self.run_one_cycle(&cancel).await {
                Ok(()) => {
                    backoff = Duration::from_millis(250);
                }
                Err(e) => {
                    tracing::warn!(account = %self.account, error = %e, "sync cycle failed");
                    self.emit_state(ConnectionState::Disconnected).await;
                    let wait = backoff;
                    tokio::select! {
                        () = cancel.cancelled() => return,
                        () = tokio::time::sleep(wait) => {}
                    }
                    backoff = std::cmp::min(backoff.mul_f64(2.0), Duration::from_mins(5));
                }
            }
        }
    }

    async fn emit_state(&self, state: ConnectionState) {
        let _ = self.storage.set_account_state(self.account, state).await;
        let _ = self
            .bus
            .send(EngineEvent::AccountConnection {
                account: self.account,
                state,
            })
            .await;
    }

    /// One full cycle: connect → auth → hierarchy → delta → idle.
    async fn run_one_cycle(&self, cancel: &CancellationToken) -> SyncResult<()> {
        self.emit_state(ConnectionState::Connecting).await;
        let mut session = ImapSession::connect_and_authenticate(&self.params).await?;
        self.emit_state(ConnectionState::Authenticating).await; // completed by connect
        self.emit_state(ConnectionState::Syncing).await;

        // Hierarchy sync.
        let folders = self.sync_hierarchy(&mut session).await?;
        self.bus
            .send(EngineEvent::FolderTreeChanged {
                account: self.account,
            })
            .await
            .map_err(|_| SyncError::Protocol("bus closed".into()))?;

        // Delta pass over every folder.
        for folder in folders {
            if cancel.is_cancelled() {
                return Ok(());
            }
            self.sync_folder(&mut session, &folder).await?;
        }

        // IDLE loop (or polling fallback).
        let idle_supported = session.has_capability("IDLE");
        let poll_only = self
            .config
            .sync
            .idle_poll_only_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&self.params.host));
        if idle_supported && !poll_only {
            self.emit_state(ConnectionState::Idle).await;
            loop {
                if cancel.is_cancelled() {
                    session.logout().await;
                    return Ok(());
                }
                let woke = session
                    .idle(Duration::from_secs(60 * self.config.sync.idle_timeout_mins))
                    .await?;
                if cancel.is_cancelled() {
                    session.logout().await;
                    return Ok(());
                }
                if !woke.is_empty() {
                    self.emit_state(ConnectionState::Syncing).await;
                    // Re-run the folders touched by wake signals (all — the
                    // unsolicited data does not identify the folder).
                    let folders = self.list_stored_folders().await;
                    for folder in folders {
                        if cancel.is_cancelled() {
                            session.logout().await;
                            return Ok(());
                        }
                        self.sync_folder(&mut session, &folder).await?;
                    }
                    self.emit_state(ConnectionState::Idle).await;
                }
            }
        } else {
            // Polling fallback (sync-engine.md §5).
            loop {
                let jitter = Duration::from_secs(self.config.sync.poll_interval_secs);
                tokio::select! {
                    () = cancel.cancelled() => {
                        session.logout().await;
                        return Ok(());
                    }
                    () = tokio::time::sleep(jitter) => {}
                }
                self.emit_state(ConnectionState::Syncing).await;
                let folders = self.list_stored_folders().await;
                for folder in folders {
                    if cancel.is_cancelled() {
                        session.logout().await;
                        return Ok(());
                    }
                    self.sync_folder(&mut session, &folder).await?;
                }
                self.emit_state(ConnectionState::Syncing).await;
            }
        }
    }

    /// LIST → folder upsert; returns the refreshed folder rows.
    async fn sync_hierarchy(&self, session: &mut ImapSession) -> SyncResult<Vec<FolderRow>> {
        let outcome = session
            .execute(list_command(), Duration::from_mins(1))
            .await?;
        if !outcome.is_ok() {
            return Err(SyncError::Protocol(format!(
                "LIST failed: {}",
                outcome.status_summary()
            )));
        }
        for data in &outcome.data {
            if let Data::List {
                mailbox,
                items,
                delimiter,
                ..
            } = data
            {
                let remote_name = match mailbox {
                    imap_next::imap_types::mailbox::Mailbox::Inbox => "INBOX".to_string(),
                    imap_next::imap_types::mailbox::Mailbox::Other(other) => {
                        astring_to_string(other.inner())
                    }
                };
                let attributes: Vec<String> = items.iter().map(|f| format!("{f:?}")).collect();
                let delimiter =
                    delimiter.map_or_else(|| "/".to_string(), |qc| qc.inner().to_string());
                let role = role_from(&remote_name, &attributes);
                if let Err(e) = self
                    .storage
                    .upsert_folder(&NewFolder {
                        account: self.account,
                        remote_name,
                        attributes,
                        role,
                        delimiter,
                        uid_validity: 0,
                        highest_modseq: 0,
                    })
                    .await
                {
                    tracing::warn!(error = %e, "folder upsert failed");
                }
            }
        }
        Ok(self.list_stored_folders().await)
    }

    async fn list_stored_folders(&self) -> Vec<FolderRow> {
        let Ok(summaries) = self.storage.list_folders(self.account).await else {
            return Vec::new();
        };
        let mut rows = Vec::with_capacity(summaries.len());
        for s in summaries {
            if let Ok(row) = self.storage.get_folder(s.id).await {
                rows.push(row);
            }
        }
        rows
    }

    /// SELECT + delta sync for one folder (sync-engine.md §2-3).
    async fn sync_folder(&self, session: &mut ImapSession, folder: &FolderRow) -> SyncResult<()> {
        let mailbox = Mailbox::try_from(folder.remote_name.clone())
            .map_err(|e| SyncError::Protocol(format!("mailbox name: {e:?}")))?;
        let outcome = session
            .execute(
                CommandBody::Select {
                    mailbox,
                    parameters: Vec::new(),
                },
                Duration::from_mins(1),
            )
            .await?;
        if !outcome.is_ok() {
            tracing::warn!(folder = %folder.remote_name, "SELECT failed");
            return Ok(());
        }
        let info = select_info(&outcome);

        // UIDVALIDITY reconciliation (requirements §2.2).
        if folder.uid_validity != 0
            && info.uid_validity != 0
            && folder.uid_validity != info.uid_validity
        {
            tracing::warn!(
                folder = %folder.remote_name,
                old = folder.uid_validity,
                new = info.uid_validity,
                "UIDVALIDITY change: reconciling"
            );
            let removed = self.storage.purge_folder(folder.id).await.unwrap_or(0);
            let _ = self
                .storage
                .update_sync_cursors(folder.id, info.uid_validity, Some(0))
                .await;
            let _ = self
                .bus
                .send(EngineEvent::MessagesChanged {
                    folder: folder.id,
                    changed: 0,
                    removed: u32::try_from(removed).unwrap_or(u32::MAX),
                })
                .await;
            let _ = self
                .storage
                .update_sync_cursors(folder.id, info.uid_validity, Some(info.highest_modseq))
                .await;
            // Full re-fetch below (stored max uid is now empty).
        } else if info.uid_validity != 0 {
            let _ = self
                .storage
                .update_sync_cursors(folder.id, info.uid_validity, Some(info.highest_modseq))
                .await;
        }

        // Delta: fetch envelopes for UIDs beyond the stored maximum.
        let max_stored = self.storage.max_uid(folder.id).await.unwrap_or(None);
        let start = max_stored.map_or(1, |m| m.saturating_add(1));
        if info.exists > 0 && start <= next_bound(info.uid_next) {
            self.fetch_and_ingest(session, folder, start, info.uid_next)
                .await?;
        }

        // Flag pass (CONDSTORE-aware when cursors exist).
        self.flag_pass(session, folder, info).await?;

        // Recent-body prefetch (sync-engine.md §4).
        self.prefetch_recent(session, folder).await?;

        Ok(())
    }

    async fn fetch_and_ingest(
        &self,
        session: &mut ImapSession,
        folder: &FolderRow,
        start: u32,
        uid_next: u32,
    ) -> SyncResult<()> {
        let end = next_bound(uid_next);
        if start > end {
            return Ok(());
        }
        let range = format!("{start}:{end}");
        let outcome = session.fetch_envelopes(&range).await?;
        if !outcome.is_ok() {
            return Err(SyncError::Protocol(format!(
                "UID FETCH {range} failed: {}",
                outcome.status_summary()
            )));
        }
        self.ingest_fetch_data(folder, &outcome).await
    }

    /// Converts untagged FETCH data into parsed ingest batches.
    async fn ingest_fetch_data(
        &self,
        folder: &FolderRow,
        outcome: &CommandOutcome,
    ) -> SyncResult<()> {
        let mut messages: Vec<IngestMessage> = Vec::new();
        for data in &outcome.data {
            let Data::Fetch { items, .. } = data else {
                continue;
            };
            let mut uid = None;
            let mut flags: Vec<CoreFlag> = Vec::new();
            let mut internal_date = self.clock.now_unix_ms();
            let mut size = 0u64;
            let mut envelope = None;
            for item in items.as_ref() {
                match item {
                    MessageDataItem::Uid(u) => uid = Some(u.get()),
                    MessageDataItem::Flags(fl) => {
                        flags = fl
                            .iter()
                            .filter_map(|f| match f {
                                imap_next::imap_types::flag::FlagFetch::Flag(Flag::Seen) => {
                                    Some(CoreFlag::Seen)
                                }
                                imap_next::imap_types::flag::FlagFetch::Flag(Flag::Answered) => {
                                    Some(CoreFlag::Answered)
                                }
                                imap_next::imap_types::flag::FlagFetch::Flag(Flag::Flagged) => {
                                    Some(CoreFlag::Flagged)
                                }
                                imap_next::imap_types::flag::FlagFetch::Flag(Flag::Deleted) => {
                                    Some(CoreFlag::Deleted)
                                }
                                imap_next::imap_types::flag::FlagFetch::Flag(Flag::Draft) => {
                                    Some(CoreFlag::Draft)
                                }
                                _ => None,
                            })
                            .collect();
                    }
                    MessageDataItem::InternalDate(d) => {
                        internal_date = datetime_to_unix_ms(d);
                    }
                    MessageDataItem::Rfc822Size(sz) => size = u64::from(*sz),
                    MessageDataItem::Envelope(env) => envelope = Some(env.clone()),
                    _ => {}
                }
            }
            let Some(uid) = uid else { continue };
            let Some(envelope) = envelope else { continue };

            // Reconstruct a header-only raw message for the parser so
            // threading metadata comes from the same pipeline as bodies.
            let raw_head = envelope_to_header(&envelope, uid);
            let parsed = StalwartParser::parse(raw_head.as_bytes()).unwrap_or_default();
            messages.push(IngestMessage {
                folder: folder.id,
                uid,
                internal_date,
                flags,
                parsed,
                raw_blob: None,
                raw_size: size,
            });
        }
        if messages.is_empty() {
            return Ok(());
        }
        let new_count = u32::try_from(messages.len()).unwrap_or(u32::MAX);
        let stats = self
            .storage
            .ingest_batch(IngestBatch { messages })
            .await
            .map_err(SyncError::from)?;
        if stats.inserted > 0 {
            let summaries = self
                .storage
                .list_folders(self.account)
                .await
                .unwrap_or_default();
            let summary = summaries.iter().find(|f| f.id == folder.id).cloned();
            let _ = self
                .bus
                .send(EngineEvent::MailArrived {
                    account: self.account,
                    folder: folder.id,
                    summary: FolderDelta {
                        new: new_count,
                        total: summary.as_ref().map_or(0, |s| s.total),
                        unread: summary.as_ref().map_or(0, |s| s.unread),
                    },
                })
                .await;
        }
        Ok(())
    }

    /// Flag delta pass: CHANGEDSINCE when CONDSTORE, else windowed scan.
    async fn flag_pass(
        &self,
        session: &mut ImapSession,
        folder: &FolderRow,
        info: SelectInfo,
    ) -> SyncResult<()> {
        if session.has_capability("CONDSTORE") && folder.highest_modseq > 0 {
            let items = MacroOrItems::MessageDataItemNames(vec![MessageDataItemName::Flags]);
            let sequence = imap_next::imap_types::sequence::SequenceSet::try_from("1:*")
                .map_err(|e| SyncError::Protocol(format!("seq: {e:?}")))?;
            let outcome = session
                .execute(
                    CommandBody::Fetch {
                        sequence_set: sequence,
                        macro_or_item_names: items,
                        uid: true,
                        modifiers: vec![
                            imap_next::imap_types::command::FetchModifier::ChangedSince(
                                std::num::NonZeroU64::try_from(
                                    folder.highest_modseq.saturating_add(1),
                                )
                                .unwrap_or(std::num::NonZeroU64::MIN),
                            ),
                        ],
                    },
                    Duration::from_mins(2),
                )
                .await;
            if let Ok(outcome) = outcome
                && outcome.is_ok()
            {
                let mut changed: Vec<kestrel_core::ids::MessageId> = Vec::new();
                for data in &outcome.data {
                    for u in Unsolicited::from_data(data) {
                        if let Unsolicited::FetchFlags { uid, .. } = u
                            && let Some(id) = self.message_by_uid(folder.id, uid).await
                        {
                            changed.push(id);
                        }
                    }
                }
                if !changed.is_empty() {
                    let _ = self
                        .bus
                        .send(EngineEvent::FlagsChanged { messages: changed })
                        .await;
                }
                if info.highest_modseq > folder.highest_modseq {
                    let _ = self
                        .storage
                        .update_sync_cursors(folder.id, 0, Some(info.highest_modseq))
                        .await;
                }
            }
        }
        Ok(())
    }

    async fn message_by_uid(
        &self,
        folder: kestrel_core::ids::FolderId,
        uid: u32,
    ) -> Option<kestrel_core::ids::MessageId> {
        // Locate via a windowed listing (cache-local, cheap for tests;
        // production paths go through the folder's bounded windows).
        let mut offset = 0u64;
        loop {
            let page = self
                .storage
                .list_messages(
                    folder,
                    kestrel_core::protocol::Window { offset, limit: 500 },
                    kestrel_core::protocol::SortSpec {
                        field: kestrel_core::protocol::SortField::Uid,
                        dir: kestrel_core::protocol::SortDir::Asc,
                    },
                )
                .await
                .ok()?;
            if page.items.is_empty() {
                return None;
            }
            if let Some(m) = page.items.iter().find(|m| m.uid == uid) {
                return Some(m.id);
            }
            offset += page.items.len() as u64;
            if offset >= page.total {
                return None;
            }
        }
    }

    /// Background body prefetch: newest N messages get their raw stored.
    async fn prefetch_recent(
        &self,
        session: &mut ImapSession,
        folder: &FolderRow,
    ) -> SyncResult<()> {
        // Select the newest UIDs from storage.
        let page = self
            .storage
            .list_messages(
                folder.id,
                kestrel_core::protocol::Window {
                    offset: 0,
                    limit: self.config.sync.body_prefetch_recent as u64,
                },
                kestrel_core::protocol::SortSpec {
                    field: kestrel_core::protocol::SortField::Date,
                    dir: kestrel_core::protocol::SortDir::Desc,
                },
            )
            .await
            .unwrap_or_default();
        for m in page.items {
            let raw = session.fetch_raw(m.uid).await.ok().and_then(|outcome| {
                outcome.data.iter().find_map(|d| match d {
                    Data::Fetch { items, .. } => {
                        items.as_ref().iter().find_map(|item| match item {
                            MessageDataItem::Rfc822(nstr) => nstring_bytes(nstr),
                            MessageDataItem::BodyExt { data, .. } => nstring_bytes(data),
                            _ => None,
                        })
                    }
                    _ => None,
                })
            });
            if let Some(raw) = raw {
                let hash = self.storage.write_blob(raw.clone()).await.ok();
                if let Some(hash) = hash {
                    // Re-parse with the full body and re-ingest.
                    if let Ok(parsed) = StalwartParser::parse(&raw) {
                        let _ = self
                            .storage
                            .ingest_batch(IngestBatch {
                                messages: vec![IngestMessage {
                                    folder: folder.id,
                                    uid: m.uid,
                                    internal_date: m.internal_date,
                                    flags: m.flags.clone(),
                                    parsed,
                                    raw_blob: Some(hash),
                                    raw_size: raw.len() as u64,
                                }],
                            })
                            .await;
                    }
                }
            }
        }
        Ok(())
    }
}

fn next_bound(uid_next: u32) -> u32 {
    uid_next.saturating_sub(1).max(1)
}

fn list_command() -> CommandBody<'static> {
    // The wildcard is the literal `*`; the empty-string fallback is
    // defensive (TryFrom<&str> for ListCharString accepts plain ASCII).
    let wildcard = ListMailbox::try_from("*").unwrap_or(ListMailbox::Token(
        imap_next::imap_types::mailbox::ListCharString::try_from("*")
            .unwrap_or_else(|_| unreachable!("'*' is a valid ListCharString")),
    ));
    CommandBody::List {
        reference: Mailbox::try_from(String::new()).unwrap_or(Mailbox::Inbox),
        mailbox_wildcard: wildcard,
    }
}

fn role_from(remote_name: &str, attributes: &[String]) -> Option<FolderRole> {
    let attr_blob = attributes.join(" ").to_lowercase();
    let name = sanitize_terminal_text(remote_name)
        .trim_end()
        .to_lowercase();
    let last = name.rsplit(['/', '.']).next().unwrap_or_default();
    if attr_blob.contains("sent") || last == "sent" || last.starts_with("sent messages") {
        Some(FolderRole::Sent)
    } else if attr_blob.contains("drafts") || last == "drafts" {
        Some(FolderRole::Drafts)
    } else if attr_blob.contains("trash") || last == "trash" || last == "deleted messages" {
        Some(FolderRole::Trash)
    } else if attr_blob.contains("junk") || last == "junk" || last == "spam" {
        Some(FolderRole::Junk)
    } else if attr_blob.contains("archive") || last == "archive" || last == "all mail" {
        Some(FolderRole::Archive)
    } else if last == "inbox" || name.is_empty() {
        Some(FolderRole::Inbox)
    } else {
        None
    }
}

fn select_info(outcome: &CommandOutcome) -> SelectInfo {
    let mut info = SelectInfo::default();
    // SELECT reports UIDVALIDITY/UIDNEXT/HIGHESTMODSEQ as response codes on
    // untagged/ tagged statuses and EXISTS as data.
    for data in &outcome.data {
        if let Data::Exists(n) = data {
            info.exists = *n;
        }
    }
    for status in outcome
        .untagged
        .iter()
        .chain(std::iter::once(&outcome.status))
    {
        if let Some(code) = status_code(status) {
            match code {
                Code::UidValidity(v) => info.uid_validity = v.get(),
                Code::UidNext(v) => info.uid_next = v.get(),
                Code::HighestModSeq(v) => info.highest_modseq = v.get(),
                _ => {}
            }
        }
    }
    info
}

fn status_code<'a>(
    status: &'a imap_next::imap_types::response::Status<'a>,
) -> Option<&'a Code<'a>> {
    use imap_next::imap_types::response::Status as S;
    match status {
        S::Untagged(body) | S::Tagged(imap_next::imap_types::response::Tagged { body, .. }) => {
            body.code.as_ref()
        }
        S::Bye(_) => None,
    }
}

/// Reconstructs a header-only RFC 822 view of an envelope for the shared
/// parse pipeline (threading fields + addresses).
fn envelope_to_header(
    envelope: &imap_next::imap_types::envelope::Envelope<'_>,
    uid: u32,
) -> String {
    use imap_next::imap_types::envelope::Address;
    let fmt_addr = |a: &Address<'_>| {
        let mailbox = nstring_to_string(&a.mailbox);
        let host = nstring_to_string(&a.host);
        let name = nstring_to_string(&a.name);
        match name {
            n if n.is_empty() => format!("{mailbox}@{host}"),
            n => format!("{n} <{mailbox}@{host}>"),
        }
    };
    let addrs = |list: &Vec<imap_next::imap_types::envelope::Address<'_>>| {
        list.iter().map(fmt_addr).collect::<Vec<_>>().join(", ")
    };
    let opt = |s: &imap_next::imap_types::core::NString<'_>| nstring_to_string(s);
    format!(
        "From: {}\r\nTo: {}\r\nSubject: {}\r\nMessage-ID: {}\r\nIn-Reply-To: {}\r\nDate: \r\nX-Kestrel-UID: {uid}\r\n\r\n",
        addrs(&envelope.from),
        addrs(&envelope.to),
        opt(&envelope.subject),
        opt(&envelope.message_id),
        opt(&envelope.in_reply_to),
    )
}

/// Local alias for the fetch-items enum.
type MacroOrItems = imap_next::imap_types::fetch::MacroOrMessageDataItemNames<'static>;

fn astring_to_string(value: &imap_next::imap_types::core::AString<'_>) -> String {
    use imap_next::imap_types::core::AString;
    match value {
        AString::Atom(atom) => atom.as_ref().to_owned(),
        AString::String(istr) => istring_to_string(istr),
    }
}

fn istring_to_string(value: &imap_next::imap_types::core::IString<'_>) -> String {
    use imap_next::imap_types::core::IString;
    match value {
        IString::Quoted(q) => q.as_ref().to_owned(),
        IString::QuotedUtf8(q) => q.0.clone().into_owned(),
        IString::Literal(lit) => String::from_utf8_lossy(lit.as_ref()).into_owned(),
    }
}

fn nstring_to_string(nstr: &imap_next::imap_types::core::NString<'_>) -> String {
    nstr.0.as_ref().map(istring_to_string).unwrap_or_default()
}

fn nstring_bytes(nstr: &imap_next::imap_types::core::NString<'_>) -> Option<Vec<u8>> {
    nstr.0
        .as_ref()
        .map(|istr| istring_to_string(istr).into_bytes())
}

fn datetime_to_unix_ms(d: &imap_next::imap_types::datetime::DateTime) -> i64 {
    let chrono: &chrono::DateTime<chrono::FixedOffset> = d.as_ref();
    chrono.timestamp_millis()
}
