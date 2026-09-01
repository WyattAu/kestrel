//! Push notification stubs for mobile platforms.
//!
//! iOS uses APNs (Apple Push Notification service); Android uses FCM
//! (Firebase Cloud Messaging). This module defines the notification
//! vocabulary and provider abstraction. Actual platform integration
//! (token registration, delivery) is future work.

/// Push notification provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushProvider {
    /// Apple Push Notification service (iOS).
    Apns,
    /// Firebase Cloud Messaging (Android).
    Fcm,
}

/// A push notification to be displayed on the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushNotification {
    /// Notification title (typically the sender name).
    pub title: String,
    /// Notification body (typically the subject line).
    pub body: String,
    /// Account id that received the message.
    pub account_id: String,
    /// Folder id the message was delivered to, if known.
    pub folder_id: Option<String>,
}

impl PushNotification {
    /// Creates a new push notification.
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        body: impl Into<String>,
        account_id: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            account_id: account_id.into(),
            folder_id: None,
        }
    }

    /// Sets the folder id.
    #[must_use]
    pub fn with_folder(mut self, folder_id: impl Into<String>) -> Self {
        self.folder_id = Some(folder_id.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_construction() {
        let n = PushNotification::new("Alice", "Meeting tomorrow", "acct-1");
        assert_eq!(n.title, "Alice");
        assert_eq!(n.body, "Meeting tomorrow");
        assert_eq!(n.account_id, "acct-1");
        assert!(n.folder_id.is_none());
    }

    #[test]
    fn notification_with_folder() {
        let n = PushNotification::new("Bob", "Re: Hi", "acct-2").with_folder("inbox-3");
        assert_eq!(n.folder_id.as_deref(), Some("inbox-3"));
    }

    #[test]
    fn push_provider_variants() {
        let apns = PushProvider::Apns;
        let fcm = PushProvider::Fcm;
        assert_ne!(apns, fcm);
    }
}
