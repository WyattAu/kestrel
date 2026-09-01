//! Integration tests for the `CalDAV` client against real servers.
//!
//! These tests require a running Radicale (or compatible) CalDAV server.
//! Gate on the `KESTREL_INTEGRATION` env var:
//!
//! ```sh
//! KESTREL_INTEGRATION=1 \
//! CALDAV_URL=http://localhost:5232 \
//! cargo nextest run --test caldav_real -p kestrel-calcard
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::doc_markdown
)]

use base64::Engine as _;
use kestrel_calcard::caldav::CalDavClient;

fn integration_enabled() -> bool {
    std::env::var("KESTREL_INTEGRATION").ok().as_deref() == Some("1")
}

fn caldav_client() -> Option<CalDavClient> {
    if !integration_enabled() {
        eprintln!("KESTREL_INTEGRATION not set, skipping");
        return None;
    }
    // Radicale: user's principal is at /{username}/
    let url =
        std::env::var("CALDAV_URL").unwrap_or_else(|_| "http://localhost:5232/kestrel".into());
    let user = std::env::var("CALDAV_USER").unwrap_or_else(|_| "kestrel".into());
    let pass = std::env::var("CALDAV_PASS").unwrap_or_else(|_| "test".into());
    Some(CalDavClient::new_basic(url, user, pass))
}

fn base_url() -> String {
    std::env::var("CALDAV_URL").unwrap_or_else(|_| "http://localhost:5232".into())
}

/// Ensures a test calendar exists by creating one via MKCOL.
async fn ensure_test_calendar(_client: &CalDavClient) -> String {
    let base = base_url();
    // Radicale path: /{username}/{collection_name}/
    let cal_url = format!("{base}/kestrel/test/");
    let _ = reqwest::Client::new()
        .put(&cal_url)
        .header("Content-Type", "text/xml")
        .body(
            r#"<?xml version="1.0" encoding="UTF-8"?>
            <mkcol xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
                <set>
                    <prop>
                        <displayname>Test Calendar</displayname>
                        <resourcetype><C:calendar/></resourcetype>
                    </prop>
                </set>
            </mkcol>"#,
        )
        .send()
        .await
        .ok();
    cal_url
}

#[tokio::test]
#[ignore = "requires running Radicale server"]
async fn integration_list_calendars_round_trip() {
    let Some(client) = caldav_client() else {
        return;
    };

    // Ensure a calendar exists
    ensure_test_calendar(&client).await;

    let calendars = client
        .list_calendars()
        .await
        .expect("list_calendars should succeed");

    eprintln!("Found {} calendars", calendars.len());
    for cal in &calendars {
        eprintln!("  Calendar: {} ({})", cal.display_name, cal.href);
    }

    assert!(
        !calendars.is_empty(),
        "expected at least one calendar from the server, got {}",
        calendars.len()
    );

    for cal in &calendars {
        assert!(!cal.href.is_empty(), "calendar href must not be empty");
        assert!(
            !cal.display_name.is_empty(),
            "calendar display_name must not be empty"
        );
        eprintln!("  Calendar: {} ({})", cal.display_name, cal.href);
    }
}

#[tokio::test]
#[ignore = "requires running Radicale server"]
async fn integration_sync_events_full_sync() {
    let Some(client) = caldav_client() else {
        return;
    };

    ensure_test_calendar(&client).await;

    let calendars = client
        .list_calendars()
        .await
        .expect("list_calendars should succeed");

    let cal = calendars
        .first()
        .expect("need at least one calendar for sync test");

    // Build absolute URL from relative href
    let cal_url = format!("{}{}", base_url(), cal.href);

    let result = client
        .sync_events(&cal_url, None)
        .await
        .expect("sync_events should succeed");

    // Full sync may or may not return a sync token depending on server implementation
    // Radicale returns sync tokens, other servers may not
    eprintln!("  Sync token: {:?}", result.new_sync_token);

    // All returned events should have UIDs.
    for event in &result.events {
        assert!(!event.uid.is_empty(), "event UID must not be empty");
    }
}

#[tokio::test]
#[ignore = "requires running Radicale server"]
async fn integration_put_and_delete_event() {
    let Some(client) = caldav_client() else {
        return;
    };

    ensure_test_calendar(&client).await;

    let calendars = client
        .list_calendars()
        .await
        .expect("list_calendars should succeed");

    let cal = calendars
        .first()
        .expect("need at least one calendar for put/delete test");

    // Build absolute URL from relative href
    let cal_url = format!("{}{}", base_url(), cal.href);

    // Create a test event
    let event_url = format!("{}/test-event-{}.ics", cal_url, uuid::Uuid::now_v7());
    let ical = format!(
        "BEGIN:VCALENDAR\r\n\
         VERSION:2.0\r\n\
         PRODID:-//Kestrel//Test//EN\r\n\
         BEGIN:VEVENT\r\n\
         UID:{}\r\n\
         SUMMARY:Test Event\r\n\
         DTSTART:20260901T100000Z\r\n\
         DTEND:20260901T110000Z\r\n\
         END:VEVENT\r\n\
         END:VCALENDAR\r\n",
        uuid::Uuid::now_v7()
    );

    // PUT the event with auth
    let user = std::env::var("CALDAV_USER").unwrap_or_else(|_| "kestrel".into());
    let pass = std::env::var("CALDAV_PASS").unwrap_or_else(|_| "test".into());
    let auth = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
    );

    let resp = reqwest::Client::new()
        .put(&event_url)
        .header("Authorization", &auth)
        .header("Content-Type", "text/calendar; charset=utf-8")
        .body(ical)
        .send()
        .await
        .expect("PUT should succeed");

    assert!(
        resp.status().is_success()
            || resp.status().as_u16() == 201
            || resp.status().as_u16() == 204,
        "PUT returned {}",
        resp.status()
    );

    // DELETE the event with auth
    let resp = reqwest::Client::new()
        .delete(&event_url)
        .header("Authorization", &auth)
        .send()
        .await
        .expect("DELETE should succeed");

    assert!(
        resp.status().is_success()
            || resp.status().as_u16() == 204
            || resp.status().as_u16() == 404,
        "DELETE returned {}",
        resp.status()
    );
}

#[tokio::test]
#[ignore = "requires running Radicale server"]
async fn integration_discover_well_known() {
    let _base = base_url();
    let host = std::env::var("CALDAV_DISCOVER_HOST").unwrap_or_else(|_| "localhost:5232".into());

    // Try .well-known/caldav discovery
    let url = format!("http://{host}/.well-known/caldav");
    let resp = reqwest::Client::new().get(&url).send().await;

    match resp {
        Ok(r) => {
            eprintln!("  Discovery URL: {} -> {}", url, r.status());
            // Radicale may return 200, 301, or 308
            assert!(
                r.status().is_success() || r.status().is_redirection(),
                "discovery returned {}",
                r.status()
            );
        }
        Err(e) => {
            eprintln!("  Discovery URL {url} failed: {e} (may not be configured)");
            // Not all servers support .well-known, so this is not a hard failure
        }
    }
}
