//! Host API traits that plugins call into.
//!
//! Each method corresponds to one [`Capability`](crate::types::Capability).
//! The host enforces that a plugin holds the required capability before
//! dispatching to the underlying service protocol.

use kestrel_core::{
    ids::{AccountId, FolderId, MessageId},
    protocol::{AccountSummary, EngineEvent, FolderSummary, MessageSummary, MessageView, Window},
};

/// Read-only host API for plugins. All methods are infallible at the
/// trait level — errors are returned as empty/default results (plugins
/// should not be able to probe internal error details).
pub trait PluginHost {
    /// List all configured accounts.
    fn list_accounts(&self) -> Vec<AccountSummary>;

    /// List folders for an account.
    fn list_folders(&self, account: AccountId) -> Vec<FolderSummary>;

    /// List messages in a folder within a window.
    fn list_messages(&self, folder: FolderId, window: Window) -> Vec<MessageSummary>;

    /// Fetch a single message with full body resolution.
    fn get_message(&self, message: MessageId) -> Option<MessageView>;

    /// Subscribe to engine events. Returns a receiver that yields events
    /// as they occur. The host owns the subscription lifetime.
    fn subscribe_events(&self) -> tokio::sync::mpsc::Receiver<EngineEvent>;
}
