//! iCalendar (RFC 5545) parser.
//!
//! Parses VCALENDAR/VEVENT components into [`CalendarEvent`] types.
//! Handles: VEVENT, VALARM, RRULE, attendee parsing.

use std::fmt::Write as _;

use crate::types::{AlarmAction, Attendee, AttendeeRole, AttendeeStatus, CalendarEvent};

/// Parses an iCalendar string into a list of events.
///
/// # Errors
/// Returns `Err` if the input is completely unparseable.
pub fn parse_ical(data: &str) -> Result<Vec<CalendarEvent>, String> {
    let mut events = Vec::new();
    let mut in_event = false;
    let mut current = CalendarEvent::default();

    for line in data.lines() {
        let line = line.trim();
        if line == "BEGIN:VEVENT" {
            in_event = true;
            current = CalendarEvent::default();
        } else if line == "END:VEVENT" {
            if in_event {
                events.push(current.clone());
            }
            in_event = false;
        } else if in_event {
            parse_property(line, &mut current);
        }
    }

    Ok(events)
}

fn parse_property(line: &str, event: &mut CalendarEvent) {
    // Handle PROPERTY;PARAM=VALUE format (e.g. ATTENDEE;ROLE=CHAIR:mailto:...)
    if let Some((key_params, value)) = line.split_once(':') {
        if let Some((key, params)) = key_params.split_once(';') {
            match key {
                "ATTENDEE" => {
                    parse_attendee(params, value, event);
                }
                "DTSTART" => {
                    if params.contains("VALUE=DATE") && !params.contains("VALUE=DATE-TIME") {
                        event.start_time = parse_date(value);
                        event.all_day = true;
                    } else {
                        event.start_time = parse_datetime(value);
                    }
                }
                "DTEND" => {
                    if params.contains("VALUE=DATE") && !params.contains("VALUE=DATE-TIME") {
                        event.end_time = parse_date(value);
                    } else {
                        event.end_time = parse_datetime(value);
                    }
                }
                _ => {}
            }
            return;
        }

        match key_params {
            "UID" => event.uid = value.to_string(),
            "SUMMARY" => event.summary = value.to_string(),
            "DESCRIPTION" => event.description = Some(value.to_string()),
            "LOCATION" => event.location = Some(value.to_string()),
            "DTSTART" => event.start_time = parse_datetime(value),
            "DTEND" => event.end_time = parse_datetime(value),
            "RRULE" => event.recurrence = Some(value.to_string()),
            "CREATED" => event.created_at = parse_datetime(value),
            "LAST-MODIFIED" => event.updated_at = parse_datetime(value),
            _ => {}
        }
    }
}

fn parse_attendee(params: &str, email: &str, event: &mut CalendarEvent) {
    let mut name = None;
    let mut role = AttendeeRole::Required;

    for param in params.split(';') {
        if let Some(cn) = param.strip_prefix("CN=") {
            name = Some(cn.to_string());
        } else if let Some(r) = param.strip_prefix("ROLE=") {
            role = match r {
                "CHAIR" => AttendeeRole::Chair,
                "OPT-PARTICIPANT" => AttendeeRole::Optional,
                "NON-PARTICIPANT" => AttendeeRole::NonParticipant,
                _ => AttendeeRole::Required,
            };
        }
    }

    let email_addr = email.strip_prefix("mailto:").unwrap_or(email);
    event.attendees.push(Attendee {
        email: email_addr.to_string(),
        name,
        role,
        status: AttendeeStatus::NeedsAction,
    });
}

fn safe_parse<T: std::str::FromStr>(s: &str, default: T) -> T {
    s.parse().unwrap_or(default)
}

/// Parse iCalendar datetime format (`20260831T143000Z` or `20260831T143000`)
/// into unix milliseconds.
fn parse_datetime(value: &str) -> i64 {
    let clean = value.trim_end_matches('Z');
    if clean.len() < 15 {
        return 0;
    }

    let year: i64 = safe_parse(&clean[0..4], 1970);
    let month: i64 = safe_parse(&clean[4..6], 1);
    let day: i64 = safe_parse(&clean[6..8], 1);
    let hour: i64 = safe_parse(&clean[9..11], 0);
    let min: i64 = safe_parse(&clean[11..13], 0);
    let sec: i64 = safe_parse(&clean[13..15], 0);

    let days = (year - 1970) * 365 + (year - 1970) / 4 - (year - 1970) / 100 + (year - 1970) / 400;
    let month_days = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let idx = usize::try_from(month - 1).unwrap_or(0).min(11);
    let days = days + month_days[idx] + day - 1;

    (days * 86400 + hour * 3600 + min * 60 + sec) * 1000
}

/// Parse iCalendar date format (`20260831`) into unix milliseconds.
fn parse_date(value: &str) -> i64 {
    if value.len() < 8 {
        return 0;
    }

    let year: i64 = safe_parse(&value[0..4], 1970);
    let month: i64 = safe_parse(&value[4..6], 1);
    let day: i64 = safe_parse(&value[6..8], 1);

    let days = (year - 1970) * 365 + (year - 1970) / 4 - (year - 1970) / 100 + (year - 1970) / 400;
    let month_days = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let idx = usize::try_from(month - 1).unwrap_or(0).min(11);
    let days = days + month_days[idx] + day - 1;

    days * 86400 * 1000
}

/// Formats a unix-millisecond timestamp as an iCalendar datetime string.
///
/// Returns `YYYYMMDDTHHMMSSZ` for non-all-day events or `YYYYMMDD` for
/// all-day events.
fn format_datetime_ical(unix_ms: i64, all_day: bool) -> String {
    let secs = unix_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (hours, mins, secs) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );

    // Civil date from day count (algorithm from Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    if all_day {
        format!("{year:04}{month:02}{d:02}")
    } else {
        format!("{year:04}{month:02}{d:02}T{hours:02}{mins:02}{secs:02}Z")
    }
}

/// Serializes a [`CalendarEvent`] into a minimal RFC 5545 iCalendar string.
///
/// The output wraps one `VEVENT` in a `VCALENDAR` container with the
/// required `VERSION:2.0` property.
#[must_use]
pub fn serialize_ical(event: &CalendarEvent) -> String {
    let mut out = String::with_capacity(256);
    let _ = out.write_str("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n");

    if !event.uid.is_empty() {
        let _ = write!(out, "UID:{}\r\n", event.uid);
    }
    if !event.summary.is_empty() {
        let _ = write!(out, "SUMMARY:{}\r\n", event.summary);
    }
    if let Some(ref desc) = event.description {
        let _ = write!(out, "DESCRIPTION:{desc}\r\n");
    }
    if let Some(ref loc) = event.location {
        let _ = write!(out, "LOCATION:{loc}\r\n");
    }

    // DTSTART / DTEND
    if event.all_day {
        let _ = write!(
            out,
            "DTSTART;VALUE=DATE:{}\r\n",
            format_datetime_ical(event.start_time, true)
        );
        let _ = write!(
            out,
            "DTEND;VALUE=DATE:{}\r\n",
            format_datetime_ical(event.end_time, true)
        );
    } else {
        let _ = write!(
            out,
            "DTSTART:{}\r\n",
            format_datetime_ical(event.start_time, false)
        );
        let _ = write!(
            out,
            "DTEND:{}\r\n",
            format_datetime_ical(event.end_time, false)
        );
    }

    if let Some(ref rrule) = event.recurrence {
        let _ = write!(out, "RRULE:{rrule}\r\n");
    }

    // Attendees
    for att in &event.attendees {
        let role_str = match att.role {
            AttendeeRole::Chair => "CHAIR",
            AttendeeRole::Optional => "OPT-PARTICIPANT",
            AttendeeRole::NonParticipant => "NON-PARTICIPANT",
            AttendeeRole::Required => "REQ-PARTICIPANT",
        };
        let cn = att
            .name
            .as_deref()
            .map_or_else(String::new, |n| format!(";CN={n}"));
        let _ = write!(out, "ATTENDEE;ROLE={role_str}{cn}:mailto:{}\r\n", att.email);
    }

    // Alarms
    for alarm in &event.alarms {
        let action_str = match alarm.action {
            AlarmAction::Display => "DISPLAY",
            AlarmAction::Email => "EMAIL",
            AlarmAction::Sound => "SOUND",
        };
        let _ = out.write_str("BEGIN:VALARM\r\n");
        let _ = write!(out, "ACTION:{action_str}\r\n");
        let _ = write!(out, "TRIGGER:{}\r\n", alarm.trigger);
        let _ = out.write_str("END:VALARM\r\n");
    }

    let _ = out.write_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_vevent() {
        let ical = "\
BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:test-uid-1@example.com
SUMMARY:Team Meeting
DESCRIPTION:Weekly sync
LOCATION:Conference Room A
DTSTART:20260831T143000Z
DTEND:20260831T153000Z
RRULE:FREQ=WEEKLY;BYDAY=MO
CREATED:20260801T000000Z
LAST-MODIFIED:20260815T120000Z
END:VEVENT
END:VCALENDAR";

        let events = parse_ical(ical).expect("parse should succeed");
        assert_eq!(events.len(), 1);

        let event = &events[0];
        assert_eq!(event.uid, "test-uid-1@example.com");
        assert_eq!(event.summary, "Team Meeting");
        assert_eq!(event.description.as_deref(), Some("Weekly sync"));
        assert_eq!(event.location.as_deref(), Some("Conference Room A"));
        assert!(event.start_time > 0);
        assert!(event.end_time > event.start_time);
        assert_eq!(event.recurrence.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO"));
        assert!(event.created_at > 0);
        assert!(event.updated_at > 0);
        assert!(!event.all_day);
    }

    #[test]
    fn parse_event_with_attendees() {
        let ical = "\
BEGIN:VEVENT
UID:attendeetest@example.com
SUMMARY:Planning
DTSTART:20260901T100000Z
DTEND:20260901T110000Z
ATTENDEE;ROLE=CHAIR;CN=Alice Smith:mailto:alice@example.com
ATTENDEE;ROLE=OPT-PARTICIPANT;CN=Bob Jones:mailto:bob@example.com
ATTENDEE;CN=Carol White:mailto:carol@example.com
END:VEVENT";

        let events = parse_ical(ical).expect("parse should succeed");
        assert_eq!(events.len(), 1);

        let attendees = &events[0].attendees;
        assert_eq!(attendees.len(), 3);

        assert_eq!(attendees[0].email, "alice@example.com");
        assert_eq!(attendees[0].name.as_deref(), Some("Alice Smith"));
        assert_eq!(attendees[0].role, AttendeeRole::Chair);
        assert_eq!(attendees[0].status, AttendeeStatus::NeedsAction);

        assert_eq!(attendees[1].email, "bob@example.com");
        assert_eq!(attendees[1].role, AttendeeRole::Optional);

        assert_eq!(attendees[2].email, "carol@example.com");
        assert_eq!(attendees[2].role, AttendeeRole::Required);
    }

    #[test]
    fn parse_all_day_event() {
        let ical = "\
BEGIN:VEVENT
UID:allday@example.com
SUMMARY:Holiday
DTSTART;VALUE=DATE:20261225
DTEND;VALUE=DATE:20261226
END:VEVENT";

        let events = parse_ical(ical).expect("parse should succeed");
        assert_eq!(events.len(), 1);
        assert!(events[0].all_day);
        assert!(events[0].start_time > 0);
        assert!(events[0].end_time > events[0].start_time);
    }

    #[test]
    fn parse_multiple_events() {
        let ical = "\
BEGIN:VEVENT
UID:e1@example.com
SUMMARY:Event 1
DTSTART:20260831T090000Z
DTEND:20260831T100000Z
END:VEVENT
BEGIN:VEVENT
UID:e2@example.com
SUMMARY:Event 2
DTSTART:20260831T110000Z
DTEND:20260831T120000Z
END:VEVENT";

        let events = parse_ical(ical).expect("parse should succeed");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].uid, "e1@example.com");
        assert_eq!(events[1].uid, "e2@example.com");
    }

    #[test]
    fn parse_empty_input() {
        let events = parse_ical("").expect("parse should succeed");
        assert!(events.is_empty());
    }

    #[test]
    fn parse_no_events() {
        let ical = "BEGIN:VCALENDAR\nVERSION:2.0\nEND:VCALENDAR";
        let events = parse_ical(ical).expect("parse should succeed");
        assert!(events.is_empty());
    }

    #[test]
    fn parse_malformed_lines_ignored() {
        let ical = "\
BEGIN:VEVENT
UID:malformed@example.com
NOT_A_REAL_PROPERTY
SUMMARY:Still Works
DTSTART:not-a-date
END:VEVENT";

        let events = parse_ical(ical).expect("parse should succeed");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].summary, "Still Works");
    }

    #[test]
    fn parse_unmatched_begin_ignored() {
        let ical = "BEGIN:VEVENT\nUID:unmatched@example.com";
        let events = parse_ical(ical).expect("parse should succeed");
        // No END:VEVENT, so event is not pushed
        assert!(events.is_empty());
    }

    #[test]
    fn datetime_parsing_valid() {
        let ts = parse_datetime("20260831T143000Z");
        assert!(ts > 0);
        // Should be the same regardless of trailing Z
        let ts2 = parse_datetime("20260831T143000");
        assert_eq!(ts, ts2);
    }

    #[test]
    fn datetime_parsing_short_returns_zero() {
        assert_eq!(parse_datetime("bad"), 0);
        assert_eq!(parse_datetime(""), 0);
    }

    #[test]
    fn date_parsing_valid() {
        let ts = parse_date("20260831");
        assert!(ts > 0);
    }

    #[test]
    fn date_parsing_short_returns_zero() {
        assert_eq!(parse_date("bad"), 0);
        assert_eq!(parse_date(""), 0);
    }

    #[test]
    fn serialize_roundtrip() {
        let event = CalendarEvent {
            id: String::new(),
            calendar_id: "cal-1".into(),
            account_id: kestrel_core::ids::AccountId::from_uuid(uuid::Uuid::nil()),
            uid: "test@example.com".into(),
            summary: "Test Event".into(),
            description: Some("A description".into()),
            location: Some("Room A".into()),
            start_time: parse_datetime("20260901T100000Z"),
            end_time: parse_datetime("20260901T110000Z"),
            all_day: false,
            recurrence: None,
            attendees: vec![Attendee {
                email: "alice@example.com".into(),
                name: Some("Alice".into()),
                role: AttendeeRole::Required,
                status: AttendeeStatus::NeedsAction,
            }],
            alarms: vec![],
            ical_data: None,
            created_at: 0,
            updated_at: 0,
        };
        let ical = serialize_ical(&event);
        assert!(ical.contains("BEGIN:VCALENDAR"));
        assert!(ical.contains("UID:test@example.com"));
        assert!(ical.contains("SUMMARY:Test Event"));
        assert!(ical.contains("DESCRIPTION:A description"));
        assert!(ical.contains("LOCATION:Room A"));
        assert!(ical.contains("DTSTART:20260901T100000Z"));
        assert!(ical.contains("DTEND:20260901T110000Z"));
        assert!(ical.contains("ATTENDEE;ROLE=REQ-PARTICIPANT;CN=Alice:mailto:alice@example.com"));
        assert!(ical.contains("END:VCALENDAR"));
    }

    #[test]
    fn serialize_all_day() {
        let event = CalendarEvent {
            uid: "allday@test.com".into(),
            summary: "Holiday".into(),
            start_time: parse_datetime("20261225T000000Z"),
            end_time: parse_datetime("20261226T000000Z"),
            all_day: true,
            ..CalendarEvent::default()
        };
        let ical = serialize_ical(&event);
        assert!(ical.contains("DTSTART;VALUE=DATE:20261225"));
        assert!(ical.contains("DTEND;VALUE=DATE:20261226"));
    }
}
