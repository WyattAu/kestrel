//! `OAuth2` unattended-refresh soak (issue #25, phase-2 exit gate).
//!
//! The Phase-2 exit criterion is "token refresh unattended for 7 days".
//! The refresh worker itself is [`kestrel_crypto::oauth::refresh_unattended`];
//! this test proves the long-horizon properties behind that criterion in
//! compressed time against a deterministic mock identity provider:
//!
//! - 7 simulated days, each one access-token expiry followed by an
//!   unattended refresh;
//! - rotation: the mock identity provider rotates the refresh token on every success and the
//!   worker must persist each rotation through `CredentialService`;
//! - fault absorption: transient 5xx and 429 responses mid-week are
//!   absorbed by the caller's retry loop (never surface as permanent
//!   failure, never consume the token lineage);
//! - terminal rejection: an `invalid_grant` day classifies as
//!   [`RefreshOutcome::Rejected`] — the account needs interactive
//!   re-authentication, not infinite retry.
//!
//! Wall-clock compression: each cycle stands in for one daily expiry; the
//! scheduled weekly soak (once the engine wires the worker) observes the
//! same assertions across real wall-clock days.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    missing_docs,
    clippy::print_stderr,
    clippy::items_after_statements,
    clippy::too_many_lines
)]

use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

use kestrel_core::{ids::AccountId, secrets::SecretString};
use kestrel_crypto::{
    CredentialService, InMemoryStore, oauth,
    oauth::{MailProvider, RefreshOutcome},
};

const SIMULATED_DAYS: u32 = 7;

// ---- mock IdP ---------------------------------------------------------------

#[derive(Default)]
struct IdpState {
    /// Successful refreshes served (the token-ledger clock).
    refresh_count: AtomicU32,
    /// Upcoming requests answered with 500 before service returns.
    fail_500_remaining: AtomicU32,
    /// Upcoming requests answered with 429 before service returns.
    fail_429_remaining: AtomicU32,
    /// When set, every refresh is rejected (revoked/rotated-out token).
    reject_refresh: AtomicBool,
    /// Every access token handed out (uniqueness assertion).
    access_tokens: Mutex<Vec<String>>,
}

impl IdpState {
    fn respond(&self, request: &str) -> (u16, String) {
        if !request.contains("grant_type=refresh_token") {
            return (400, r#"{"error":"unsupported_grant_type"}"#.into());
        }
        if self.reject_refresh.load(Ordering::SeqCst) {
            return (
                400,
                r#"{"error":"invalid_grant","error_description":"token revoked"}"#.into(),
            );
        }
        if take_fault(&self.fail_500_remaining) {
            return (500, r#"{"error":"temporarily_unavailable"}"#.into());
        }
        if take_fault(&self.fail_429_remaining) {
            return (429, r#"{"error":"slow_down"}"#.into());
        }
        let n = self.refresh_count.fetch_add(1, Ordering::SeqCst);
        let access = format!("access_day_{n}");
        self.access_tokens.lock().unwrap().push(access.clone());
        let refresh = format!("refresh_day_{n}");
        (
            200,
            format!(
                r#"{{"access_token":"{access}","token_type":"Bearer","expires_in":86400,"refresh_token":"{refresh}"}}"#
            ),
        )
    }
}

/// Consumes one pre-armed fault if any remain (returns true = inject).
fn take_fault(counter: &AtomicU32) -> bool {
    loop {
        let cur = counter.load(Ordering::SeqCst);
        if cur == 0 {
            return false;
        }
        if counter
            .compare_exchange(cur, cur - 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return true;
        }
    }
}

async fn spawn_idp(state: Arc<IdpState>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let state = state.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt as _;
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                // The toolkit posts form-encoded; the form body carries the
                // grant parameters (headers above it are irrelevant here).
                let body_start = request.find("grant_type=").unwrap_or(n);
                let body = &request[body_start..];
                let (status, json) = state.respond(body);
                let body = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                use tokio::io::AsyncWriteExt as _;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.flush().await;
            });
        }
    });
    (addr, handle)
}

// ---- the soak ---------------------------------------------------------------

fn provider_for(addr: SocketAddr) -> MailProvider {
    MailProvider {
        auth_url: format!("http://{addr}/auth"),
        token_url: format!("http://{addr}/token"),
        client_id: "kestrel-soak".into(),
        imap_scopes: vec!["https://mail.example.org/imap".into()],
        smtp_scopes: vec!["https://mail.example.org/smtp".into()],
        extra_scopes: vec!["offline_access".into()],
    }
}

/// One simulated day: refresh unattended, retrying transient failures with
/// capped exponential backoff. Returns (outcome, transient attempts seen).
async fn simulated_day(
    http: &reqwest::Client,
    provider: &MailProvider,
    creds: &CredentialService,
    account: AccountId,
) -> (RefreshOutcome, u32) {
    let mut backoff = Duration::from_millis(50);
    let mut transient_attempts = 0u32;
    for _ in 0..10 {
        match oauth::refresh_unattended(http, provider, creds, account).await {
            Ok(outcome @ (RefreshOutcome::Refreshed | RefreshOutcome::Rejected)) => {
                return (outcome, transient_attempts);
            }
            Ok(RefreshOutcome::Transient) => {
                transient_attempts += 1;
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_millis(500));
            }
            Err(e) => panic!("refresh plumbing failed unexpectedly: {e}"),
        }
    }
    (RefreshOutcome::Transient, transient_attempts)
}

#[tokio::test]
#[ignore = "KESTREL_INTEGRATION=1 (exit gate; deterministic, docker-free)"]
async fn integration_oauth_refresh_survives_seven_simulated_days_with_faults() {
    if std::env::var("KESTREL_INTEGRATION").is_err() {
        return;
    }

    let idp = Arc::new(IdpState::default());
    let (addr, server) = spawn_idp(idp.clone()).await;
    let provider = provider_for(addr);

    let creds = CredentialService::new(Arc::new(InMemoryStore::default()));
    let account = AccountId::from_uuid(uuid::Uuid::now_v7());
    creds
        .set_refresh_token(account, &SecretString::new("refresh_seed".into()))
        .unwrap();

    // rustls-pinned per ADR 0016 (loopback http, but policy is uniform).
    let http = reqwest::Client::builder().use_rustls_tls().build().unwrap();

    let mut total_transient = 0u32;
    let mut last_refresh_token = "refresh_seed".to_string();
    let mut day_outcomes = Vec::new();

    for day in 1..=SIMULATED_DAYS {
        // Fault schedule: days 2, 5, 6 have transient IdP outages.
        match day {
            2 => idp.fail_500_remaining.store(3, Ordering::SeqCst),
            5 => idp.fail_429_remaining.store(2, Ordering::SeqCst),
            6 => idp.fail_500_remaining.store(1, Ordering::SeqCst),
            _ => {}
        }

        let (outcome, transient) = simulated_day(&http, &provider, &creds, account).await;
        assert_eq!(outcome, RefreshOutcome::Refreshed, "day {day} must refresh");
        total_transient += transient;

        // Rotation persisted: the stored token must differ from yesterday's.
        let stored = creds
            .refresh_token(account)
            .unwrap()
            .expect("rotation must leave a token stored");
        assert_ne!(
            stored.expose(),
            last_refresh_token,
            "day {day}: rotated refresh token must be persisted"
        );
        last_refresh_token = stored.expose().to_owned();
        day_outcomes.push(format!("day{day}:ok"));
    }

    // Token ledger: exactly one successful refresh per day; access tokens
    // all unique.
    assert_eq!(
        idp.refresh_count.load(Ordering::SeqCst),
        SIMULATED_DAYS,
        "ledger: one success per simulated day"
    );
    let (token_count, unique_count) = {
        let tokens = idp.access_tokens.lock().unwrap();
        let mut unique = tokens.clone();
        unique.sort();
        unique.dedup();
        (tokens.len(), unique.len())
    };
    assert_eq!(unique_count, token_count, "access tokens must be unique");

    // The absorbed transients actually happened (fault injection ran).
    assert_eq!(total_transient, 6, "3x500 + 2x429 + 1x500 absorbed");

    // Terminal rejection: a revoked token classifies as Rejected — the
    // account needs interactive re-auth, not endless retry.
    idp.reject_refresh.store(true, Ordering::SeqCst);
    let (outcome, transient) = simulated_day(&http, &provider, &creds, account).await;
    assert_eq!(outcome, RefreshOutcome::Rejected);
    assert_eq!(transient, 0, "rejection is terminal, not transient");

    server.abort();
    eprintln!("soak ledger: {:?}", day_outcomes.join(" "));
}
