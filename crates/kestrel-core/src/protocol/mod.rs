//! The frozen core↔frontend message protocol (`docs/message-protocol.md`).
//!
//! Any change here is ADR-level (ADR 0000). Additive enum variants are minor
//! bumps of [`PROTOCOL_VERSION`]; anything else is major and requires an ADR.

use std::{sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::{
    config::Config,
    error::KestrelError,
    ids::{AccountId, FolderId, MessageId, OutboxId, RequestId},
};

/// Protocol major version, emitted in `EngineStarted`.
pub const PROTOCOL_VERSION: u32 = 1;

/// Which frontend originated a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrontendKind {
    /// The terminal UI.
    Tui,
    /// The desktop GUI.
    Gui,
}

/// Command envelope (`docs/message-protocol.md` §2).
#[derive(Debug)]
pub struct Command {
    /// UUID v7, monotonic; echoed in events where applicable.
    pub id: RequestId,
    /// Originating frontend.
    pub origin: FrontendKind,
    /// Payload.
    pub payload: CommandPayload,
}

/// Command payloads. Fire-and-forget variants carry no oneshot.
///
/// `oneshot::Sender` payloads make this enum large; that is inherent to the
/// in-process protocol (ADR 0004) and bounded by the channel capacity.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum CommandPayload {
    // ---- mailbox navigation & reads ------------------------------------
    /// List configured accounts.
    ListAccounts {
        /// Reply channel.
        reply: oneshot::Sender<Reply>,
    },
    /// List folders of an account.
    ListFolders {
        /// Account whose folders to list.
        account: AccountId,
        /// Reply channel.
        reply: oneshot::Sender<Reply>,
    },
    /// List a window of messages in a folder.
    ListMessages {
        /// Folder to list.
        folder: FolderId,
        /// Window into the sorted result.
        window: Window,
        /// Sort specification.
        sort: SortSpec,
        /// Reply channel.
        reply: oneshot::Sender<Reply>,
    },
    /// Fetch one message with resolved body.
    GetMessage {
        /// Message to fetch.
        message: MessageId,
        /// Body preference for lazy fetch.
        body: BodyPreference,
        /// Reply channel.
        reply: oneshot::Sender<Reply>,
    },
    /// Full-text search.
    Search {
        /// Structured query.
        query: SearchQuery,
        /// Reply channel.
        reply: oneshot::Sender<Reply>,
    },

    // ---- state mutations (flag changes flow through sync engine) ---------
    /// Apply a flag operation to messages.
    SetFlags {
        /// Target messages.
        messages: Vec<MessageId>,
        /// Operation.
        flags: FlagOp,
        /// Reply channel.
        reply: oneshot::Sender<Reply>,
    },
    /// Move messages to another folder.
    MoveMessages {
        /// Target messages.
        messages: Vec<MessageId>,
        /// Destination.
        to: FolderId,
        /// Reply channel.
        reply: oneshot::Sender<Reply>,
    },
    /// Delete messages (optionally expunge).
    DeleteMessages {
        /// Target messages.
        messages: Vec<MessageId>,
        /// `true` = expunge immediately; `false` = move to trash.
        expunge: bool,
        /// Reply channel.
        reply: oneshot::Sender<Reply>,
    },

    // ---- composition ------------------------------------------------------
    /// Submit a draft to the outbox.
    ComposeSubmit {
        /// Draft content.
        draft: Draft,
        /// Reply channel.
        reply: oneshot::Sender<Reply>,
    },
    /// Cancel a queued outbox entry.
    CancelOutbox {
        /// Entry to cancel.
        id: OutboxId,
        /// Reply channel.
        reply: oneshot::Sender<Reply>,
    },

    // ---- sync control -------------------------------------------------------
    /// Trigger a sync (fire-and-forget).
    TriggerSync {
        /// Account to sync.
        account: AccountId,
        /// What kind of sync.
        kind: SyncKind,
    },
    /// Enter offline mode.
    GoOffline,
    /// Leave offline mode.
    GoOnline,
    /// Re-fetch authoritative state after event-stream lag.
    ResyncState {
        /// Reply channel.
        reply: oneshot::Sender<Reply>,
    },

    // ---- config & lifecycle ---------------------------------------------------
    /// Add a new mail account (onboarding). Credentials are stored in the
    /// keyring (never in config/SQLite — threat model §4.8).
    AddAccount {
        /// Full server configuration.
        config: crate::provider::AccountConfig,
        /// Password (for "password" auth kind); zeroized after storage.
        password: crate::secrets::SecretString,
        /// Reply channel.
        reply: oneshot::Sender<Reply>,
    },
    /// Test connection to the configured servers (without storing).
    TestConnection {
        /// Configuration to probe.
        config: crate::provider::AccountConfig,
        /// Password for the probe.
        password: crate::secrets::SecretString,
        /// Reply channel.
        reply: oneshot::Sender<Reply>,
    },
    /// Remove an account and all its local data.
    RemoveAccount {
        /// Account to remove.
        account: AccountId,
        /// Reply channel.
        reply: oneshot::Sender<Reply>,
    },
    /// New config snapshot (from the config watcher).
    ConfigUpdated {
        /// Immutable snapshot.
        snapshot: Arc<Config>,
    },
    /// Shut the engine down; `drain` flushes the outbox first (bounded ≤ 5 s).
    Shutdown {
        /// Whether to drain queues before stopping.
        drain: bool,
    },
}

/// Replies to request-style commands (`docs/message-protocol.md` §2).
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Reply {
    /// Account list.
    Accounts(Vec<AccountSummary>),
    /// Folder list.
    Folders(Vec<FolderSummary>),
    /// Windowed message page plus total count.
    Messages(MessagePage),
    /// One message with resolved body.
    Message(MessageView),
    /// Search hits.
    SearchResults(Vec<SearchHit>),
    /// Queued/applied; follow-up events will follow.
    Accepted,
    /// Typed failure (ADR 0007 taxonomy payload).
    Err(KestrelError),
}

impl Reply {
    /// Convenience: an error reply.
    #[must_use]
    pub fn err(err: KestrelError) -> Self {
        Self::Err(err)
    }
}

/// Events broadcast by the engine (`docs/message-protocol.md` §3).
///
/// Events are hints, never source of truth: authoritative state comes from
/// command replies. Events are `Clone`; the bus is a bounded broadcast
/// channel (capacity 1024).
#[derive(Clone, Debug)]
pub enum EngineEvent {
    // ---- lifecycle ----
    /// Engine started; protocol version included.
    EngineStarted {
        /// [`PROTOCOL_VERSION`] of the running engine.
        version: u32,
        /// Accounts known at startup.
        accounts: Vec<AccountSummary>,
    },
    /// A service degraded and will restart (never silent).
    ServiceDegraded {
        /// Which service.
        service: ServiceId,
        /// What went wrong.
        error: KestrelError,
        /// Restart delay.
        restart_in: Duration,
    },
    /// Progress through ordered shutdown.
    EngineShutdownProgress {
        /// Current stage.
        stage: ShutdownStage,
    },

    // ---- connectivity ----
    /// Connection state machine transition.
    AccountConnection {
        /// Account.
        account: AccountId,
        /// New state.
        state: ConnectionState,
    },

    // ---- mailbox changes ----
    /// New mail arrived.
    MailArrived {
        /// Account.
        account: AccountId,
        /// Folder.
        folder: FolderId,
        /// Delta summary.
        summary: FolderDelta,
    },
    /// Messages changed/removed in a folder.
    MessagesChanged {
        /// Folder.
        folder: FolderId,
        /// Changed count.
        changed: u32,
        /// Removed count.
        removed: u32,
    },
    /// Flags changed on messages.
    FlagsChanged {
        /// Affected messages.
        messages: Vec<MessageId>,
    },
    /// Folder tree changed (LIST result differs).
    FolderTreeChanged {
        /// Account.
        account: AccountId,
    },

    // ---- composition ----
    /// Draft accepted into the outbox.
    OutboxEnqueued {
        /// Outbox entry.
        id: OutboxId,
    },
    /// A send attempt failed; retry scheduled.
    OutboxRetry {
        /// Outbox entry.
        id: OutboxId,
        /// Attempt number (1-based).
        attempt: u32,
        /// Delay until next attempt.
        next_in: Duration,
        /// Last error summary (no secrets; ADR 0008).
        last_error: String,
    },
    /// Message sent and filed to Sent.
    MailSent {
        /// Outbox entry.
        id: OutboxId,
        /// Resulting message id.
        message: MessageId,
    },
    /// Sending failed permanently.
    MailFailed {
        /// Outbox entry.
        id: OutboxId,
        /// Failure.
        error: KestrelError,
        /// `true` = will not be retried.
        permanent: bool,
    },

    // ---- index & search ----
    /// Indexing progress.
    IndexProgress {
        /// Account.
        account: AccountId,
        /// Documents indexed so far.
        indexed: u64,
        /// Total to index.
        total: u64,
    },

    // ---- security ----
    /// Remote content was blocked in a rendered message.
    RemoteContentBlocked {
        /// Message.
        message: MessageId,
        /// Number of blocked items.
        count: u32,
    },
    /// A suspicious link was detected (punycode/homograph/mismatch).
    SuspiciousLink {
        /// Message containing the link.
        message: MessageId,
        /// The target href.
        href: String,
    },

    // ---- protocol upkeep ----
    /// New config snapshot published (ADR 0006).
    ConfigUpdated {
        /// Immutable snapshot.
        snapshot: Arc<Config>,
    },
    /// A receiver lagged; resync state via `Command::ResyncState`.
    EventStreamLagged {
        /// Missed event count.
        missed: u64,
    },
}

/// Account connection state (sync-engine.md §1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionState {
    /// No connection attempt active.
    Disconnected,
    /// TCP/TLS handshake in progress.
    Connecting,
    /// Credentials being exchanged.
    Authenticating,
    /// Hierarchy/delta sync running.
    Syncing,
    /// IDLE loop waiting for pushes.
    Idle,
    /// User-requested offline mode.
    OfflineMode,
}

/// Identifies a supervised service (ADR 0004).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServiceId {
    /// Metadata/index/blob storage service.
    Storage,
    /// Tantivy writer service.
    Index,
    /// Query-side search service.
    Search,
    /// Outbox queue + SMTP flush.
    Outbox,
    /// Credential service (keyring/OAuth).
    Credentials,
    /// Config watcher.
    Config,
    /// Per-account sync service.
    Sync(AccountId),
}

impl std::fmt::Display for ServiceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage => write!(f, "storage"),
            Self::Index => write!(f, "index"),
            Self::Search => write!(f, "search"),
            Self::Outbox => write!(f, "outbox"),
            Self::Credentials => write!(f, "credentials"),
            Self::Config => write!(f, "config"),
            Self::Sync(a) => write!(f, "sync/{a}"),
        }
    }
}

/// Ordered shutdown stages (architecture §3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownStage {
    /// Frontends detaching.
    DetachFrontends,
    /// Services cancelling.
    CancelServices,
    /// Outbox final bounded flush.
    FlushOutbox,
    /// Storage checkpoint/commit.
    StorageCheckpoint,
    /// Shutdown complete.
    Done,
}

/// Window into a sorted result set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// Zero-based index of the first row.
    pub offset: u64,
    /// Maximum rows returned.
    pub limit: u64,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: 50,
        }
    }
}

/// Sort specification for message listings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortSpec {
    /// Field to sort by.
    pub field: SortField,
    /// Direction.
    pub dir: SortDir,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self {
            field: SortField::Date,
            dir: SortDir::Desc,
        }
    }
}

/// Sortable message fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortField {
    /// `internal_date`.
    Date,
    /// Subject text.
    Subject,
    /// First from-address.
    Sender,
    /// IMAP UID.
    Uid,
}

/// Sort direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortDir {
    /// Ascending.
    Asc,
    /// Descending.
    Desc,
}

/// Structured search query; all fields are AND-combined.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Free-text terms (subject, body, addresses).
    pub text: Option<String>,
    /// Restrict to sender address (tokenized or exact).
    pub from: Option<String>,
    /// Restrict to recipient.
    pub to: Option<String>,
    /// Restrict to subject.
    pub subject: Option<String>,
    /// Messages received at/after this unix-ms timestamp.
    pub since: Option<i64>,
    /// Messages received before this unix-ms timestamp.
    pub until: Option<i64>,
    /// Restrict to a folder.
    pub folder: Option<FolderId>,
    /// Restrict to an account.
    pub account: Option<AccountId>,
    /// Only messages with attachments.
    pub has_attachment: bool,
    /// Maximum hits returned.
    pub limit: Option<u64>,
}

impl SearchQuery {
    /// `true` when no constraint is set (matches everything, bounded by
    /// `limit`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
            || (self.text.is_none()
                && self.from.is_none()
                && self.to.is_none()
                && self.subject.is_none()
                && self.since.is_none()
                && self.until.is_none()
                && self.folder.is_none()
                && self.account.is_none()
                && !self.has_attachment)
    }
}

/// One search hit: message summary plus snippet.
#[derive(Clone, Debug)]
pub struct SearchHit {
    /// Message metadata.
    pub message: MessageSummary,
    /// Highlighting snippet, if available.
    pub snippet: Option<String>,
}

/// IMAP-style flag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Flag {
    /// `\Seen`
    Seen,
    /// `\Answered`
    Answered,
    /// `\Flagged`
    Flagged,
    /// `\Deleted`
    Deleted,
    /// `\Draft`
    Draft,
    /// Custom keyword.
    Custom(String),
}

/// Flag mutation applied via `SetFlags`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlagOp {
    /// Replace the whole flag set.
    Set(Vec<Flag>),
    /// Add flags.
    Add(Vec<Flag>),
    /// Remove flags.
    Remove(Vec<Flag>),
}

/// Body fetch preference for `GetMessage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyPreference {
    /// Metadata + parts only (no lazy network fetch).
    MetadataOnly,
    /// Fetch raw body if missing (blocking-ish; UI shows progress).
    Full,
}

/// What kind of sync `TriggerSync` requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncKind {
    /// Full re-sync (hierarchy + all folders).
    Full,
    /// Delta pass only.
    Delta,
    /// Re-fetch a specific folder (post-`UIDVALIDITY`).
    Folder(FolderId),
}

/// Account summary for listings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSummary {
    /// Account id.
    pub id: AccountId,
    /// Display name.
    pub name: String,
    /// Primary email address.
    pub email: String,
    /// Provider family.
    pub provider: Provider,
    /// Mail protocol.
    pub protocol: MailProtocol,
    /// Current connection state.
    pub state: ConnectionState,
}

/// Provider preset families (requirements §2.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// No preset.
    Generic,
    /// Google Workspace.
    Gmail,
    /// Microsoft 365 / Outlook.
    Outlook,
    /// Fastmail.
    Fastmail,
    /// JMAP-native provider.
    Jmap,
}

/// Mail protocol of an account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailProtocol {
    /// IMAP (RFC 3501/9051).
    Imap,
    /// JMAP (RFC 8620/8621).
    Jmap,
}

/// Folder summary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderSummary {
    /// Folder id.
    pub id: FolderId,
    /// Owning account.
    pub account: AccountId,
    /// Server-side name (e.g. `INBOX/Sent`).
    pub remote_name: String,
    /// Canonical role, if recognized.
    pub role: Option<FolderRole>,
    /// Hierarchy delimiter.
    pub delimiter: String,
    /// Unread count (server `\Seen` inverse), maintained locally.
    pub unread: u64,
    /// Total messages, maintained locally.
    pub total: u64,
}

/// Recognized folder roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderRole {
    /// Inbox.
    Inbox,
    /// Sent mail.
    Sent,
    /// Drafts.
    Drafts,
    /// Trash.
    Trash,
    /// Archive.
    Archive,
    /// Junk/spam.
    Junk,
}

/// One page of messages plus the total result count.
#[derive(Clone, Debug, Default)]
pub struct MessagePage {
    /// Rows in this window.
    pub items: Vec<MessageSummary>,
    /// Total rows matching the query (before windowing).
    pub total: u64,
}

/// Message metadata as shown in listings and stored in `SQLite`.
// clippy::struct_excessive_bools: denormalized flag shortcuts mirrored from
// the schema (docs/schema.md §3.2); a bitflags type would break serde
// compatibility with the stored JSON flags.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageSummary {
    /// Message id.
    pub id: MessageId,
    /// Owning folder.
    pub folder: FolderId,
    /// IMAP UID.
    pub uid: u32,
    /// `internal_date`, unix ms.
    pub internal_date: i64,
    /// Server flags.
    pub flags: Vec<Flag>,
    /// Normalized `Message-ID` (angle brackets stripped).
    pub message_id: Option<String>,
    /// `In-Reply-To` message id.
    pub in_reply_to: Option<String>,
    /// Subject.
    pub subject: Option<String>,
    /// From addresses (canonical JSON in storage).
    pub from: Vec<Address>,
    /// To addresses.
    pub to: Vec<Address>,
    /// Cc addresses.
    pub cc: Vec<Address>,
    /// Raw size in bytes.
    pub size: u64,
    /// `\Seen` shortcut.
    pub is_read: bool,
    /// `\Flagged` shortcut.
    pub is_flagged: bool,
    /// `\Answered` shortcut.
    pub is_answered: bool,
    /// Has at least one attachment-disposition part.
    pub has_attachments: bool,
    /// Thread assignment.
    pub thread: ThreadIdLite,
}

/// Thread reference in summaries: engine issues these; frontends treat them
/// as opaque grouping keys (serialized as the storage TEXT key).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadIdLite {
    /// Storage key of the thread root.
    pub key: String,
}

/// A resolved message body view (`Reply::Message`).
#[derive(Clone, Debug)]
pub struct MessageView {
    /// Summary metadata.
    pub summary: MessageSummary,
    /// Flattened MIME parts.
    pub parts: Vec<MessagePartView>,
    /// Best-effort plain-text body (always escape-sanitized by frontends).
    pub body_plain: Option<String>,
    /// Sanitized HTML body (ammonia-cleaned; remote content stripped,
    /// `cid:`/`data:` images only). The webview CSP is still enforced.
    pub body_html: Option<String>,
    /// Number of remote items stripped from the HTML.
    pub remote_blocked: u32,
    /// Parser degradations observed while ingesting this message.
    pub warnings: Vec<String>,
    /// Suspicious links detected in the body.
    pub suspicious_links: Vec<SuspiciousLinkInfo>,
}

/// One MIME part in a message view.
#[derive(Clone, Debug)]
pub struct MessagePartView {
    /// Part id.
    pub id: PartIdView,
    /// Traversal order.
    pub seq: u32,
    /// Lowercased `type/subtype`.
    pub mime_type: String,
    /// `Content-ID` (for `cid:` resolution).
    pub content_id: Option<String>,
    /// `inline` / `attachment` / `None`.
    pub disposition: Option<String>,
    /// Suggested filename.
    pub filename: Option<String>,
    /// Decoded byte size.
    pub byte_size: u64,
}

/// Opaque part handle used by the `kestrel-cid://` viewport protocol
/// (threat model §5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartIdView {
    /// Opaque engine-issued id.
    pub key: String,
}

/// A link flagged by the classifier (threat model §4.5).
#[derive(Clone, Debug)]
pub struct SuspiciousLinkInfo {
    /// Target href.
    pub href: String,
    /// Why it was flagged.
    pub reason: SuspiciousLinkReason,
}

/// Reason a link requires confirmation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuspiciousLinkReason {
    /// Punycode/IDN host label (`xn--`).
    Punycode,
    /// Mixed-script confusable host.
    MixedScript,
    /// Display text host differs from target host.
    DisplayMismatch,
}

/// A composition draft submitted to the outbox.
#[derive(Clone, Debug)]
pub struct Draft {
    /// Sending account.
    pub account: AccountId,
    /// Sender.
    pub from: Address,
    /// Recipients.
    pub to: Vec<Address>,
    /// Cc recipients.
    pub cc: Vec<Address>,
    /// Bcc recipients.
    pub bcc: Vec<Address>,
    /// Subject.
    pub subject: String,
    /// Thread linkage: message this replies to.
    pub in_reply_to: Option<String>,
    /// Thread linkage: accumulated `References`.
    pub references: Vec<String>,
    /// Body written in Markdown; rendered to `multipart/alternative` on send
    /// (requirements §5).
    pub body_markdown: String,
    /// Attachments.
    pub attachments: Vec<DraftAttachment>,
}

/// An attachment on a draft.
#[derive(Clone, Debug)]
pub struct DraftAttachment {
    /// File name.
    pub name: String,
    /// MIME type.
    pub mime_type: String,
    /// Raw bytes.
    pub data: Vec<u8>,
}

/// Folder delta delivered with `MailArrived`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FolderDelta {
    /// New messages.
    pub new: u32,
    /// Total after the change.
    pub total: u64,
    /// Unread after the change.
    pub unread: u64,
}

/// Email address with optional display name (canonical form in storage:
/// JSON `{name?, email}`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    /// Display name, if any.
    pub name: Option<String>,
    /// Bare address (`local@domain`).
    pub email: String,
}

impl Address {
    /// Builds an address with no display name.
    #[must_use]
    pub fn bare(email: impl Into<String>) -> Self {
        Self {
            name: None,
            email: email.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_version_is_one() {
        assert_eq!(PROTOCOL_VERSION, 1);
    }

    #[test]
    fn search_query_emptiness() {
        assert!(SearchQuery::default().is_empty());
        assert!(
            !SearchQuery {
                text: Some("hello".into()),
                ..SearchQuery::default()
            }
            .is_empty()
        );
        assert!(
            !SearchQuery {
                has_attachment: true,
                ..SearchQuery::default()
            }
            .is_empty()
        );
    }
}
