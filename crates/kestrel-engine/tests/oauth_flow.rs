//! Engine-level exit gate for #28: the `OAuth2` browser flow completes
//! server-side and its tokens reach an account.
//!
//! Boots the real engine with the token + authorization endpoints pointed
//! at a hermetic in-process mock identity provider
//! (`KESTREL_OAUTH2_TOKEN_URL` / `KESTREL_OAUTH2_AUTH_URL`), then proves:
//!
//! 1. `StartOAuth2Flow` returns an authorization URL carrying a single-use
//!    `state` and a loopback redirect port.
//! 2. A browser-simulated redirect (raw TCP GET to the loopback capture)
//!    triggers the engine's autonomous completer: the code is exchanged at
//!    the mock identity provider and the success event publishes.
//! 3. `CompleteOAuth2Flow { state }` retrieves the serialized credential
//!    set exactly once — a second call and an unknown `state` are both
//!    rejected (single-use, threat model §4.8).
//! 4. The exchanged refresh token links to an account via `AddAccount`,
//!    and the #26 refresh worker immediately refreshes against the mock
//!    identity provider with it — the full hands-free lifecycle.
//!
//! Runs under nextest (one process per test): the env-var seams are
//! process-global by design, which is why these are `#[ignore]`-gated
//! integration tests.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs, unsafe_code)]

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

use kestrel_core::{
    config::Config,
    paths::Paths,
    protocol::{CommandPayload, EngineEvent, FrontendKind, Provider, Reply},
    testkit::temp_paths,
};
use kestrel_engine::{Engine, command};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
};

// ---- mock IdP: authorization_code + refresh_token grants --------------------

#[derive(Default)]
struct IdpState {
    exchange_count: AtomicU32,
    refresh_count: AtomicU32,
    reject_refresh: AtomicBool,
}

async fn spawn_idp(state: Arc<IdpState>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let state = state.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                let body_start = request.find("grant_type=").unwrap_or(n);
                let body = &request[body_start..];
                let (status, json) = if body.contains("grant_type=authorization_code") {
                    let n = state.exchange_count.fetch_add(1, Ordering::SeqCst);
                    (
                        200u16,
                        format!(
                            r#"{{"access_token":"access_flow_{n}","token_type":"Bearer","expires_in":3600,"refresh_token":"refresh_flow_{n}"}}"#
                        ),
                    )
                } else if !body.contains("grant_type=refresh_token") {
                    (
                        400u16,
                        r#"{\"error\":\"unsupported_grant_type\"}"#.to_string(),
                    )
                } else if state.reject_refresh.load(Ordering::SeqCst) {
                    (
                        400,
                        r#"{\"error\":\"invalid_grant\",\"error_description\":\"revoked\"}"#.into(),
                    )
                } else {
                    let n = state.refresh_count.fetch_add(1, Ordering::SeqCst);
                    (
                        200,
                        format!(
                            r#"{{"access_token":"access_refresh_{n}","token_type":"Bearer","expires_in":3600,"refresh_token":"refresh_engine_{n}"}}"#
                        ),
                    )
                };
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.flush().await;
            });
        }
    });
    (addr, handle)
}

// ---- helpers ----------------------------------------------------------------

async fn wait_event(
    events: &mut tokio::sync::broadcast::Receiver<EngineEvent>,
    timeout: Duration,
    predicate: &dyn Fn(&EngineEvent) -> bool,
) -> Option<EngineEvent> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        match tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
            Ok(Ok(ev)) if predicate(&ev) => return Some(ev),
            Ok(_) | Err(_) => {}
        }
    }
}

/// Extracts the `state` query parameter and loopback redirect port from an
/// authorization URL.
fn parse_state_and_port(url: &str) -> (String, u16) {
    let state = url
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_owned();
    let port: u16 = url
        .split("redirect_uri=http%3A%2F%2F127.0.0.1%3A")
        .nth(1)
        .unwrap()
        .split('%')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    (state, port)
}

/// Simulates the browser: one raw HTTP GET against the loopback capture.
async fn browser_redirect(port: u16, path: &str) -> String {
    let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    sock.write_all(request.as_bytes()).await.unwrap();
    let mut buf = String::new();
    let _ = sock.read_to_string(&mut buf).await;
    buf
}

async fn spawn_engine() -> (
    kestrel_engine::EngineHandle,
    tokio::sync::broadcast::Receiver<EngineEvent>,
    tempfile::TempDir,
) {
    let (dir, _paths) = temp_paths();
    let paths = Arc::new(Paths::nested_under(dir.path()));
    paths.ensure().unwrap();
    let engine = Engine::spawn_with(
        Arc::new(Config::default()),
        paths,
        Arc::new(kestrel_core::ids::SystemIdGenerator),
        Arc::new(kestrel_core::clock::SystemClock),
        Arc::new(kestrel_crypto::InMemoryStore::new()),
    )
    .await
    .expect("engine spawns");
    let events = engine.events();
    (engine, events, dir)
}

// ---- the gates ----------------------------------------------------------------

#[tokio::test]
#[ignore = "KESTREL_INTEGRATION=1 (exit gate; hermetic mock IdP, no docker)"]
// The gate reads as four sequential proof steps; extracting more helpers
// would scatter the acceptance narrative.
#[allow(clippy::too_many_lines)]
async fn integration_oauth_flow_completion_end_to_end() {
    if std::env::var("KESTREL_INTEGRATION").is_err() {
        return;
    }

    let idp = Arc::new(IdpState::default());
    let (addr, server) = spawn_idp(idp.clone()).await;
    // SAFETY: single-threaded test process under nextest; the engine spawns
    // after these writes.
    unsafe {
        std::env::set_var("KESTREL_OAUTH2_TOKEN_URL", format!("http://{addr}/token"));
        std::env::set_var(
            "KESTREL_OAUTH2_AUTH_URL",
            format!("http://{addr}/authorize"),
        );
    }

    let (engine, mut events, _dir) = spawn_engine().await;

    // 1. Start the flow; the URL carries state + loopback redirect port.
    let (tx, rx) = tokio::sync::oneshot::channel();
    engine
        .commands
        .send(command(
            FrontendKind::Tui,
            CommandPayload::StartOAuth2Flow {
                provider: Provider::Gmail,
                reply: tx,
            },
        ))
        .await
        .expect("command accepted");
    let Reply::OAuthUrl(url) = rx.await.expect("reply") else {
        panic!("expected OAuthUrl reply");
    };
    let (state, port) = parse_state_and_port(&url);

    // 2. Browser redirect → autonomous completer exchanges the code.
    let page = browser_redirect(port, &format!("/cb?code=AUTHZ_X&state={state}")).await;
    assert!(page.contains("200 OK"), "capture served a page: {page}");
    let ev = wait_event(&mut events, Duration::from_secs(10), &|ev| {
        matches!(ev, EngineEvent::OAuth2FlowCompleted { .. })
    })
    .await
    .expect("flow completion event never published");
    let EngineEvent::OAuth2FlowCompleted {
        state: done_state,
        result: Ok(()),
    } = ev
    else {
        panic!("expected successful completion, got {ev:?}");
    };
    assert_eq!(done_state, state, "event keyed by the flow's state");
    assert_eq!(idp.exchange_count.load(Ordering::SeqCst), 1, "one exchange");

    // 3. Retrieve the tokens via CompleteOAuth2Flow — exactly once.
    let tokens = complete(&engine, &state).await;
    assert!(tokens.contains("access_flow_0"), "exchanged set: {tokens}");
    assert!(tokens.contains("refresh_flow_0"), "refresh token present");
    // Second completion of the same state must fail (single-use).
    let (tx, rx) = tokio::sync::oneshot::channel();
    engine
        .commands
        .send(command(
            FrontendKind::Tui,
            CommandPayload::CompleteOAuth2Flow {
                state: state.clone(),
                reply: tx,
            },
        ))
        .await
        .expect("command accepted");
    assert!(matches!(rx.await.expect("reply"), Reply::Err(_)));
    // Unknown state must fail too.
    assert!(complete_err(&engine, "nosuchstate").await);

    // 4. The exchanged refresh token links to an account via AddAccount
    //    (the #26 keyring path) and the refresh worker refreshes with it.
    let refresh_secret = extract_refresh_token(&tokens);
    let (tx, rx) = tokio::sync::oneshot::channel();
    engine
        .commands
        .send(command(
            FrontendKind::Tui,
            CommandPayload::AddAccount {
                config: kestrel_core::provider::AccountConfig {
                    display_name: "flow".into(),
                    email: "flow@gmail.com".into(),
                    provider: Provider::Gmail,
                    imap_host: "127.0.0.1".into(),
                    imap_port: 1,
                    imap_security: "tls".into(),
                    smtp_host: "127.0.0.1".into(),
                    smtp_port: 1,
                    smtp_security: "tls".into(),
                    auth_kind: "oauth2".into(),
                    username: Some("flow@gmail.com".into()),
                },
                password: kestrel_core::secrets::SecretString::new(refresh_secret),
                reply: tx,
            },
        ))
        .await
        .expect("command accepted");
    let Reply::Accounts(accounts) = rx.await.expect("reply") else {
        panic!("expected Accounts reply");
    };
    assert_eq!(accounts.len(), 1, "account linked");

    // The worker must refresh the exchanged credential unattended.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if idp.refresh_count.load(Ordering::SeqCst) >= 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "worker never refreshed the exchanged token"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    server.abort();
    // SAFETY: see the set_var note; test teardown.
    unsafe {
        std::env::remove_var("KESTREL_OAUTH2_TOKEN_URL");
        std::env::remove_var("KESTREL_OAUTH2_AUTH_URL");
    }
}

/// Second gate: the failure paths — wrong `state` at the capture (CSRF),
/// and no redirect at all (timeout) — publish typed failures and leave no
/// retrievable flow behind.
#[tokio::test]
#[ignore = "KESTREL_INTEGRATION=1 (exit gate; hermetic mock IdP, no docker)"]
async fn integration_oauth_flow_rejects_bad_state_and_timeout() {
    if std::env::var("KESTREL_INTEGRATION").is_err() {
        return;
    }

    let idp = Arc::new(IdpState::default());
    let (addr, server) = spawn_idp(idp.clone()).await;
    // SAFETY: single-threaded test process under nextest.
    unsafe {
        std::env::set_var("KESTREL_OAUTH2_TOKEN_URL", format!("http://{addr}/token"));
        std::env::set_var(
            "KESTREL_OAUTH2_AUTH_URL",
            format!("http://{addr}/authorize"),
        );
    }

    let (engine, mut events, _dir) = spawn_engine().await;

    // (a) Wrong state in the redirect: the capture validates and fails.
    let (tx, rx) = tokio::sync::oneshot::channel();
    engine
        .commands
        .send(command(
            FrontendKind::Tui,
            CommandPayload::StartOAuth2Flow {
                provider: Provider::Gmail,
                reply: tx,
            },
        ))
        .await
        .expect("command accepted");
    let Reply::OAuthUrl(url) = rx.await.expect("reply") else {
        panic!("expected OAuthUrl reply");
    };
    let (_state, port) = parse_state_and_port(&url);
    let page = browser_redirect(port, "/cb?code=EVIL&state=NOT_THE_STATE").await;
    assert!(page.contains("200 OK"));
    let ev = wait_event(&mut events, Duration::from_secs(10), &|ev| {
        matches!(ev, EngineEvent::OAuth2FlowCompleted { result: Err(_), .. })
    })
    .await
    .expect("failure event never published for state mismatch");
    assert!(matches!(ev, EngineEvent::OAuth2FlowCompleted { .. }));
    assert_eq!(
        idp.exchange_count.load(Ordering::SeqCst),
        0,
        "no exchange may happen after a state mismatch"
    );
    assert!(
        complete_err(&engine, "whatever").await,
        "no flow retrievable"
    );

    server.abort();
    // SAFETY: see the set_var note; test teardown.
    unsafe {
        std::env::remove_var("KESTREL_OAUTH2_TOKEN_URL");
        std::env::remove_var("KESTREL_OAUTH2_AUTH_URL");
    }
}

// ---- small protocol helpers --------------------------------------------------

async fn complete(engine: &kestrel_engine::EngineHandle, state: &str) -> String {
    let (tx, rx) = tokio::sync::oneshot::channel();
    engine
        .commands
        .send(command(
            FrontendKind::Tui,
            CommandPayload::CompleteOAuth2Flow {
                state: state.to_owned(),
                reply: tx,
            },
        ))
        .await
        .expect("command accepted");
    match rx.await.expect("reply") {
        Reply::OAuthTokens(tokens) => tokens.expose().to_owned(),
        other => panic!("expected OAuthTokens, got {other:?}"),
    }
}

async fn complete_err(engine: &kestrel_engine::EngineHandle, state: &str) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel();
    engine
        .commands
        .send(command(
            FrontendKind::Tui,
            CommandPayload::CompleteOAuth2Flow {
                state: state.to_owned(),
                reply: tx,
            },
        ))
        .await
        .expect("command accepted");
    matches!(rx.await.expect("reply"), Reply::Err(_))
}

/// Pulls `refresh_token` out of the serialized token-set blob (the test
/// links the account through the same JSON the frontend would).
fn extract_refresh_token(tokens: &str) -> String {
    let idx = tokens
        .find("refresh_token\":")
        .expect("refresh_token field");
    let rest = &tokens[idx + "refresh_token\":".len()..];
    let quoted = rest.trim_start().strip_prefix('"').expect("quoted");
    quoted.split('"').next().expect("closing quote").to_owned()
}
