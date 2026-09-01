//! SLA benchmark: GUI cold-start time (engineering-standards §5).
//! Gate: < 200 ms cold start.
//!
//! Measures the time to load config, create paths, and initialize the
//! viewport state — the minimum non-display work required before the
//! Slint event loop can start. `AppWindow::new()` is excluded because it
//! requires a display server (cannot run in headless CI).

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};
use kestrel_core::config::Config;

fn bench_cold_start(c: &mut Criterion) {
    let mut group = c.benchmark_group("gui_cold_start");
    group.sample_size(20);

    group.bench_function("viewport_state_default", |b| {
        b.iter(|| {
            let _vp = kestrel_gui::ViewportState::default();
        });
    });

    group.bench_function("config_paths_viewport_state", |b| {
        b.iter(|| {
            let (_dir, paths) = kestrel_core::testkit::temp_paths();
            paths.ensure().unwrap();
            let loaded = Config::load(&paths).unwrap();
            let _vp = kestrel_gui::ViewportState::default();
            drop(loaded);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_cold_start);
criterion_main!(benches);
