//! Phase-2 exit-gate integration tests (docs/roadmap.md; issues #21/#22/#23).
//!
//! Each test proves one roadmap exit criterion against the real
//! Dovecot/GreenMail compose stack (`tests/integration/docker-compose.yml`):
//!
//! 1. `integration_outbox_survives_storage_reopen` (#21) — an envelope
//!    enqueued to the durable outbox survives dropping every storage handle
//!    and reopens with its state intact, then flushes through SMTP.
//! 2. `integration_uidvalidity_reconciliation_purges_and_resyncs` (#22) — a
//!    server-side UIDVALIDITY change is detected on a fresh sync, the cached
//!    messages for that folder are purged, and the folder re-syncs cleanly.
//! 3. `integration_condstore_flag_delta_without_refetch` (#23) — a flag
//!    change made outside the session is picked up as a CHANGEDSINCE delta
//!    (persisted + event emitted) without a message re-ingest.
//!
//! Docker-gated: `KESTREL_INTEGRATION=1 cargo nextest run --profile
//! integration --run-ignored ignored-only`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    missing_docs,
    clippy::print_stderr,
    clippy::too_many_lines
)]

use std::{sync::Arc, time::Duration};

use imap_next::imap_types::{
    command::CommandBody,
    core::Literal,
    extensions::binary::LiteralOrLiteral8,
    flag::{Flag, StoreResponse, StoreType},
    mailbox::Mailbox,
    response::Code,
};
use kestrel_core::{
    clock::SystemClock,
    config::Config,
    ids::{AccountId, SystemIdGenerator},
    paths::Paths,
    protocol::{Draft, EngineEvent, Priority},
    sasl::SaslMechanism,
    secrets::SecretString,
    store_model::{MailStore, OutboxEnvelope},
    testkit::temp_paths,
};
use kestrel_storage::{NewAccount, StorageHandle, StorageService};
use kestrel_sync::{ConnectParams, ImapSession, OutboxService, Security, SmtpParams, SyncService};
use tokio_util::sync::CancellationToken;

const IMAP_HOST: &str = "127.0.0.1";
const IMAP_PORT: u16 = 1143;
const SMTP_PORT: u16 = 1025;
const USERNAME: &str = "kestrel";
const PASSWORD: &str = "testpass";

fn fixture_ready() -> bool {
    std::env::var("KESTREL_INTEGRATION").is_ok()
}

async fn wait_fixture() {
    for _ in 0..60 {
        let imap = tokio::net::TcpStream::connect((IMAP_HOST, IMAP_PORT)).await;
        let smtp = tokio::net::TcpStream::connect((IMAP_HOST, SMTP_PORT)).await;
        if imap.is_ok() && smtp.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("fixtures not reachable (docker compose -f tests/integration/docker-compose.yml up?)");
}

fn connect_params() -> ConnectParams {
    ConnectParams {
        host: IMAP_HOST.into(),
        port: IMAP_PORT,
        security: Security::Insecure,
        username: USERNAME.into(),
        secret: SecretString::new(PASSWORD.into()),
        secret_override: None,
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
        secret_override: None,
        oauth2: false,
        security: kestrel_sync::SmtpSecurity::Insecure,
    }
}

use tokio_rustls::TlsConnector;

fn test_tls_config() -> Arc<rustls::ClientConfig> {
    // Loopback fixture: the insecure transport means this connector is
    // never actually used.
    kestrel_crypto::tls_config(None).unwrap()
}

/// Waits for a specific event within `timeout`, discarding unrelated ones.
async fn await_event(
    rx: &mut tokio::sync::mpsc::Receiver<EngineEvent>,
    timeout: Duration,
    predicate: &dyn Fn(&EngineEvent) -> bool,
) -> Option<EngineEvent> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(ev)) if predicate(&ev) => return Some(ev),
            Ok(_) | Err(_) => {}
        }
    }
    None
}

/// Drains whatever is currently queued on the event bus.
fn drain(rx: &mut tokio::sync::mpsc::Receiver<EngineEvent>) -> Vec<EngineEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

/// Polls an async check (capturing only shared state) until it holds or
/// `timeout` elapses.
async fn eventually(timeout: Duration, mut check: impl AsyncFnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if check().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Boots a storage service over a fresh temp home. The returned guard keeps
/// the directory alive; drop it last.
fn boot_storage() -> (tempfile::TempDir, StorageHandle, CancellationToken) {
    let (dir, paths) = temp_paths();
    paths.ensure().unwrap();
    let (storage, cancel) = StorageService::spawn(
        paths.clone(),
        Arc::new(SystemIdGenerator),
        Arc::new(SystemClock),
    );
    (dir, storage, cancel)
}

async fn make_account(storage: &StorageHandle, name: &str) -> AccountId {
    storage
        .upsert_account(NewAccount {
            name: name.into(),
            email: "kestrel@example.org".into(),
            provider: kestrel_core::protocol::Provider::Generic,
            protocol: kestrel_core::protocol::MailProtocol::Imap,
            auth_kind: "password".into(),
            host: String::new(),
        })
        .await
        .unwrap()
}

/// Creates an RFC 5322 draft body with the given subject.
fn build_message(subject: &str) -> Vec<u8> {
    kestrel_core::compose::build_rfc5322(
        &Draft {
            account: AccountId::from_uuid(uuid::Uuid::now_v7()),
            from: kestrel_core::protocol::Address::bare("kestrel@example.org"),
            to: vec![kestrel_core::protocol::Address::bare("dest@example.org")],
            cc: vec![],
            bcc: vec![],
            subject: subject.into(),
            in_reply_to: None,
            references: vec![],
            body_markdown: "delta test body".into(),
            attachments: vec![],
            pgp_sign: false,
            pgp_encrypt: false,
            smime_sign: false,
            smime_encrypt: false,
            send_after: None,
            priority: Priority::Normal,
        },
        &kestrel_core::ids::SystemIdGenerator,
        &kestrel_core::clock::SystemClock,
    )
    .unwrap()
}

/// APPENDs a message to a mailbox over a dedicated session.
async fn append_message(session: &mut ImapSession, mailbox: &str, raw: &[u8]) {
    let outcome = session
        .execute(
            CommandBody::Append {
                mailbox: Mailbox::try_from(mailbox.to_string()).unwrap(),
                flags: vec![],
                date: None,
                message: LiteralOrLiteral8::Literal(Literal::try_from(raw.to_vec()).unwrap()),
            },
            Duration::from_secs(30),
        )
        .await
        .expect("APPEND ok");
    assert!(outcome.is_ok(), "APPEND rejected: {:?}", outcome.status);
}

/// Runs `cmd` and asserts the tagged status is OK.
async fn exec_ok(session: &mut ImapSession, cmd: CommandBody<'static>, what: &str) {
    let outcome = session
        .execute(cmd, Duration::from_secs(30))
        .await
        .expect(what);
    assert!(outcome.is_ok(), "{what} failed: {:?}", outcome.status);
}

/// Best-effort mailbox cleanup between runs.
async fn purge_mailbox(session: &mut ImapSession, mailbox: &str) {
    let _ = session
        .execute(
            CommandBody::Delete {
                mailbox: Mailbox::try_from(mailbox.to_string()).unwrap(),
            },
            Duration::from_secs(30),
        )
        .await;
}

/// SELECTs a mailbox and extracts (UIDVALIDITY, HIGHESTMODSEQ) from the
/// untagged/ tagged response codes (same extraction as the sync engine).
async fn select_cursors(session: &mut ImapSession, mailbox: &str) -> (u32, u64) {
    fn code_of<'a>(
        status: &'a imap_next::imap_types::response::Status<'a>,
    ) -> Option<&'a Code<'a>> {
        use imap_next::imap_types::response::Status as S;
        match status {
            S::Untagged(body) | S::Tagged(imap_next::imap_types::response::Tagged { body, .. }) => {
                body.code.as_ref()
            }
            S::Bye(_) => None,
        }
    }
    let outcome = session
        .execute(
            CommandBody::Select {
                mailbox: Mailbox::try_from(mailbox.to_string()).unwrap(),
                parameters: Vec::new(),
            },
            Duration::from_secs(30),
        )
        .await
        .expect("SELECT ok");
    assert!(outcome.is_ok(), "SELECT failed: {:?}", outcome.status);
    let mut uid_validity = 0u32;
    let mut highest_modseq = 0u64;
    for status in outcome
        .untagged
        .iter()
        .chain(std::iter::once(&outcome.status))
    {
        if let Some(code) = code_of(status) {
            match code {
                Code::UidValidity(v) => uid_validity = v.get(),
                Code::HighestModSeq(v) => highest_modseq = v.get(),
                _ => {}
            }
        }
    }
    (uid_validity, highest_modseq)
}

/// All stored messages of a folder (single window; test-scale volumes).
async fn all_messages(
    storage: &StorageHandle,
    folder: kestrel_core::ids::FolderId,
) -> Vec<kestrel_core::protocol::MessageSummary> {
    storage
        .list_messages(
            folder,
            kestrel_core::protocol::Window {
                offset: 0,
                limit: 500,
            },
            kestrel_core::protocol::SortSpec::default(),
        )
        .await
        .unwrap()
        .items
}

// ---------------------------------------------------------------------------
// #21 — outbox survives restart
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "docker: tests/integration/docker-compose.yml"]
async fn integration_outbox_survives_storage_reopen() {
    if !fixture_ready() {
        eprintln!("skipping: KESTREL_INTEGRATION not set");
        return;
    }
    wait_fixture().await;

    // Boot storage, create the account, enqueue — and do NOT flush.
    let (dir, storage, cancel) = boot_storage();
    let account = make_account(&storage, "OutboxRestartIt").await;

    let subject = format!("restart-survival-{}", uuid::Uuid::now_v7().simple());
    let envelope = OutboxEnvelope {
        from: kestrel_core::protocol::Address::bare("kestrel@example.org"),
        to: vec![kestrel_core::protocol::Address::bare("dest@example.org")],
        cc: vec![],
        bcc: vec![],
        subject: subject.clone(),
    };
    let raw = build_message(&subject);
    let id = storage
        .outbox_enqueue(account, envelope, raw, None)
        .await
        .unwrap();

    // Sanity: the row is queued with a clean retry counter.
    let due = storage.outbox_due().await.unwrap();
    let row = due.iter().find(|r| r.id == id).expect("row queued");
    assert_eq!(row.retry_count, 0, "fresh row has no retries");

    // Drop EVERY storage handle (true reopen — not a clone) exactly as the
    // engine would die between enqueue and flush. The dir guard stays alive;
    // on-disk state must survive.
    cancel.cancel();
    drop(storage);
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Reopen storage over the same on-disk home.
    let paths = Paths::nested_under(dir.path());
    let (storage, cancel2) =
        StorageService::spawn(paths, Arc::new(SystemIdGenerator), Arc::new(SystemClock));

    // The envelope is still queued, unchanged by the restart.
    let due = storage.outbox_due().await.unwrap();
    let row = due
        .iter()
        .find(|r| r.id == id)
        .expect("row survived reopen");
    assert_eq!(
        row.retry_count, 0,
        "restart must not advance the retry counter"
    );
    assert_eq!(row.envelope.subject, subject, "envelope intact");

    // Flush through SMTP and verify sent + persisted exactly as the
    // flush-through test does.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
    let online = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let store: Arc<dyn MailStore> = Arc::new(storage.clone());
    let service = OutboxService::new(
        store,
        smtp_params(),
        connect_params(),
        Arc::new(SystemClock),
        event_tx,
        online,
    );
    let cancel_outbox = CancellationToken::new();
    let outbox_handle = {
        let c = cancel_outbox.clone();
        tokio::spawn(async move { service.run(c).await })
    };

    let sent = await_event(&mut event_rx, Duration::from_secs(40), &|ev| {
        matches!(ev, EngineEvent::MailSent { .. })
    })
    .await;
    assert!(sent.is_some(), "MailSent within deadline after reopen");
    cancel_outbox.cancel();
    let _ = outbox_handle.await;

    let due = storage.outbox_due().await.unwrap();
    assert!(due.iter().all(|r| r.id != id), "sent row no longer due");

    cancel2.cancel();
    drop(storage);
    drop(dir);
}

// ---------------------------------------------------------------------------
// #22 — UIDVALIDITY reconciliation
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "docker: tests/integration/docker-compose.yml"]
async fn integration_uidvalidity_reconciliation_purges_and_resyncs() {
    if !fixture_ready() {
        eprintln!("skipping: KESTREL_INTEGRATION not set");
        return;
    }
    wait_fixture().await;

    let tag = uuid::Uuid::now_v7().simple().to_string();
    let mailbox = format!("KestrelReconcile-{tag}");

    let (dir, storage, cancel_storage) = boot_storage();
    let account = make_account(&storage, "ReconcileIt").await;

    // Fresh server-side mailbox.
    let mut session = ImapSession::connect_and_authenticate(&connect_params())
        .await
        .expect("connect");
    purge_mailbox(&mut session, &mailbox).await;
    exec_ok(
        &mut session,
        CommandBody::Create {
            mailbox: Mailbox::try_from(mailbox.clone()).unwrap(),
        },
        "CREATE",
    )
    .await;

    // Baseline sync cycle.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(256);
    let store: Arc<dyn MailStore> = Arc::new(storage.clone());
    let cancel_sync = CancellationToken::new();
    let sync = SyncService::new(
        account,
        connect_params(),
        Arc::clone(&store),
        Arc::new(Config::default()),
        Arc::new(SystemClock),
        event_tx,
    );
    let handle = {
        let c = cancel_sync.clone();
        tokio::spawn(async move { sync.run(c).await })
    };

    assert!(
        await_event(&mut event_rx, Duration::from_secs(20), &|ev| {
            matches!(ev, EngineEvent::FolderTreeChanged { .. })
        })
        .await
        .is_some(),
        "FolderTreeChanged within 20s"
    );

    // Locate the synced folder row and wait for its UIDVALIDITY cursor.
    let mailbox_probe = mailbox.clone();
    assert!(
        eventually(Duration::from_secs(15), || async {
            storage
                .list_folders(account)
                .await
                .unwrap()
                .iter()
                .any(|f| f.remote_name == mailbox_probe)
        })
        .await,
        "folder {mailbox} synced"
    );
    let folder_id = storage
        .list_folders(account)
        .await
        .unwrap()
        .into_iter()
        .find(|f| f.remote_name == mailbox)
        .unwrap()
        .id;
    assert!(
        eventually(Duration::from_secs(10), || async {
            storage.get_folder(folder_id).await.unwrap().uid_validity != 0
        })
        .await,
        "UIDVALIDITY cursor recorded"
    );
    let uv_old = storage.get_folder(folder_id).await.unwrap().uid_validity;

    // Baseline content: two messages, synced into the cache.
    let mut injector = ImapSession::connect_and_authenticate(&connect_params())
        .await
        .expect("connect for injection");
    for n in 1..=2 {
        let raw = build_message(&format!("reconcile-baseline-{n}-{tag}"));
        append_message(&mut injector, &mailbox, &raw).await;
    }
    injector.logout().await;
    assert!(
        eventually(Duration::from_secs(20), || async {
            all_messages(&storage, folder_id).await.len() == 2
        })
        .await,
        "baseline messages synced"
    );

    // Stop the baseline service BEFORE mutating the mailbox: a running
    // session holds a SELECT on the folder and would race the recreation.
    // The reconciliation under test is a client-side reaction to the
    // server's SELECT codes on a FRESH connection.
    cancel_sync.cancel();
    let _ = handle.await;

    // Server-side UIDVALIDITY change: recreate the mailbox. Dovecot derives
    // UIDVALIDITY from the creation timestamp (seconds), so pause to
    // guarantee a different value.
    let mut mutator = ImapSession::connect_and_authenticate(&connect_params())
        .await
        .expect("connect for mutation");
    exec_ok(
        &mut mutator,
        CommandBody::Delete {
            mailbox: Mailbox::try_from(mailbox.clone()).unwrap(),
        },
        "DELETE",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(2_200)).await;
    exec_ok(
        &mut mutator,
        CommandBody::Create {
            mailbox: Mailbox::try_from(mailbox.clone()).unwrap(),
        },
        "CREATE",
    )
    .await;
    let (uv_new, _) = select_cursors(&mut mutator, &mailbox).await;
    mutator.logout().await;
    assert_ne!(uv_new, uv_old, "recreated mailbox has a new UIDVALIDITY");
    assert_ne!(uv_new, 0, "UIDVALIDITY reported by SELECT");

    // Fresh sync cycle (new connection — the reconciliation is a client-side
    // reaction to the server's SELECT codes, not in-session state).
    let (recon_tx, mut recon_rx) = tokio::sync::mpsc::channel(256);
    let cancel_sync2 = CancellationToken::new();
    let sync2 = SyncService::new(
        account,
        connect_params(),
        Arc::clone(&store),
        Arc::new(Config::default()),
        Arc::new(SystemClock),
        recon_tx,
    );
    let handle2 = {
        let c = cancel_sync2.clone();
        tokio::spawn(async move { sync2.run(c).await })
    };

    // Convergence: reconciliation event (purge of the 2 cached rows), the
    // cursor updated to the new UIDVALIDITY, and a clean re-sync of the
    // (still empty) mailbox with UIDs restarting at 1.
    let reconciled = await_event(
        &mut recon_rx,
        Duration::from_secs(25),
        &(move |ev: &EngineEvent| {
            matches!(
                ev,
                EngineEvent::MessagesChanged { folder, removed, .. }
                    if *folder == folder_id && *removed >= 2
            )
        }),
    )
    .await;
    assert!(
        reconciled.is_some(),
        "MessagesChanged(purge) emitted on UIDVALIDITY change"
    );
    assert!(
        eventually(Duration::from_secs(10), || async {
            storage.get_folder(folder_id).await.unwrap().uid_validity == uv_new
        })
        .await,
        "cursor updated to the new UIDVALIDITY"
    );
    let msgs = all_messages(&storage, folder_id).await;
    assert!(msgs.is_empty(), "recreated mailbox is empty after resync");

    // UIDs restart on the recreated mailbox: a new message lands at UID 1.
    let mut injector = ImapSession::connect_and_authenticate(&connect_params())
        .await
        .expect("reconnect for injection");
    append_message(
        &mut injector,
        &mailbox,
        &build_message(&format!("post-reconcile-{tag}")),
    )
    .await;
    injector.logout().await;
    assert!(
        eventually(Duration::from_secs(20), || async {
            all_messages(&storage, folder_id).await.len() == 1
        })
        .await,
        "post-reconcile message synced"
    );
    let uids: Vec<u32> = all_messages(&storage, folder_id)
        .await
        .iter()
        .map(|m| m.uid)
        .collect();
    assert_eq!(uids, vec![1], "no duplicate/stale UIDs after resync");

    cancel_sync2.cancel();
    let _ = handle2.await;
    cancel_storage.cancel();
    drop(dir);

    let mut cleaner = ImapSession::connect_and_authenticate(&connect_params())
        .await
        .expect("cleanup connect");
    purge_mailbox(&mut cleaner, &mailbox).await;
    cleaner.logout().await;
}

// ---------------------------------------------------------------------------
// #23 — CONDSTORE flag deltas
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "docker: tests/integration/docker-compose.yml"]
async fn integration_condstore_flag_delta_without_refetch() {
    if !fixture_ready() {
        eprintln!("skipping: KESTREL_INTEGRATION not set");
        return;
    }
    wait_fixture().await;

    let tag = uuid::Uuid::now_v7().simple().to_string();
    let mailbox = format!("KestrelDelta-{tag}");

    let (dir, storage, cancel_storage) = boot_storage();
    let account = make_account(&storage, "DeltaIt").await;

    // Fresh server-side mailbox with one message.
    let mut session = ImapSession::connect_and_authenticate(&connect_params())
        .await
        .expect("connect");
    purge_mailbox(&mut session, &mailbox).await;
    exec_ok(
        &mut session,
        CommandBody::Create {
            mailbox: Mailbox::try_from(mailbox.clone()).unwrap(),
        },
        "CREATE",
    )
    .await;
    append_message(
        &mut session,
        &mailbox,
        &build_message(&format!("delta-target-{tag}")),
    )
    .await;

    // Baseline sync cycle.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(256);
    let store: Arc<dyn MailStore> = Arc::new(storage.clone());
    let cancel_sync = CancellationToken::new();
    // The engine's `Command::TriggerSync` wakes the service out of IDLE so a
    // fresh cycle re-passes every folder — exactly what a frontend does when
    // the user hits refresh. The test drives the same mechanism.
    let trigger = Arc::new(tokio::sync::Notify::new());
    let sync = SyncService::new(
        account,
        connect_params(),
        Arc::clone(&store),
        Arc::new(Config::default()),
        Arc::new(SystemClock),
        event_tx,
    )
    .with_trigger(Arc::clone(&trigger));
    let handle = {
        let c = cancel_sync.clone();
        tokio::spawn(async move { sync.run(c).await })
    };

    assert!(
        await_event(&mut event_rx, Duration::from_secs(20), &|ev| {
            matches!(ev, EngineEvent::FolderTreeChanged { .. })
        })
        .await
        .is_some(),
        "FolderTreeChanged within 20s"
    );
    let mailbox_probe = mailbox.clone();
    assert!(
        eventually(Duration::from_secs(15), || async {
            storage
                .list_folders(account)
                .await
                .unwrap()
                .iter()
                .any(|f| f.remote_name == mailbox_probe)
        })
        .await,
        "folder {mailbox} synced"
    );
    let folder_id = storage
        .list_folders(account)
        .await
        .unwrap()
        .into_iter()
        .find(|f| f.remote_name == mailbox)
        .unwrap()
        .id;
    assert!(
        eventually(Duration::from_secs(20), || async {
            all_messages(&storage, folder_id).await.len() == 1
        })
        .await,
        "baseline message synced"
    );
    let msg_id = all_messages(&storage, folder_id).await[0].id;

    // The gate: with CONDSTORE enabled post-auth, SELECT must report a
    // HIGHESTMODSEQ cursor for the folder (0 would leave the delta path dead).
    assert!(
        eventually(Duration::from_secs(10), || async {
            storage.get_folder(folder_id).await.unwrap().highest_modseq > 0
        })
        .await,
        "HIGHESTMODSEQ cursor recorded (ENABLE CONDSTORE → SELECT code)"
    );
    let baseline_modseq = storage.get_folder(folder_id).await.unwrap().highest_modseq;

    // Event baseline: everything synced so far was the initial ingest.
    let mail_arrived_before = drain(&mut event_rx)
        .iter()
        .filter(|ev| matches!(ev, EngineEvent::MailArrived { .. }))
        .count();
    let _ = mail_arrived_before;

    // Out-of-band flag change on a second session (as another client would).
    let mut mutator = ImapSession::connect_and_authenticate(&connect_params())
        .await
        .expect("connect for mutation");
    exec_ok(
        &mut mutator,
        CommandBody::Select {
            mailbox: Mailbox::try_from(mailbox.clone()).unwrap(),
            parameters: Vec::new(),
        },
        "SELECT for STORE",
    )
    .await;
    exec_ok(
        &mut mutator,
        CommandBody::Store {
            sequence_set: "1".try_into().unwrap(),
            kind: StoreType::Add,
            response: StoreResponse::Silent,
            flags: vec![Flag::Flagged],
            uid: true,
            modifiers: Vec::new(),
        },
        "UID STORE +FLAGS",
    )
    .await;
    let (_, modseq_after) = select_cursors(&mut mutator, &mailbox).await;
    mutator.logout().await;
    assert!(
        modseq_after > baseline_modseq,
        "flag change bumped HIGHESTMODSEQ ({baseline_modseq} → {modseq_after})"
    );

    // Delta convergence: trigger a re-pass (the IDLE session only receives
    // pushes for the mailbox it is selected on — the correct engine-level
    // trigger for "refresh everything" is TriggerSync). CHANGEDSINCE then
    // delivers the flag delta, which must be persisted and emitted — with NO
    // message re-ingest (a full refetch would also converge flags, but the
    // delta path is the contract under test).
    trigger.notify_one();
    let flag_msg_id = msg_id;
    let converged = await_event(
        &mut event_rx,
        Duration::from_secs(25),
        &(move |ev: &EngineEvent| {
            matches!(
                ev,
                EngineEvent::FlagsChanged { messages }
                    if messages.contains(&flag_msg_id)
            )
        }),
    )
    .await;
    assert!(
        converged.is_some(),
        "FlagsChanged delta emitted for the changed message"
    );
    assert!(
        eventually(Duration::from_secs(5), || async {
            all_messages(&storage, folder_id)
                .await
                .iter()
                .any(|m| m.id == msg_id && m.is_flagged)
        })
        .await,
        "\\Flagged persisted into the cache"
    );
    assert!(
        eventually(Duration::from_secs(5), || async {
            storage.get_folder(folder_id).await.unwrap().highest_modseq >= modseq_after
        })
        .await,
        "HIGHESTMODSEQ cursor advanced past the delta"
    );
    let refetched = drain(&mut event_rx)
        .iter()
        .any(|ev| matches!(ev, EngineEvent::MailArrived { .. }));
    assert!(
        !refetched,
        "flag delta must not re-ingest messages (no full refetch)"
    );

    cancel_sync.cancel();
    let _ = handle.await;
    cancel_storage.cancel();
    drop(dir);

    let mut cleaner = ImapSession::connect_and_authenticate(&connect_params())
        .await
        .expect("cleanup connect");
    purge_mailbox(&mut cleaner, &mailbox).await;
    cleaner.logout().await;
}
