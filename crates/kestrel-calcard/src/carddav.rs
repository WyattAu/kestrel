//! `CardDAV` client for contact synchronization (RFC 6352).
//!
//! # Protocol Overview
//!
//! 1. **Discovery** (`/.well-known/carddav`): resolve the addressbook home URL.
//! 2. **PROPFIND** on addressbook home: list available address books.
//! 3. **REPORT** with `sync-collection`: delta sync contacts.
//! 4. **PUT/DELETE**: create/update/remove contacts.

use std::sync::LazyLock;

use kestrel_core::error::KestrelError;
use reqwest::Method;
use tracing::instrument;

use crate::types::Contact;

/// Authentication method for the `CardDAV` client.
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

/// A `CardDAV` address book as discovered from the server.
#[derive(Clone, Debug)]
pub struct CardDavAddressBook {
    /// Address book display name.
    pub display_name: String,
    /// `CardDAV` href path.
    pub href: String,
    /// Sync token for incremental sync.
    pub sync_token: Option<String>,
}

/// Result of a `sync-collection` REPORT.
#[derive(Clone, Debug)]
pub struct CardDavSyncResult {
    /// Contacts changed since the last sync.
    pub contacts: Vec<Contact>,
    /// New sync token to persist for the next sync.
    pub new_sync_token: Option<String>,
    /// Hrefs of contacts deleted since the last sync.
    pub deleted_urls: Vec<String>,
}

/// `CardDAV` client.
pub struct CardDavClient {
    base_url: String,
    auth: AuthMethod,
    http: reqwest::Client,
}

impl CardDavClient {
    /// Creates a new `CardDAV` client with a known base URL and bearer token.
    #[must_use]
    pub fn new(base_url: String, auth_token: String) -> Self {
        Self {
            base_url,
            auth: AuthMethod::Bearer(auth_token),
            http: reqwest::Client::new(),
        }
    }

    /// Creates a new `CardDAV` client with Basic authentication.
    #[must_use]
    pub fn new_basic(base_url: String, username: String, password: String) -> Self {
        Self {
            base_url,
            auth: AuthMethod::Basic { username, password },
            http: reqwest::Client::new(),
        }
    }

    /// Discovers the `CardDAV` endpoint via `/.well-known/carddav` (RFC 6764).
    ///
    /// Returns a client with the resolved base URL. The auth token must be
    /// set via [`set_auth_token`](Self::set_auth_token) before making
    /// authenticated requests.
    ///
    /// # Errors
    /// Returns [`KestrelError::ConnectionLost`] if the discovery request fails.
    #[instrument(skip_all, fields(host))]
    pub async fn discover(host: &str) -> Result<Self, KestrelError> {
        let url = format!("https://{host}/.well-known/carddav");
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

    /// Lists address books via `PROPFIND` Depth:1 on the addressbook home.
    ///
    /// # Errors
    /// Returns [`KestrelError::ConnectionLost`] on transport failure.
    #[instrument(skip(self))]
    pub async fn list_address_books(&self) -> Result<Vec<CardDavAddressBook>, KestrelError> {
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

    /// Syncs contacts from an address book using `sync-collection` REPORT.
    ///
    /// When `sync_token` is `None`, a full sync is performed. Otherwise only
    /// changes since the given token are returned.
    ///
    /// # Errors
    /// Returns [`KestrelError::ConnectionLost`] on transport failure.
    #[instrument(skip(self))]
    pub async fn sync_contacts(
        &self,
        address_book_url: &str,
        sync_token: Option<&str>,
    ) -> Result<CardDavSyncResult, KestrelError> {
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
            .request(REPORT.clone(), address_book_url)
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

    /// Creates or updates a contact via `PUT`.
    ///
    /// If `etag` is provided, an `If-Match` header is sent for conditional
    /// updates.
    ///
    /// # Errors
    /// Returns [`KestrelError::ConnectionLost`] on transport failure.
    #[instrument(skip(self, vcard_data))]
    pub async fn put_contact(
        &self,
        contact_url: &str,
        vcard_data: &str,
        etag: Option<&str>,
    ) -> Result<(), KestrelError> {
        let mut req = self
            .http
            .put(contact_url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "text/vcard; charset=utf-8")
            .body(vcard_data.to_string());

        if let Some(etag_val) = etag {
            req = req.header("If-Match", etag_val);
        }

        req.send().await.map_err(|e| KestrelError::ConnectionLost {
            detail: e.to_string(),
        })?;

        Ok(())
    }

    /// Deletes a contact via `DELETE`.
    ///
    /// If `etag` is provided, an `If-Match` header is sent for conditional
    /// deletes.
    ///
    /// # Errors
    /// Returns [`KestrelError::ConnectionLost`] on transport failure.
    #[instrument(skip(self))]
    pub async fn delete_contact(
        &self,
        contact_url: &str,
        etag: Option<&str>,
    ) -> Result<(), KestrelError> {
        let mut req = self
            .http
            .delete(contact_url)
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

fn parse_propfind_response(xml: &str) -> Vec<CardDavAddressBook> {
    let mut address_books = Vec::new();

    for response_block in xml.split("<d:response>").skip(1) {
        let Some(end) = response_block.find("</d:response>") else {
            continue;
        };
        let response = &response_block[..end];

        // Only include collections (address books), not plain files.
        if !response.contains("<d:collection/>") {
            continue;
        }

        let Some(href) = extract_tag(response, "d:href") else {
            continue;
        };
        let display_name = extract_tag(response, "d:displayname").unwrap_or_default();
        let sync_token = extract_tag(response, "d:getctag");

        address_books.push(CardDavAddressBook {
            display_name,
            href,
            sync_token,
        });
    }

    address_books
}

fn parse_sync_collection_response(xml: &str) -> CardDavSyncResult {
    let mut contacts = Vec::new();
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

        if let Some(vcard_data) = extract_tag(response, "card:address-data")
            && let Ok(mut parsed) = crate::vcard::parse_vcard(&vcard_data)
        {
            for mut contact in parsed.drain(..) {
                contact.id.clone_from(&href);
                contacts.push(contact);
            }
        }
    }

    CardDavSyncResult {
        contacts,
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
    fn parse_propfind_single_address_book() {
        let xml = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<d:multistatus xmlns:d=\"DAV:\">
  <d:response>
    <d:href>/addressbooks/user/contacts/</d:href>
    <d:propstat>
      <d:prop>
        <d:displayname>My Contacts</d:displayname>
        <d:resourcetype><d:collection/></d:resourcetype>
        <d:getctag>ctag-ab-1</d:getctag>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
</d:multistatus>";

        let books = parse_propfind_response(xml);
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].display_name, "My Contacts");
        assert_eq!(books[0].href, "/addressbooks/user/contacts/");
        assert_eq!(books[0].sync_token.as_deref(), Some("ctag-ab-1"));
    }

    #[test]
    fn parse_propfind_skips_non_collections() {
        let xml = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<d:multistatus xmlns:d=\"DAV:\">
  <d:response>
    <d:href>/addressbooks/user/contacts/card.vcf</d:href>
    <d:propstat>
      <d:prop>
        <d:displayname>card.vcf</d:displayname>
        <d:resourcetype/>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/addressbooks/user/contacts/</d:href>
    <d:propstat>
      <d:prop>
        <d:displayname>Work</d:displayname>
        <d:resourcetype><d:collection/></d:resourcetype>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
</d:multistatus>";

        let books = parse_propfind_response(xml);
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].display_name, "Work");
    }

    #[test]
    fn parse_sync_collection_with_contacts() {
        let xml = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<d:multistatus xmlns:d=\"DAV:\" xmlns:card=\"urn:ietf:params:xml:ns:carddav\">
  <d:response>
    <d:href>/addressbooks/user/contacts/alice.vcf</d:href>
    <d:propstat>
      <d:prop>
        <d:getetag>\"etag-alice\"</d:getetag>
        <card:address-data>BEGIN:VCARD
VERSION:4.0
UID:alice@example.com
FN:Alice Smith
N:Smith;Alice;;;
EMAIL:alice@example.com
TEL:+1-555-0101
ORG:Acme Corp
END:VCARD</card:address-data>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:sync-token>ab-sync-7</d:sync-token>
</d:multistatus>";

        let result = parse_sync_collection_response(xml);
        assert_eq!(result.contacts.len(), 1);
        assert_eq!(result.contacts[0].uid, "alice@example.com");
        assert_eq!(result.contacts[0].display_name, "Alice Smith");
        assert_eq!(
            result.contacts[0].id,
            "/addressbooks/user/contacts/alice.vcf"
        );
        assert_eq!(result.new_sync_token.as_deref(), Some("ab-sync-7"));
        assert!(result.deleted_urls.is_empty());
    }

    #[test]
    fn parse_sync_collection_with_deleted() {
        let xml = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<d:multistatus xmlns:d=\"DAV:\">
  <d:response>
    <d:href>/addressbooks/user/contacts/old.vcf</d:href>
    <d:propstat>
      <d:prop/>
      <d:status>HTTP/1.1 404 Not Found</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/addressbooks/user/contacts/gone.vcf</d:href>
    <d:propstat>
      <d:prop/>
      <d:status>HTTP/1.1 410 Gone</d:status>
    </d:propstat>
  </d:response>
  <d:sync-token>ab-tok-3</d:sync-token>
</d:multistatus>";

        let result = parse_sync_collection_response(xml);
        assert!(result.contacts.is_empty());
        assert_eq!(result.deleted_urls.len(), 2);
        assert_eq!(
            result.deleted_urls[0],
            "/addressbooks/user/contacts/old.vcf"
        );
        assert_eq!(
            result.deleted_urls[1],
            "/addressbooks/user/contacts/gone.vcf"
        );
        assert_eq!(result.new_sync_token.as_deref(), Some("ab-tok-3"));
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
    fn auth_header_bearer() {
        let client = CardDavClient::new("http://localhost".into(), "my-token".into());
        assert_eq!(client.auth_header(), "Bearer my-token");
    }

    #[test]
    fn auth_header_basic() {
        use base64::Engine;
        let client =
            CardDavClient::new_basic("http://localhost".into(), "user".into(), "pass".into());
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("user:pass")
        );
        assert_eq!(client.auth_header(), expected);
    }
}
