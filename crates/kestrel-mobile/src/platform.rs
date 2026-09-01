//! Platform-specific implementations for iOS and Android.
//!
//! This module provides a unified interface that dispatches to the correct
//! platform implementation based on the compile target. On desktop, stub
//! implementations are used for development and testing.

#[cfg(target_os = "ios")]
pub mod ios;

#[cfg(target_os = "android")]
pub mod android;

#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub mod desktop_stub;

use crate::push::PushNotification;

/// Schedules a background sync task using the platform's native scheduler.
///
/// # Errors
///
/// Returns an error if the platform scheduler rejects the request.
pub fn schedule_background_sync(interval_minutes: u32) -> Result<(), String> {
    #[cfg(target_os = "ios")]
    {
        ios::schedule_background_sync(interval_minutes)
    }
    #[cfg(target_os = "android")]
    {
        android::schedule_background_sync(interval_minutes)
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        desktop_stub::schedule_background_sync(interval_minutes)
    }
}

/// Sends a push notification using the platform's native service.
///
/// # Errors
///
/// Returns an error if the notification cannot be delivered.
pub fn send_push_notification(notification: &PushNotification) -> Result<(), String> {
    #[cfg(target_os = "ios")]
    {
        let config = ios::IosPushConfig {
            apns_token: String::new(),
            bundle_id: String::from("com.kestrel.mail"),
        };
        ios::send_apns_notification(&config, notification)
    }
    #[cfg(target_os = "android")]
    {
        let config = android::AndroidPushConfig {
            fcm_token: String::new(),
            project_id: String::new(),
        };
        android::send_fcm_notification(&config, notification)
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        desktop_stub::send_push_notification(notification)
    }
}

/// Stores a credential using the platform's native secure storage.
///
/// # Errors
///
/// Returns an error if the platform secure storage is unavailable.
pub fn store_credential(key: &str, value: &str) -> Result<(), String> {
    #[cfg(target_os = "ios")]
    {
        ios::store_credential_keychain(key, value)
    }
    #[cfg(target_os = "android")]
    {
        android::store_credential_keystore(key, value)
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        desktop_stub::store_credential(key, value)
    }
}

/// Retrieves a credential using the platform's native secure storage.
///
/// # Errors
///
/// Returns an error if the platform secure storage is unavailable.
pub fn get_credential(key: &str) -> Result<Option<String>, String> {
    #[cfg(target_os = "ios")]
    {
        ios::get_credential_keychain(key)
    }
    #[cfg(target_os = "android")]
    {
        android::get_credential_keystore(key)
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        desktop_stub::get_credential(key)
    }
}
