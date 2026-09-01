//! SLA benchmark: large-mailbox operations at 10k messages.
//!
//! Measures folder listing, paginated message list, and search latency
//! against a realistic 10k-message mailbox. Target SLAs:
//! - folder listing < 100 ms
//! - message list (50/page) < 50 ms
//! - search < 100 ms

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use kestrel_core::{
    clock::SystemClock,
    ids::{AccountId, FolderId},
    mime::{MimeParser, StalwartParser},
    protocol::{MailProtocol, Provider, SearchQuery, SortSpec, Window},
    testkit::{SequentialIds, temp_paths},
};
use kestrel_storage::{
    IndexDoc, IndexService, IngestBatch, IngestMessage, NewAccount, NewFolder, SearchService,
    StorageService,
};

const MAILBOX_SIZE: u64 = 10_000;

fn bench_large_mailbox(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let (storage, folder, search_handle, _dir) = rt.block_on(async {
        let (dir, paths) = temp_paths();
        paths.ensure().unwrap();
        let ids = Arc::new(SequentialIds::new());
        let clock = Arc::new(SystemClock);

        let (storage, _cancel) = StorageService::spawn(paths.clone(), ids, clock.clone());
        let account = storage
            .upsert_account(NewAccount {
                name: "bench".into(),
                email: "bench@x.example".into(),
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

        // Ingest 10k messages in chunks.
        let chunk_size: u64 = 500;
        for base in (0..MAILBOX_SIZE).step_by(usize::try_from(chunk_size).unwrap_or(500)) {
            let batch = make_batch(folder, base, chunk_size.min(MAILBOX_SIZE - base));
            storage.ingest_batch(batch).await.unwrap();
        }

        // Build search index.
        let index = IndexService::spawn(&paths.index_dir(), storage.clone(), clock).unwrap();
        build_index(&index, MAILBOX_SIZE, folder, account).await;
        let search = SearchService::from_index(&index, storage.clone());

        (storage, folder, search, dir)
    });

    let mut group = c.benchmark_group("large_mailbox");
    group.sample_size(20);

    // Folder listing SLA: < 100 ms.
    group.bench_function("folder_listing", |b| {
        b.iter_batched(
            || (),
            |()| {
                rt.block_on(async {
                    let accounts = storage.list_accounts().await.unwrap();
                    if let Some(acc) = accounts.first() {
                        let _ = storage.list_folders(acc.id).await.unwrap();
                    }
                });
            },
            BatchSize::SmallInput,
        );
    });

    // Message list pagination SLA: < 50 ms (50 messages/page).
    group.bench_function("message_list_50", |b| {
        b.iter_batched(
            || (),
            |()| {
                rt.block_on(async {
                    let _ = storage
                        .list_messages(
                            folder,
                            Window {
                                offset: 0,
                                limit: 50,
                            },
                            SortSpec::default(),
                        )
                        .await
                        .unwrap();
                });
            },
            BatchSize::SmallInput,
        );
    });

    // Search SLA: < 100 ms.
    group.bench_function("search", |b| {
        b.iter_batched(
            || (),
            |()| {
                rt.block_on(async {
                    let _ = search_handle
                        .search(&SearchQuery {
                            text: Some("budget quarterly".into()),
                            limit: Some(50),
                            ..SearchQuery::default()
                        })
                        .await
                        .unwrap();
                });
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

fn make_batch(folder: FolderId, base: u64, count: u64) -> IngestBatch {
    let mut messages = Vec::with_capacity(usize::try_from(count).unwrap_or(500));
    for i in 0..count {
        let n = base + i;
        let raw = format!(
            "From: bench{n} <b{n}@x.example>\r\n\
             To: rcpt@example.net\r\n\
             Subject: bench message {n}\r\n\
             Message-ID: <bench-{n}@x.example>\r\n\
             Date: Fri, 28 Aug 2026 10:00:00 +0000\r\n\
             Content-Type: text/plain\r\n\r\n\
             body of message {n} with searchable text about budgets and quarterly reports"
        );
        let parsed = StalwartParser::parse(raw.as_bytes()).unwrap();
        messages.push(IngestMessage {
            folder,
            uid: 1_000_000 + u32::try_from(n).unwrap_or(0),
            internal_date: 1_700_000_000_000 + (n * 1000).cast_signed(),
            flags: vec![],
            parsed,
            raw_blob: None,
            raw_size: 512,
        });
    }
    IngestBatch { messages }
}

async fn build_index(
    index: &kestrel_storage::IndexHandle,
    count: u64,
    folder: FolderId,
    account: AccountId,
) {
    const CHUNK: u64 = 5_000;
    for base in (0..count).step_by(usize::try_from(CHUNK).unwrap_or(5_000)) {
        let mut docs = Vec::with_capacity(usize::try_from(CHUNK).unwrap_or(5_000));
        for i in base..(base + CHUNK).min(count) {
            docs.push(IndexDoc {
                id: kestrel_core::ids::MessageId::from_uuid(uuid::Uuid::now_v7()),
                folder,
                account,
                subject: Some(format!("quarterly budget report {i}")),
                body: format!(
                    "the quarterly budget for team {i} includes travel and lunch line items"
                ),
                from: vec![format!("sender{}@example.org", i % 97)],
                to: vec!["rcpt@example.net".to_string()],
                cc: vec![],
                attachment_names: vec![],
                date: 1_700_000_000_000 + (i % 100_000).cast_signed(),
                has_attachment: false,
            });
        }
        index.add_fire_and_forget(docs).await;
    }
    index.commit().await.unwrap();
}

criterion_group!(benches, bench_large_mailbox);
criterion_main!(benches);
