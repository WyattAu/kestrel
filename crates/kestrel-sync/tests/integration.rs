//! Docker-gated integration tests for the sync engine
//! (`docs/testing-strategy.md` §5, `docs/sync-engine.md` §9 matrix).
//!
//! Every test is `#[ignore]`d and named `integration_*`: the default
//! nextest profile skips them; `--profile integration` runs them with
//! retries. Fixtures: `tests/integration/docker-compose.yml` (Dovecot on
//! 1143, Greenmail SMTP on 1025).
//!
//! The Dovecot fixture runs cleartext IMAP on loopback — test-only
//! posture (`Security::Insecure` documents this constraint).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    missing_docs,
    clippy::print_stderr
)]

use std::{sync::Arc, time::Duration};

use kestrel_core::{
    clock::SystemClock, config::Config, ids::SystemIdGenerator, sasl::SaslMechanism,
    secrets::SecretString,
};
use kestrel_crypto::{CredentialService, credentials::InMemoryStore};
use kestrel_storage::{NewAccount, StorageService};
use kestrel_sync::{ConnectParams, ImapSession, OutboxService, Security, SmtpParams, SyncService};

const IMAP_HOST: &str = "127.0.0.1";
const IMAP_PORT: u16 = 1143;
const SMTP_PORT: u16 = 1025;
const USERNAME: &str = "kestrel";
const PASSWORD: &str = "testpass";

fn fixture_ready() -> bool {
    std::env::var("KESTREL_INTEGRATION").is_ok()
}

async fn wait_dovecot() {
    for _ in 0..60 {
        if tokio::net::TcpStream::connect((IMAP_HOST, IMAP_PORT))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("dovecot fixture not reachable on {IMAP_HOST}:{IMAP_PORT} (docker compose up?)");
}

fn connect_params() -> ConnectParams {
    ConnectParams {
        host: IMAP_HOST.into(),
        port: IMAP_PORT,
        security: Security::Insecure,
        username: USERNAME.into(),
        secret: SecretString::new(PASSWORD.into()),
        mechanisms: vec![SaslMechanism::Plain],
        tls: TlsConnector::from(test_tls_config()),
        sasl_factory: Arc::new(|mech, user, secret| {
            kestrel_crypto::sasl::start(mech, user, secret)
        }),
    }
}

fn smtp_params() -> SmtpParams {
    SmtpParams {
        host: IMAP_HOST.into(),
        port: SMTP_PORT,
        username: USERNAME.into(),
        secret: SecretString::new(PASSWORD.into()),
        oauth2: false,
        security: kestrel_sync::SmtpSecurity::Insecure,
    }
}

use tokio_rustls::TlsConnector;

fn test_tls_config() -> Arc<rustls::ClientConfig> {
    // Loopback fixture: trust the webpki roots; the insecure transport
    // means this connector is never actually used.
    kestrel_crypto::tls_config(None).unwrap()
}

#[tokio::test]
#[ignore = "docker: tests/integration/docker-compose.yml"]
async fn integration_cold_initial_sync_lists_folders() {
    if !fixture_ready() {
        eprintln!("skipping: KESTREL_INTEGRATION not set");
        return;
    }
    wait_dovecot().await;

    let mut session = ImapSession::connect_and_authenticate(&connect_params())
        .await
        .expect("connect+auth against dovecot");
    assert!(session.capabilities().iter().count() > 3);
    let outcome = session
        .execute(
            imap_next::imap_types::command::CommandBody::List {
                reference: imap_next::imap_types::mailbox::Mailbox::Inbox,
                mailbox_wildcard: imap_next::imap_types::mailbox::ListMailbox::try_from("*")
                    .unwrap(),
            },
            Duration::from_secs(30),
        )
        .await
        .expect("LIST ok");
    assert!(outcome.is_ok());
    assert!(
        outcome
            .data
            .iter()
            .any(|d| matches!(d, imap_next::imap_types::response::Data::List { .. }))
    );
    session.logout().await;
}

#[tokio::test]
#[ignore = "docker: tests/integration/docker-compose.yml"]
async fn integration_select_fetch_and_idle_on_inbox() {
    if !fixture_ready() {
        eprintln!("skipping: KESTREL_INTEGRATION not set");
        return;
    }
    wait_dovecot().await;

    let mut session = ImapSession::connect_and_authenticate(&connect_params())
        .await
        .expect("connect");

    // SELECT INBOX.
    let outcome = session
        .execute(
            imap_next::imap_types::command::CommandBody::Select {
                mailbox: imap_next::imap_types::mailbox::Mailbox::Inbox,
                parameters: Vec::new(),
            },
            Duration::from_secs(30),
        )
        .await
        .expect("SELECT ok");
    assert!(outcome.is_ok());

    // APPEND a message so the mailbox is non-empty.
    let raw = b"From: sender@example.org\r\nTo: kestrel@example.org\r\nSubject: integration hello\r\n\r\nhello from the fixture\r\n";
    let outcome = session
        .execute(
            imap_next::imap_types::command::CommandBody::Append {
                mailbox: imap_next::imap_types::mailbox::Mailbox::Inbox,
                flags: vec![],
                date: None,
                message: imap_next::imap_types::extensions::binary::LiteralOrLiteral8::Literal(
                    imap_next::imap_types::core::Literal::try_from(raw.to_vec()).unwrap(),
                ),
            },
            Duration::from_secs(30),
        )
        .await
        .expect("APPEND ok");
    assert!(outcome.is_ok());

    // Envelope fetch.
    let outcome = session.fetch_envelopes("1:*").await.expect("FETCH ok");
    assert!(outcome.is_ok());
    assert!(
        outcome
            .data
            .iter()
            .any(|d| matches!(d, imap_next::imap_types::response::Data::Fetch { .. }))
    );

    // Raw fetch.
    let outcome = session.fetch_raw(1).await.expect("raw fetch ok");
    assert!(outcome.is_ok());

    // A short IDLE round (no server push expected within 2s → empty wake).
    let woke = session.idle(Duration::from_secs(2)).await.expect("idle");
    // Dovecot supports IDLE; empty wake is a valid outcome.
    let _ = woke;
    session.logout().await;
}

#[tokio::test]
#[ignore = "docker: tests/integration/docker-compose.yml"]
async fn integration_smtp_submit_via_greenmail() {
    if !fixture_ready() {
        eprintln!("skipping: KESTREL_INTEGRATION not set");
        return;
    }
    for _ in 0..60 {
        if tokio::net::TcpStream::connect((IMAP_HOST, SMTP_PORT))
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let raw = b"From: kestrel@example.org\r\nTo: dest@example.org\r\nSubject: smtp roundtrip\r\n\r\nvia greenmail\r\n";
    kestrel_sync::submit_envelope(
        &smtp_params(),
        "kestrel@example.org",
        &["dest@example.org".to_string()],
        raw,
    )
    .await
    .expect("SMTP submit accepted by greenmail");
}

#[tokio::test]
#[ignore = "docker: tests/integration/docker-compose.yml"]
async fn integration_sync_service_full_cycle() {
    if !fixture_ready() {
        eprintln!("skipping: KESTREL_INTEGRATION not set");
        return;
    }
    wait_dovecot().await;
    let _ = tracing_subscriber::fmt::try_init();

    let (dir, paths) = kestrel_core::testkit::temp_paths();
    paths.ensure().unwrap();
    let ids = Arc::new(SystemIdGenerator);
    let clock = Arc::new(SystemClock);
    let (storage, cancel_storage) = StorageService::spawn(paths.clone(), ids, clock.clone());

    let account = storage
        .upsert_account(NewAccount {
            name: "Integration".into(),
            email: "kestrel@example.org".into(),
            provider: kestrel_core::protocol::Provider::Generic,
            protocol: kestrel_core::protocol::MailProtocol::Imap,
            auth_kind: "password".into(),
            host: String::new(),
        })
        .await
        .unwrap();

    // Seed the mailbox directly first.
    {
        let mut session = ImapSession::connect_and_authenticate(&connect_params())
            .await
            .unwrap();
        session
            .execute(
                imap_next::imap_types::command::CommandBody::Append {
                    mailbox: imap_next::imap_types::mailbox::Mailbox::Inbox,
                    flags: vec![],
                    date: None,
                    message: imap_next::imap_types::extensions::binary::LiteralOrLiteral8::Literal(
                        imap_next::imap_types::core::Literal::try_from(
                            b"From: news@example.org\r\nTo: kestrel@example.org\r\nSubject: cycle news\r\n\r\nbody of cycle\r\n".to_vec()
                        )
                        .unwrap(),
                    ),
                },
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        session.logout().await;
    }

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
    let cancel = tokio_util::sync::CancellationToken::new();
    let store: Arc<dyn kestrel_core::store_model::MailStore> = Arc::new(storage.clone());
    let service = SyncService::new(
        account,
        connect_params(),
        store,
        Arc::new(Config::default()),
        clock,
        event_tx,
    );
    let cancel_for_task = cancel.clone();
    let handle = tokio::spawn(async move { service.run(cancel_for_task).await });

    // Expect folder tree + (eventually) a message ingested.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut saw_folders = false;
    let mut saw_mail = false;
    while tokio::time::Instant::now() < deadline && !(saw_folders && saw_mail) {
        match tokio::time::timeout(Duration::from_secs(5), event_rx.recv()).await {
            Ok(Some(kestrel_core::protocol::EngineEvent::FolderTreeChanged { .. })) => {
                saw_folders = true;
            }
            Ok(Some(kestrel_core::protocol::EngineEvent::MailArrived { .. })) => {
                saw_mail = true;
            }
            Ok(Some(kestrel_core::protocol::EngineEvent::AccountConnection { state, .. })) => {
                tracing::debug!(?state, "connection transition");
            }
            _ => {}
        }
    }
    cancel.cancel();
    let _ = handle.await;

    assert!(saw_folders, "folder tree event");
    assert!(saw_mail, "mail arrived event");
    let folders = storage.list_folders(account).await.unwrap();
    assert!(!folders.is_empty(), "at least INBOX stored");
    cancel_storage.cancel();
    drop(dir);
}

#[tokio::test]
#[ignore = "docker: tests/integration/docker-compose.yml"]
async fn integration_outbox_flush_through_smtp_and_append() {
    if !fixture_ready() {
        eprintln!("skipping: KESTREL_INTEGRATION not set");
        return;
    }
    wait_dovecot().await;

    let (dir, paths) = kestrel_core::testkit::temp_paths();
    paths.ensure().unwrap();
    let ids = Arc::new(SystemIdGenerator);
    let clock = Arc::new(SystemClock);
    let (storage, cancel_storage) = StorageService::spawn(paths.clone(), ids, clock.clone());

    let account = storage
        .upsert_account(NewAccount {
            name: "OutboxIt".into(),
            email: "kestrel@example.org".into(),
            provider: kestrel_core::protocol::Provider::Generic,
            protocol: kestrel_core::protocol::MailProtocol::Imap,
            auth_kind: "password".into(),
            host: String::new(),
        })
        .await
        .unwrap();

    // Enqueue a draft.
    let envelope = kestrel_core::store_model::OutboxEnvelope {
        from: kestrel_core::protocol::Address::bare("kestrel@example.org"),
        to: vec![kestrel_core::protocol::Address::bare("dest@example.org")],
        cc: vec![],
        bcc: vec![],
        subject: "outbox roundtrip".into(),
    };
    let raw = kestrel_core::compose::build_rfc5322(
        &kestrel_core::protocol::Draft {
            account,
            from: kestrel_core::protocol::Address::bare("kestrel@example.org"),
            to: vec![kestrel_core::protocol::Address::bare("dest@example.org")],
            cc: vec![],
            bcc: vec![],
            subject: "outbox roundtrip".into(),
            in_reply_to: None,
            references: vec![],
            body_markdown: "**hello** outbox".into(),
            attachments: vec![],
            pgp_sign: false,
            pgp_encrypt: false,
            smime_sign: false,
            smime_encrypt: false,
            send_after: None,
            priority: kestrel_core::protocol::Priority::Normal,
        },
        &kestrel_core::ids::SystemIdGenerator,
        &kestrel_core::clock::SystemClock,
    )
    .unwrap();
    let id = storage
        .outbox_enqueue(account, envelope, raw, None)
        .await
        .unwrap();

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
    let online = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let store: Arc<dyn kestrel_core::store_model::MailStore> = Arc::new(storage.clone());
    let service = OutboxService::new(
        store,
        smtp_params(),
        connect_params(),
        clock,
        event_tx,
        online,
    );
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_for_task = cancel.clone();
    let handle = tokio::spawn(async move { service.run(cancel_for_task).await });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut sent = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), event_rx.recv()).await {
            Ok(Some(kestrel_core::protocol::EngineEvent::MailSent { .. })) => {
                sent = true;
                break;
            }
            Ok(Some(kestrel_core::protocol::EngineEvent::OutboxRetry { attempt, .. })) => {
                // Tolerate retries while greenmail warms up.
                tracing::debug!(attempt, "outbox retry tolerated");
            }
            _ => {}
        }
    }
    cancel.cancel();
    let _ = handle.await;
    assert!(sent, "MailSent within deadline");

    // The row is marked sent and not due again.
    let due = storage.outbox_due().await.unwrap();
    assert!(due.iter().all(|r| r.id != id), "sent row no longer due");
    cancel_storage.cancel();
    drop(dir);
}

#[tokio::test]
#[ignore = "docker: tests/integration/docker-compose.yml"]
async fn integration_credential_service_roundtrip() {
    // Not docker-gated in substance, but exercises the full stack alongside
    // the fixture boot path.
    if !fixture_ready() {
        eprintln!("skipping: KESTREL_INTEGRATION not set");
        return;
    }
    let svc = CredentialService::new(std::sync::Arc::new(InMemoryStore::new()));
    let account = kestrel_core::ids::AccountId::from_uuid(uuid::Uuid::now_v7());
    svc.set_password(account, &SecretString::new("s3cret".into()))
        .unwrap();
    assert_eq!(svc.password(account).unwrap().unwrap().expose(), "s3cret");
    svc.purge(account).unwrap();
    assert!(svc.password(account).unwrap().is_none());
}
