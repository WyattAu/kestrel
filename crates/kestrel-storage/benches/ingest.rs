//! SLA benchmark: envelope ingestion rate (engineering-standards §5).
//! Fail gate: < 800 msgs/sec; warn gate: < 1,500 msgs/sec.
//! Real harness lands with Phase 1 (kestrel-storage).
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, criterion_group, criterion_main};

fn bench_ingest(c: &mut Criterion) {
    c.bench_function("ingest_placeholder", |b| b.iter(|| 1 + 1));
}

criterion_group!(benches, bench_ingest);
criterion_main!(benches);
