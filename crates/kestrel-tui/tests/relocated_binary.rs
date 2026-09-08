//! Relocated-binary regression test (the rc.1 release P0 class).
//!
//! The first release artifact failed at startup on every machine except
//! the build machine: `db.rs` resolved the sqlx migration directory from
//! `env!("CARGO_MANIFEST_DIR")`, baking the build tree path into the
//! binary. Every in-tree check passes on the build machine; only running
//! the binary from a copied location — with a scratch `$HOME` — exposes
//! it.
//!
//! This test copies `CARGO_BIN_EXE_kestrel-tui` to a temp dir and boots
//! it with a scratch `HOME`/`XDG_*`. With stdout a pipe, the TUI's
//! expected outcome is the non-TTY refusal — which fires *after* the
//! engine (and its embedded migrations) has started. Reaching that
//! refusal is therefore the pass signal; the pre-fix failure mode was
//! `migration_failed` before any TTY check. CI never catches this class
//! in-tree, which is why the same check also runs as a release.yml gate.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::process::Command;

#[test]
fn relocated_binary_boots_engine_from_scratch_home() {
    let bin = env!("CARGO_BIN_EXE_kestrel-tui");
    let scratch = tempfile::tempdir().unwrap();
    let data_home = tempfile::tempdir().unwrap();

    // Copy — not hardlink, not reference-in-place — so any path the binary
    // resolved at compile time points outside a directory that exists.
    let copied = scratch.path().join("kestrel-tui");
    std::fs::copy(bin, &copied).unwrap();

    let output = Command::new(&copied)
        .arg("--help")
        .env("HOME", scratch.path())
        .env("XDG_DATA_HOME", data_home.path())
        .env("XDG_CONFIG_HOME", scratch.path().join("config"))
        .env("XDG_CACHE_HOME", scratch.path().join("cache"))
        .output()
        .expect("spawn relocated kestrel-tui");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("stdout is not a terminal"),
        "expected the TTY refusal (which fires after engine boot — proving \
         storage/migrations started); got exit {output:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    for stream in [&stdout, &stderr] {
        assert!(
            !stream.contains("migration_failed"),
            "engine reported migration failure — migration paths are not embedded\n{stream}"
        );
        assert!(
            !stream.contains(env!("CARGO_MANIFEST_DIR")),
            "binary references its build tree at runtime\n{stream}"
        );
    }
}
