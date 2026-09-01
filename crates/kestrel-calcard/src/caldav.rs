//! `CalDAV` client for calendar synchronization (RFC 4791).
//!
//! # Protocol Overview
//!
//! 1. **Discovery** (`/.well-known/caldav`): resolve the calendar home URL.
//! 2. **PROPFIND** on calendar home: list available calendars.
//! 3. **REPORT** with `sync-collection`: delta sync events.
//! 4. **PUT/DELETE**: create/update/remove events.

use std::sync::LazyLock;

use kestrel_core::error::KestrelError;
use reqwest::Method;
use tracing::instrument;

use crate::types::CalendarEvent;

/// Authentication method for the `CalDAV` client.
#[derive(Clone)]
pub enum AuthMethod {
    /// OAuth 2.0 Bearer token.
    Bearer(String),
    /// HTTP Basic authentication with username and password.
    Basic {
        /// Username for Basic auth.
        username: String,
        /// Password for Basic auth.
        password: String,
    },
}

static PROPFIND: LazyLock<Method> =
    LazyLock::new(|| Method::from_bytes(b"PROPFIND").unwrap_or(Method::GET));
static REPORT: LazyLock<Method> =
    LazyLock::new(|| Method::from_bytes(b"REPORT").unwrap_or(Method::GET));

/// A `CalDAV` calendar as discovered from the server.
#[derive(Clone, Debug)]
pub struct CalDavCalendar {
    /// Calendar display name.
    pub display_name: String,
    /// `CalDAV` href path.
    pub href: String,
    /// Sync token for incremental sync.
    pub sync_token: Option<String>,
}

/// Result of a `sync-collection` REPORT.
#[derive(Clone, Debug)]
pub struct CalDavSyncResult {
    /// Events changed since the last sync.
    pub events: Vec<CalendarEvent>,
    /// New sync token to persist for the next sync.
    pub new_sync_token: Option<String>,
    /// Hrefs of events deleted since the last sync.
    pub deleted_urls: Vec<String>,
}

/// `CalDAV` client.
pub struct CalDavClient {
    base_url: String,
    auth: AuthMethod,
    http: reqwest::Client,
}

impl CalDavClient {
    /// Creates a new `CalDAV` client with a known base URL and bearer token.
    #[must_use]
    pub fn new(base_url: String, auth_token: String) -> Self {
        Self {
            base_url,
            auth: AuthMethod::Bearer(auth_token),
            http: reqwest::Client::new(),
        }
    }

    /// Creates a new `CalDAV` client with Basic authentication.
    #[must_use]
    pub fn new_basic(base_url: String, username: String, password: String) -> Self {
        Self {
            base_url,
            auth: AuthMethod::Basic { username, password },
            http: reqwest::Client::new(),
        }
    }

    /// Discovers the `CalDAV` endpoint via `/.well-known/caldav` (RFC 6764).
    ///
    /// Returns a client with the resolved base URL. The auth token must be
    /// set via [`set_auth_token`](Self::set_auth_token) before making
    /// authenticated requests.
    ///
    /// # Errors
    /// Returns [`KestrelError::ConnectionLost`] if the discovery request fails.
    #[instrument(skip_all, fields(host))]
    pub async fn discover(host: &str) -> Result<Self, KestrelError> {
        let url = format!("https://{host}/.well-known/caldav");
        let resp = reqwest::Client::new().get(&url).send().await.map_err(|e| {
            KestrelError::ConnectionLost {
                detail: e.to_string(),
            }
        })?;
        let location = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(&url)
            .to_string();
        Ok(Self {
            base_url: location,
            auth: AuthMethod::Bearer(String::new()),
            http: reqwest::Client::new(),
        })
    }

    /// Sets or replaces the auth token on an existing client.
    pub fn set_auth_token(&mut self, token: String) {
        self.auth = AuthMethod::Bearer(token);
    }

    fn auth_header(&self) -> String {
        use base64::Engine;
        match &self.auth {
            AuthMethod::Bearer(token) => format!("Bearer {token}"),
            AuthMethod::Basic { username, password } => {
                let creds = base64::engine::general_purpose::STANDARD
                    .encode(format!("{username}:{password}"));
                format!("Basic {creds}")
            }
        }
    }

    /// Lists calendars via `PROPFIND` Depth:1 on the calendar home.
    ///
    /// # Errors
    /// Returns [`KestrelError::ConnectionLost`] on transport failure.
    #[instrument(skip(self))]
    pub async fn list_calendars(&self) -> Result<Vec<CalDavCalendar>, KestrelError> {
        let body = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<d:propfind xmlns:d=\"DAV:\">
  <d:prop>
    <d:displayname/>
    <d:resourcetype/>
    <d:getctag/>
  </d:prop>
</d:propfind>";

        let resp = self
            .http
            .request(PROPFIND.clone(), &self.base_url)
            .header("Authorization", self.auth_header())
            .header("Depth", "1")
            .header("Content-Type", "application/xml")
            .body(body)
            .send()
            .await
            .map_err(|e| KestrelError::ConnectionLost {
                detail: e.to_string(),
            })?;

        let text = resp
            .text()
            .await
            .map_err(|e| KestrelError::ConnectionLost {
                detail: e.to_string(),
            })?;

        Ok(parse_propfind_response(&text))
    }

    /// Syncs events from a calendar using `sync-collection` REPORT.
    ///
    /// When `sync_token` is `None`, a full sync is performed. Otherwise only
    /// changes since the given token are returned.
    ///
    /// # Errors
    /// Returns [`KestrelError::ConnectionLost`] on transport failure.
    #[instrument(skip(self))]
    pub async fn sync_events(
        &self,
        calendar_url: &str,
        sync_token: Option<&str>,
    ) -> Result<CalDavSyncResult, KestrelError> {
        let token = sync_token.unwrap_or("");
        let body = format!(
            "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<d:sync-collection xmlns:d=\"DAV:\">
  <d:synclevel>1</d:synclevel>
  <d:prop>
    <d:getetag/>
    <d:getcontenttype/>
    <d:displayname/>
  </d:prop>
  <d:sync-token>{token}</d:sync-token>
</d:sync-collection>"
        );

        let resp = self
            .http
            .request(REPORT.clone(), calendar_url)
            .header("Authorization", self.auth_header())
            .header("Depth", "1")
            .header("Content-Type", "application/xml")
            .body(body)
            .send()
            .await
            .map_err(|e| KestrelError::ConnectionLost {
                detail: e.to_string(),
            })?;

        let text = resp
            .text()
            .await
            .map_err(|e| KestrelError::ConnectionLost {
                detail: e.to_string(),
            })?;

        Ok(parse_sync_collection_response(&text))
    }

    /// Creates or updates an event via `PUT`.
    ///
    /// If `etag` is provided, an `If-Match` header is sent for conditional
    /// updates.
    ///
    /// # Errors
    /// Returns [`KestrelError::ConnectionLost`] on transport failure.
    #[instrument(skip(self, ical_data))]
    pub async fn put_event(
        &self,
        event_url: &str,
        ical_data: &str,
        etag: Option<&str>,
    ) -> Result<(), KestrelError> {
        let mut req = self
            .http
            .put(event_url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "text/calendar; charset=utf-8")
            .body(ical_data.to_string());

        if let Some(etag_val) = etag {
            req = req.header("If-Match", etag_val);
        }

        req.send().await.map_err(|e| KestrelError::ConnectionLost {
            detail: e.to_string(),
        })?;

        Ok(())
    }

    /// Deletes an event via `DELETE`.
    ///
    /// If `etag` is provided, an `If-Match` header is sent for conditional
    /// deletes.
    ///
    /// # Errors
    /// Returns [`KestrelError::ConnectionLost`] on transport failure.
    #[instrument(skip(self))]
    pub async fn delete_event(
        &self,
        event_url: &str,
        etag: Option<&str>,
    ) -> Result<(), KestrelError> {
        let mut req = self
            .http
            .delete(event_url)
            .header("Authorization", self.auth_header());

        if let Some(etag_val) = etag {
            req = req.header("If-Match", etag_val);
        }

        req.send().await.map_err(|e| KestrelError::ConnectionLost {
            detail: e.to_string(),
        })?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// XML response parsing
// ---------------------------------------------------------------------------

fn parse_propfind_response(xml: &str) -> Vec<CalDavCalendar> {
    let mut calendars = Vec::new();

    // Handle both <response> and <d:response> formats
    let patterns = ["<response>", "<d:response>"];
    let close_patterns = ["</response>", "</d:response>"];

    for (open, close) in patterns.iter().zip(close_patterns.iter()) {
        for response_block in xml.split(open).skip(1) {
            let Some(end) = response_block.find(close) else {
                continue;
            };
            let response = &response_block[..end];

            // Check if this is a collection (calendar or addressbook)
            // Skip principal collections (user's home directory)
            let has_collection = response.contains("collection");
            if !has_collection {
                continue;
            }
            // Skip principal collections
            if response.contains("principal") {
                continue;
            }

            // Extract href (handles both d:href and href)
            let href = extract_tag(response, "d:href")
                .or_else(|| extract_tag(response, "href"))
                .unwrap_or_default();
            let display_name = extract_tag(response, "d:displayname")
                .or_else(|| extract_tag(response, "displayname"))
                .unwrap_or_default();
            let sync_token =
                extract_tag(response, "d:getctag").or_else(|| extract_tag(response, "getctag"));

            // Avoid duplicates
            if !calendars.iter().any(|c: &CalDavCalendar| c.href == href) {
                calendars.push(CalDavCalendar {
                    display_name,
                    href,
                    sync_token,
                });
            }
        }
    }

    calendars
}

fn parse_sync_collection_response(xml: &str) -> CalDavSyncResult {
    let mut events = Vec::new();
    let mut deleted_urls = Vec::new();
    let new_sync_token = extract_tag(xml, "d:sync-token");

    for response_block in xml.split("<d:response>").skip(1) {
        let Some(end) = response_block.find("</d:response>") else {
            continue;
        };
        let response = &response_block[..end];
        let href = extract_tag(response, "d:href").unwrap_or_default();

        // Deleted resources are indicated by 404/410 status.
        if response.contains("<d:status>HTTP/1.1 404")
            || response.contains("<d:status>HTTP/1.1 410")
        {
            deleted_urls.push(href);
            continue;
        }

        if let Some(calendar_data) = extract_tag(response, "cal:calendar-data")
            && let Ok(mut parsed) = crate::ical::parse_ical(&calendar_data)
        {
            for mut event in parsed.drain(..) {
                event.id.clone_from(&href);
                events.push(event);
            }
        }
    }

    CalDavSyncResult {
        events,
        new_sync_token,
        deleted_urls,
    }
}

fn extract_tag(xml: &str, tag: &str) -> Option<String> {
    let start_tag = format!("<{tag}>");
    let end_tag = format!("</{tag}>");
    let start = xml.find(&start_tag)?;
    let content_start = start + start_tag.len();
    let rest = &xml[content_start..];
    let end = rest.find(&end_tag)?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_propfind_single_calendar() {
        let xml = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<d:multistatus xmlns:d=\"DAV:\">
  <d:response>
    <d:href>/calendars/user/default/</d:href>
    <d:propstat>
      <d:prop>
        <d:displayname>My Calendar</d:displayname>
        <d:resourcetype><d:collection/></d:resourcetype>
        <d:getctag>ctag-1</d:getctag>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
</d:multistatus>";

        let calendars = parse_propfind_response(xml);
        assert_eq!(calendars.len(), 1);
        assert_eq!(calendars[0].display_name, "My Calendar");
        assert_eq!(calendars[0].href, "/calendars/user/default/");
        assert_eq!(calendars[0].sync_token.as_deref(), Some("ctag-1"));
    }

    #[test]
    fn parse_propfind_skips_non_collections() {
        let xml = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<d:multistatus xmlns:d=\"DAV:\">
  <d:response>
    <d:href>/calendars/user/default/file.ics</d:href>
    <d:propstat>
      <d:prop>
        <d:displayname>event.ics</d:displayname>
        <d:resourcetype/>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/calendars/user/default/</d:href>
    <d:propstat>
      <d:prop>
        <d:displayname>Work</d:displayname>
        <d:resourcetype><d:collection/></d:resourcetype>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
</d:multistatus>";

        let calendars = parse_propfind_response(xml);
        assert_eq!(calendars.len(), 1);
        assert_eq!(calendars[0].display_name, "Work");
    }

    #[test]
    fn parse_propfind_empty_response() {
        let xml = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<d:multistatus xmlns:d=\"DAV:\">
</d:multistatus>";

        let calendars = parse_propfind_response(xml);
        assert!(calendars.is_empty());
    }

    #[test]
    fn parse_sync_collection_with_events() {
        let xml = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<d:multistatus xmlns:d=\"DAV:\" xmlns:cal=\"urn:ietf:params:xml:ns:caldav\">
  <d:response>
    <d:href>/calendars/user/default/event1.ics</d:href>
    <d:propstat>
      <d:prop>
        <d:getetag>\"etag-1\"</d:getetag>
        <cal:calendar-data>BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:event1@example.com
SUMMARY:Team Standup
DTSTART:20260831T090000Z
DTEND:20260831T093000Z
END:VEVENT
END:VCALENDAR</cal:calendar-data>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:sync-token>new-sync-token-42</d:sync-token>
</d:multistatus>";

        let result = parse_sync_collection_response(xml);
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].uid, "event1@example.com");
        assert_eq!(result.events[0].summary, "Team Standup");
        assert_eq!(result.events[0].id, "/calendars/user/default/event1.ics");
        assert_eq!(result.new_sync_token.as_deref(), Some("new-sync-token-42"));
        assert!(result.deleted_urls.is_empty());
    }

    #[test]
    fn parse_sync_collection_with_deleted() {
        let xml = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<d:multistatus xmlns:d=\"DAV:\">
  <d:response>
    <d:href>/calendars/user/default/old.ics</d:href>
    <d:propstat>
      <d:prop/>
      <d:status>HTTP/1.1 404 Not Found</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/calendars/user/default/gone.ics</d:href>
    <d:propstat>
      <d:prop/>
      <d:status>HTTP/1.1 410 Gone</d:status>
    </d:propstat>
  </d:response>
  <d:sync-token>tok-2</d:sync-token>
</d:multistatus>";

        let result = parse_sync_collection_response(xml);
        assert!(result.events.is_empty());
        assert_eq!(result.deleted_urls.len(), 2);
        assert_eq!(result.deleted_urls[0], "/calendars/user/default/old.ics");
        assert_eq!(result.deleted_urls[1], "/calendars/user/default/gone.ics");
        assert_eq!(result.new_sync_token.as_deref(), Some("tok-2"));
    }

    #[test]
    fn extract_tag_basic() {
        let xml = "<d:href>/path</d:href>";
        assert_eq!(extract_tag(xml, "d:href").as_deref(), Some("/path"));
    }

    #[test]
    fn extract_tag_missing() {
        let xml = "<d:href>/path</d:href>";
        assert!(extract_tag(xml, "d:displayname").is_none());
    }

    #[test]
    fn extract_tag_empty_content() {
        let xml = "<d:displayname></d:displayname>";
        assert_eq!(extract_tag(xml, "d:displayname").as_deref(), Some(""));
    }

    #[test]
    fn auth_header_bearer() {
        let client = CalDavClient::new("http://localhost".into(), "my-token".into());
        assert_eq!(client.auth_header(), "Bearer my-token");
    }

    #[test]
    fn auth_header_basic() {
        use base64::Engine;
        let client =
            CalDavClient::new_basic("http://localhost".into(), "user".into(), "pass".into());
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("user:pass")
        );
        assert_eq!(client.auth_header(), expected);
    }
}
