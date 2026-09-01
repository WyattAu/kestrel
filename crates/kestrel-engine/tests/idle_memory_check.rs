//! Standalone test: idle memory SLA check.
//!
//! Spawns an Engine with `InMemoryStore`, waits for `EngineStarted`,
//! reads RSS from `/proc/self/statm`, prints the value, then shuts down.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    missing_docs
)]

#[cfg(target_os = "linux")]
#[tokio::test]
async fn idle_memory_check() {
    use std::sync::Arc;

    use kestrel_core::{
        config::Config,
        ids::{RequestId, SystemIdGenerator},
        paths::Paths,
        protocol::{Command, CommandPayload, EngineEvent, FrontendKind},
    };
    use kestrel_engine::Engine;

    let dir = tempfile::tempdir().unwrap();
    let paths = Arc::new(Paths::nested_under(dir.path()));
    paths.ensure().unwrap();

    let handle = Engine::spawn_with(
        Arc::new(Config::default()),
        paths,
        Arc::new(SystemIdGenerator),
        Arc::new(kestrel_core::clock::SystemClock),
        Arc::new(kestrel_crypto::InMemoryStore::new()),
    )
    .await
    .expect("engine spawns");

    let mut events = handle.events();
    let mut got_started = false;
    while let Ok(ev) = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv()).await
    {
        if let Ok(EngineEvent::EngineStarted { .. }) = ev {
            got_started = true;
            break;
        }
    }
    assert!(got_started, "EngineStarted not received within 5s");

    // Read RSS from /proc/self/statm.
    let rss_bytes = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| {
            let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
            Some(pages * 4096)
        })
        .unwrap_or(0);
    eprintln!("MEMORY_RSS_BYTES={rss_bytes}");

    // Shutdown.
    let _ = handle
        .commands
        .send(Command {
            id: RequestId::from_uuid(uuid::Uuid::now_v7()),
            origin: FrontendKind::Gui,
            payload: CommandPayload::Shutdown { drain: false },
        })
        .await;
}
