//! Plugin manifest and capability types.
//!
//! A plugin declares its identity and required capabilities in a manifest.
//! The host validates the manifest at load time and enforces capabilities
//! at every API call boundary.

use std::path::PathBuf;

/// Current plugin API version supported by this host.
pub const PLUGIN_API_VERSION: &str = "1.0";

/// Deserialized plugin manifest (`plugin.json` or inline TOML/JSON).
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct PluginManifest {
    /// Unique plugin name (reverse-domain recommended, e.g.
    /// `com.example.spam-filter`).
    pub name: String,
    /// `SemVer` version string.
    pub version: String,
    /// Plugin author (display name or organization).
    pub author: String,
    /// Short human-readable description.
    pub description: String,
    /// Capabilities the plugin requires to function.
    pub capabilities: Vec<Capability>,
    /// Plugin API version this plugin targets (must match major of
    /// [`PLUGIN_API_VERSION`]).
    pub api_version: String,
}

/// Capabilities a plugin may request. Each maps to a host API method.
/// Plugins cannot access credentials, local files, or the network — only
/// the data exposed through these capabilities.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// List accounts and their connection state.
    ReadAccounts,
    /// List folders within an account.
    ReadFolders,
    /// List message summaries in a folder.
    ReadMessages,
    /// Read full message bodies (plain text, HTML, attachments).
    ReadMessageBodies,
    /// Subscribe to engine events (new mail, flags changed, etc.).
    SubscribeEvents,
    /// Register UI components (sidebar panels, toolbar buttons).
    RegisterUI,
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadAccounts => write!(f, "read_accounts"),
            Self::ReadFolders => write!(f, "read_folders"),
            Self::ReadMessages => write!(f, "read_messages"),
            Self::ReadMessageBodies => write!(f, "read_message_bodies"),
            Self::SubscribeEvents => write!(f, "subscribe_events"),
            Self::RegisterUI => write!(f, "register_ui"),
        }
    }
}

/// Metadata about an installed plugin, combining the manifest with runtime
/// state.
#[derive(Clone, Debug)]
pub struct PluginInfo {
    /// The parsed manifest.
    pub manifest: PluginManifest,
    /// Whether the plugin is currently enabled.
    pub enabled: bool,
    /// Filesystem path to the plugin WASM module.
    pub path: PathBuf,
}
