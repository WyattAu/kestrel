# Kestrel

[![CI](https://github.com/WyattAu/kestrel/actions/workflows/ci.yml/badge.svg)](https://github.com/WyattAu/kestrel/actions/workflows/ci.yml)
[![Release](https://github.com/WyattAu/kestrel/actions/workflows/release.yml/badge.svg)](https://github.com/WyattAu/kestrel/actions/workflows/release.yml)
[![SBOM](https://github.com/WyattAu/kestrel/actions/workflows/sbom.yml/badge.svg)](https://github.com/WyattAu/kestrel/actions/workflows/sbom.yml)
[![release](https://img.shields.io/github/v/release/WyattAu/kestrel)](https://github.com/WyattAu/kestrel/releases/latest)
[![license](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

A high-performance, modular email client in Rust with frontends sharing one
core engine:

- **`kestrel-tui`** — keyboard-driven terminal client (ratatui)
- **`kestrel-gui`** — native desktop client (Slint shell + sandboxed webview
  for HTML mail)
- **`kestrel-mobile`** — Slint mobile shell (ADR 0013; platform stubs)

Offline-first, IMAP/JMAP + SMTP, SQLite + Tantivy full-text search,
zero-trust HTML rendering. Licensed under [Apache-2.0](LICENSE).

## Install

Prebuilt, attested binaries for Linux (x64/aarch64), macOS (Intel/Apple
Silicon), and Windows: grab the latest archive from
[Releases](https://github.com/WyattAu/kestrel/releases/latest) and verify it
before running:

```bash
# checksum (SHA256SUMS.txt is attached to each release)
sha256sum -c <(grep linux-gnu SHA256SUMS.txt)

# SLSA build provenance (proves GitHub Actions built the artifact)
gh attestation verify kestrel-*-x86_64-unknown-linux-gnu.tar.gz -R WyattAu/kestrel
```

Status: implementation is **ahead of the roadmap** — core storage/parsing,
sync, TUI and GUI code all exist (see the [roadmap](docs/roadmap.md) status
column for what is wired vs. outstanding, and ADRs 0011–0014 for the crates
added beyond the v1 spec).

## Documentation

| Doc | Purpose |
|-----|---------|
| [requirements.md](requirements.md) | Specification (authoritative) |
| [docs/architecture.md](docs/architecture.md) | Crate graph, concurrency model, data flow |
| [docs/message-protocol.md](docs/message-protocol.md) | Frozen core ↔ frontend contract |
| [docs/schema.md](docs/schema.md) | SQLite/Tantivy/blob-store persistence design |
| [docs/threat-model.md](docs/threat-model.md) | Security analysis & mitigation matrix |
| [docs/error-taxonomy.md](docs/error-taxonomy.md) | Error kinds & recovery classes |
| [docs/engineering-standards.md](docs/engineering-standards.md) | The bar for code, review, CI |
| [docs/adr/](docs/adr/) | Architecture Decision Records (binding) |
| [docs/roadmap.md](docs/roadmap.md) | Phase overview (tasks live on GitHub) |
| [docs/sync-engine.md](docs/sync-engine.md) · [docs/testing-strategy.md](docs/testing-strategy.md) | Design docs (phase-gated) |

## Build

Requires a pinned Rust toolchain (see `rust-toolchain.toml`), a C compiler,
and `pkg-config`. Docker is needed only for integration tests.

```bash
cargo build --workspace
cargo nextest run --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo +nightly fmt --all --check
```

## Provider Support

Kestrel supports 20+ email providers with auto-detection:

| Provider | IMAP | SMTP | OAuth2 | Status |
|----------|------|------|--------|--------|
| Gmail | ✅ | ✅ | ✅ | Validated |
| Outlook | ✅ | ✅ | ✅ | Ready |
| Yahoo | ✅ | ✅ | ✅ | Ready |
| iCloud | ✅ | ✅ | ❌ | Ready |
| ... | ... | ... | ... | ... |

See `docs/provider-compatibility.md` for the full matrix.

## Search stack

Kestrel uses [tantivy](https://github.com/quickwit-oss/tantivy) **0.26** directly
(`crates/kestrel-storage`): fixed mail schema (stemmed/raw text, u64 folder/account
facets, i64 date fast field), single-writer batched commits, and fast-field-ordered
hits. This is deliberately version-aligned with
[tantivy-helper 0.2](https://crates.io/crates/tantivy-helper) (also tantivy 0.26).
Migrating kestrel onto tantivy-helper's `SearchEngine`/`QueryBuilder` was assessed
and rejected for now — its API does not yet express fast-field ordering, custom
per-field tokenizers, range/fuzzy-across-fields queries, or writer heap/commit
batching control. Migration is future work pending helper API growth.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) (workflow) and
[docs/engineering-standards.md](docs/engineering-standards.md) (the bar).
Security reports: [SECURITY.md](SECURITY.md).
