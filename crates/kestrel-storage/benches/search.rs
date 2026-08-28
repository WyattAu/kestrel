//! SLA benchmark: search latency at scale (engineering-standards §5).
//! Gates: 100k → fail > 50 ms / warn > 15 ms; 500k → fail > 30 ms first-50.
//!
//! Index build is done once per size (setup, not measured); the measured
//! iteration is the query path (parse → tantivy → hydrate).

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use kestrel_core::{
    clock::SystemClock,
    protocol::SearchQuery,
    testkit::{SequentialIds, temp_paths},
};
use kestrel_storage::{IndexDoc, IndexService, SearchService, StorageService};

fn bench_search(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (dir, paths) = temp_paths();
        paths.ensure().unwrap();
        let ids = Arc::new(SequentialIds::new());
        let clock = Arc::new(SystemClock);
        let (storage, _cancel) = StorageService::spawn(paths.clone(), ids, clock.clone());
        storage.list_accounts().await.unwrap();
        let index = IndexService::spawn(&paths.index_dir(), storage.clone(), clock).unwrap();
        let search = SearchService::from_index(&index, storage.clone());

        let folder = kestrel_core::ids::FolderId::from_uuid(uuid::Uuid::now_v7());
        let account = kestrel_core::ids::AccountId::from_uuid(uuid::Uuid::now_v7());
        for (name, count) in [("search_100k", 100_000u64), ("search_500k", 500_000)] {
            let mut group = c.benchmark_group(name);
            group.sample_size(20);
            build_index(&index, count, folder, account).await;
            group.bench_function("text_query_first_50", |b| {
                b.iter(|| {
                    rt.block_on(async {
                        search
                            .search(&SearchQuery {
                                text: Some("budget quarterly".into()),
                                limit: Some(50),
                                ..SearchQuery::default()
                            })
                            .await
                            .unwrap();
                    });
                });
            });
            group.finish();
        }
        drop(dir);
    });
}

/// Benches are 64-bit-hosted; `u64` counters are bounded by `count`.
#[allow(clippy::cast_possible_truncation)]
async fn build_index(
    index: &kestrel_storage::IndexHandle,
    count: u64,
    folder: kestrel_core::ids::FolderId,
    account: kestrel_core::ids::AccountId,
) {
    // Direct doc adds (storage round-trip per message would dominate setup).
    const CHUNK: u64 = 5_000;
    for base in (0..count).step_by(CHUNK as usize) {
        let mut docs = Vec::with_capacity(CHUNK as usize);
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
                attachment_names: if i % 10 == 0 {
                    vec!["spreadsheet.xlsx".to_string()]
                } else {
                    vec![]
                },
                date: 1_700_000_000_000 + (i % 100_000).cast_signed(),
                has_attachment: i % 10 == 0,
            });
        }
        index.add_fire_and_forget(docs).await;
    }
    // Wait for the commit ticker to settle before measuring.
    index.commit().await.unwrap();
}

criterion_group!(benches, bench_search);
criterion_main!(benches);
