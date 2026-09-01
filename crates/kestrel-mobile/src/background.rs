//! Background task management for mobile.
//!
//! On iOS, background work is dispatched via `BGTaskScheduler`; on Android,
//! via `WorkManager`. This module defines the task vocabulary that the
//! platform layer will schedule. Actual platform integration is future work.

/// Background task types that can be scheduled on mobile platforms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundTask {
    /// Synchronize mailboxes (delta or full, depending on connectivity).
    Sync,
    /// Flush queued outgoing messages from the outbox.
    OutboxFlush,
    /// Check for expired snooze reminders and emit `SnoozeExpired` events.
    SnoozeCheck,
    /// Evaluate filter rules against recently arrived messages.
    FilterEvaluation,
}

impl BackgroundTask {
    /// Human-readable task name for platform scheduling APIs.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Sync => "com.kestrel.background.sync",
            Self::OutboxFlush => "com.kestrel.background.outbox",
            Self::SnoozeCheck => "com.kestrel.background.snooze",
            Self::FilterEvaluation => "com.kestrel.background.filter",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_names_are_reverse_dns() {
        assert_eq!(BackgroundTask::Sync.name(), "com.kestrel.background.sync");
        assert_eq!(
            BackgroundTask::OutboxFlush.name(),
            "com.kestrel.background.outbox"
        );
        assert_eq!(
            BackgroundTask::SnoozeCheck.name(),
            "com.kestrel.background.snooze"
        );
        assert_eq!(
            BackgroundTask::FilterEvaluation.name(),
            "com.kestrel.background.filter"
        );
    }

    #[test]
    fn tasks_are_cloneable() {
        let task = BackgroundTask::Sync;
        let cloned = task;
        assert_eq!(task, cloned);
    }
}
