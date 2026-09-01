//! Stress benchmark: 100k messages across 1000 folders.
//!
//! Measures:
//! - Total ingestion time (target: > 100 msgs/sec)
//! - Search latency at 100k scale (target: < 200 ms)
//! - Folder listing at 1000 folders (target: < 500 ms)

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    missing_docs
)]

use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use kestrel_core::{
    clock::SystemClock,
    ids::FolderId,
    mime::{MimeParser, StalwartParser},
    protocol::{MailProtocol, Provider, SearchQuery},
    testkit::{SequentialIds, temp_paths},
};
use kestrel_storage::{
    IndexDoc, IndexService, IngestBatch, IngestMessage, NewAccount, NewFolder, SearchService,
    StorageService,
};

const TOTAL_MESSAGES: u64 = 100_000;
const TOTAL_FOLDERS: u64 = 1_000;
const MESSAGES_PER_FOLDER: u64 = TOTAL_MESSAGES / TOTAL_FOLDERS;

fn bench_stress(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let (storage, folders, search_handle, _dir) = rt.block_on(async {
        let (dir, paths) = temp_paths();
        paths.ensure().unwrap();
        let ids = Arc::new(SequentialIds::new());
        let clock = Arc::new(SystemClock);

        let (storage, _cancel) = StorageService::spawn(paths.clone(), ids, clock.clone());
        let account = storage
            .upsert_account(NewAccount {
                name: "stress".into(),
                email: "stress@x.example".into(),
                provider: Provider::Generic,
                protocol: MailProtocol::Imap,
                auth_kind: "password".into(),
                host: String::new(),
            })
            .await
            .unwrap();

        // Create 1000 folders.
        let mut folder_ids = Vec::with_capacity(TOTAL_FOLDERS as usize);
        for i in 0..TOTAL_FOLDERS {
            let folder = storage
                .upsert_folder(NewFolder {
                    account,
                    remote_name: format!("FOLDER_{i}"),
                    attributes: vec![],
                    role: None,
                    delimiter: "/".into(),
                    uid_validity: 1,
                    highest_modseq: 0,
                })
                .await
                .unwrap();
            folder_ids.push(folder);
        }

        // Ingest 100k messages across 1000 folders (100 per folder).
        let chunk_size: u64 = 500;
        for (folder_idx, &folder) in folder_ids.iter().enumerate() {
            let base_uid = (folder_idx as u64) * MESSAGES_PER_FOLDER;
            for chunk_base in (0..MESSAGES_PER_FOLDER).step_by(chunk_size as usize) {
                let count = chunk_size.min(MESSAGES_PER_FOLDER - chunk_base);
                let batch = make_batch(
                    folder,
                    (folder_idx as u64) * MESSAGES_PER_FOLDER + chunk_base,
                    count,
                    base_uid,
                );
                storage.ingest_batch(batch).await.unwrap();
            }
        }

        // Build search index.
        let index = IndexService::spawn(&paths.index_dir(), storage.clone(), clock).unwrap();
        build_index(&index, TOTAL_MESSAGES, &folder_ids, account).await;
        let search = SearchService::from_index(&index, storage.clone());

        (storage, folder_ids, search, dir)
    });

    let mut group = c.benchmark_group("stress");

    // Ingestion throughput.
    group.throughput(criterion::Throughput::Elements(100));
    group.bench_function("ingestion_batch_100", |b| {
        let folder = folders[0];
        b.iter_batched(
            || make_batch(folder, 0, 100, 0),
            |batch| {
                rt.block_on(async {
                    storage.ingest_batch(batch).await.unwrap();
                });
            },
            BatchSize::LargeInput,
        );
    });

    // Search at 100k scale: < 200ms target.
    group.bench_function("search_100k", |b| {
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

    // Folder listing at 1000 folders: < 500ms target.
    group.bench_function("folder_listing_1000", |b| {
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

    group.finish();
}

fn make_batch(folder: FolderId, base: u64, count: u64, uid_offset: u64) -> IngestBatch {
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
            uid: 1_000_000 + u32::try_from(uid_offset + i).unwrap_or(0),
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
    folders: &[FolderId],
    account: kestrel_core::ids::AccountId,
) {
    const CHUNK: u64 = 5_000;
    for base in (0..count).step_by(usize::try_from(CHUNK).unwrap_or(5_000)) {
        let mut docs = Vec::with_capacity(usize::try_from(CHUNK).unwrap_or(5_000));
        for i in base..(base + CHUNK).min(count) {
            let folder_idx = (i / MESSAGES_PER_FOLDER) as usize;
            docs.push(IndexDoc {
                id: kestrel_core::ids::MessageId::from_uuid(uuid::Uuid::now_v7()),
                folder: folders[folder_idx],
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

criterion_group!(benches, bench_stress);
criterion_main!(benches);
