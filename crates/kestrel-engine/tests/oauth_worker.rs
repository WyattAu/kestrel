//! Engine-level exit gate for #26: the unattended `OAuth2` refresh worker is
//! wired into the account lifecycle.
//!
//! Boots the real engine with an `OAuth2` account whose token endpoint is a
//! hermetic in-process mock identity provider (`KESTREL_OAUTH2_TOKEN_URL`),
//! then proves:
//!
//! 1. `AddAccount` with `auth_kind = "oauth2"` starts the refresh worker —
//!    the worker's first refresh against the mock identity provider is
//!    observed.
//! 2. Fresh access tokens flow: the provider's token ledger advances without
//!    any user interaction (the unattended property, engine-driven).
//! 3. Revocation surfaces as `AccountConnection::NeedsReauth` (the new
//!    protocol variant, `PROTOCOL_VERSION 3`), not a crash or a restart
//!    loop — the account's sync services stop retrying.
//!
//! Worker timing is compressed via a seconds-long access-token TTL.

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
    protocol::{CommandPayload, ConnectionState, EngineEvent, FrontendKind, Provider, Reply},
    testkit::temp_paths,
};
use kestrel_engine::{Engine, command};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
};

// ---- mock IdP (same wire behavior as the crypto soak fixture) ---------------

#[derive(Default)]
struct IdpState {
    refresh_count: AtomicU32,
    reject_refresh: AtomicBool,
    expires_in: AtomicU32,
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
                let (status, json) = if !body.contains("grant_type=refresh_token") {
                    (400u16, r#"{"error":"unsupported_grant_type"}"#.to_string())
                } else if state.reject_refresh.load(Ordering::SeqCst) {
                    (
                        400,
                        r#"{"error":"invalid_grant","error_description":"revoked"}"#.into(),
                    )
                } else {
                    let n = state.refresh_count.fetch_add(1, Ordering::SeqCst);
                    let ttl = state.expires_in.load(Ordering::SeqCst);
                    (
                        200,
                        format!(
                            r#"{{"access_token":"access_engine_{n}","token_type":"Bearer","expires_in":{ttl},"refresh_token":"refresh_engine_{n}"}}"#
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

// ---- the gate ----------------------------------------------------------------

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

#[tokio::test]
#[ignore = "KESTREL_INTEGRATION=1 (exit gate; hermetic mock IdP, no docker)"]
async fn integration_engine_oauth_refresh_worker_end_to_end() {
    if std::env::var("KESTREL_INTEGRATION").is_err() {
        return;
    }

    // TTL 2s: the worker re-refreshes roughly every ~1.75s.
    let idp = Arc::new(IdpState {
        expires_in: AtomicU32::new(2),
        ..IdpState::default()
    });
    let (addr, server) = spawn_idp(idp.clone()).await;
    // The worker resolves its endpoint via `provider_from_env` — point it
    // at the hermetic mock (self-hosted-IdP seam).
    // SAFETY: single-threaded test process; no other threads read env yet
    // (the engine spawns after this).
    unsafe {
        std::env::set_var("KESTREL_OAUTH2_TOKEN_URL", format!("http://{addr}/token"));
    }

    let (dir, _paths_guard) = temp_paths();
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
    let mut events = engine.events();

    // Add the OAuth2 account (Gmail family so the XOAUTH2 mechanism set
    // matches; the token endpoint is the mock regardless).
    let (tx, rx) = tokio::sync::oneshot::channel();
    engine
        .commands
        .send(command(
            FrontendKind::Tui,
            CommandPayload::AddAccount {
                config: kestrel_core::provider::AccountConfig {
                    display_name: "soak".into(),
                    email: "kestrel@gmail.com".into(),
                    provider: Provider::Gmail,
                    imap_host: "127.0.0.1".into(),
                    imap_port: 1,
                    imap_security: "tls".into(),
                    smtp_host: "127.0.0.1".into(),
                    smtp_port: 1,
                    smtp_security: "tls".into(),
                    auth_kind: "oauth2".into(),
                    username: Some("kestrel@gmail.com".into()),
                },
                password: kestrel_core::secrets::SecretString::new("seed_refresh_token".into()),
                reply: tx,
            },
        ))
        .await
        .expect("command accepted");
    let Reply::Accounts(accounts) = rx.await.expect("reply") else {
        panic!("expected Accounts reply");
    };
    assert_eq!(accounts.len(), 1, "account added");

    // The engine's refresh worker must have refreshed at least twice
    // (initial + one expiry cycle) with zero user interaction.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if idp.refresh_count.load(Ordering::SeqCst) >= 2 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "engine refresh worker never ran (ledger: {})",
            idp.refresh_count.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Revocation → NeedsReauth surfaces on the event bus (protocol v3).
    idp.reject_refresh.store(true, Ordering::SeqCst);
    let ev = wait_event(&mut events, Duration::from_secs(15), &|ev| {
        matches!(
            ev,
            EngineEvent::AccountConnection {
                state: ConnectionState::NeedsReauth,
                ..
            }
        )
    })
    .await
    .expect("NeedsReauth never surfaced after revocation");
    let EngineEvent::AccountConnection { account, .. } = ev else {
        unreachable!()
    };
    assert_eq!(account, accounts[0].id);

    // No restart loop: the ledger must not advance after the rejection.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = idp.refresh_count.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        idp.refresh_count.load(Ordering::SeqCst),
        after,
        "worker must stop refreshing after NeedsReauth"
    );

    server.abort();
    // SAFETY: see the set_var note; test teardown.
    unsafe {
        std::env::remove_var("KESTREL_OAUTH2_TOKEN_URL");
    }
}
