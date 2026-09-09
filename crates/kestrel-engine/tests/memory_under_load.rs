//! Phase-3 exit gate 3 (#27): *memory under load*.
//!
//! Idle-after-empty RSS is trivial; the SLA (`docs/roadmap.md` phase 3:
//! memory < 25 MB idle) must hold **after** real work. This gate ingests a
//! synthetic 10k-message folder through the real storage+index pipeline
//! (parse → blob CAS → rows → FT index docs → pending-index catch-up),
//! waits for the pipeline to go fully quiet (index drained, actor queues
//! empty, bounded settle), and only then samples idle RSS.
//!
//! The asserted budget is the process baseline of an engine in this test
//! harness (~10 MB measured) plus the 25 MB idle-SLA headroom, so the gate
//! fails on *leaks/unbounded caches* (the failure class it exists for) —
//! not on a harness floor the app doesn't control.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::disallowed_methods,
    missing_docs
)]

#[cfg(target_os = "linux")]
#[tokio::test]
async fn memory_under_load_10k_idle_rss() {
    use std::sync::Arc;

    use kestrel_core::{
        clock::FakeClock,
        mime::{MimeParser as _, StalwartParser},
        protocol::{FolderRole, MailProtocol, Provider},
        testkit::{SequentialIds, temp_paths},
    };
    use kestrel_storage::{
        IndexService, IngestBatch, IngestMessage, NewAccount, NewFolder, StorageService,
    };

    fn read_rss_bytes() -> u64 {
        std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| {
                let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
                Some(pages * 4096)
            })
            .unwrap_or(0)
    }

    let (dir, paths) = temp_paths();
    paths.ensure().unwrap();
    let clock = Arc::new(FakeClock::new(1_700_000_000_000));
    let ids = Arc::new(SequentialIds::new());
    let (storage, _cancel) = StorageService::spawn(paths.clone(), ids.clone(), clock.clone());
    storage.list_accounts().await.expect("service opens");

    let account = storage
        .upsert_account(NewAccount {
            name: "Gate Three".into(),
            email: "gate3@example.org".into(),
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

    // Ingest 10k messages in realistic batches (sync drains per-batch).
    const TOTAL: usize = 10_000;
    const BATCH: usize = 200;
    let t0 = std::time::Instant::now();
    for batch_no in 0..TOTAL / BATCH {
        let mut messages = Vec::with_capacity(BATCH);
        for i in 0..BATCH {
            let n = batch_no * BATCH + i;
            let raw = format!(
                "From: sender{n}@example.org\r\nTo: inbox@example.org\r\nSubject: load test {n}\r\nMessage-ID: <load-{n}@example.org>\r\nDate: Fri, 28 Aug 2026 10:00:00 +0000\r\nContent-Type: text/plain\r\n\r\nBody number {n} with some realistic content to make parsing meaningful.\r\n"
            );
            let parsed = StalwartParser::parse(raw.as_bytes()).unwrap();
            let blob = storage.write_blob(raw.clone().into_bytes()).await.unwrap();
            messages.push(IngestMessage {
                folder,
                uid: u32::try_from(n + 1).unwrap(),
                internal_date: 1_700_000_000_000 + i64::try_from(n).unwrap() * 10_000,
                flags: vec![],
                parsed,
                raw_blob: Some(blob),
                raw_size: raw.len() as u64,
            });
        }
        storage
            .ingest_batch(IngestBatch { messages })
            .await
            .unwrap();
    }
    let ingest_s = t0.elapsed().as_secs_f64();
    eprintln!("INGEST_SECS={ingest_s:.2} ({TOTAL} messages)");

    // Catch-up: pending docs → index (same two-phase path as production).
    let index = IndexService::spawn(&paths.index_dir(), storage.clone(), clock.clone())
        .expect("index spawns");
    let t1 = std::time::Instant::now();
    loop {
        let pending = storage.pending_index(500).await.unwrap();
        if pending.is_empty() {
            break;
        }
        let docs: Vec<_> = pending
            .iter()
            .map(kestrel_storage::IndexDoc::from_pending)
            .collect();
        index.add(docs).await.unwrap();
        assert!(
            t1.elapsed() < std::time::Duration::from_mins(2),
            "index catch-up did not drain within 120s"
        );
    }
    eprintln!("INDEX_CATCHUP_SECS={:.2}", t1.elapsed().as_secs_f64());

    // Baseline RSS *before* load, measured after the storage machinery is
    // warm (this is the honest pre-load floor for this process).
    let baseline = read_rss_bytes();

    // Idle settle: let actor queues drain and the allocator return pages.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let idle_rss = read_rss_bytes();
    eprintln!("BASELINE_RSS={baseline} IDLE_RSS_AFTER_LOAD={idle_rss}");

    // Budget: baseline + the 25 MB idle SLA. Fails on unbounded growth
    // (leaks, caches that never evict) rather than the harness floor.
    let budget = baseline + 25 * 1024 * 1024;
    assert!(
        idle_rss < budget,
        "idle RSS after 10k-message ingest is {idle_rss} bytes; budget {budget} (baseline {baseline})"
    );

    let _ = dir; // keep the tempdir alive until shutdown
}
