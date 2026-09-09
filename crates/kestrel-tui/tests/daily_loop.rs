//! Phase-3 exit gate 2 + 2b (#27): the *daily-loop journey* through the
//! real TUI event loop against the real compose stack (Dovecot IMAP +
//! Greenmail SMTP), plus the keypress→paint latency budget.
//!
//! What is proven, end to end:
//!
//! 1. The engine boots with no accounts; an account is added through the
//!    protocol (`AddAccount`, exactly like the setup wizard) against the
//!    Docker Dovecot fixture.
//! 2. A message is injected server-side via IMAP APPEND (simulated
//!    delivery) and the sync engine ingests it into the local cache.
//! 3. The real `event::event_loop` runs on a `TestBackend` with scripted
//!    keys: read (open preview) → reply (`$EDITOR` is a script that
//!    rewrites the draft) → archive (`x`) → quit. These are the same key
//!    handlers a human exercises.
//! 4. Server-side asserts: the reply round-trips outbox → Greenmail SMTP →
//!    Sent APPEND on Dovecot; the archived message leaves INBOX server-side
//!    (UID MOVE pushed by the mutation-push path) and lands in Archive.
//! 5. Latency gate 2b: every scripted keypress records a key→paint sample
//!    in `AppState`; p50 must stay under the 16 ms interaction budget.
//!
//! Docker-gated (`KESTREL_INTEGRATION=1`, nextest `integration` profile):
//! same discipline as the other exit gates — real dependency, not a stub.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::{sync::Arc, time::Duration};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kestrel_core::{
    config::Config,
    protocol::{CommandPayload, FrontendKind, Provider, Reply},
    testkit::temp_paths,
};
use kestrel_engine::{Engine, command};
use kestrel_sync::{ConnectParams, ImapSession, Security};
use kestrel_tui::{
    app::AppState,
    event::{TermEvent, event_loop, test_refresh_all, test_select_folder},
};

const IMAP_HOST: &str = "127.0.0.1";
const IMAP_PORT: u16 = 1143;
const SMTP_PORT: u16 = 1025;

fn connect_params() -> ConnectParams {
    ConnectParams {
        host: IMAP_HOST.into(),
        port: IMAP_PORT,
        security: Security::Insecure,
        username: "kestrel".into(),
        secret: kestrel_core::secrets::SecretString::new("testpass".into()),
        secret_override: None,
        mechanisms: vec![kestrel_core::sasl::SaslMechanism::Plain],
        tls: tokio_rustls::TlsConnector::from(
            kestrel_crypto::tls_config(None).expect("tls config"),
        ),
        sasl_factory: Arc::new(|mech, user, secret| {
            kestrel_crypto::sasl::start(mech, user, secret)
        }),
    }
}

/// Appends a raw RFC 5322 message to Dovecot's INBOX (simulated delivery).
async fn deliver_to_inbox(raw: &[u8]) {
    let mut session = ImapSession::connect_and_authenticate(&connect_params())
        .await
        .expect("IMAP connect for APPEND");
    let outcome = session
        .execute(
            imap_next::imap_types::command::CommandBody::Append {
                mailbox: imap_next::imap_types::mailbox::Mailbox::Inbox,
                flags: vec![],
                date: None,
                message: imap_next::imap_types::extensions::binary::LiteralOrLiteral8::Literal(
                    imap_next::imap_types::core::Literal::try_from(raw.to_vec()).expect("literal"),
                ),
            },
            Duration::from_secs(20),
        )
        .await
        .expect("IMAP APPEND");
    assert!(
        outcome.is_ok(),
        "APPEND failed: {}",
        outcome.status_summary()
    );
    session.logout().await;
}

/// Server-side truth: the UIDs currently in a Dovecot mailbox.
async fn server_uids(mailbox: &str) -> Vec<u32> {
    use imap_next::imap_types::{
        command::CommandBody, mailbox::Mailbox, response::Data, search::SearchKey,
    };
    let mut session = ImapSession::connect_and_authenticate(&connect_params())
        .await
        .expect("IMAP connect for SEARCH");
    let select_mailbox = Mailbox::try_from(mailbox.to_owned()).expect("mailbox");
    session
        .execute(
            CommandBody::Select {
                mailbox: select_mailbox,
                parameters: Vec::new(),
            },
            Duration::from_secs(20),
        )
        .await
        .expect("SELECT");
    let outcome = session
        .execute(
            CommandBody::Search {
                charset: None,
                criteria: imap_next::imap_types::core::Vec1::from(SearchKey::All),
                uid: true,
            },
            Duration::from_secs(20),
        )
        .await
        .expect("UID SEARCH");
    session.logout().await;
    let mut uids = Vec::new();
    for data in &outcome.data {
        if let Data::Search(found, _) = data {
            uids.extend(found.iter().map(|u| u.get()));
        }
    }
    uids.sort_unstable();
    uids
}

/// Clears a Dovecot mailbox (all flags removed + EXPUNGE) so test reruns
/// start from a known server state (nextest retries re-run the journey).
async fn clear_mailbox(mailbox: &str) {
    use imap_next::imap_types::{
        command::CommandBody,
        core::Vec1,
        flag::{StoreResponse, StoreType},
        mailbox::Mailbox,
        response::Data,
        search::SearchKey,
        sequence::SequenceSet,
    };
    let mut session = ImapSession::connect_and_authenticate(&connect_params())
        .await
        .expect("IMAP connect for clear");
    let select_mailbox = Mailbox::try_from(mailbox.to_owned()).expect("mailbox");
    session
        .execute(
            CommandBody::Select {
                mailbox: select_mailbox,
                parameters: Vec::new(),
            },
            Duration::from_secs(20),
        )
        .await
        .expect("SELECT");
    // Collect UIDs (SELECT responses may include EXISTS but not a UID list).
    let outcome = session
        .execute(
            CommandBody::Search {
                charset: None,
                criteria: Vec1::from(SearchKey::All),
                uid: true,
            },
            Duration::from_secs(20),
        )
        .await
        .expect("UID SEARCH");
    let mut uids = Vec::new();
    for data in &outcome.data {
        if let Data::Search(found, _) = data {
            uids.extend(found.iter().copied());
        }
    }
    if !uids.is_empty() {
        let set = SequenceSet::try_from(uids).expect("sequence set");
        let outcome = session
            .execute(
                CommandBody::Store {
                    sequence_set: set,
                    kind: StoreType::Replace,
                    response: StoreResponse::Silent,
                    flags: vec![imap_next::imap_types::flag::Flag::Deleted],
                    uid: true,
                    modifiers: Vec::new(),
                },
                Duration::from_secs(20),
            )
            .await
            .expect("STORE");
        assert!(
            outcome.is_ok(),
            "STORE \\Deleted failed: {}",
            outcome.status_summary()
        );
        let outcome = session
            .execute(CommandBody::Expunge, Duration::from_secs(20))
            .await
            .expect("EXPUNGE");
        assert!(
            outcome.is_ok(),
            "EXPUNGE failed: {}",
            outcome.status_summary()
        );
    }
    session.logout().await;
}

/// The scripted `$EDITOR`: overwrites the draft file with a deterministic
/// reply body (the same contract a real editor fulfills).
fn write_editor_script(dir: &std::path::Path) -> std::path::PathBuf {
    let script = dir.join("fake-editor.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '# Re: gate-two\\n---\\ngate-two reply body\\n' > \"$1\"\n",
    )
    .expect("editor script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    script
}

// ---- the gate ----------------------------------------------------------------

#[tokio::test]
#[ignore = "docker-gated integration test (nextest integration profile)"]
#[allow(
    clippy::too_many_lines,
    clippy::print_stderr,
    clippy::disallowed_methods
)] // integration journey: one readable narrative; diagnostics; latency sampling
async fn integration_daily_loop_journey() {
    let _ = tracing_subscriber::fmt::try_init();
    // 1. Engine on a scratch profile.
    let (dir, _paths) = temp_paths();
    let paths = Arc::new(kestrel_core::paths::Paths::nested_under(dir.path()));
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
    let _events = engine.events();

    // 2. Account via the protocol (setup wizard path). Dovecot's Archive
    //    mailbox is pre-created by the compose stack entrypoint.
    let (tx, rx) = tokio::sync::oneshot::channel();
    engine
        .commands
        .send(command(
            FrontendKind::Tui,
            CommandPayload::AddAccount {
                config: kestrel_core::provider::AccountConfig {
                    display_name: "Gate Two".into(),
                    email: "kestrel@example.org".into(),
                    provider: Provider::Generic,
                    imap_host: IMAP_HOST.into(),
                    imap_port: IMAP_PORT,
                    imap_security: "insecure".into(),
                    smtp_host: IMAP_HOST.into(),
                    smtp_port: SMTP_PORT,
                    smtp_security: "insecure".into(),
                    auth_kind: "password".into(),
                    username: Some("kestrel".into()),
                },
                password: kestrel_core::secrets::SecretString::new("testpass".into()),
                reply: tx,
            },
        ))
        .await
        .expect("command accepted");
    let Reply::Accounts(accounts) = rx.await.expect("reply") else {
        panic!("expected Accounts reply");
    };
    assert_eq!(accounts.len(), 1, "account added through the protocol");

    // 2b. Retry hygiene: wipe the mailboxes the journey mutates so every
    //     attempt (nextest retries included) starts from a known state.
    for box_name in ["INBOX", "Archive", "Sent"] {
        clear_mailbox(box_name).await;
    }

    // 3. Seed a message server-side (simulated delivery), then wait until
    //    the sync engine ingests it (server → cache; `MailArrived`-style
    //    events or a folder delta expose it via a MessagesChanged).
    deliver_to_inbox(
        b"From: sender@example.org\r\n\
          To: kestrel@example.org\r\n\
          Subject: gate-two\r\n\
          Message-ID: <gate-two@example.org>\r\n\
          Date: Tue, 08 Sep 2026 10:00:00 +0000\r\n\
          Content-Type: text/plain; charset=utf-8\r\n\
          \r\n\
          daily loop journey seed body\r\n",
    )
    .await;
    let mut ingested = false;
    let deadline = tokio::time::Instant::now() + Duration::from_mins(1);
    let account_id = accounts[0].id;
    while tokio::time::Instant::now() < deadline {
        // Wait until the folder tree exists, then poll the listing.
        if let Ok(Reply::Folders(folders)) = list_folders(&engine, account_id).await
            && let Some(inbox) = folders
                .iter()
                .find(|f| f.remote_name.eq_ignore_ascii_case("INBOX"))
            && let Ok(Reply::Messages(page)) = list_messages(&engine, inbox.id).await
            && page
                .items
                .iter()
                .any(|m| m.subject.as_deref() == Some("gate-two"))
        {
            ingested = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(ingested, "seeded message never reached the local cache");

    // 4. Config with the scripted editor (composition must not depend on a
    //    real terminal; the wizard path allows an explicit editor command).
    let mut config = Config::default();
    config.editor.command = Some(
        write_editor_script(dir.path())
            .to_string_lossy()
            .to_string(),
    );
    let config = Arc::new(config);

    // 5. The real loop on a TestBackend with a scripted-key channel.
    //    The initial load runs *before* the loop starts (the test owns the
    //    state then), so the scripted keys hit a fully populated state —
    //    no race between the startup refresh and the first keypress.
    let (term_tx, mut term_rx) = tokio::sync::mpsc::channel::<TermEvent>(256);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).expect("backend");
    let handle = engine.clone();
    let mut state = AppState {
        status: "Kestrel".into(),
        ..AppState::default()
    };
    test_refresh_all(&handle, &mut state).await;
    // Focus INBOX (folder order from the engine is not guaranteed to put
    // it first — e.g. Archive sorts earlier) and reload its list.
    assert!(
        test_select_folder(&handle, &mut state, "INBOX").await,
        "journey precondition: INBOX folder exists"
    );
    assert!(
        !state.page.items.is_empty(),
        "journey precondition: INBOX has the seeded message"
    );
    let loop_task = tokio::spawn(async move {
        event_loop(
            &handle,
            &config,
            &mut terminal,
            &mut term_rx,
            &mut state,
            None,
        )
        .await
        .expect("event loop runs");
        state
    });

    // 6. Read: Tab to the message list, then Enter opens the preview.
    send_key(&term_tx, KeyCode::Tab).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    send_key(&term_tx, KeyCode::Enter).await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    // 7. Reply through $EDITOR (scripted): 'r' → editor → outbox queued.
    send_key(&term_tx, KeyCode::Char('r')).await;
    tokio::time::sleep(Duration::from_millis(800)).await;

    // 8. Archive: 'x' moves the message (locally + server push).
    send_key(&term_tx, KeyCode::Char('x')).await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    // 9. Quit via Ctrl-C (proves the loop exit path cleanly).
    term_tx
        .send(TermEvent::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )))
        .await
        .expect("send quit key");
    let state = tokio::time::timeout(Duration::from_secs(10), loop_task)
        .await
        .expect("loop exits on quit")
        .expect("loop task joins");

    // Post-journey state assertions (the loop owned the state; we get it
    // back after exit).
    eprintln!(
        "[gate2] final status: {:?}; preview_opens={} items={} focus={:?} latency={:?}",
        state.status,
        state.preview_opens,
        state.page.items.len(),
        state.focus,
        state.key_latency_us
    );
    assert!(
        state.preview_opens > 0,
        "preview never opened during the read step"
    );
    assert!(
        state.key_latency_us.len() >= 3,
        "expected ≥3 latency samples, got {}",
        state.key_latency_us.len()
    );
    let p50 = state.key_latency_p50_us().expect("latency samples present");
    assert!(
        p50 <= 16_000,
        "key→paint p50 {p50}µs exceeds the 16 ms interaction budget"
    );

    // 10. Server-side asserts.
    // 10a. Reply: outbox flushed via Greenmail SMTP → Sent APPEND on
    //      Dovecot (the outbox's own filing); the Sent folder gains a row.
    wait_for_server("sent append", Duration::from_secs(45), || async {
        !server_uids("Sent").await.is_empty()
    })
    .await;
    // 10b. Archive: the push drain moved the message server-side — the
    //      seed left INBOX and appeared in Archive.
    wait_for_server("archive move", Duration::from_secs(45), || async {
        server_uids("INBOX").await.is_empty() && !server_uids("Archive").await.is_empty()
    })
    .await;

    engine.shutdown(true).await;
}

// ---- helpers ------------------------------------------------------------------

async fn send_key(tx: &tokio::sync::mpsc::Sender<TermEvent>, code: KeyCode) {
    tx.send(TermEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)))
        .await
        .expect("loop alive");
}

/// Lists an account's folders through the protocol.
async fn list_folders(
    engine: &kestrel_engine::EngineHandle,
    account: kestrel_core::ids::AccountId,
) -> Result<Reply, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    engine
        .commands
        .send(command(
            FrontendKind::Tui,
            CommandPayload::ListFolders { account, reply: tx },
        ))
        .await
        .map_err(|e| e.to_string())?;
    Ok(rx.await.expect("reply"))
}

/// Lists one folder's messages through the protocol.
async fn list_messages(
    engine: &kestrel_engine::EngineHandle,
    folder: kestrel_core::ids::FolderId,
) -> Result<Reply, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    engine
        .commands
        .send(command(
            FrontendKind::Tui,
            CommandPayload::ListMessages {
                folder,
                window: kestrel_core::protocol::Window::default(),
                sort: kestrel_core::protocol::SortSpec::default(),
                reply: tx,
            },
        ))
        .await
        .map_err(|e| e.to_string())?;
    Ok(rx.await.expect("reply"))
}

/// Polls a server-side condition until it holds or the deadline passes.
async fn wait_for_server<F, Fut>(label: &str, timeout: Duration, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if condition().await {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "server-side condition never became true: {label}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
