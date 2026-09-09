//! Phase-2 exit gate for issue #24: non-CONDSTORE flag-pass fallback.
//!
//! Dovecot (our other fixture) advertises CONDSTORE, so the CHANGEDSINCE
//! delta branch always wins there — the fallback branch in `flag_pass`
//! would never execute against a real fixture. This test drives a real
//! `SyncService` against a minimal mock IMAP server that:
//!
//! - never advertises CONDSTORE (forcing the windowed scan fallback),
//! - answers the ingest `UID FETCH (ALL)` and the scan
//!   `UID FETCH (FLAGS)` uniformly (both passes read FLAGS from it),
//! - flips flags between cycles via shared state (simulating an
//!   out-of-band, server-side flag change — no client STORE involved).
//!
//! Convergence proof: cycle 1 ingests 2 messages, `TriggerSync` starts
//! cycle 2 with the flags flipped server-side, and the fallback scan must
//! persist + emit `FlagsChanged` without any re-ingest (row count and
//! UIDNEXT unchanged).
//!
//! Docker-free: runs against an in-process `TcpListener`, but still gated
//! on `KESTREL_INTEGRATION=1` to sit beside the other exit gates.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    missing_docs,
    clippy::print_stderr,
    clippy::too_many_lines
)]

use std::{
    collections::HashMap,
    fmt::Write as _,
    sync::{Arc, Mutex},
    time::Duration,
};

use kestrel_core::{
    clock::SystemClock,
    config::Config,
    ids::{AccountId, SystemIdGenerator},
    protocol::{EngineEvent, Flag, SortDir, SortField, Window},
    sasl::SaslMechanism,
    secrets::SecretString,
    store_model::MailStore,
    testkit::temp_paths,
};
use kestrel_storage::{NewAccount, StorageService};
use kestrel_sync::{ConnectParams, Security, SyncService};
use tokio::{io::AsyncWriteExt, net::TcpListener};
use tokio_util::sync::CancellationToken;

// ---- mock IMAP server ------------------------------------------------------

/// One message the mock server exposes.
#[derive(Clone)]
struct MockMessage {
    uid: u32,
    flags: Vec<&'static str>,
    subject: String,
}

/// Shared per-test scenario state the test mutates between cycles.
#[derive(Clone)]
struct Scenario {
    messages: Arc<Mutex<Vec<MockMessage>>>,
    uid_validity: u32,
}

impl Scenario {
    fn new(uid_validity: u32, messages: Vec<MockMessage>) -> Self {
        Self {
            messages: Arc::new(Mutex::new(messages)),
            uid_validity,
        }
    }

    fn set_flags(&self, uid: u32, flags: &[&'static str]) {
        let mut msgs = self.messages.lock().unwrap();
        for m in msgs.iter_mut() {
            if m.uid == uid {
                m.flags = flags.to_vec();
            }
        }
    }
}

/// Serves the exact command vocabulary `SyncService` speaks: greeting,
/// CAPABILITY, AUTHENTICATE PLAIN, LIST, SELECT, UID FETCH, IDLE, LOGOUT.
/// No CONDSTORE is ever advertised — that is the point.
async fn serve_mock(listener: TcpListener, scenario: Scenario) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let scenario = scenario.clone();
        tokio::spawn(async move {
            let _ = sock
                .write_all(b"* OK [CAPABILITY IMAP4rev1 IDLE] mock ready\r\n")
                .await;
            let mut buf = vec![0u8; 8192];
            loop {
                let n = tokio::io::AsyncReadExt::read(&mut sock, &mut buf)
                    .await
                    .unwrap_or(0);
                if n == 0 {
                    return; // client closed
                }
                let line = String::from_utf8_lossy(&buf[..n]).into_owned();
                let line = line.trim_end();
                let Some((tag, rest)) = line.split_once(' ') else {
                    continue;
                };
                let upper = rest.to_ascii_uppercase();
                let reply: Vec<u8> = if upper.starts_with("CAPABILITY") {
                    format!("* CAPABILITY IMAP4rev1 IDLE\r\n{tag} OK CAPABILITY completed\r\n")
                        .into_bytes()
                } else if upper.starts_with("AUTHENTICATE") || upper.starts_with("LOGIN") {
                    format!("{tag} OK [CAPABILITY IMAP4rev1 IDLE] authenticated\r\n").into_bytes()
                } else if upper.starts_with("LIST") {
                    format!(
                        "* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\n{tag} OK LIST completed\r\n"
                    )
                    .into_bytes()
                } else if upper.starts_with("SELECT") {
                    let msgs = scenario.messages.lock().unwrap();
                    let exists = msgs.len();
                    let next_uid = msgs.iter().map(|m| m.uid).max().unwrap_or(0) + 1;
                    format!(
                        "* {exists} EXISTS\r\n\
                         * 0 RECENT\r\n\
                         * OK [UIDVALIDITY {}] UIDs valid\r\n\
                         * OK [UIDNEXT {next_uid}] Predicted next UID\r\n\
                         * FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n\
                         {tag} OK [READ-WRITE] SELECT completed\r\n",
                        scenario.uid_validity
                    )
                    .into_bytes()
                } else if upper.starts_with("UID FETCH") {
                    // Uniform answer: UID + FLAGS + INTERNALDATE + RFC822.SIZE
                    // + ENVELOPE. The ingest pass uses the envelope; the scan
                    // pass reads the flags; both tolerate the extra items.
                    // The range is the 3rd whitespace token; items (which may
                    // contain spaces) are ignored.
                    let range = rest.split_whitespace().nth(2).unwrap_or("1:*");
                    let (lo, hi) = parse_range(range);
                    let msgs = scenario.messages.lock().unwrap();
                    let mut out = String::new();
                    for (seq, m) in msgs
                        .iter()
                        .enumerate()
                        .filter(|(_, m)| m.uid >= lo && m.uid <= hi)
                    {
                        let flags = m.flags.join(" ");
                        let subject = m.subject.replace('\\', "\\\\").replace('"', "\\\"");
                        let _ = write!(
                            out,
                            "* {} FETCH (UID {} FLAGS ({flags}) INTERNALDATE \"01-Jan-2026 00:00:00 +0000\" RFC822.SIZE 512 ENVELOPE (NIL \"{subject}\" ((NIL NIL \"kestrel\" \"example.org\")) NIL NIL ((NIL NIL \"dest\" \"example.org\")) NIL NIL NIL \"<m{}@mock>\"))\r\n",
                            seq + 1,
                            m.uid,
                            m.uid
                        );
                    }
                    let _ = write!(out, "{tag} OK FETCH completed\r\n");
                    out.into_bytes()
                } else if upper.starts_with("IDLE") {
                    b"+ idling\r\n".to_vec()
                } else if upper.starts_with("DONE") || line == "DONE" {
                    b"A9 OK IDLE terminated\r\n".to_vec()
                } else if upper.starts_with("LOGOUT") {
                    let _ = sock.write_all(b"* BYE mock logging out\r\n").await;
                    let _ = sock
                        .write_all(format!("{tag} OK LOGOUT completed\r\n").as_bytes())
                        .await;
                    let _ = sock.flush().await;
                    return;
                } else {
                    // Unknown command: generic completion so the state
                    // machine never wedges.
                    format!("{tag} OK completed\r\n").into_bytes()
                };
                if sock.write_all(&reply).await.is_err() {
                    return;
                }
                let _ = sock.flush().await;
            }
        });
    }
}

/// Parses `1:2` / `1:*` / `3` into inclusive (lo, hi) bounds.
fn parse_range(range: &str) -> (u32, u32) {
    let (a, b) = range.split_once(':').unwrap_or((range, range));
    let lo = a.trim().parse().unwrap_or(1);
    let hi = if b.trim() == "*" {
        u32::MAX
    } else {
        b.trim().parse().unwrap_or(lo)
    };
    (lo, hi)
}

// ---- helpers (mirroring exit_gates.rs) -------------------------------------

async fn wait_mock(addr: &str) {
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("mock server not reachable");
}

fn connect_params(port: u16) -> ConnectParams {
    ConnectParams {
        host: "127.0.0.1".into(),
        port,
        security: Security::Insecure,
        username: "kestrel".into(),
        secret: SecretString::new("testpass".into()),
        mechanisms: vec![SaslMechanism::Plain],
        tls: tokio_rustls::TlsConnector::from(kestrel_crypto::tls_config(None).unwrap()),
        sasl_factory: Arc::new(|mech, user, secret| {
            kestrel_crypto::sasl::start(mech, user, secret)
        }),
    }
}

/// Waits for a specific event within `timeout`.
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

// ---- the exit gate ---------------------------------------------------------

#[tokio::test]
#[ignore = "KESTREL_INTEGRATION=1 (exit gate; docker-free but sits with the integration profile)"]
async fn integration_flag_scan_converges_without_condstore() {
    if std::env::var("KESTREL_INTEGRATION").is_err() {
        return;
    }

    // Storage over a fresh temp home.
    let (dir, paths) = temp_paths();
    paths.ensure().unwrap();
    let (storage, cancel_storage) = StorageService::spawn(
        paths.clone(),
        Arc::new(SystemIdGenerator),
        Arc::new(SystemClock),
    );
    let account: AccountId = storage
        .upsert_account(NewAccount {
            name: "mock".into(),
            email: "kestrel@example.org".into(),
            provider: kestrel_core::protocol::Provider::Generic,
            protocol: kestrel_core::protocol::MailProtocol::Imap,
            auth_kind: "password".into(),
            host: String::new(),
        })
        .await
        .unwrap();

    // Mock server (never CONDSTORE).
    let scenario = Scenario::new(
        4242,
        vec![
            MockMessage {
                uid: 1,
                flags: vec!["\\Seen"],
                subject: "alpha".into(),
            },
            MockMessage {
                uid: 2,
                flags: vec![],
                subject: "beta".into(),
            },
        ],
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let addr = listener.local_addr().unwrap().to_string();
    let server_scenario = scenario.clone();
    tokio::spawn(serve_mock(listener, server_scenario));
    wait_mock(&addr).await;

    // Event tap: the sync service publishes straight onto the channel.
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);

    // Sync service: slow poll (we drive cycles via TriggerSync), short IDLE.
    let mut config = Config::default();
    config.sync.poll_interval_secs = 3600;
    config.sync.idle_timeout_mins = 1;
    let store: Arc<dyn MailStore> = Arc::new(storage.clone());
    let trigger = Arc::new(tokio::sync::Notify::new());
    let sync = SyncService::new(
        account,
        connect_params(port),
        Arc::clone(&store),
        Arc::new(config),
        Arc::new(SystemClock),
        tx,
    )
    .with_trigger(trigger.clone());
    let cancel_sync = CancellationToken::new();
    tokio::spawn({
        let c = cancel_sync.clone();
        async move { sync.run(c).await }
    });

    // Cycle 1: ingest of the two mock messages.
    let folder_id = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(EngineEvent::MailArrived { folder, .. }) =
                await_event(&mut rx, Duration::from_secs(5), &|ev| {
                    matches!(ev, EngineEvent::MailArrived { .. })
                })
                .await
            {
                return folder;
            }
        }
    })
    .await
    .expect("cycle 1 never ingested");
    // Cycle 1 reaches Idle only after every folder's passes complete —
    // waiting for it prevents the flip below from racing cycle 1's scan.
    await_event(&mut rx, Duration::from_secs(20), &|ev| {
        matches!(
            ev,
            EngineEvent::AccountConnection {
                state: kestrel_core::protocol::ConnectionState::Idle,
                ..
            }
        )
    })
    .await
    .expect("cycle 1 never reached Idle");

    // Out-of-band server-side flag change (no client STORE involved).
    scenario.set_flags(1, &["\\Seen", "\\Flagged"]);
    scenario.set_flags(2, &["\\Answered"]);

    // Cycle 2: fresh connect → select → scan sees flipped flags.
    trigger.notify_one();
    let flags_changed = await_event(&mut rx, Duration::from_secs(30), &|ev| {
        matches!(ev, EngineEvent::FlagsChanged { .. })
    })
    .await
    .expect("fallback scan never reported the out-of-band change");
    let EngineEvent::FlagsChanged { messages } = flags_changed else {
        unreachable!()
    };
    assert_eq!(messages.len(), 2, "both messages' flags changed");

    // Deltas persisted, and no re-ingest happened (still exactly 2 rows).
    let page = storage
        .list_messages(
            folder_id,
            Window {
                offset: 0,
                limit: 100,
            },
            sort_spec(),
        )
        .await
        .unwrap();
    assert_eq!(page.total, 2, "scan must not re-ingest messages");
    let by_uid: HashMap<u32, &Vec<Flag>> = page.items.iter().map(|m| (m.uid, &m.flags)).collect();
    assert!(
        by_uid[&1].contains(&Flag::Flagged),
        "uid 1 must carry \\Flagged, got {:?}",
        by_uid[&1]
    );
    assert!(
        by_uid[&2].contains(&Flag::Answered),
        "uid 2 must carry \\Answered, got {:?}",
        by_uid[&2]
    );
    assert!(!by_uid[&2].contains(&Flag::Seen));

    cancel_sync.cancel();
    cancel_storage.cancel();
    drop(dir);
}

/// UID-ascending sort used by the listing assertions.
fn sort_spec() -> kestrel_core::protocol::SortSpec {
    kestrel_core::protocol::SortSpec {
        field: SortField::Uid,
        dir: SortDir::Asc,
    }
}
