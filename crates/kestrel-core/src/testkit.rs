//! Shared test fixtures (docs/testing-strategy.md §1): injected clock, ID
//! sequences, temp paths, and the MIME corpus loader. No test outside this
//! module may call wall-clock or real-path APIs directly.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use uuid::Uuid;

pub use crate::{clock::FakeClock, ids::IdGenerator, paths::Paths};

/// Deterministic ID generator: UUID v7-shaped, sequential counters (unique
/// within a process run).
#[derive(Debug, Default)]
pub struct SequentialIds {
    counter: AtomicU64,
}

impl SequentialIds {
    /// New generator starting at 0.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl IdGenerator for SequentialIds {
    fn next_id(&self) -> Uuid {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        // UUID v7 layout: unix_ts_ms(48) | ver(4) | rand_a(12) | var(2) | rand_b(62).
        // Fixed timestamp + counter keeps ids unique and monotonically
        // ordered by byte comparison.
        let ts: u64 = 0;
        let rand_a: u16 = (n & 0x0fff) as u16;
        let rand_b: u64 = n >> 12;
        let mut bytes = [0u8; 16];
        bytes[0..6].copy_from_slice(&ts.to_be_bytes()[2..]);
        bytes[6] = 0x70 | ((rand_a >> 8) as u8 & 0x0f);
        bytes[7] = (rand_a & 0xff) as u8;
        bytes[8] = 0x80 | ((rand_b >> 56) as u8 & 0x3f);
        bytes[9..16].copy_from_slice(&(rand_b & 0x00ff_ffff_ffff_ffff).to_be_bytes()[1..]);
        Uuid::from_bytes(bytes)
    }
}

/// Creates a temp-dir-backed [`Paths`] (all roots isolated under the temp
/// dir) — the standard fixture for any test touching storage.
///
/// # Panics
/// Panics only when the OS cannot create a temp directory (test fixture).
#[must_use]
pub fn temp_paths() -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let paths = Paths::nested_under(dir.path());
    (dir, paths)
}

/// Workspace-root path of the MIME corpus (`tests/mime-corpus/`).
#[must_use]
pub fn mime_corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/mime-corpus")
}

/// Returns the proptest case count, configurable via `KESTREL_PROPTOP_CASES` env var.
#[must_use]
pub fn proptest_cases() -> u32 {
    std::env::var("KESTREL_PROPTOP_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(128)
}

/// Loads every `.eml` file under the corpus root (recursive). Returns
/// `(relative_name, bytes)` pairs sorted by name.
#[must_use]
pub fn load_mime_corpus() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let root = mime_corpus_dir();
    if !root.exists() {
        return out;
    }
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "eml")
                && let Ok(bytes) = std::fs::read(&path)
                && let Some(name) = path.strip_prefix(&root).ok().and_then(|p| p.to_str())
            {
                out.push((name.to_owned(), bytes));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Builds a small RFC 5322 message for tests.
#[must_use]
pub fn sample_message(subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: Test Sender <sender@example.org>\r\nTo: rcpt@example.net\r\nSubject: {subject}\r\nMessage-ID: <{subject}@test.example>\r\nDate: Fri, 28 Aug 2026 10:00:00 +0000\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn sequential_ids_are_unique_and_ordered() {
        let seq = SequentialIds::new();
        let a = seq.next_id();
        let b = seq.next_id();
        assert_ne!(a, b);
        assert_eq!(a.get_version_num(), 7);
        assert!(b > a, "uuid v7 monotonicity within generator");
    }

    #[test]
    fn temp_paths_isolated() {
        let (_guard, paths) = temp_paths();
        assert!(
            paths
                .data_db()
                .starts_with(paths.blob_root().parent().unwrap())
        );
    }
}
