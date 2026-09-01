//! SLA benchmark: envelope ingestion rate (engineering-standards §5).
//! Fail gate: < 800 msgs/sec; warn gate: < 1,500 msgs/sec.
//!
//! Measures the full envelope path: MIME parse → CAS write → `SQLite` ingest
//! (single-writer service), release profile.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use kestrel_core::{
    clock::SystemClock,
    ids::FolderId,
    mime::{MimeParser, StalwartParser},
    protocol::{MailProtocol, Provider},
    testkit::{SequentialIds, temp_paths},
};
use kestrel_storage::{IngestBatch, IngestMessage, NewAccount, NewFolder, StorageService};

fn bench_ingest(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (storage, folder, cancel, _dir) = rt.block_on(async {
        let (dir, paths) = temp_paths();
        paths.ensure().unwrap();
        let ids = Arc::new(SequentialIds::new());
        let clock = Arc::new(SystemClock);
        let (storage, cancel) = StorageService::spawn(paths, ids, clock);
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
        (storage, folder, cancel, dir)
    });

    let mut group = c.benchmark_group("ingest");
    group.sample_size(10);
    group.throughput(criterion::Throughput::Elements(100));
    group.bench_function("envelopes_100/batch", |b| {
        b.iter_batched(
            || make_batch(folder, 100),
            |batch| {
                rt.block_on(async {
                    storage.ingest_batch(batch).await.unwrap();
                });
            },
            BatchSize::LargeInput,
        );
    });
    group.finish();

    cancel.cancel();
}

fn make_batch(folder: FolderId, n: u64) -> IngestBatch {
    let mut messages = Vec::with_capacity(usize::try_from(n).unwrap_or(100));
    for i in 0..n {
        let raw = format!(
            "From: bench{i} <b{i}@x.example>\r\nTo: rcpt@example.net\r\nSubject: bench message {i}\r\nMessage-ID: <bench-{i}@x.example>\r\nDate: Fri, 28 Aug 2026 10:00:00 +0000\r\nContent-Type: text/plain\r\n\r\nbody of message {i} with some searchable text about budgets and lunch"
        );
        let parsed = StalwartParser::parse(raw.as_bytes()).unwrap();
        messages.push(IngestMessage {
            folder,
            uid: 1_000_000 + u32::try_from(i).unwrap_or(0),
            internal_date: 1_700_000_000_000 + i.cast_signed() * 1000,
            flags: vec![],
            parsed,
            raw_blob: None,
            raw_size: 512,
        });
    }
    IngestBatch { messages }
}

criterion_group!(benches, bench_ingest);
criterion_main!(benches);
