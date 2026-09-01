//! Memory leak detection: run 100 sync cycles and assert no sustained RSS growth.
//!
//! Each cycle performs: list accounts → list folders → search → wait 100ms.
//! Samples RSS from `/proc/self/statm` every cycle and asserts that the final
//! RSS is less than 115% of the initial RSS.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    missing_docs
)]

#[cfg(target_os = "linux")]
#[tokio::test]
async fn memory_leak_100_cycles() {
    use std::{sync::Arc, time::Duration};

    use kestrel_core::{
        config::Config,
        paths::Paths,
        protocol::{Command, CommandPayload, EngineEvent, FrontendKind, SearchQuery},
    };
    use kestrel_engine::Engine;

    fn read_rss_bytes() -> u64 {
        std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| {
                let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
                Some(pages * 4096)
            })
            .unwrap_or(0)
    }

    let dir = tempfile::tempdir().unwrap();
    let paths = Arc::new(Paths::nested_under(dir.path()));
    paths.ensure().unwrap();

    let handle = Engine::spawn_with(
        Arc::new(Config::default()),
        paths,
        Arc::new(kestrel_core::ids::SystemIdGenerator),
        Arc::new(kestrel_core::clock::SystemClock),
        Arc::new(kestrel_crypto::InMemoryStore::new()),
    )
    .await
    .expect("engine spawns");

    // Wait for EngineStarted.
    let mut events = handle.events();
    let mut got_started = false;
    while let Ok(ev) = tokio::time::timeout(Duration::from_secs(5), events.recv()).await {
        if let Ok(EngineEvent::EngineStarted { .. }) = ev {
            got_started = true;
            break;
        }
    }
    assert!(got_started, "EngineStarted not received within 5s");

    let initial_rss = read_rss_bytes();
    eprintln!("INITIAL_RSS={initial_rss}");

    for cycle in 0..100u32 {
        // list accounts
        let (tx, rx) = tokio::sync::oneshot::channel();
        handle
            .commands
            .send(Command {
                id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                origin: FrontendKind::Tui,
                payload: CommandPayload::ListAccounts { reply: tx },
            })
            .await
            .expect("command accepted");
        let _ = rx.await;

        // list folders (uses a dummy folder id; the engine returns empty or error)
        let (tx, rx) = tokio::sync::oneshot::channel();
        handle
            .commands
            .send(Command {
                id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                origin: FrontendKind::Tui,
                payload: CommandPayload::ListFolders {
                    account: kestrel_core::ids::AccountId::from_uuid(uuid::Uuid::now_v7()),
                    reply: tx,
                },
            })
            .await
            .expect("command accepted");
        let _ = rx.await;

        // search (empty query; bounded by the engine)
        let (tx, rx) = tokio::sync::oneshot::channel();
        handle
            .commands
            .send(Command {
                id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                origin: FrontendKind::Tui,
                payload: CommandPayload::Search {
                    query: SearchQuery::default(),
                    reply: tx,
                },
            })
            .await
            .expect("command accepted");
        let _ = rx.await;

        tokio::time::sleep(Duration::from_millis(100)).await;

        if cycle % 10 == 0 {
            let rss = read_rss_bytes();
            eprintln!("CYCLE={cycle} RSS={rss}");
        }
    }

    let final_rss = read_rss_bytes();
    eprintln!("FINAL_RSS={final_rss}");

    // Allow up to 15% growth. Transient spikes from allocator fragmentation
    // are expected, but sustained growth indicates a leak.
    let threshold = (initial_rss as f64) * 1.15;
    assert!(
        (final_rss as f64) < threshold,
        "memory leak suspected: initial RSS {initial_rss} bytes, final RSS {final_rss} bytes (threshold {threshold:.0})"
    );

    // Clean shutdown.
    let _ = handle
        .commands
        .send(Command {
            id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
            origin: FrontendKind::Gui,
            payload: CommandPayload::Shutdown { drain: false },
        })
        .await;
}
