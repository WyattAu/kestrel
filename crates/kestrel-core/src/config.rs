//! Configuration (ADR 0006): figment layering (defaults → file → env),
//! validation with precise errors, `notify`-driven hot reload publishing
//! `Arc<Config>` snapshots.
//!
//! Layered keys: `KESTREL_<SECTION>__<KEY>` environment overrides
//! (double underscore separates nesting), used mainly by tests and CI.

use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use notify::Watcher;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::paths::Paths;

/// A saved search: name + structured query for reuse.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedSearch {
    /// User-chosen label.
    pub name: String,
    /// Structured search query to execute.
    pub query: crate::protocol::SearchQuery,
}

/// Per-account notification settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccountNotificationConfig {
    /// Whether notifications are enabled for this account.
    pub enabled: bool,
    /// Whether to include the subject line in OS notifications.
    pub show_subject: bool,
    /// Optional custom notification sound.
    pub sound: Option<String>,
    /// Whether this account is muted (suppresses all notifications).
    pub mute: bool,
}

impl Default for AccountNotificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            show_subject: true,
            sound: None,
            mute: false,
        }
    }
}

/// Configurable keybindings for the TUI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeybindingsConfig {
    /// Reply to sender.
    pub reply: String,
    /// Reply to all recipients.
    pub reply_all: String,
    /// Forward message.
    pub forward: String,
    /// Delete message.
    pub delete: String,
    /// Archive message.
    pub archive: String,
    /// Toggle flagged/starred.
    pub flag: String,
    /// Compose new message.
    pub compose: String,
    /// Open search.
    pub search: String,
    /// Next message (vi-down).
    pub next: String,
    /// Previous message (vi-up).
    pub prev: String,
}

impl Default for KeybindingsConfig {
    fn default() -> Self {
        Self {
            reply: "r".into(),
            reply_all: "a".into(),
            forward: "f".into(),
            delete: "d".into(),
            archive: "x".into(),
            flag: "s".into(),
            compose: "c".into(),
            search: "/".into(),
            next: "j".into(),
            prev: "k".into(),
        }
    }
}

impl KeybindingsConfig {
    /// Returns the first `char` of a keybinding string, or `None` if empty.
    #[must_use]
    pub fn key_char(binding: &str) -> Option<char> {
        binding.chars().next()
    }
}

/// Root configuration model. UI preferences (keybindings, theme) live in the
/// same file; engine-level state does not (that is `data.db settings`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// General/UI preferences.
    pub general: GeneralConfig,
    /// Sync engine tuning.
    pub sync: SyncConfig,
    /// Storage tuning + quotas.
    pub storage: StorageConfig,
    /// Search tuning.
    pub search: SearchConfig,
    /// Notification policy (threat model §6: privacy).
    pub notifications: NotificationsConfig,
    /// Per-account notification overrides (email → config).
    pub account_notifications: HashMap<String, AccountNotificationConfig>,
    /// Security policy.
    pub security: SecurityConfig,
    /// External editor for TUI composition (`$EDITOR` when absent).
    pub editor: EditorConfig,
    /// Logging (ADR 0008).
    pub log: LogConfig,
    /// User-defined compose templates (name → body content).
    pub templates: HashMap<String, String>,
    /// User-saved searches for quick reuse.
    pub saved_searches: Vec<SavedSearch>,
    /// Per-account email signatures (keyed by email address).
    pub account_signatures: HashMap<String, String>,
    /// Undo-send delay in seconds (0 = immediate send). When > 0, the
    /// outbox entry is enqueued with a delayed `next_attempt_at` so the
    /// user can cancel within the window.
    pub send_delay_seconds: u32,
    /// Configurable keybindings for the TUI and GUI.
    pub keybindings: KeybindingsConfig,
}

// clippy::derivable_impls: intentional — adding a section without a
// Default impl must be a compile error here, not a silent skip.
#[allow(clippy::derivable_impls)]
impl Default for Config {
    fn default() -> Self {
        let mut templates = HashMap::new();
        templates.insert("signature".into(), "-- \nBest regards,\nYour Name".into());
        Self {
            general: GeneralConfig::default(),
            sync: SyncConfig::default(),
            storage: StorageConfig::default(),
            search: SearchConfig::default(),
            notifications: NotificationsConfig::default(),
            account_notifications: HashMap::new(),
            security: SecurityConfig::default(),
            editor: EditorConfig::default(),
            log: LogConfig::default(),
            templates,
            saved_searches: Vec::new(),
            account_signatures: HashMap::new(),
            send_delay_seconds: 0,
            keybindings: KeybindingsConfig::default(),
        }
    }
}

/// General/UI preferences.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GeneralConfig {
    /// UI theme name; `auto` follows the OS.
    pub theme: String,
    /// Date format for listings (`strftime`-style subset).
    pub date_format: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            theme: "auto".to_string(),
            date_format: "%Y-%m-%d %H:%M".to_string(),
        }
    }
}

/// Sync engine tuning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyncConfig {
    /// Poll interval fallback when IDLE is unavailable/broken (seconds).
    pub poll_interval_secs: u64,
    /// IDLE re-issue interval; must stay under the common 30 min server
    /// cutoff (sync-engine.md §5).
    pub idle_timeout_mins: u64,
    /// How many recent bodies per folder to prefetch in the background
    /// (sync-engine.md §4).
    pub body_prefetch_recent: usize,
    /// Hosts where IDLE is known broken: always poll.
    pub idle_poll_only_hosts: Vec<String>,
    /// Connection attempts before parking in Disconnected for a cooloff.
    pub connect_attempts: u32,
    /// Per-connect timeout in seconds.
    pub connect_timeout_secs: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: 120,
            idle_timeout_mins: 29,
            body_prefetch_recent: 200,
            idle_poll_only_hosts: Vec::new(),
            connect_attempts: 3,
            connect_timeout_secs: 30,
        }
    }
}

/// Storage tuning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// Per-account quota in MiB (threat model §4.3); 0 disables.
    pub per_account_quota_mib: u64,
    /// Blob GC grace period in hours (schema.md §4.3).
    pub blob_gc_grace_hours: u64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            per_account_quota_mib: 2048,
            blob_gc_grace_hours: 24,
        }
    }
}

/// Search tuning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchConfig {
    /// Default page size for search hits.
    pub default_limit: u64,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self { default_limit: 50 }
    }
}

/// Notification policy (threat model §6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationsConfig {
    /// Include the subject line in OS notifications; sender-only default.
    pub show_subject: bool,
    /// Emit desktop notifications at all.
    pub enabled: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            show_subject: false,
            enabled: true,
        }
    }
}

/// Security policy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityConfig {
    /// Remote-content policy: blocked by default; per-sender allowlist.
    pub remote_content: RemoteContentPolicy,
    /// Require confirmation for punycode/homograph/mismatched links.
    pub link_confirmation: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            remote_content: RemoteContentPolicy::default(),
            link_confirmation: true,
        }
    }
}

/// Remote content policy (requirements §4.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteContentPolicy {
    /// `true` = block for all senders except `allowed_senders`.
    pub block_by_default: bool,
    /// Senders whose remote content is allowed.
    pub allowed_senders: Vec<String>,
}

impl Default for RemoteContentPolicy {
    fn default() -> Self {
        Self {
            block_by_default: true,
            allowed_senders: Vec::new(),
        }
    }
}

/// External editor for TUI composition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EditorConfig {
    /// Explicit editor command (shell-words split); `None` → `$EDITOR`/`$VISUAL`.
    pub command: Option<String>,
}

#[allow(clippy::derivable_impls)]
impl Default for EditorConfig {
    fn default() -> Self {
        Self { command: None }
    }
}

/// Logging (ADR 0008).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogConfig {
    /// `tracing` `EnvFilter` directive, e.g. `info` or `kestrel_sync=debug`.
    pub level: String,
    /// Output format.
    pub format: LogFormat,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: LogFormat::Pretty,
        }
    }
}

/// Log output format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-friendly terminal output.
    Pretty,
    /// Machine-readable JSON.
    Json,
}

/// Load outcome: the snapshot plus non-fatal validation warnings.
#[derive(Debug)]
pub struct LoadedConfig {
    /// The validated snapshot.
    pub config: Arc<Config>,
    /// Warnings (unknown keys are rejected by `deny_unknown_fields`, so
    /// these are deprecation/override notes instead).
    pub warnings: Vec<String>,
}

impl Config {
    /// Loads the layered configuration: defaults → file (if present) → env.
    ///
    /// # Errors
    /// [`crate::error::KestrelError::InvalidToml`] with path + detail when
    /// the file exists but fails to parse or validate.
    pub fn load(paths: &Paths) -> Result<LoadedConfig, crate::error::KestrelError> {
        let file = paths.config_file();
        let mut figment = Figment::from(Serialized::defaults(Config::default()));
        if file.exists() {
            // Read manually so IO errors carry the path; parse/validate
            // errors surface at extract with figment's span information.
            let _ = std::fs::read_to_string(&file).map_err(|e| {
                crate::error::KestrelError::InvalidToml {
                    path: file.display().to_string(),
                    detail: e.to_string(),
                }
            })?;
            figment = figment.merge(Toml::file(&file));
        }
        figment = figment.merge(Env::prefixed("KESTREL_").split("__"));

        let config: Config =
            figment
                .extract()
                .map_err(|e| crate::error::KestrelError::InvalidToml {
                    path: file.display().to_string(),
                    detail: e.to_string(),
                })?;
        let warnings = config.validate();
        Ok(LoadedConfig {
            config: Arc::new(config),
            warnings,
        })
    }

    /// Cross-field validation; returns user-facing warnings (non-fatal).
    #[must_use]
    fn validate(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.sync.idle_timeout_mins >= 30 {
            warnings.push(format!(
                "sync.idle_timeout_mins = {} risks server-side IDLE cutoffs; use ≤ 29",
                self.sync.idle_timeout_mins
            ));
        }
        if self.search.default_limit == 0 {
            warnings.push("search.default_limit = 0 returns no hits".to_string());
        }
        warnings
    }

    /// Whether `sender` is allowed to load remote content.
    #[must_use]
    pub fn remote_content_allowed(&self, sender: &str) -> bool {
        if !self.security.remote_content.block_by_default {
            return true;
        }
        self.security
            .remote_content
            .allowed_senders
            .iter()
            .any(|s| s.eq_ignore_ascii_case(sender))
    }
}

/// Watches the config file and emits reloaded snapshots on change
/// (ADR 0006). Debounced 250 ms; invalid reloads are surfaced as warnings in
/// the event (the last good snapshot persists).
///
/// Sends terminate when `stop` fires.
///
/// # Errors
/// Returns an error only when the watcher infrastructure fails to start.
pub async fn watch_config(
    paths: Arc<Paths>,
    stop: tokio_util::sync::CancellationToken,
    sink: mpsc::Sender<crate::protocol::EngineEvent>,
) -> Result<(), crate::error::KestrelError> {
    let (tx, mut rx) = mpsc::channel(16);
    let mut watcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            if res.is_ok() {
                let _ = tx.blocking_send(());
            }
        })
        .map_err(|e| crate::error::KestrelError::InvalidToml {
            path: paths.config_file().display().to_string(),
            detail: format!("watcher start failed: {e}"),
        })?;

    let file = paths.config_file();
    if let Some(dir) = file.parent() {
        watcher
            .watch(dir, notify::RecursiveMode::NonRecursive)
            .map_err(|e| crate::error::KestrelError::InvalidToml {
                path: dir.display().to_string(),
                detail: e.to_string(),
            })?;
    }

    let mut last_good: Option<Arc<Config>> = Config::load(&paths).ok().map(|l| l.config);
    loop {
        tokio::select! {
            () = stop.cancelled() => { return Ok(()); }
            maybe = rx.recv() => {
                let Some(()) = maybe else { return Ok(()); };
                // Debounce: coalesce bursts within 250 ms.
                tokio::time::sleep(Duration::from_millis(250)).await;
                while rx.try_recv().is_ok() {}
                match Config::load(&paths) {
                    Ok(loaded) => {
                        last_good = Some(loaded.config.clone());
                        let _ = sink
                            .send(crate::protocol::EngineEvent::ConfigUpdated {
                                snapshot: loaded.config,
                            })
                            .await;
                    }
                    Err(e) => {
                        // Keep last good snapshot; surface the failure
                        // (never silent) without spamming the event bus.
                        tracing::warn!(error = %e, "config reload rejected; keeping last good snapshot");
                    }
                }
                let _ = &last_good;
            }
        }
    }
}

/// Ensures a config file exists; writes defaults (first-run experience).
///
/// # Errors
/// Filesystem errors propagate.
pub fn write_default_if_missing(path: &Path, config: &Config) -> Result<(), std::io::Error> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(config)
        .map_err(|e| std::io::Error::other(format!("default config serialization failed: {e}")))?;
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, unsafe_code)]

    use super::*;

    fn test_paths(dir: &std::path::Path) -> Paths {
        Paths::nested_under(dir)
    }

    #[test]
    fn defaults_load_without_file() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = Config::load(&test_paths(tmp.path())).unwrap();
        assert_eq!(loaded.config.sync.poll_interval_secs, 120);
        assert_eq!(loaded.config.log.format, LogFormat::Pretty);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn file_overrides_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        paths.ensure().unwrap();
        std::fs::write(
            paths.config_file(),
            "[sync]\npoll_interval_secs = 45\n[general]\ntheme = \"light\"\n",
        )
        .unwrap();
        let loaded = Config::load(&paths).unwrap();
        assert_eq!(loaded.config.sync.poll_interval_secs, 45);
        assert_eq!(loaded.config.general.theme, "light");
        assert_eq!(loaded.config.storage.per_account_quota_mib, 2048);
    }

    #[test]
    fn invalid_file_fails_with_path() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        paths.ensure().unwrap();
        std::fs::write(paths.config_file(), "[sync\nbroken").unwrap();
        let err = Config::load(&paths).unwrap_err();
        assert_eq!(err.kind(), "config.invalid_toml");
        assert!(
            err.to_string().contains("config.toml")
                || matches!(err, crate::error::KestrelError::InvalidToml { .. })
        );
    }

    #[test]
    fn unknown_key_is_rejected_not_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        paths.ensure().unwrap();
        std::fs::write(paths.config_file(), "[sync]\nno_such_key = 1\n").unwrap();
        assert!(Config::load(&paths).is_err());
    }

    #[test]
    fn env_overrides_file() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        paths.ensure().unwrap();
        std::fs::write(paths.config_file(), "[sync]\npoll_interval_secs = 45\n").unwrap();
        // SAFETY(test): nextest runs each test in its own process; no
        // concurrent env access. Var removed before assertions complete.
        unsafe {
            std::env::set_var("KESTREL_SYNC__POLL_INTERVAL_SECS", "60");
        }
        let loaded = Config::load(&paths).unwrap();
        unsafe { std::env::remove_var("KESTREL_SYNC__POLL_INTERVAL_SECS") };
        assert_eq!(loaded.config.sync.poll_interval_secs, 60);
    }

    #[test]
    fn validation_warns_on_idle_cutoff() {
        let mut cfg = Config::default();
        cfg.sync.idle_timeout_mins = 30;
        assert!(!cfg.validate().is_empty());
    }

    #[test]
    fn remote_content_policy_allowlist() {
        let mut cfg = Config::default();
        cfg.security
            .remote_content
            .allowed_senders
            .push("trusted@example.org".into());
        assert!(!cfg.remote_content_allowed("other@example.org"));
        assert!(cfg.remote_content_allowed("trusted@example.org"));
        assert!(cfg.remote_content_allowed("TRUSTED@example.org"));
        cfg.security.remote_content.block_by_default = false;
        assert!(cfg.remote_content_allowed("anyone@example.org"));
    }

    #[test]
    fn write_default_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("config.toml");
        write_default_if_missing(&file, &Config::default()).unwrap();
        write_default_if_missing(&file, &Config::default()).unwrap();
        assert!(file.exists());
    }

    #[test]
    fn keybindings_defaults() {
        let kb = KeybindingsConfig::default();
        assert_eq!(kb.reply, "r");
        assert_eq!(kb.reply_all, "a");
        assert_eq!(kb.forward, "f");
        assert_eq!(kb.delete, "d");
        assert_eq!(kb.archive, "x");
        assert_eq!(kb.flag, "s");
        assert_eq!(kb.compose, "c");
        assert_eq!(kb.search, "/");
        assert_eq!(kb.next, "j");
        assert_eq!(kb.prev, "k");
    }

    #[test]
    fn keybindings_key_char() {
        assert_eq!(KeybindingsConfig::key_char("r"), Some('r'));
        assert_eq!(KeybindingsConfig::key_char("/"), Some('/'));
        assert_eq!(KeybindingsConfig::key_char(""), None);
    }

    #[test]
    fn keybindings_load_from_file() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        paths.ensure().unwrap();
        std::fs::write(
            paths.config_file(),
            "[keybindings]\nreply = \"u\"\nforward = \"w\"\n",
        )
        .unwrap();
        let loaded = Config::load(&paths).unwrap();
        assert_eq!(loaded.config.keybindings.reply, "u");
        assert_eq!(loaded.config.keybindings.forward, "w");
        // Defaults preserved for unspecified keys.
        assert_eq!(loaded.config.keybindings.delete, "d");
    }
}
