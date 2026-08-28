//! `OAuth2` flows (requirements §2.3): loopback redirect capture on
//! `127.0.0.1:<ephemeral>` with single-use `state` and PKCE (RFC 7636,
//! S256), token exchange + refresh via the provider's endpoints, and
//! token persistence through [`CredentialService`].
//!
//! The loopback server binds loopback only, serves exactly one redirect,
//! then shuts down (threat model §4.8).

use std::{fmt::Write as _, net::SocketAddr};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::{
    credentials::{CredentialService, CredentialStore, SecretString},
    error::{CryptoError, CryptoResult},
};

/// Provider endpoints and scopes.
#[derive(Clone, Debug)]
pub struct OAuthProvider {
    /// Authorization endpoint.
    pub auth_url: String,
    /// Token endpoint.
    pub token_url: String,
    /// Requested scopes.
    pub scopes: Vec<String>,
    /// `OAuth2` client id (Kestrel's registered app).
    pub client_id: String,
}

impl OAuthProvider {
    /// Google Workspace preset (requirements §2.3).
    #[must_use]
    pub fn gmail(client_id: &str) -> Self {
        Self {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            scopes: vec![
                "https://mail.google.com/".into(),
                "openid".into(),
                "email".into(),
            ],
            client_id: client_id.to_owned(),
        }
    }

    /// Microsoft 365 preset.
    #[must_use]
    pub fn outlook(client_id: &str, tenant: &str) -> Self {
        Self {
            auth_url: format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/authorize"),
            token_url: format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"),
            scopes: vec![
                "https://outlook.office.com/IMAP.AccessAsUser.All".into(),
                "https://outlook.office.com/SMTP.Send".into(),
                "offline_access".into(),
            ],
            client_id: client_id.to_owned(),
        }
    }

    /// Fastmail preset.
    #[must_use]
    pub fn fastmail(client_id: &str) -> Self {
        Self {
            auth_url: "https://app.fastmail.com/oauth/authorize".into(),
            token_url: "https://api.fastmail.com/oauth/token".into(),
            scopes: vec![
                "https://www.fastmail.com/dev/protocolIMAP".into(),
                "https://www.fastmail.com/dev/protocolSMTP".into(),
            ],
            client_id: client_id.to_owned(),
        }
    }
}

/// PKCE pair (RFC 7636 §4.1/4.2).
#[derive(Clone)]
pub struct Pkce {
    /// Unhashed verifier (kept client-side only).
    verifier: String,
    /// S256 challenge sent in the authorization request.
    challenge: String,
}

impl Pkce {
    /// Generates a verifier/challenge pair.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        let digest = Sha256::digest(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(digest);
        Self {
            verifier,
            challenge,
        }
    }

    /// The S256 challenge (for the auth URL).
    #[must_use]
    pub fn challenge(&self) -> &str {
        &self.challenge
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

/// Single-use state token.
fn fresh_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Builds the browser authorization URL (RFC 8252 style; the user opens it
/// externally) and returns it with the state + PKCE pair needed for capture.
#[must_use]
pub fn build_authorization_url(
    provider: &OAuthProvider,
    redirect_port: u16,
    state: &str,
    pkce: &Pkce,
    login_hint: Option<&str>,
) -> String {
    let mut url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&state={}&scope={}&code_challenge={}&code_challenge_method=S256",
        provider.auth_url,
        provider.client_id,
        urlencoding_of(&format!("http://127.0.0.1:{redirect_port}/cb")),
        state,
        urlencoding_of(&provider.scopes.join(" ")),
        pkce.challenge(),
    );
    if let Some(hint) = login_hint {
        let _ = write!(url, "&login_hint={}", urlencoding_of(hint));
    }
    url
}

fn urlencoding_of(s: &str) -> String {
    // Minimal percent-encoding for query components.
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(b));
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len() + 1
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"),
                16,
            )
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A started flow: authorization URL + the capture future.
pub struct AuthorizationFlow {
    /// URL the user opens in a browser.
    pub url: String,
    /// PKCE pair (verifier needed for the exchange).
    pub pkce: Pkce,
}

/// Starts the flow: generates PKCE, spins the capture server, returns the
/// URL plus the join handle yielding the code.
///
/// # Errors
/// [`CryptoError::OAuth`] on loopback bind failure.
pub async fn start_flow(
    provider: OAuthProvider,
    login_hint: Option<String>,
    timeout: std::time::Duration,
) -> CryptoResult<(
    AuthorizationFlow,
    tokio::task::JoinHandle<CryptoResult<(String, Pkce)>>,
)> {
    let pkce = Pkce::generate();
    // Bind the port now so the URL is stable for the browser.
    let state = fresh_state();
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .map_err(|e| CryptoError::OAuth(format!("loopback bind: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| CryptoError::OAuth(format!("loopback addr: {e}")))?
        .port();
    let url = build_authorization_url(&provider, port, &state, &pkce, login_hint.as_deref());

    let provider_clone = provider.clone();
    let pkce_for_capture = pkce.clone();
    let handle = tokio::spawn(async move {
        let (code, _port) = capture_on(
            listener,
            &provider_clone,
            &state,
            &pkce_for_capture,
            timeout,
        )
        .await?;
        Ok((code, pkce_for_capture))
    });

    Ok((AuthorizationFlow { url, pkce }, handle))
}

async fn capture_on(
    listener: tokio::net::TcpListener,
    _provider: &OAuthProvider,
    state: &str,
    _pkce: &Pkce,
    timeout: std::time::Duration,
) -> CryptoResult<(String, u16)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let port = listener
        .local_addr()
        .map_err(|e| CryptoError::OAuth(format!("loopback addr: {e}")))?
        .port();
    let (mut socket, _) = tokio::time::timeout(timeout, listener.accept())
        .await
        .map_err(|_| CryptoError::OAuth("loopback capture timed out".into()))?
        .map_err(|e| CryptoError::OAuth(format!("loopback accept: {e}")))?;
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 1024];
    loop {
        let n = tokio::time::timeout(timeout, socket.read(&mut chunk))
            .await
            .map_err(|_| CryptoError::OAuth("redirect read timed out".into()))?
            .map_err(|e| CryptoError::OAuth(format!("redirect read: {e}")))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 8192 {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buf).into_owned();
    let request_line = request.lines().next().unwrap_or_default().to_owned();
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let query = path.split_once('?').map_or("", |(_, q)| q);
    let mut code = None;
    let mut got_state = String::new();
    let mut oauth_error = String::new();
    for kv in query.split('&') {
        let (k, v) = kv.split_once('=').unwrap_or(("", ""));
        let decoded = percent_decode(v);
        match k {
            "code" => code = Some(decoded),
            "state" => got_state = decoded,
            "error" => oauth_error = decoded,
            _ => {}
        }
    }
    let body_ok =
        "<html><body><h3>Signed in</h3>You may close this tab and return to Kestrel.</body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        body_ok.len(),
        body_ok
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.flush().await;

    if !oauth_error.is_empty() {
        return Err(CryptoError::OAuth(format!("provider error: {oauth_error}")));
    }
    if got_state != state {
        return Err(CryptoError::OAuth("state mismatch (possible CSRF)".into()));
    }
    code.map(|c| (c, port))
        .ok_or_else(|| CryptoError::OAuth("redirect missing code".into()))
}

/// Exchanges an authorization code for tokens (PKCE verifier required).
///
/// # Errors
/// [`CryptoError::OAuth`] on HTTP/protocol failure.
pub async fn exchange_code(
    http: &reqwest::Client,
    provider: &OAuthProvider,
    client_secret: Option<&SecretString>,
    code: &str,
    redirect_port: u16,
    pkce: &Pkce,
) -> CryptoResult<TokenSet> {
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        (
            "redirect_uri",
            format!("http://127.0.0.1:{redirect_port}/cb"),
        ),
        ("client_id", provider.client_id.clone()),
        ("code_verifier", pkce.verifier.clone()),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret.expose().to_owned()));
    }
    token_request(http, &provider.token_url, &form).await
}

/// Refreshes an access token with a stored refresh token.
///
/// # Errors
/// [`CryptoError::OAuth`] when the refresh is rejected (revoked/expired).
pub async fn refresh(
    http: &reqwest::Client,
    provider: &OAuthProvider,
    client_secret: Option<&SecretString>,
    refresh_token: &SecretString,
) -> CryptoResult<TokenSet> {
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.expose().to_owned()),
        ("client_id", provider.client_id.clone()),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret.expose().to_owned()));
    }
    token_request(http, &provider.token_url, &form).await
}

async fn token_request(
    http: &reqwest::Client,
    url: &str,
    form: &[(&str, String)],
) -> CryptoResult<TokenSet> {
    let response = http
        .post(url)
        .form(form)
        .send()
        .await
        .map_err(|e| CryptoError::OAuth(format!("token endpoint: {e}")))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| CryptoError::OAuth(format!("token body: {e}")))?;
    if !status.is_success() {
        return Err(CryptoError::OAuth(format!(
            "token endpoint returned {status}"
        )));
    }
    let parsed: TokenResponse =
        serde_json::from_str(&text).map_err(|e| CryptoError::OAuth(format!("token JSON: {e}")))?;
    let expires_at = now_unix_ms() + parsed.expires_in.unwrap_or(3600) * 1000;
    Ok(TokenSet {
        access_token: SecretString::new(parsed.access_token),
        refresh_token: parsed.refresh_token.map(SecretString::new),
        expires_at,
    })
}

/// Wall-clock read for token-expiry math (inherently wall-clock: the token
/// endpoint defines lifetimes; the injected clock is for engine logic).
fn now_unix_ms() -> i64 {
    // Audited escape of the clock ban — see fn docs.
    #[allow(clippy::disallowed_methods)]
    let now = std::time::SystemTime::now();
    now.duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// Persists a token set's refresh token.
///
/// # Errors
/// Credential store failure.
pub fn persist_refresh<S: CredentialStore>(
    creds: &CredentialService<S>,
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

    use super::*;

    #[test]
    fn pkce_challenge_is_s256_of_verifier() {
        let pkce = Pkce::generate();
        assert_eq!(pkce.challenge().len(), 43, "sha256 b64url length");
        let digest = Sha256::digest(pkce.verifier.as_bytes());
        assert_eq!(pkce.challenge(), URL_SAFE_NO_PAD.encode(digest));
    }

    #[test]
    fn authorization_url_contains_required_params() {
        let provider = OAuthProvider::gmail("cid-123");
        let pkce = Pkce::generate();
        let url = build_authorization_url(&provider, 53123, "st4te", &pkce, Some("a@b.c"));
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("client_id=cid-123"));
        assert!(url.contains("state=st4te"));
        assert!(url.contains("code_challenge="));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A53123%2Fcb"));
        assert!(url.contains("login_hint=a%40b.c"));
        assert!(url.contains("https%3A%2F%2Fmail.google.com%2F"));
    }

    #[test]
    fn provider_presets_shape() {
        let g = OAuthProvider::gmail("g");
        assert!(g.token_url.contains("googleapis"));
        let o = OAuthProvider::outlook("o", "common");
        assert!(o.auth_url.contains("login.microsoftonline.com/common"));
        assert!(o.scopes.iter().any(|s| s.contains("IMAP")));
        let f = OAuthProvider::fastmail("f");
        assert!(f.token_url.contains("fastmail"));
    }

    #[tokio::test]
    async fn loopback_capture_accepts_valid_redirect() {
        let provider = OAuthProvider::gmail("cid");
        let (flow, handle) = start_flow(provider, None, std::time::Duration::from_secs(5))
            .await
            .unwrap();
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
        let (code, _pkce) = handle.await.unwrap().unwrap();
        assert_eq!(code, "AC123");
    }

    #[tokio::test]
    async fn loopback_capture_rejects_state_mismatch() {
        let provider = OAuthProvider::gmail("cid");
        let (flow, handle) = start_flow(provider, None, std::time::Duration::from_secs(5))
            .await
            .unwrap();
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
        let result = handle.await.unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn percent_decode_roundtrip() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("a%2Fb%3Ac"), "a/b:c");
    }
}
