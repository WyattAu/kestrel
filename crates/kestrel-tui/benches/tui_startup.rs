//! SLA benchmark: TUI cold-start time (engineering-standards §5).
//! Gate: < 50 ms cold start.
//!
//! Measures the time to load config, create paths, and initialize `AppState`
//! (the pre-materialized windowed model) — the minimum work required before
//! the terminal event loop can start drawing frames.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};
use kestrel_core::config::Config;

fn bench_cold_start(c: &mut Criterion) {
    let mut group = c.benchmark_group("tui_cold_start");
    group.sample_size(20);

    group.bench_function("app_state_default", |b| {
        b.iter(|| {
            kestrel_tui::app::AppState::default();
        });
    });

    group.bench_function("config_paths_app_state", |b| {
        b.iter(|| {
            let (_dir, paths) = kestrel_core::testkit::temp_paths();
            paths.ensure().unwrap();
            let loaded = Config::load(&paths).unwrap();
            let _state = kestrel_tui::app::AppState::default();
            drop(loaded);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_cold_start);
criterion_main!(benches);
