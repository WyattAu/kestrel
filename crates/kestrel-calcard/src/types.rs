//! Calendar and contact domain types.
//!
//! These types mirror the iCalendar (RFC 5545) and vCard (RFC 6350) data
//! models used by `CalDAV` and `CardDAV` servers. They are the local
//! representation stored in `data.db` and used by the engine and frontends.

use kestrel_core::ids::AccountId;

/// A calendar on a `CalDAV` server.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Calendar {
    /// Local storage id.
    pub id: String,
    /// Owning account.
    pub account_id: AccountId,
    /// Server display name.
    pub display_name: String,
    /// Calendar color (CSS hex, e.g. `#FF5722`).
    pub color: Option<String>,
    /// `CalDAV` sync token for delta sync.
    pub sync_token: Option<String>,
    /// Unix ms creation timestamp.
    pub created_at: i64,
    /// Unix ms last-modified timestamp.
    pub updated_at: i64,
}

/// An address book on a `CardDAV` server.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AddressBook {
    /// Local storage id.
    pub id: String,
    /// Owning account.
    pub account_id: AccountId,
    /// Server display name.
    pub display_name: String,
    /// `CardDAV` sync token for delta sync.
    pub sync_token: Option<String>,
    /// Unix ms creation timestamp.
    pub created_at: i64,
    /// Unix ms last-modified timestamp.
    pub updated_at: i64,
}

impl Default for CalendarEvent {
    fn default() -> Self {
        Self {
            id: String::new(),
            calendar_id: String::new(),
            account_id: AccountId::from_uuid(uuid::Uuid::nil()),
            uid: String::new(),
            summary: String::new(),
            description: None,
            location: None,
            start_time: 0,
            end_time: 0,
            all_day: false,
            recurrence: None,
            attendees: Vec::new(),
            alarms: Vec::new(),
            ical_data: None,
            created_at: 0,
            updated_at: 0,
        }
    }
}

/// A calendar event (VEVENT).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CalendarEvent {
    /// Local storage id.
    pub id: String,
    /// Calendar this event belongs to.
    pub calendar_id: String,
    /// Owning account.
    pub account_id: AccountId,
    /// iCalendar UID (RFC 5545 §3.8.4.7).
    pub uid: String,
    /// Event summary (title).
    pub summary: String,
    /// Free-text description.
    pub description: Option<String>,
    /// Location string.
    pub location: Option<String>,
    /// Start time as unix milliseconds.
    pub start_time: i64,
    /// End time as unix milliseconds.
    pub end_time: i64,
    /// Whether this is an all-day event.
    pub all_day: bool,
    /// RRULE string for recurring events (RFC 5545 §3.8.5.3).
    pub recurrence: Option<String>,
    /// Attendees.
    pub attendees: Vec<Attendee>,
    /// Alarms / reminders.
    pub alarms: Vec<Alarm>,
    /// Raw iCalendar data for round-trip fidelity.
    pub ical_data: Option<String>,
    /// Unix ms creation timestamp.
    pub created_at: i64,
    /// Unix ms last-modified timestamp.
    pub updated_at: i64,
}

impl Default for Contact {
    fn default() -> Self {
        Self {
            id: String::new(),
            address_book_id: String::new(),
            account_id: AccountId::from_uuid(uuid::Uuid::nil()),
            uid: String::new(),
            display_name: String::new(),
            given_name: None,
            family_name: None,
            email_addresses: Vec::new(),
            phone_numbers: Vec::new(),
            organization: None,
            photo: None,
            vcard_data: None,
            created_at: 0,
            updated_at: 0,
        }
    }
}

/// A contact (vCard).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Contact {
    /// Local storage id.
    pub id: String,
    /// Address book this contact belongs to.
    pub address_book_id: String,
    /// Owning account.
    pub account_id: AccountId,
    /// vCard UID (RFC 6350 §6.7.6).
    pub uid: String,
    /// Display name.
    pub display_name: String,
    /// Given (first) name.
    pub given_name: Option<String>,
    /// Family (last) name.
    pub family_name: Option<String>,
    /// Email addresses.
    pub email_addresses: Vec<EmailAddr>,
    /// Phone numbers.
    pub phone_numbers: Vec<Phone>,
    /// Organization / company.
    pub organization: Option<String>,
    /// Raw photo bytes (JPEG/PNG).
    pub photo: Option<Vec<u8>>,
    /// Raw vCard data for round-trip fidelity.
    pub vcard_data: Option<String>,
    /// Unix ms creation timestamp.
    pub created_at: i64,
    /// Unix ms last-modified timestamp.
    pub updated_at: i64,
}

/// An attendee on a calendar event.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Attendee {
    /// Email address (calendar user address).
    pub email: String,
    /// Display name.
    pub name: Option<String>,
    /// Role in the event.
    pub role: AttendeeRole,
    /// Participation status.
    pub status: AttendeeStatus,
}

/// Participant role (RFC 5545 §3.2.16).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AttendeeRole {
    /// REQ-PARTICIPANT; role=CHAIR.
    Chair,
    /// REQ-PARTICIPANT (default).
    Required,
    /// OPT-PARTICIPANT.
    Optional,
    /// NON-PARTICIPANT.
    NonParticipant,
}

/// Participation status (RFC 5545 §3.2.12).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AttendeeStatus {
    /// Needs-action (default).
    NeedsAction,
    /// Accepted.
    Accepted,
    /// Declined.
    Declined,
    /// Tentative.
    Tentative,
    /// Delegated.
    Delegated,
}

/// An alarm / reminder on a calendar event.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Alarm {
    /// Trigger offset in milliseconds (negative = before start).
    pub trigger: i64,
    /// Alarm action.
    pub action: AlarmAction,
}

/// Alarm action type (RFC 5545 §3.6.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlarmAction {
    /// Display a notification.
    Display,
    /// Send an email.
    Email,
    /// Play a sound.
    Sound,
}

/// An email address with an optional label.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EmailAddr {
    /// Email address.
    pub address: String,
    /// Label (e.g. "work", "home").
    pub label: Option<String>,
}

/// A phone number with an optional label.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Phone {
    /// Phone number.
    pub number: String,
    /// Label (e.g. "work", "home", "mobile").
    pub label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_event_serializes() {
        let event = CalendarEvent {
            id: "evt-1".into(),
            calendar_id: "cal-1".into(),
            account_id: kestrel_core::ids::AccountId::from_uuid(uuid::Uuid::nil()),
            uid: "uid-1@example.com".into(),
            summary: "Test Event".into(),
            description: None,
            location: None,
            start_time: 1_700_000_000_000,
            end_time: 1_700_003_600_000,
            all_day: false,
            recurrence: None,
            attendees: vec![],
            alarms: vec![],
            ical_data: None,
            created_at: 1_700_000_000_000,
            updated_at: 1_700_000_000_000,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let deserialized: CalendarEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(event, deserialized);
    }

    #[test]
    fn contact_serializes() {
        let contact = Contact {
            id: "c-1".into(),
            address_book_id: "ab-1".into(),
            account_id: kestrel_core::ids::AccountId::from_uuid(uuid::Uuid::nil()),
            uid: "uid-2@example.com".into(),
            display_name: "Jane Doe".into(),
            given_name: Some("Jane".into()),
            family_name: Some("Doe".into()),
            email_addresses: vec![EmailAddr {
                address: "jane@example.com".into(),
                label: Some("work".into()),
            }],
            phone_numbers: vec![Phone {
                number: "+1-555-0100".into(),
                label: Some("work".into()),
            }],
            organization: Some("Example Corp".into()),
            photo: None,
            vcard_data: None,
            created_at: 1_700_000_000_000,
            updated_at: 1_700_000_000_000,
        };
        let json = serde_json::to_string(&contact).expect("serialize");
        let deserialized: Contact = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(contact, deserialized);
    }

    #[test]
    fn attendee_roles_serialize() {
        let roles = [
            AttendeeRole::Required,
            AttendeeRole::Optional,
            AttendeeRole::Chair,
            AttendeeRole::NonParticipant,
        ];
        for role in &roles {
            let json = serde_json::to_string(role).expect("serialize");
            let back: AttendeeRole = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*role, back);
        }
    }

    #[test]
    fn attendee_status_serializes() {
        let statuses = [
            AttendeeStatus::NeedsAction,
            AttendeeStatus::Accepted,
            AttendeeStatus::Declined,
            AttendeeStatus::Tentative,
            AttendeeStatus::Delegated,
        ];
        for status in &statuses {
            let json = serde_json::to_string(status).expect("serialize");
            let back: AttendeeStatus = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*status, back);
        }
    }
}
