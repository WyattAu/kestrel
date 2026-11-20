# Pattern adoption

Declared per references/adoption-format.md; IDs resolve against
references/catalog.md in pattern-standards.

| Pattern | Status | Evidence |
|---|---|---|
| T13 | adopted | docs/message-protocol.md:6 (frozen core-frontend contract; kestrel-core is the pure engine, tui/gui/mobile are shells) |
| T8 | partial | crates/kestrel-plugin/src/host.rs:4 (manifest-declared capabilities enforced at the host boundary; attenuates ambient authority, no unforgeable handles) |
| T25 | adopted | crates/kestrel-sync/src/sync.rs:1 (offline-first per-account sync state machine over the local store) |
| T32 | partial | crates/kestrel-sync/src/outbox_service.rs:1 (durable outbox rows drained through SMTP with retry and sent events; no CDC half) |
| P64 | adopted | crates/kestrel-core/src/links.rs:232 (proptest suites; weekly soak with fault injection in .github/workflows/soak-weekly.yml:24) |
| P67 | adopted | fuzz/fuzz_targets/fuzz_mime_adapter.rs:1 (parser fuzz targets over MIME, IMAP, and HTML paths; weekly run in .github/workflows/fuzz-weekly.yml) |
| P72 | adopted | crates/kestrel-core/src/error.rs:17 (KestrelError with RecoveryClass taxonomy; docs/error-taxonomy.md) |
