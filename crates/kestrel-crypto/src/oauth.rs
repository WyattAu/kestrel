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

/// Builds the provider preset from env-configurable `OAuth2` app
/// credentials (the same sources `Router::start_oauth2_flow` uses).
///
/// `KESTREL_OAUTH2_TOKEN_URL` overrides the token endpoint (with a generic
/// preset construction), enabling self-hosted identity providers and
/// hermetic fixtures.
/// Shared by the interactive flow and the unattended refresh worker so
/// both always talk to the same endpoint with the same client identity.
///
/// # Errors
/// [`CryptoError::OAuth`] when the provider has no `OAuth2` preset.
pub fn provider_from_env(
    provider: &kestrel_core::protocol::Provider,
) -> CryptoResult<MailProvider> {
    let client_id =
        std::env::var("KESTREL_OAUTH2_CLIENT_ID").unwrap_or_else(|_| "kestrel-desktop".into());
    if let Ok(token_url) = std::env::var("KESTREL_OAUTH2_TOKEN_URL") {
        return Ok(MailProvider {
            auth_url: std::env::var("KESTREL_OAUTH2_AUTH_URL")
                .unwrap_or_else(|_| token_url.clone()),
            token_url,
            client_id,
            imap_scopes: std::env::var("KESTREL_OAUTH2_IMAP_SCOPES")
                .map(|s| s.split(',').map(str::to_owned).collect())
                .unwrap_or_default(),
            smtp_scopes: Vec::new(),
            extra_scopes: std::env::var("KESTREL_OAUTH2_EXTRA_SCOPES").map_or_else(
                |_| vec!["offline_access".into()],
                |s| s.split(',').map(str::to_owned).collect(),
            ),
        });
    }
    Ok(match provider {
        kestrel_core::protocol::Provider::Gmail => MailProvider::gmail(&client_id),
        kestrel_core::protocol::Provider::Outlook => {
            let tenant = std::env::var("KESTREL_OAUTH2_TENANT").unwrap_or_else(|_| "common".into());
            MailProvider::outlook(&client_id, &tenant)
        }
        kestrel_core::protocol::Provider::Yahoo => MailProvider::yahoo(&client_id),
        kestrel_core::protocol::Provider::Fastmail => MailProvider::fastmail(&client_id),
        _ => {
            return Err(CryptoError::OAuth(
                "provider does not support OAuth2".into(),
            ));
        }
    })
}

/// Outcome of one unattended refresh attempt.
#[derive(Clone, Debug)]
pub enum RefreshOutcome {
    /// Tokens refreshed and rotated credentials persisted.
    Refreshed {
        /// The new token set: `access_token` goes to the live-secret cell,
        /// `expires_at` schedules the next refresh.
        tokens: TokenSet,
    },
    /// Transient failure (network, 5xx, 429): back off and retry; the
    /// stored refresh token is still valid.
    Transient,
    /// The refresh token was rejected (revoked/expired): the account needs
    /// interactive re-authentication. Retrying cannot succeed.
    Rejected,
}

/// One unattended refresh cycle: read the stored refresh token, exchange
/// it, persist rotation, and classify the outcome for the caller's
/// backoff/reauth policy.
///
/// Classification (sync-engine.md §5): network/HTTP/JSON failures and 5xx
/// or 429 statuses are [`RefreshOutcome::Transient`]; any other status is
/// [`RefreshOutcome::Rejected`] (typically `invalid_grant`).
///
/// # Errors
/// [`CryptoError::OAuth`] when the refresh token is missing from the
/// credential store entirely (nothing to refresh). Transient and rejected
/// refreshes are reported via the return value, not the error.
pub async fn refresh_unattended(
    http: &reqwest::Client,
    provider: &MailProvider,
    creds: &CredentialService,
    account: kestrel_core::ids::AccountId,
) -> CryptoResult<RefreshOutcome> {
    let Some(refresh_token) = creds.refresh_token(account)? else {
        return Err(CryptoError::OAuth(format!(
            "no refresh token stored for account {account}"
        )));
    };
    match oauth_toolkit::token::refresh(
        http,
        &provider.token_url,
        &provider.client_id,
        None,
        refresh_token.expose(),
    )
    .await
    {
        Ok(set) => {
            let tokens = secret_set(set);
            // Rotation must persist: dropping it desynchronizes the
            // keyring from the server's token lineage.
            persist_refresh(creds, account, &tokens)?;
            Ok(RefreshOutcome::Refreshed { tokens })
        }
        Err(
            oauth_toolkit::token::TokenError::Http(_) | oauth_toolkit::token::TokenError::Json(_),
        ) => Ok(RefreshOutcome::Transient),
        Err(oauth_toolkit::token::TokenError::Status(status)) => {
            if status.as_u16() == 429 || status.is_server_error() {
                Ok(RefreshOutcome::Transient)
            } else {
                Ok(RefreshOutcome::Rejected)
            }
        }
    }
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

/// Builds the shared rustls-pinned HTTP client for `OAuth2` endpoint
/// traffic (ADR 0016: rustls everywhere; plain-http only reaches loopback
/// in fixtures).
///
/// # Errors
/// [`CryptoError::OAuth`] when the client cannot be built (TLS backend
/// initialization failure).
pub fn shared_http_client() -> CryptoResult<std::sync::Arc<reqwest::Client>> {
    Ok(std::sync::Arc::new(
        reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .map_err(|e| CryptoError::OAuth(format!("http client: {e}")))?,
    ))
}

/// Tuning for [`refresh_worker`] (tests compress these).
#[derive(Clone, Copy, Debug)]
pub struct RefreshWorkerConfig {
    /// Refresh this long before expiry.
    pub refresh_skew: Duration,
    /// Backoff base after a transient failure.
    pub retry_base: Duration,
    /// Backoff cap after repeated transient failures.
    pub retry_max: Duration,
}

impl Default for RefreshWorkerConfig {
    fn default() -> Self {
        Self {
            refresh_skew: Duration::from_mins(5),
            retry_base: Duration::from_secs(30),
            retry_max: Duration::from_mins(15),
        }
    }
}

/// Everything [`refresh_worker`] needs; bundles the arguments so the
/// constructor stays readable.
#[derive(Clone)]
pub struct RefreshWorkerSpec {
    /// Shared HTTP client (rustls-pinned per ADR 0016).
    pub http: std::sync::Arc<reqwest::Client>,
    /// Provider preset (token endpoint + client id).
    pub provider: MailProvider,
    /// Credential store backing the refresh token.
    pub creds: std::sync::Arc<CredentialService>,
    /// Owning account.
    pub account: kestrel_core::ids::AccountId,
    /// Time source for expiry scheduling.
    pub clock: std::sync::Arc<dyn kestrel_core::clock::Clock>,
    /// Live-token handoff (`ConnectParams`/`SmtpParams` `secret_override`).
    pub secret_cell: std::sync::Arc<std::sync::RwLock<Option<SecretString>>>,
    /// Expiry from a prior session's token; `None` refreshes immediately.
    pub initial_expires_at: Option<i64>,
    /// Timing tuning.
    pub cfg: RefreshWorkerConfig,
}

/// Why the unattended refresh loop stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshWorkerOutcome {
    /// The cancellation token fired (account removed / shutdown).
    Cancelled,
    /// The refresh token was rejected: interactive re-authentication is
    /// required. The caller must surface
    /// [`kestrel_core::protocol::ConnectionState::NeedsReauth`].
    Rejected,
}

/// The unattended `OAuth2` refresh loop (`sync-engine.md` §5): sleeps until
/// `expires_at - refresh_skew`, refreshes through [`refresh_unattended`],
/// publishes the fresh access token into `secret_cell` (the
/// `ConnectParams`/`SmtpParams` live-token handoff), and retries transient
/// failures with capped exponential backoff. Runs until cancelled or the
/// token is rejected.
///
/// With `initial_expires_at` (`Some`, from a prior session's token) the
/// first sleep honors it; `None` refreshes immediately (a stored refresh
/// token with no known expiry).
pub async fn refresh_worker(
    spec: RefreshWorkerSpec,
    cancel: tokio_util::sync::CancellationToken,
) -> RefreshWorkerOutcome {
    let RefreshWorkerSpec {
        http,
        provider,
        creds,
        account,
        clock,
        secret_cell,
        initial_expires_at,
        cfg,
    } = spec;
    let mut expires_at = initial_expires_at;
    let mut transient_streak: u32 = 0;
    loop {
        // Sleep until the refresh window opens (immediately when the
        // expiry is unknown).
        let now = clock.now_unix_ms();
        let skew_ms = i64::try_from(cfg.refresh_skew.as_millis()).unwrap_or(i64::MAX);
        let wake_in = expires_at.map_or(Duration::ZERO, |exp| {
            let delay_ms = (exp - skew_ms).saturating_sub(now);
            Duration::from_millis(u64::try_from(delay_ms).unwrap_or(0))
        });
        tokio::select! {
            () = cancel.cancelled() => return RefreshWorkerOutcome::Cancelled,
            () = tokio::time::sleep(wake_in) => {}
        }
        match refresh_unattended(&http, &provider, &creds, account).await {
            Ok(RefreshOutcome::Refreshed { tokens }) => {
                transient_streak = 0;
                *secret_cell
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tokens.access_token);
                expires_at = Some(tokens.expires_at);
            }
            Ok(RefreshOutcome::Transient) => {
                transient_streak = transient_streak.saturating_add(1);
                let shift = transient_streak.saturating_sub(1).min(16);
                let wait = cfg
                    .retry_base
                    .checked_mul(1u32 << shift)
                    .unwrap_or(cfg.retry_max)
                    .min(cfg.retry_max);
                tokio::select! {
                    () = cancel.cancelled() => return RefreshWorkerOutcome::Cancelled,
                    () = tokio::time::sleep(wait) => {}
                }
            }
            Ok(RefreshOutcome::Rejected) | Err(_) => return RefreshWorkerOutcome::Rejected,
        }
    }
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
        // rustls-pinned per ADR 0016 (loopback http, but policy is uniform).
        let http = reqwest::Client::builder().use_rustls_tls().build().unwrap();
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
        // rustls-pinned per ADR 0016 (loopback http, but policy is uniform).
        let http = reqwest::Client::builder().use_rustls_tls().build().unwrap();
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
        // rustls-pinned per ADR 0016 (loopback http, but policy is uniform).
        let http = reqwest::Client::builder().use_rustls_tls().build().unwrap();
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
