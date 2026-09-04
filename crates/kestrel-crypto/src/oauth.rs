//! `OAuth2` flows (requirements §2.3), delegated to `oauth-toolkit`
//! (extracted crate): loopback redirect capture on `127.0.0.1:<ephemeral>`
//! with single-use `state` and PKCE (RFC 7636, S256), token exchange +
//! refresh via the provider's endpoints, and token persistence through
//! [`CredentialService`].
//!
//! Kestrel's boundary wraps the toolkit's plain-`String` tokens in
//! [`SecretString`] ([`TokenSet`], [`persist_refresh`]) and maps toolkit
//! errors onto [`CryptoError::OAuth`]. Provider presets are the toolkit's
//! [`MailProvider`]; unlike the former flat scope list it splits
//! `imap_scopes`/`smtp_scopes`/`extra_scopes` — authorization requests use
//! the de-duplicated union (`MailProvider::authorization_scopes`).
//!
//! The loopback server binds loopback only, serves exactly one redirect,
//! then shuts down (threat model §4.8). Flow/algorithm coverage lives
//! upstream in `oauth-toolkit`; this module keeps the integration-level
//! tests (preset URL building, capture delegation, refresh roundtrip
//! through `CredentialService`).

use std::time::Duration;

use kestrel_core::secrets::SecretString;
pub use oauth_toolkit::{providers::MailProvider, token::TokenResponse};
use tracing::instrument;

use crate::{
    credentials::CredentialService,
    error::{CryptoError, CryptoResult},
};

impl From<oauth_toolkit::loopback::LoopbackError> for CryptoError {
    fn from(err: oauth_toolkit::loopback::LoopbackError) -> Self {
        CryptoError::OAuth(err.to_string())
    }
}

impl From<oauth_toolkit::token::TokenError> for CryptoError {
    fn from(err: oauth_toolkit::token::TokenError) -> Self {
        CryptoError::OAuth(err.to_string())
    }
}

/// Outcome of a completed authorization-code exchange.
#[derive(Clone, Debug)]
pub struct TokenSet {
    /// Access token (short-lived).
    pub access_token: SecretString,
    /// Refresh token (persisted via `CredentialService`).
    pub refresh_token: Option<SecretString>,
    /// Access-token expiry in unix ms.
    pub expires_at: i64,
}

fn secret_set(set: oauth_toolkit::token::TokenSet) -> TokenSet {
    TokenSet {
        access_token: SecretString::new(set.access_token),
        refresh_token: set.refresh_token.map(SecretString::new),
        expires_at: set.expires_at,
    }
}

/// A started flow: the authorization URL plus the capture handle.
pub struct AuthorizationFlow {
    /// URL the user opens in a browser.
    pub url: String,
}

/// Starts the flow: binds an ephemeral loopback port, builds the
/// authorization URL (PKCE S256 + single-use `state`), and spawns the
/// redirect capture on a blocking worker. The handle yields the code once
/// the browser redirect arrives.
///
/// # Errors
/// [`CryptoError::OAuth`] on loopback bind failure.
#[instrument(skip_all)]
pub async fn start_flow(
    provider: &MailProvider,
    login_hint: Option<String>,
    timeout: Duration,
) -> CryptoResult<(
    AuthorizationFlow,
    tokio::task::JoinHandle<CryptoResult<String>>,
)> {
    let flow = oauth_toolkit::loopback::LoopbackFlow::start_for_provider(
        provider,
        login_hint.as_deref(),
        timeout,
    )?;
    let url = flow.authorization_url().to_owned();
    let handle = tokio::task::spawn_blocking(move || {
        flow.wait_for_code()
            .map(|c| c.code)
            .map_err(CryptoError::from)
    });
    Ok((AuthorizationFlow { url }, handle))
}

/// Exchanges an authorization code for tokens (`code_verifier` is the PKCE
/// verifier returned alongside the captured code).
///
/// # Errors
/// [`CryptoError::OAuth`] on HTTP/protocol failure.
#[instrument(skip_all)]
pub async fn exchange_code(
    http: &reqwest::Client,
    provider: &MailProvider,
    client_secret: Option<&SecretString>,
    code: &str,
    redirect_port: u16,
    code_verifier: &str,
) -> CryptoResult<TokenSet> {
    let set = oauth_toolkit::token::exchange_code(
        http,
        &provider.token_url,
        &provider.client_id,
        client_secret.map(SecretString::expose),
        code,
        &oauth_toolkit::loopback::loopback_redirect_uri(redirect_port),
        code_verifier,
    )
    .await?;
    Ok(secret_set(set))
}

/// Refreshes an access token with a stored refresh token.
///
/// # Errors
/// [`CryptoError::OAuth`] when the refresh is rejected (revoked/expired).
#[instrument(skip_all)]
pub async fn refresh(
    http: &reqwest::Client,
    provider: &MailProvider,
    client_secret: Option<&SecretString>,
    refresh_token: &SecretString,
) -> CryptoResult<TokenSet> {
    let set = oauth_toolkit::token::refresh(
        http,
        &provider.token_url,
        &provider.client_id,
        client_secret.map(SecretString::expose),
        refresh_token.expose(),
    )
    .await?;
    Ok(secret_set(set))
}

/// Refreshes an access token using string parameters (simpler API than
/// [`refresh`]); returns the raw response for rotation handling.
///
/// # Errors
/// [`CryptoError::OAuth`] when the refresh is rejected (revoked/expired).
#[instrument(skip_all)]
pub async fn refresh_access_token(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    refresh_token: &str,
) -> CryptoResult<TokenResponse> {
    Ok(
        oauth_toolkit::token::refresh_access_token(http, token_endpoint, client_id, refresh_token)
            .await?,
    )
}

/// Persists a token set's refresh token.
///
/// # Errors
/// Credential store failure.
pub fn persist_refresh(
    creds: &CredentialService,
    account: kestrel_core::ids::AccountId,
    tokens: &TokenSet,
) -> CryptoResult<()> {
    if let Some(rt) = &tokens.refresh_token {
        creds.set_refresh_token(account, rt)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::items_after_statements
    )]

    use std::sync::Arc;

    use oauth_toolkit::loopback::LoopbackFlow;

    use super::*;
    use crate::credentials::InMemoryStore;

    const FIVE_SECS: Duration = Duration::from_secs(5);

    #[test]
    fn authorization_urls_from_real_presets() {
        // Gmail: umbrella scope + openid/email extras, login hint encoded.
        let url = LoopbackFlow::start_for_provider(
            &MailProvider::gmail("cid-123"),
            Some("a@b.c"),
            FIVE_SECS,
        )
        .unwrap()
        .authorization_url()
        .to_owned();
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("client_id=cid-123"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("scope=https%3A%2F%2Fmail.google.com%2F"));
        assert!(url.contains("openid"));
        assert!(url.contains("login_hint=a%40b.c"));

        // Outlook: tenant-aware endpoints; imap/smtp/extra scope split
        // must union into the authorization request.
        let outlook = MailProvider::outlook("o-123", "common");
        assert!(outlook.auth_url.contains("/common/oauth2/v2.0/authorize"));
        assert!(outlook.token_url.contains("/common/oauth2/v2.0/token"));
        let scopes = outlook.authorization_scopes();
        assert!(scopes.iter().any(|s| s.contains("IMAP.AccessAsUser.All")));
        assert!(scopes.iter().any(|s| s.contains("SMTP.Send")));
        assert!(scopes.contains(&"offline_access".to_string()));

        // Yahoo + Fastmail endpoints survive the preset move.
        let yahoo = MailProvider::yahoo("y-123");
        assert!(yahoo.auth_url.contains("login.yahoo.com"));
        assert!(yahoo.token_url.contains("login.yahoo.com"));
        assert_eq!(yahoo.authorization_scopes(), vec!["mail-w"]);
        let fastmail = MailProvider::fastmail("f-123");
        assert!(fastmail.auth_url.contains("app.fastmail.com"));
        assert!(fastmail.token_url.contains("api.fastmail.com"));
        assert!(
            fastmail
                .authorization_scopes()
                .iter()
                .all(|s| s.contains("fastmail.com/dev/protocol"))
        );
    }

    #[tokio::test]
    async fn loopback_capture_accepts_valid_redirect() {
        let provider = MailProvider::gmail("cid");
        let (flow, handle) = start_flow(&provider, None, FIVE_SECS).await.unwrap();
        // Extract state from the URL to forge the redirect.
        let state = flow
            .url
            .split("state=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_owned();
        let port: u16 = flow
            .url
            .split("redirect_uri=http%3A%2F%2F127.0.0.1%3A")
            .nth(1)
            .unwrap()
            .split('%')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        // Simulate the browser redirect.
        let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let request = format!(
            "GET /cb?code=AC123&state={state} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        );
        sock.write_all(request.as_bytes()).await.unwrap();
        let mut buf = String::new();
        let _ = sock.read_to_string(&mut buf).await;
        assert!(buf.contains("200 OK"), "{buf}");
        assert!(buf.contains("Signed in"));
        let code = handle.await.unwrap().unwrap();
        assert_eq!(code, "AC123");
    }

    #[tokio::test]
    async fn loopback_capture_rejects_state_mismatch() {
        let provider = MailProvider::gmail("cid");
        let (flow, handle) = start_flow(&provider, None, FIVE_SECS).await.unwrap();
        let port: u16 = flow
            .url
            .split("redirect_uri=http%3A%2F%2F127.0.0.1%3A")
            .nth(1)
            .unwrap()
            .split('%')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        use tokio::io::AsyncWriteExt;
        let request =
            "GET /cb?code=X&state=evil HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
        sock.write_all(request.as_bytes()).await.unwrap();
        drop(sock);
        let err = handle.await.unwrap().unwrap_err();
        assert!(
            err.to_string().contains("state mismatch"),
            "CSRF surfaced: {err}"
        );
    }

    /// Spawns a minimal mock token endpoint and returns its base URL.
    async fn spawn_mock_token_server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let base = format!("http://127.0.0.1:{port}");
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = vec![0u8; 4096];
            let n = stream.read(&mut buf).await.expect("read");
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            // Determine response based on request body.
            let body = if request.contains("refresh_token=bad") {
                r#"{"error":"invalid_grant","error_description":"token is invalid"}"#
            } else if request.contains("grant_type=refresh_token") {
                r#"{"access_token":"new_at","token_type":"Bearer","expires_in":3600,"refresh_token":"new_rt"}"#
            } else {
                r#"{"error":"unsupported_grant_type"}"#
            };
            let status = if request.contains("bad") {
                "400"
            } else {
                "200"
            };
            let response = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            stream.write_all(response.as_bytes()).await.expect("write");
        });
        (base, handle)
    }

    /// Integration-level refresh roundtrip: the toolkit-backed [`refresh`]
    /// hands back [`SecretString`] tokens, [`persist_refresh`] stores the
    /// rotated refresh token via `CredentialService`, and the store reads
    /// back the new value (not the original).
    #[tokio::test]
    async fn refresh_roundtrip_via_credential_service() {
        let (base, handle) = spawn_mock_token_server().await;
        let http = reqwest::Client::new();
        // Gmail preset shape, token endpoint overridden with the mock
        // (the real preset URL would hit Google's production endpoint).
        let provider = MailProvider {
            token_url: format!("{base}/token"),
            ..MailProvider::gmail("test-client-id")
        };
        let original = SecretString::new("test-refresh-token".to_owned());

        let tokens = refresh(&http, &provider, None, &original)
            .await
            .expect("refresh");
        assert_eq!(tokens.access_token.expose(), "new_at");
        assert!(tokens.expires_at > 0);
        assert!(tokens.refresh_token.is_some());

        let creds = CredentialService::new(Arc::new(InMemoryStore::new()));
        let account = kestrel_core::ids::AccountId::from_uuid(uuid::Uuid::now_v7());
        persist_refresh(&creds, account, &tokens).expect("persist");
        let stored = creds.refresh_token(account).expect("read");
        assert_eq!(
            stored.map(|s| s.expose().to_owned()),
            Some("new_rt".to_owned())
        );
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn refresh_access_token_rejects_invalid_token() {
        let (base, handle) = spawn_mock_token_server().await;
        let http = reqwest::Client::new();
        let err = refresh_access_token(&http, &format!("{base}/token"), "client", "bad")
            .await
            .expect_err("should fail");
        // Toolkit TokenError mapped onto CryptoError::OAuth with the status.
        match err {
            CryptoError::OAuth(msg) => assert!(msg.contains("400"), "{msg}"),
            other => panic!("unexpected: {other}"),
        }
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn refresh_access_token_network_error() {
        let http = reqwest::Client::new();
        let err = refresh_access_token(&http, "http://127.0.0.1:1/nope", "client", "rt")
            .await
            .expect_err("should fail");
        assert!(matches!(err, CryptoError::OAuth(_)), "{err}");
    }

    #[test]
    fn token_response_deserialize_minimal() {
        let json = r#"{"access_token":"at","token_type":"Bearer","expires_in":300}"#;
        let resp: TokenResponse = serde_json::from_str(json).expect("parse");
        assert_eq!(resp.access_token, "at");
        assert_eq!(resp.token_type.as_deref(), Some("Bearer"));
        assert_eq!(resp.expires_in, Some(300));
        assert!(resp.refresh_token.is_none());
    }

    #[test]
    fn token_response_deserialize_full() {
        let json =
            r#"{"access_token":"at","token_type":"Bearer","expires_in":3600,"refresh_token":"rt"}"#;
        let resp: TokenResponse = serde_json::from_str(json).expect("parse");
        assert_eq!(resp.refresh_token.as_deref(), Some("rt"));
    }
}
