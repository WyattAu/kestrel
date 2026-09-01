//! Storage service round-trip tests (Phase 1 exit evidence):
//! account→folder→ingest→list→get→flags→delete, threading, outbox,
//! blob GC two-phase, and index/search.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::sync::Arc;

use kestrel_core::{
    clock::{Clock as _, FakeClock},
    ids::MessageId,
    mime::{MimeParser, StalwartParser},
    protocol::{
        Address, BodyPreference, ConnectionState, Flag, FlagOp, FolderRole, MailProtocol, Provider,
        SearchQuery, SortField, SortSpec, Window,
    },
    testkit::{SequentialIds, temp_paths},
};
use kestrel_storage::{
    IndexService, IngestBatch, IngestMessage, NewAccount, NewFolder, OutboxEnvelope, SearchService,
    StorageService,
};

async fn setup(
    name: &str,
) -> (
    kestrel_storage::StorageHandle,
    kestrel_storage::IndexHandle,
    kestrel_storage::SearchHandle,
    Arc<FakeClock>,
    tempfile::TempDir,
) {
    let (dir, paths) = temp_paths();
    paths.ensure().unwrap();
    let clock = Arc::new(FakeClock::new(1_700_000_000_000));
    let ids = Arc::new(SequentialIds::new());
    let (storage, _cancel) = StorageService::spawn(paths.clone(), ids.clone(), clock.clone());
    // Wait for open by performing a no-op query.
    storage.list_accounts().await.expect("service opens");
    let index = IndexService::spawn(&paths.index_dir(), storage.clone(), clock.clone())
        .expect("index spawns");
    let search = SearchService::from_index(&index, storage.clone());
    let _ = name;
    (storage, index, search, clock, dir)
}

fn parsed(subject: &str, body: &str, message_id: &str) -> kestrel_core::mime::ParsedMessage {
    let raw = format!(
        "From: Sender <s@example.org>\r\nTo: r@example.net\r\nSubject: {subject}\r\nMessage-ID: <{message_id}>\r\nDate: Fri, 28 Aug 2026 10:00:00 +0000\r\nContent-Type: text/plain\r\n\r\n{body}"
    );
    StalwartParser::parse(raw.as_bytes()).unwrap()
}

#[tokio::test]
async fn account_folder_message_roundtrip() {
    let (storage, _index, _search, _clock, _dir) = setup("roundtrip").await;
    let account = storage
        .upsert_account(NewAccount {
            name: "Test".into(),
            email: "test@example.org".into(),
            provider: Provider::Generic,
            protocol: MailProtocol::Imap,
            auth_kind: "password".into(),
            host: String::new(),
        })
        .await
        .unwrap();
    // Idempotent by email.
    let again = storage
        .upsert_account(NewAccount {
            name: "Test".into(),
            email: "test@example.org".into(),
            provider: Provider::Generic,
            protocol: MailProtocol::Imap,
            auth_kind: "password".into(),
            host: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(account, again);

    let folder = storage
        .upsert_folder(NewFolder {
            account,
            remote_name: "INBOX".into(),
            attributes: vec!["\\HasNoChildren".into()],
            role: Some(FolderRole::Inbox),
            delimiter: "/".into(),
            uid_validity: 42,
            highest_modseq: 0,
        })
        .await
        .unwrap();

    let raw = b"From: s@example.org\r\nSubject: hi\r\n\r\nbody".to_vec();
    let blob = storage.write_blob(raw.clone()).await.unwrap();
    storage
        .ingest_batch(IngestBatch {
            messages: vec![IngestMessage {
                folder,
                uid: 1,
                internal_date: 1_700_000_100_000,
                flags: vec![Flag::Seen],
                parsed: parsed("hi", "body", "m1@x"),
                raw_blob: Some(blob),
                raw_size: raw.len() as u64,
            }],
        })
        .await
        .unwrap();

    let page = storage
        .list_messages(
            folder,
            Window::default(),
            SortSpec {
                field: SortField::Date,
                dir: kestrel_core::protocol::SortDir::Desc,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].subject.as_deref(), Some("hi"));
    assert!(page.items[0].is_read);

    let folders = storage.list_folders(account).await.unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].unread, 0);
    assert_eq!(folders[0].total, 1);

    let id = page.items[0].id;
    let load = storage.get_message(id).await.unwrap();
    assert_eq!(load.view.body_plain.as_deref(), Some("body"));
    assert!(load.raw.is_some());

    // Unread flag flips folder counts.
    storage
        .set_flags(vec![id], FlagOp::Remove(vec![Flag::Seen]))
        .await
        .unwrap();
    let folders = storage.list_folders(account).await.unwrap();
    assert_eq!(folders[0].unread, 1);

    storage.delete_messages(vec![id]).await.unwrap();
    let page = storage
        .list_messages(folder, Window::default(), SortSpec::default())
        .await
        .unwrap();
    assert_eq!(page.total, 0);
}

#[tokio::test]
async fn threading_groups_replies() {
    let (storage, _i, _s, _c, _d) = setup("threading").await;
    let account = storage
        .upsert_account(NewAccount {
            name: "T".into(),
            email: "t@x.example".into(),
            provider: Provider::Generic,
            protocol: MailProtocol::Imap,
            auth_kind: "password".into(),
            host: String::new(),
        })
        .await
        .unwrap();
    let folder = storage
        .upsert_folder(NewFolder {
            account,
            remote_name: "INBOX".into(),
            attributes: vec![],
            role: Some(FolderRole::Inbox),
            delimiter: "/".into(),
            uid_validity: 1,
            highest_modseq: 0,
        })
        .await
        .unwrap();

    let msgs: Vec<(u32, &str, &str, Option<&str>)> = vec![
        (1, "root", "root body", None),
        (2, "Re: root", "reply body", Some("root@x")),
        (3, "Re: root", "reply 2", Some("root@x")),
    ];
    let _ = &msgs;
    let batch: Vec<IngestMessage> = msgs
        .iter()
        .map(|(uid, subject, body, irt)| {
            let mid = if *uid == 1 {
                "root@x".to_string()
            } else {
                format!("{uid}@x")
            };
            let mut p = parsed(subject, body, &mid);
            p.in_reply_to = irt.map(str::to_owned);
            IngestMessage {
                folder,
                uid: *uid,
                internal_date: 1_700_000_000_000 + i64::from(*uid) * 1000,
                flags: vec![],
                parsed: p,
                raw_blob: None,
                raw_size: 100,
            }
        })
        .collect();
    storage
        .ingest_batch(IngestBatch { messages: batch })
        .await
        .unwrap();

    let page = storage
        .list_messages(folder, Window::default(), SortSpec::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 3);
    let keys: Vec<&str> = page.items.iter().map(|m| m.thread.key.as_str()).collect();
    assert_eq!(keys[0], keys[1]);
    assert_eq!(keys[1], keys[2], "all replies share one thread: {keys:?}");
}

#[tokio::test]
async fn outbox_lifecycle_and_blob_refs() {
    let (storage, _i, _s, clock, _d) = setup("outbox").await;
    let account = storage
        .upsert_account(NewAccount {
            name: "O".into(),
            email: "o@x.example".into(),
            provider: Provider::Generic,
            protocol: MailProtocol::Imap,
            auth_kind: "password".into(),
            host: String::new(),
        })
        .await
        .unwrap();
    let envelope = OutboxEnvelope {
        from: Address::bare("me@x.example"),
        to: vec![Address::bare("you@y.example")],
        cc: vec![],
        bcc: vec![],
        subject: "draft".into(),
    };
    let id = storage
        .outbox_enqueue(account, envelope, b"raw draft bytes".to_vec(), None)
        .await
        .unwrap();
    let due = storage.outbox_due().await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, id);

    // Retry then send.
    storage
        .outbox_mark_retry(
            id,
            1,
            clock.now_unix_ms() + 60_000,
            "429 go away".to_owned(),
        )
        .await
        .unwrap();
    assert!(
        storage.outbox_due().await.unwrap().is_empty(),
        "backoff defers"
    );
    clock.advance(61_000);
    assert_eq!(
        storage.outbox_due().await.unwrap().len(),
        1,
        "due after backoff"
    );
    storage
        .outbox_mark_sent(id, clock.now_unix_ms())
        .await
        .unwrap();
    assert!(
        storage.outbox_due().await.unwrap().is_empty(),
        "sent is not due"
    );
}

#[tokio::test]
async fn blob_gc_two_phase_with_grace() {
    let (storage, _i, _s, clock, _d) = setup("gc").await;
    let hash = storage.write_blob(b"gc-me".to_vec()).await.unwrap();
    // No references: mark should stamp it.
    let marked = storage.gc_mark(clock.now_unix_ms()).await.unwrap();
    assert!(marked >= 1);
    // Before grace: nothing swept.
    let swept = storage
        .gc_sweep(clock.now_unix_ms(), 24 * 3600 * 1000)
        .await
        .unwrap();
    assert_eq!(swept, 0);
    // After grace: swept (file + row).
    clock.advance(25 * 3600 * 1000);
    let swept = storage
        .gc_sweep(clock.now_unix_ms(), 24 * 3600 * 1000)
        .await
        .unwrap();
    assert!(swept >= 1);
    assert!(
        storage.read_blob(hash).await.is_err(),
        "blob file must be gone"
    );
}

#[tokio::test]
async fn referenced_blob_survives_gc() {
    let (storage, _i, _s, clock, _d) = setup("gc-refed").await;
    let account = storage
        .upsert_account(NewAccount {
            name: "G".into(),
            email: "g@x.example".into(),
            provider: Provider::Generic,
            protocol: MailProtocol::Imap,
            auth_kind: "password".into(),
            host: String::new(),
        })
        .await
        .unwrap();
    let folder = storage
        .upsert_folder(NewFolder {
            account,
            remote_name: "INBOX".into(),
            attributes: vec![],
            role: None,
            delimiter: "/".into(),
            uid_validity: 1,
            highest_modseq: 0,
        })
        .await
        .unwrap();
    let raw = b"From: g@x\r\nSubject: keep\r\n\r\nbody".to_vec();
    let blob = storage.write_blob(raw).await.unwrap();
    storage
        .ingest_batch(IngestBatch {
            messages: vec![IngestMessage {
                folder,
                uid: 7,
                internal_date: 1,
                flags: vec![],
                parsed: parsed("keep", "body", "keep@x"),
                raw_blob: Some(blob),
                raw_size: 30,
            }],
        })
        .await
        .unwrap();
    let _ = storage.gc_mark(clock.now_unix_ms()).await.unwrap();
    clock.advance(30 * 3600 * 1000);
    let swept = storage
        .gc_sweep(clock.now_unix_ms(), 24 * 3600 * 1000)
        .await
        .unwrap();
    assert_eq!(swept, 0, "referenced blobs are never swept");
}

#[tokio::test]
async fn cross_db_folder_fk_is_enforced() {
    let (storage, _i, _s, _c, _d) = setup("fk").await;
    let ghost =
        kestrel_core::ids::AccountId::parse("00000000-0000-7000-8000-000000000000").unwrap();
    let err = storage
        .upsert_folder(NewFolder {
            account: ghost,
            remote_name: "INBOX".into(),
            attributes: vec![],
            role: None,
            delimiter: "/".into(),
            uid_validity: 1,
            highest_modseq: 0,
        })
        .await
        .unwrap_err();
    assert_eq!(
        err.kind(),
        "engine.bug",
        "cross-DB FK violation is Bug-class"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines, clippy::cast_possible_wrap)]
async fn index_and_search_roundtrip() {
    let (storage, index, search, _c, _d) = setup("search").await;
    let account = storage
        .upsert_account(NewAccount {
            name: "S".into(),
            email: "s@x.example".into(),
            provider: Provider::Generic,
            protocol: MailProtocol::Imap,
            auth_kind: "password".into(),
            host: String::new(),
        })
        .await
        .unwrap();
    let folder = storage
        .upsert_folder(NewFolder {
            account,
            remote_name: "INBOX".into(),
            attributes: vec![],
            role: Some(FolderRole::Inbox),
            delimiter: "/".into(),
            uid_validity: 1,
            highest_modseq: 0,
        })
        .await
        .unwrap();

    let subjects = [
        (
            "quarterly budget",
            "numbers for the quarterly budget review",
        ),
        ("lunch plans", "where shall we eat lunch"),
        ("budget overrun", "the budget for travel overran"),
    ];
    for (i, (subject, body)) in subjects.iter().enumerate() {
        let raw = format!(
            "From: s@example.org\r\nSubject: {subject}\r\nMessage-ID: <q{i}@x>\r\n\r\n{body}"
        );
        let blob = storage.write_blob(raw.clone().into_bytes()).await.unwrap();
        storage
            .ingest_batch(IngestBatch {
                messages: vec![IngestMessage {
                    folder,
                    uid: u32::try_from(i).unwrap() + 1,
                    internal_date: 1_700_000_000_000 + i as i64 * 10_000,
                    flags: vec![],
                    parsed: parsed(subject, body, &format!("q{i}@x")),
                    raw_blob: Some(blob),
                    raw_size: raw.len() as u64,
                }],
            })
            .await
            .unwrap();
    }

    // Catch-up path: pending docs → index → mark indexed.
    let pending = storage.pending_index(50).await.unwrap();
    assert_eq!(pending.len(), 3);
    let docs: Vec<_> = pending
        .iter()
        .map(kestrel_storage::IndexDoc::from_pending)
        .collect();
    index.add(docs).await.unwrap();

    let hits = search
        .search(&SearchQuery {
            text: Some("budget".into()),
            ..SearchQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(hits.len(), 2, "two budget hits");
    let hit_subjects: Vec<&str> = hits
        .iter()
        .map(|h| h.message.subject.as_deref().unwrap_or_default())
        .collect();
    assert!(hit_subjects.contains(&"quarterly budget"));
    assert!(hit_subjects.contains(&"budget overrun"));

    // Exact sender filter.
    let hits = search
        .search(&SearchQuery {
            from: Some("s@example.org".into()),
            ..SearchQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(hits.len(), 3);

    // Date range excluding everything.
    let hits = search
        .search(&SearchQuery {
            text: Some("budget".into()),
            since: Some(1_800_000_000_000),
            ..SearchQuery::default()
        })
        .await
        .unwrap();
    assert!(hits.is_empty(), "future range excludes all");

    // Unknown-folder facet ⇒ no results, not a panic.
    let hits = search
        .search(&SearchQuery {
            folder: Some(
                kestrel_core::ids::FolderId::parse("00000000-0000-7000-8000-000000000001").unwrap(),
            ),
            ..SearchQuery::default()
        })
        .await
        .unwrap();
    assert!(hits.is_empty());

    // Rebuild keeps everything queryable.
    let rebuilt = index.rebuild().await.unwrap();
    assert_eq!(rebuilt, 3, "rebuild reindexes all messages");
    let hits = search
        .search(&SearchQuery {
            text: Some("lunch".into()),
            ..SearchQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn message_view_sanitizes_and_flags_links() {
    let (storage, _i, _s, _c, _d) = setup("view").await;
    let account = storage
        .upsert_account(NewAccount {
            name: "V".into(),
            email: "v@x.example".into(),
            provider: Provider::Generic,
            protocol: MailProtocol::Imap,
            auth_kind: "password".into(),
            host: String::new(),
        })
        .await
        .unwrap();
    let folder = storage
        .upsert_folder(NewFolder {
            account,
            remote_name: "INBOX".into(),
            attributes: vec![],
            role: None,
            delimiter: "/".into(),
            uid_validity: 1,
            highest_modseq: 0,
        })
        .await
        .unwrap();
    let html = "<html><body><p>Click <a href=\"https://xn--80ak6aa92e.com/login\">bank</a></p><img src=\"https://tracker.example/p.png\"></body></html>";
    let raw = format!(
        "From: v@x\r\nSubject: phish\r\nContent-Type: multipart/alternative; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nplain\r\n--b\r\nContent-Type: text/html\r\n\r\n{html}\r\n--b--\r\n"
    );
    let blob = storage.write_blob(raw.clone().into_bytes()).await.unwrap();
    storage
        .ingest_batch(IngestBatch {
            messages: vec![IngestMessage {
                folder,
                uid: 1,
                internal_date: 1,
                flags: vec![],
                parsed: StalwartParser::parse(raw.as_bytes()).unwrap(),
                raw_blob: Some(blob),
                raw_size: raw.len() as u64,
            }],
        })
        .await
        .unwrap();
    let page = storage
        .list_messages(folder, Window::default(), SortSpec::default())
        .await
        .unwrap();
    let load = storage.get_message(page.items[0].id).await.unwrap();
    let view_html = load.view.body_html.as_deref().unwrap_or_default();
    assert!(!view_html.contains("tracker.example"), "{view_html}");
    assert!(load.view.remote_blocked >= 1);
    assert!(
        load.view
            .suspicious_links
            .iter()
            .any(|l| l.href.contains("xn--")),
        "punycode link must be flagged: {:?}",
        load.view.suspicious_links
    );
    // Part content shape: 2 leaf parts.
    assert_eq!(load.view.parts.len(), 2);
    let _ = BodyPreference::Full;
    let _ = ConnectionState::Disconnected;
}

#[tokio::test]
async fn snooze_schedule_and_due_detection() {
    let (storage, _index, _search, clock, _dir) = setup("snooze").await;
    let account = storage
        .upsert_account(NewAccount {
            name: "SnoozeTest".into(),
            email: "snooze@example.org".into(),
            provider: Provider::Generic,
            protocol: MailProtocol::Imap,
            auth_kind: "password".into(),
            host: String::new(),
        })
        .await
        .unwrap();

    let folder = storage
        .upsert_folder(NewFolder {
            account,
            remote_name: "INBOX".into(),
            attributes: vec![],
            role: Some(FolderRole::Inbox),
            delimiter: "/".into(),
            uid_validity: 1,
            highest_modseq: 0,
        })
        .await
        .unwrap();

    let message = MessageId::from_uuid(uuid::Uuid::now_v7());

    // Snooze for 5 minutes from now.
    let now = clock.now_unix_ms();
    let until = now + 5 * 60 * 1000;
    storage
        .enqueue_snooze(message, account, folder, until)
        .await
        .unwrap();

    // Not due yet — clock hasn't advanced.
    let due = storage.get_due_snoozes().await.unwrap();
    assert!(due.is_empty(), "snooze should not be due yet");

    // Advance clock past the snooze time.
    clock.advance(6 * 60 * 1000);

    // Now it should be due.
    let due = storage.get_due_snoozes().await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].message_id, message);
    assert_eq!(due[0].account_id, account);
    assert_eq!(due[0].folder_id, folder);

    // Remove the snooze.
    storage.remove_snooze(message).await.unwrap();

    // No longer due.
    let due = storage.get_due_snoozes().await.unwrap();
    assert!(due.is_empty(), "snooze should be removed");
}

#[tokio::test]
async fn snooze_removal_by_message() {
    let (storage, _index, _search, clock, _dir) = setup("snooze_remove").await;
    let account = storage
        .upsert_account(NewAccount {
            name: "SnoozeRemoveTest".into(),
            email: "snooze_rm@example.org".into(),
            provider: Provider::Generic,
            protocol: MailProtocol::Imap,
            auth_kind: "password".into(),
            host: String::new(),
        })
        .await
        .unwrap();

    let folder = storage
        .upsert_folder(NewFolder {
            account,
            remote_name: "INBOX".into(),
            attributes: vec![],
            role: Some(FolderRole::Inbox),
            delimiter: "/".into(),
            uid_validity: 1,
            highest_modseq: 0,
        })
        .await
        .unwrap();

    let msg1 = MessageId::from_uuid(uuid::Uuid::now_v7());
    let msg2 = MessageId::from_uuid(uuid::Uuid::now_v7());
    let until = clock.now_unix_ms() + 3_600_000;

    storage
        .enqueue_snooze(msg1, account, folder, until)
        .await
        .unwrap();
    storage
        .enqueue_snooze(msg2, account, folder, until)
        .await
        .unwrap();

    // Remove only msg1.
    storage.remove_snooze(msg1).await.unwrap();

    let due = storage.get_due_snoozes().await.unwrap();
    assert_eq!(due.len(), 0, "neither snooze is due yet");

    // Advance clock so both would be due.
    clock.advance(3_601_000);
    let due = storage.get_due_snoozes().await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].message_id, msg2);
}

#[tokio::test]
async fn outbox_send_after_defers_flush() {
    let (storage, _index, _search, clock, _dir) = setup("send_after").await;
    let account = storage
        .upsert_account(NewAccount {
            name: "SendAfterTest".into(),
            email: "sa@example.org".into(),
            provider: Provider::Generic,
            protocol: MailProtocol::Imap,
            auth_kind: "password".into(),
            host: String::new(),
        })
        .await
        .unwrap();

    let envelope = OutboxEnvelope {
        from: Address::bare("sa@example.org"),
        to: vec![Address::bare("dest@example.org")],
        cc: vec![],
        bcc: vec![],
        subject: "scheduled".into(),
    };

    let future = clock.now_unix_ms() + 3_600_000;
    let _id = storage
        .outbox_enqueue(account, envelope, b"scheduled draft".to_vec(), Some(future))
        .await
        .unwrap();

    // Not due yet — send_after is in the future.
    let due = storage.outbox_due().await.unwrap();
    assert!(due.is_empty(), "send_after in the future should not be due");

    // Advance past send_after.
    clock.advance(3_601_000);
    let due = storage.outbox_due().await.unwrap();
    assert_eq!(due.len(), 1, "send_after in the past should be due");
}
