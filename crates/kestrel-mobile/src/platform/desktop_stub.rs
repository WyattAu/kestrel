//! Desktop stub for development/testing.
//!
//! Provides no-op implementations for platform-specific functions.

use crate::push::PushNotification;

/// Schedules a background sync task (no-op on desktop).
///
/// # Errors
///
/// This function never returns an error.
pub fn schedule_background_sync(_interval_minutes: u32) -> Result<(), String> {
    Ok(())
}

/// Sends a push notification (no-op on desktop).
///
/// # Errors
///
/// This function never returns an error.
pub fn send_push_notification(_notification: &PushNotification) -> Result<(), String> {
    Ok(())
}

/// Stores a credential (no-op on desktop).
///
/// # Errors
///
/// This function never returns an error.
pub fn store_credential(_key: &str, _value: &str) -> Result<(), String> {
    Ok(())
}

/// Retrieves a credential (always returns `None` on desktop).
///
/// # Errors
///
/// This function never returns an error.
pub fn get_credential(_key: &str) -> Result<Option<String>, String> {
    Ok(None)
}
