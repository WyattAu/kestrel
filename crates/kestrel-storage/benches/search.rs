//! SLA benchmark: search query latency at 100k/500k docs
//! (engineering-standards §5). Fail gate: > 50 ms (100k), > 30 ms first-50
//! (500k); warn gate: > 15 ms (100k). Real harness lands with Phase 1.

#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]
use criterion::{Criterion, criterion_group, criterion_main};

fn bench_search(c: &mut Criterion) {
    c.bench_function("search_placeholder", |b| b.iter(|| 1 + 1));
}

criterion_group!(benches, bench_search);
criterion_main!(benches);
