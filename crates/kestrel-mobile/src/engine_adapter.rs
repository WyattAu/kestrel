//! Mobile-specific engine configuration.
//!
//! Overrides defaults from `kestrel-core::config::Config` with values
//! appropriate for mobile resource constraints (storage quotas, cache
//! limits, background sync intervals).

use std::sync::atomic::{AtomicBool, Ordering};

use crate::background::BackgroundTask;

/// Configuration for the engine when running on a mobile device.
///
/// Mobile devices have tighter storage, battery, and network constraints
/// than desktop. These defaults are conservative and tunable per-platform.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct MobileEngineConfig {
    /// Maximum storage quota in bytes.
    pub storage_quota: u64,
    /// Maximum cache age in days. Messages older than this are evicted
    /// from the local cache on low-storage conditions.
    pub cache_max_age_days: u32,
    /// Enable background sync when the app is not in the foreground.
    pub background_sync: bool,
    /// Sync interval in minutes when running in the background.
    pub background_sync_interval: u32,
}

impl Default for MobileEngineConfig {
    fn default() -> Self {
        Self {
            storage_quota: 500 * 1024 * 1024, // 500 MB
            cache_max_age_days: 30,
            background_sync: true,
            background_sync_interval: 15,
        }
    }
}

/// Opaque handle to a configured mobile engine.
///
/// The handle owns the engine's configuration and background task scheduler.
/// It is safe to share across threads via `Arc`.
pub struct EngineHandle {
    config: MobileEngineConfig,
    background_tasks: Vec<BackgroundTask>,
    destroyed: AtomicBool,
}

impl EngineHandle {
    /// Returns a reference to the engine configuration.
    #[must_use]
    pub fn config(&self) -> &MobileEngineConfig {
        &self.config
    }

    /// Returns the list of registered background tasks.
    #[must_use]
    pub fn background_tasks(&self) -> &[BackgroundTask] {
        &self.background_tasks
    }

    /// Marks this handle as destroyed. After this call, FFI operations
    /// that reference this handle must treat it as invalid.
    pub fn mark_destroyed(&self) {
        self.destroyed.store(true, Ordering::Release);
    }

    /// Returns `true` if this handle has been marked as destroyed.
    #[must_use]
    pub fn is_destroyed(&self) -> bool {
        self.destroyed.load(Ordering::Acquire)
    }
}

/// Creates a new engine handle with the given configuration.
#[must_use]
pub fn create_engine_handle(config: MobileEngineConfig) -> EngineHandle {
    let background_tasks = if config.background_sync {
        vec![
            BackgroundTask::Sync,
            BackgroundTask::OutboxFlush,
            BackgroundTask::SnoozeCheck,
            BackgroundTask::FilterEvaluation,
        ]
    } else {
        vec![]
    };
    EngineHandle {
        config,
        background_tasks,
        destroyed: AtomicBool::new(false),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn default_storage_quota_is_500mb() {
        let cfg = MobileEngineConfig::default();
        assert_eq!(cfg.storage_quota, 500 * 1024 * 1024);
    }

    #[test]
    fn default_cache_max_age_is_30_days() {
        let cfg = MobileEngineConfig::default();
        assert_eq!(cfg.cache_max_age_days, 30);
    }

    #[test]
    fn default_background_sync_enabled() {
        let cfg = MobileEngineConfig::default();
        assert!(cfg.background_sync);
    }

    #[test]
    fn default_background_sync_interval_is_15_minutes() {
        let cfg = MobileEngineConfig::default();
        assert_eq!(cfg.background_sync_interval, 15);
    }

    #[test]
    fn config_is_cloneable() {
        let cfg = MobileEngineConfig::default();
        let cloned = cfg.clone();
        assert_eq!(cfg, cloned);
    }

    #[test]
    fn background_sync_false_produces_empty_task_list() {
        let cfg = MobileEngineConfig {
            background_sync: false,
            ..Default::default()
        };
        let handle = create_engine_handle(cfg);
        assert!(handle.background_tasks().is_empty());
    }

    #[test]
    fn custom_config_values() {
        let cfg = MobileEngineConfig {
            storage_quota: 1024,
            cache_max_age_days: 7,
            background_sync: true,
            background_sync_interval: 5,
        };
        let handle = create_engine_handle(cfg.clone());
        assert_eq!(handle.config().storage_quota, 1024);
        assert_eq!(handle.config().cache_max_age_days, 7);
        assert_eq!(handle.config().background_sync_interval, 5);
        assert_eq!(handle.config(), &cfg);
    }

    #[test]
    fn destroyed_guard_starts_false() {
        let handle = create_engine_handle(MobileEngineConfig::default());
        assert!(!handle.is_destroyed());
    }

    #[test]
    fn mark_destroyed_sets_flag() {
        let handle = create_engine_handle(MobileEngineConfig::default());
        handle.mark_destroyed();
        assert!(handle.is_destroyed());
    }
}
