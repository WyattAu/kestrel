# Kestrel

A high-performance, modular email client in Rust with two frontends sharing
one core engine:

- **`kestrel-tui`** — keyboard-driven terminal client (ratatui)
- **`kestrel-gui`** — native desktop client (Slint shell + sandboxed webview
  for HTML mail)

Offline-first, IMAP/JMAP + SMTP, SQLite + Tantivy full-text search,
zero-trust HTML rendering. Licensed under [Apache-2.0](LICENSE).

Status: **Phase 1 in progress** — see the [roadmap](docs/roadmap.md).

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

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) (workflow) and
[docs/engineering-standards.md](docs/engineering-standards.md) (the bar).
Security reports: [SECURITY.md](SECURITY.md).
