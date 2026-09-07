//! Shared GUI state: the bundle of `Arc<Mutex<…>>` fields that multiple
//! callback modules need access to.

use std::sync::{Arc, atomic::AtomicU32};

use kestrel_core::{
    config::Config,
    ids::{AccountId, FolderId, MessageId},
    paths::Paths,
};
use kestrel_gui::ViewportState;

/// Type alias for the shared viewport state.
pub type SharedViewportState = Arc<std::sync::Mutex<ViewportState>>;

/// All shared mutable state needed by the GUI callback modules.
///
/// Created once in `main()` and passed by reference to each `install()`
/// function.  Cloning an `AppHandle` is cheap (it's `Arc`-backed).
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

    // ── ID caches (index == UI list position) ──
    pub folder_ids: Arc<std::sync::Mutex<Vec<FolderId>>>,
    pub message_ids: Arc<std::sync::Mutex<Vec<MessageId>>>,
    pub account_ids_cache: Arc<std::sync::Mutex<Vec<AccountId>>>,
    pub account_emails_cache: Arc<std::sync::Mutex<Vec<String>>>,

    // ── Current message state ──
    pub current_attachment_keys: Arc<std::sync::Mutex<Vec<String>>>,
    pub current_message_for_attachments: Arc<std::sync::Mutex<Option<MessageId>>>,
    pub current_message_html: Arc<std::sync::Mutex<Option<String>>>,

    // ── Reply threading context ──
    pub reply_in_reply_to: Arc<std::sync::Mutex<Option<String>>>,
    pub reply_references: Arc<std::sync::Mutex<Vec<String>>>,

    // ── Compose attachments ──
    pub pending_compose_attachments:
        Arc<std::sync::Mutex<Vec<kestrel_core::protocol::DraftAttachment>>>,

    /// Shared unread counter for tray tooltip.
    pub unread_count: Arc<AtomicU32>,
}

impl GuiState {
    /// Build from the raw pieces created during `main()` bootstrap.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        app_weak: slint::Weak<crate::AppWindow>,
        handle: kestrel_engine::EngineHandle,
        config: Arc<Config>,
        paths: Arc<Paths>,
        vp_state: SharedViewportState,
        folder_ids: Arc<std::sync::Mutex<Vec<FolderId>>>,
        message_ids: Arc<std::sync::Mutex<Vec<MessageId>>>,
        account_ids_cache: Arc<std::sync::Mutex<Vec<AccountId>>>,
        account_emails_cache: Arc<std::sync::Mutex<Vec<String>>>,
        current_attachment_keys: Arc<std::sync::Mutex<Vec<String>>>,
        current_message_for_attachments: Arc<std::sync::Mutex<Option<MessageId>>>,
        current_message_html: Arc<std::sync::Mutex<Option<String>>>,
        reply_in_reply_to: Arc<std::sync::Mutex<Option<String>>>,
        reply_references: Arc<std::sync::Mutex<Vec<String>>>,
        pending_compose_attachments: Arc<
            std::sync::Mutex<Vec<kestrel_core::protocol::DraftAttachment>>,
        >,
        unread_count: Arc<AtomicU32>,
    ) -> Self {
        Self {
            app_weak,
            handle,
            config,
            paths,
            vp_state,
            folder_ids,
            message_ids,
            account_ids_cache,
            account_emails_cache,
            current_attachment_keys,
            current_message_for_attachments,
            current_message_html,
            reply_in_reply_to,
            reply_references,
            pending_compose_attachments,
            unread_count,
        }
    }
}
