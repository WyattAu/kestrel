//! SLA benchmark: engine cold-start time (engineering-standards §5).
//! Gate: < 50 ms cold start.
//!
//! Measures the time from `Engine::spawn` to receiving `EngineStarted`.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use kestrel_core::{
    config::Config,
    protocol::{EngineEvent, FrontendKind},
};

fn bench_cold_start(c: &mut Criterion) {
    let mut group = c.benchmark_group("engine_cold_start");
    group.sample_size(20);

    for account_count in [0, 1, 3] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{account_count}_accounts")),
            &account_count,
            |b, &_count| {
                let rt = tokio::runtime::Runtime::new().unwrap();
                b.iter(|| {
                    rt.block_on(async {
                        let (_dir, paths) = kestrel_core::testkit::temp_paths();
                        paths.ensure().unwrap();
                        let handle = kestrel_engine::Engine::spawn_with(
                            Arc::new(Config::default()),
                            Arc::new(paths),
                            Arc::new(kestrel_core::ids::SystemIdGenerator),
                            Arc::new(kestrel_core::clock::SystemClock),
                            Arc::new(kestrel_crypto::InMemoryStore::new()),
                        )
                        .await
                        .unwrap();

                        let mut events = handle.events();
                        let mut got_started = false;
                        while let Ok(ev) =
                            tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
                                .await
                        {
                            if let Ok(EngineEvent::EngineStarted { .. }) = ev {
                                got_started = true;
                                break;
                            }
                        }
                        assert!(got_started, "EngineStarted not received within 5s");

                        let _ = handle
                            .commands
                            .send(kestrel_core::protocol::Command {
                                id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                                origin: FrontendKind::Gui,
                                payload: kestrel_core::protocol::CommandPayload::Shutdown {
                                    drain: false,
                                },
                            })
                            .await;
                    });
                });
            },
        );
    }
    group.finish();
}

fn bench_memory_idle(c: &mut Criterion) {
    let mut group = c.benchmark_group("engine_memory_idle");
    group.sample_size(5);

    group.bench_function("idle_rss_bytes", |b| {
        let rt = tokio::runtime::Runtime::new().unwrap();
        b.iter(|| {
            rt.block_on(async {
                let (_dir, paths) = kestrel_core::testkit::temp_paths();
                paths.ensure().unwrap();
                let handle = kestrel_engine::Engine::spawn_with(
                    Arc::new(Config::default()),
                    Arc::new(paths),
                    Arc::new(kestrel_core::ids::SystemIdGenerator),
                    Arc::new(kestrel_core::clock::SystemClock),
                    Arc::new(kestrel_crypto::InMemoryStore::new()),
                )
                .await
                .unwrap();

                let mut events = handle.events();
                let _ =
                    tokio::time::timeout(std::time::Duration::from_secs(5), events.recv()).await;

                let rss_bytes = get_rss_bytes();
                let _ = handle
                    .commands
                    .send(kestrel_core::protocol::Command {
                        id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                        origin: FrontendKind::Gui,
                        payload: kestrel_core::protocol::CommandPayload::Shutdown { drain: false },
                    })
                    .await;

                rss_bytes
            });
        });
    });
    group.finish();
}

#[cfg(target_os = "linux")]
fn get_rss_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| {
            let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
            // Assume 4 KiB pages (standard on x86_64 Linux).
            Some(pages * 4096)
        })
        .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
fn get_rss_bytes() -> u64 {
    0
}

criterion_group!(benches, bench_cold_start, bench_memory_idle);
criterion_main!(benches);
