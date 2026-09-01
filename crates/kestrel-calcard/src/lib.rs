//! `kestrel-calcard` — `CalDAV`/`CardDAV` types and client stubs.
//!
//! Provides calendar event and contact domain types for `CalDAV`/`CardDAV`
//! integration. Client stubs document the protocol (RFC 4791, RFC 6352)
//! without full implementation; they return `FeatureNotYetAvailable` errors.
//!
//! Calendar and contact data lives in `data.db` (durable, ADR 0009) since
//! contacts are not re-fetchable from a mail server and events represent
//! user-created data that must survive cache wipes.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, missing_docs))]

pub mod caldav;
pub mod carddav;
pub mod ical;
pub mod types;
pub mod vcard;

pub use caldav::{AuthMethod as CalDavAuthMethod, CalDavCalendar, CalDavClient, CalDavSyncResult};
pub use carddav::{
    AuthMethod as CardDavAuthMethod, CardDavAddressBook, CardDavClient, CardDavSyncResult,
};
pub use ical::{parse_ical, serialize_ical};
pub use types::{
    AddressBook, Alarm, AlarmAction, Attendee, AttendeeRole, AttendeeStatus, Calendar,
    CalendarEvent, Contact, EmailAddr, Phone,
};
pub use vcard::parse_vcard;
