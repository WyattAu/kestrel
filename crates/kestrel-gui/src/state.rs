//! Shared GUI state (issue #5): cohesive groups instead of a loose bag of
//! `Arc<Mutex<…>>` cells.
//!
//! Each group owns one cluster of UI state and the invariants over it, so
//! callback modules (`navigation`, `compose`, `setup_wizard`, `settings`)
//! call methods instead of locking raw cells. Groups are `Arc`-wrapped:
//! background threads clone only the group they touch (cheap, no
//! whole-state capture), and cross-field invariants are enforced in one
//! place — with unit tests.
//!
//! Conventions mirror the engine's actor/leaf-data split: the Slint event
//! loop owns the widgets; these groups are the leaf data it reads/writes
//! from `invoke_from_event_loop` closures and short-lived worker threads.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};

use kestrel_core::{
    config::Config,
    ids::{AccountId, FolderId, MessageId},
    paths::Paths,
};
use kestrel_gui::ViewportState;

/// Type alias for the shared viewport state.
pub type SharedViewportState = Arc<Mutex<ViewportState>>;

/// Lock a mutex, treating a poisoned lock as recoverable (the GUI state
/// cells hold plain data; a panic in one writer must not brick the UI).
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ────────────────────────── lists ──────────────────────────

/// Folder/message ID lists parallel to the Slint models: index *i* in
/// `folder_ids`/`message_ids` is the row the user selected.
#[derive(Default)]
pub(crate) struct ListCaches {
    folder_ids: Mutex<Vec<FolderId>>,
    message_ids: Mutex<Vec<MessageId>>,
}

impl ListCaches {
    /// Folder ID at a UI row, if in range.
    pub(crate) fn folder_at(&self, idx: usize) -> Option<FolderId> {
        lock(&self.folder_ids).get(idx).copied()
    }

    /// Replace the folder list (call after a folder listing reply).
    pub(crate) fn set_folders(&self, ids: Vec<FolderId>) {
        *lock(&self.folder_ids) = ids;
    }

    /// Message ID at a UI row, if in range.
    pub(crate) fn message_at(&self, idx: usize) -> Option<MessageId> {
        lock(&self.message_ids).get(idx).copied()
    }

    /// Replace the message list (call after a listing/search reply).
    pub(crate) fn set_messages(&self, ids: Vec<MessageId>) {
        *lock(&self.message_ids) = ids;
    }

    /// First folder whose ID differs from `skip` — the move target for
    /// "archive" (skips the virtual Unified Inbox row).
    pub(crate) fn first_folder_except(&self, skip: FolderId) -> Option<FolderId> {
        lock(&self.folder_ids)
            .iter()
            .find(|id| **id != skip)
            .copied()
    }
}

// ────────────────────────── accounts ──────────────────────────

/// Account ID/email lists parallel to the Slint account models. IDs and
/// emails are written together wherever possible so row *i* means the same
/// account in both.
#[derive(Default)]
pub(crate) struct AccountCache {
    ids: Mutex<Vec<AccountId>>,
    emails: Mutex<Vec<String>>,
}

impl AccountCache {
    /// Replace both lists atomically (call after `ListAccounts`).
    pub(crate) fn replace(&self, ids: Vec<AccountId>, emails: Vec<String>) {
        *lock(&self.ids) = ids;
        *lock(&self.emails) = emails;
    }

    /// Account ID at a UI row, if in range.
    pub(crate) fn id_at(&self, idx: usize) -> Option<AccountId> {
        lock(&self.ids).get(idx).copied()
    }

    /// Email at a UI row, if in range.
    pub(crate) fn email_at(&self, idx: usize) -> Option<String> {
        lock(&self.emails).get(idx).cloned()
    }

    /// Snapshot of the ID list (for compose's first-account fallback).
    pub(crate) fn ids(&self) -> Vec<AccountId> {
        lock(&self.ids).clone()
    }

    /// Snapshot of the email list.
    pub(crate) fn emails(&self) -> Vec<String> {
        lock(&self.emails).clone()
    }

    /// UI row of `id`, if the account is cached.
    pub(crate) fn row_of(&self, id: AccountId) -> Option<usize> {
        lock(&self.ids).iter().position(|cached| *cached == id)
    }
}

// ────────────────────────── message view ──────────────────────────

/// State of the message currently displayed: the raw HTML cache for the
/// remote-content toggle, plus the attachment rows parallel to the Slint
/// attachment model.
#[derive(Default)]
pub(crate) struct MessageView {
    attachment_keys: Mutex<Vec<String>>,
    attachment_message: Mutex<Option<MessageId>>,
    html: Mutex<Option<String>>,
}

impl MessageView {
    /// Record everything a `GetMessage` reply displayed: attachment part
    /// keys for row *i* of the attachment model, which message they belong
    /// to, and the raw (unsanitized) HTML for the remote-content toggle.
    pub(crate) fn display(
        &self,
        message: MessageId,
        attachment_keys: Vec<String>,
        raw_html: Option<String>,
    ) {
        *lock(&self.attachment_keys) = attachment_keys;
        *lock(&self.attachment_message) = Some(message);
        *lock(&self.html) = raw_html;
    }

    /// Attachment part key + owning message for a UI row, if in range.
    pub(crate) fn attachment_at(&self, idx: usize) -> Option<(String, MessageId)> {
        let keys = lock(&self.attachment_keys);
        let message = lock(&self.attachment_message);
        match (keys.get(idx), *message) {
            (Some(key), Some(message)) => Some((key.clone(), message)),
            _ => None,
        }
    }

    /// Raw HTML of the displayed message, if any.
    pub(crate) fn html(&self) -> Option<String> {
        lock(&self.html).clone()
    }
}

// ────────────────────────── reply context ──────────────────────────

/// RFC 5322 threading headers for the compose draft.
#[derive(Default)]
pub(crate) struct ReplyContext {
    in_reply_to: Mutex<Option<String>>,
    references: Mutex<Vec<String>>,
}

impl ReplyContext {
    /// Fresh compose with no threading context.
    pub(crate) fn clear(&self) {
        *lock(&self.in_reply_to) = None;
        lock(&self.references).clear();
    }
    /// Start a reply to a message with the given threading headers.
    ///
    /// `In-Reply-To` falls back to the replied-to message's own
    /// `Message-ID` when it has none (a message that was never replied to).
    /// `References` is the parent's chain, extended by its `Message-ID`
    /// unless that is already the last entry. Consumes the parent headers.
    pub(crate) fn start_reply_from(
        &self,
        parent_in_reply_to: Option<String>,
        parent_message_id: Option<String>,
    ) {
        let in_reply_to = parent_in_reply_to
            .clone()
            .or_else(|| parent_message_id.clone());
        *lock(&self.in_reply_to) = in_reply_to;
        let mut references = lock(&self.references);
        references.clear();
        if let Some(irt) = parent_in_reply_to {
            references.push(irt);
        }
        if let Some(mid) = parent_message_id
            && (references.is_empty() || references.last() != Some(&mid))
        {
            references.push(mid);
        }
    }

    /// Threading headers for the draft being submitted.
    pub(crate) fn headers(&self) -> (Option<String>, Vec<String>) {
        (
            lock(&self.in_reply_to).clone(),
            lock(&self.references).clone(),
        )
    }
}

// ────────────────────────── compose draft ──────────────────────────

/// Attachments accumulated for the next submitted draft (file drops,
/// clipboard pastes). Drained on submit.
#[derive(Default)]
pub(crate) struct ComposeDraft {
    attachments: Mutex<Vec<kestrel_core::protocol::DraftAttachment>>,
}

impl ComposeDraft {
    /// Queue an attachment for the next submission.
    // Attachment entry points (file drop, image paste) are `tray`-gated.
    #[cfg_attr(not(feature = "tray"), allow(dead_code))]
    pub(crate) fn attach(&self, attachment: kestrel_core::protocol::DraftAttachment) {
        lock(&self.attachments).push(attachment);
    }

    /// Take all queued attachments, leaving the draft empty.
    pub(crate) fn take_attachments(&self) -> Vec<kestrel_core::protocol::DraftAttachment> {
        std::mem::take(&mut lock(&self.attachments))
    }
}

// ────────────────────────── unread counter ──────────────────────────

/// Shared unread counter for the tray tooltip (atomic: written by the
/// engine-event pump, read by the tray thread).
#[derive(Default)]
pub(crate) struct UnreadCounter(AtomicU32);

impl UnreadCounter {
    pub(crate) fn set(&self, n: u32) {
        self.0.store(n, Ordering::Relaxed);
    }

    /// Current value. Consumed by the tray tooltip once it refreshes on
    /// `MailArrived` (today the tooltip text is static); until then this
    /// is exercised by the unit test below.
    #[allow(dead_code)]
    pub(crate) fn get(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }
}

// ────────────────────────── GuiState ──────────────────────────

/// All shared state needed by the GUI callback modules.
///
/// Created once in `main()` and passed by reference to each `install()`
/// function; the `Arc` groups are cloned individually into worker threads.
pub(crate) struct GuiState {
    /// Weak reference to the Slint app window.
    pub app_weak: slint::Weak<crate::AppWindow>,
    /// Engine handle for sending commands.
    pub handle: kestrel_engine::EngineHandle,
    /// App configuration (loaded once).
    pub config: Arc<Config>,
    /// XDG paths.
    pub paths: Arc<Paths>,
    /// Viewport state for HTML body rendering.
    pub vp_state: SharedViewportState,
    /// Folder/message row caches.
    pub lists: Arc<ListCaches>,
    /// Account row caches.
    pub accounts: Arc<AccountCache>,
    /// Currently displayed message.
    pub message_view: Arc<MessageView>,
    /// Threading headers for the compose draft.
    pub reply: Arc<ReplyContext>,
    /// Compose attachments.
    pub draft: Arc<ComposeDraft>,
}

impl GuiState {
    /// Build from the pieces created during `main()` bootstrap. The state
    /// groups start empty and are filled by the callback modules as
    /// replies arrive.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        app_weak: slint::Weak<crate::AppWindow>,
        handle: kestrel_engine::EngineHandle,
        config: Arc<Config>,
        paths: Arc<Paths>,
        vp_state: SharedViewportState,
        lists: Arc<ListCaches>,
        accounts: Arc<AccountCache>,
        message_view: Arc<MessageView>,
        reply: Arc<ReplyContext>,
        draft: Arc<ComposeDraft>,
    ) -> Self {
        Self {
            app_weak,
            handle,
            config,
            paths,
            vp_state,
            lists,
            accounts,
            message_view,
            reply,
            draft,
        }
    }
}

// ────────────────────────── tests ──────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_round_trip_and_bounds() {
        let lists = ListCaches::default();
        assert_eq!(lists.folder_at(0), None);
        assert_eq!(lists.message_at(3), None);

        let fids: Vec<FolderId> = (0..4)
            .map(|i| FolderId::from_uuid(uuid::Uuid::from_u128(i)))
            .collect();
        let mids: Vec<MessageId> = (0..2)
            .map(|i| MessageId::from_uuid(uuid::Uuid::from_u128(i)))
            .collect();
        lists.set_folders(fids.clone());
        lists.set_messages(mids.clone());
        assert_eq!(lists.folder_at(2), fids.get(2).copied());
        assert_eq!(lists.folder_at(4), None);
        assert_eq!(lists.message_at(1), mids.get(1).copied());
        assert_eq!(lists.message_at(2), None);
    }

    #[test]
    fn accounts_replace_is_atomic_and_row_of_resolves() {
        let accounts = AccountCache::default();
        assert_eq!(accounts.id_at(0), None);

        let ids: Vec<AccountId> = (0..3)
            .map(|i| AccountId::from_uuid(uuid::Uuid::from_u128(i)))
            .collect();
        let emails: Vec<String> = vec!["a@x".into(), "b@x".into(), "c@x".into()];
        accounts.replace(ids.clone(), emails.clone());

        assert_eq!(accounts.id_at(2), ids.get(2).copied());
        assert_eq!(accounts.email_at(0).as_deref(), Some("a@x"));
        assert_eq!(accounts.email_at(5), None);
        assert_eq!(accounts.row_of(ids[1]), Some(1));
        assert_eq!(
            accounts.row_of(AccountId::from_uuid(uuid::Uuid::from_u128(99))),
            None
        );
    }

    #[test]
    fn message_view_display_and_attachment_lookup() {
        let view = MessageView::default();
        assert_eq!(view.attachment_at(0), None);
        assert_eq!(view.html(), None);

        let msg = MessageId::from_uuid(uuid::Uuid::from_u128(7));
        view.display(
            msg,
            vec!["k0".into(), "k1".into()],
            Some("<b>hi</b>".into()),
        );
        assert_eq!(view.attachment_at(1), Some(("k1".into(), msg)));
        assert_eq!(view.attachment_at(2), None);
        assert_eq!(view.html().as_deref(), Some("<b>hi</b>"));

        // A new display overwrites all three fields together.
        let other = MessageId::from_uuid(uuid::Uuid::from_u128(9));
        view.display(other, vec![], None);
        assert_eq!(view.attachment_at(0), None);
        assert_eq!(view.html(), None);
    }

    #[test]
    fn reply_context_threading_headers() {
        let reply = ReplyContext::default();
        reply.clear();
        assert_eq!(reply.headers(), (None, vec![]));

        // Parent with its own Message-ID: chain = [irt, mid] (distinct).
        reply.start_reply_from(Some("<a@x>".into()), Some("<b@x>".into()));
        assert_eq!(
            reply.headers(),
            (Some("<a@x>".into()), vec!["<a@x>".into(), "<b@x>".into()])
        );

        // Parent without Message-ID: In-Reply-To falls back to it, chain
        // does not duplicate the trailing entry.
        reply.start_reply_from(None, Some("<b@x>".into()));
        assert_eq!(
            reply.headers(),
            (Some("<b@x>".into()), vec!["<b@x>".into()])
        );

        // No threading headers at all: both stay empty.
        reply.start_reply_from(None, None);
        assert_eq!(reply.headers(), (None, vec![]));

        reply.clear();
        assert_eq!(reply.headers(), (None, vec![]));
    }

    #[test]
    fn compose_draft_attach_and_drain() {
        let draft = ComposeDraft::default();
        assert!(draft.take_attachments().is_empty());

        draft.attach(kestrel_core::protocol::DraftAttachment {
            name: "a.txt".into(),
            mime_type: "text/plain".into(),
            data: vec![1],
        });
        draft.attach(kestrel_core::protocol::DraftAttachment {
            name: "b.png".into(),
            mime_type: "image/png".into(),
            data: vec![2, 3],
        });
        let taken = draft.take_attachments();
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].name, "a.txt");
        assert!(draft.take_attachments().is_empty());
    }

    #[test]
    fn unread_counter_round_trip() {
        let unread = UnreadCounter::default();
        assert_eq!(unread.get(), 0);
        unread.set(41);
        assert_eq!(unread.get(), 41);
    }
}
