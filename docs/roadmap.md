# Kestrel Roadmap

Status: **v1.0** · Epics and tasks are tracked in **GitHub Issues/Projects**
(see "Project board" below). This file stays a thin overview — do not maintain
task lists here.

---

## Project board

- **Project:** `Kestrel` (GitHub Projects, board view)
- **Views:** `Backlog`, `In progress`, `Per phase` (grouped by milestone)
- **Milestones:** one GitHub milestone per phase below; issues carry a
  `phase:N` label and an `epic:` label.
- **Epics** are tracked issues whose body links child tasks; the templates in
  `.github/ISSUE_TEMPLATE/` enforce structure.

## Phases

| Phase | Milestone | Status | Scope (from `requirements.md` §9) | Exit criteria |
|-------|-----------|--------|-----------------------------------|---------------|
| **1 — Core storage & parsing** | `phase-1` | **In progress — implementation landed; exit gates run in CI** | `kestrel-core` types & protocol types, SQLite schema + migrations (ADR 0003, `docs/schema.md`), `MimeParser` adapter (ADR 0002), Tantivy indexing pipeline, threading, blob CAS + GC | Ingestion benchmark ≥ 800 msgs/sec (target 1,500); fuzz corpus green; schema + parser crates reviewed against threat model §4 |
| **2 — Sync engine** | `phase-2` | **Code present ahead of milestone (see “Known gaps” below)** | IMAP `FETCH`/`IDLE`/`STORE` via `imap-flow`/`imap-next` (ADR 0005/0010), QRESYNC/CONDSTORE deltas, `UIDVALIDITY` reconciliation, SMTP sender, OAuth2 loopback + PKCE, outbox with backoff, credential service (`docs/sync-engine.md`); JMAP sync service also landed early | Offline-first flows pass integration suite (Dovecot/Greenmail); outbox survives restart; token refresh unattended for 7 days |
| **3 — TUI MVP** | `phase-3` | **MVP implemented ahead of milestone** | `kestrel-tui`: 3-pane + focus mode, vi keys, OSC 8, `$EDITOR` compose, Markdown → `multipart/alternative`, fuzzy search | Cold start < 50 ms; full read/ reply/ archive loop usable daily; memory < 25 MB idle |
| **4 — GUI MVP** | `phase-4` | **Shell + viewport implemented; security matrix partial** | `kestrel-gui`: Slint shell (ADR 0001), sandboxed `wry` viewport + `kestrel-cid://`, composer, tray, notifications, theme | Threat-model §7 webview test matrix green; cold start < 200 ms; CSP verified on every load |
| **5 — Hardening** | `phase-5` | **Partial — OpenPGP/S-MIME/JMAP landed early** | Broken-MIME stress corpora, JMAP (RFC 8620/8621), OpenPGP via Sequoia (sign/encrypt), S/MIME (CMS) sign/verify, performance polish to SLA targets | All SLA benchmarks at target (not just hard limit); JMAP account E2E; PGP round-trip interop tests |

## Known gaps (docs vs. shipped code)

Closed this cycle (Wave 0 + CI verification):
- Per-account services are supervised with restart-on-panic
  (`ServiceDegraded`), `Command::TriggerSync` is wired end-to-end
  (per-account `Notify` in the sync services), `RemoveAccount`/shutdown stop
  startup-resumed accounts too, and the three per-account spawn sites are
  unified behind `accounts.rs` (issues #1, #2).
- CI now runs `cargo machete` (unused deps, issue #9) and a
  relative-markdown-link check (`scripts/check-doc-links.sh`, issue #11), and
  the `benches` job enforces the > 10 % baseline regression gate with
  runner pinning via the `BENCH_RUNNER` repository variable (issue #8).

Remaining gaps:
- **Transport TLS posture resolved (ADR 0016):** rustls is the sole
  transport-TLS backend (IMAP/STARTTLS, SMTP, JMAP, CalDAV/CardDAV, OAuth
  endpoints) and every `reqwest::Client` construction pins
  `.use_rustls_tls()` — enforced by `scripts/check-tls-backend.sh` in CI.
  Two documented `openssl-sys` exceptions remain in the dependency tree,
  neither terminating a transport connection: Sequoia's `crypto-openssl`
  primitive backend (ADR 0012) and mailkit's ungated `resend` reqwest
  (workspace `Cargo.toml` comment; expires when mailkit fixes the gate).
- **Phase 4 security matrix:** every §7 row (T1–T7) now has named,
  CI-enforced coverage — including the `unshare -n` webview
  network-isolation runtime test (issue #6, `scripts/webview-netns-test.sh`)
  and a real-binary startup/RSS harness (issue #3,
  `scripts/measure-process-startup.sh`, post-merge job): it times the app's
  exec-to-detect boot and gates the real process's idle RSS against the
  requirements §8 hard limits (TUI 40 MB / GUI 200 MB); the interactive
  cold-start SLA stays with the criterion benches. The coverage-gate
  decision (issue #10) and the §7 matrix rows are enforced by
  `scripts/check-threat-matrix.sh` in the `test` job.

Issue numbers refer to the GitHub backlog (phase/epic labels). The
`protocol_surface_matches_documentation` test in `kestrel-core` guards
`docs/message-protocol.md` drift at compile time.

Phase-2 status note (post v0.1.0): all three sync exit criteria now have
docker-gated integration coverage in `crates/kestrel-sync/tests/exit_gates.rs`
— outbox restart-survival (#21), `UIDVALIDITY` reconciliation (#22), and
QRESYNC/CONDSTORE deltas (#23) — and the full integration suite (18 tests)
is green. Writing them surfaced and fixed three production bugs: `ENABLE
CONDSTORE/QRESYNC` was never sent (the delta path was dead code), flag
deltas were emitted but never persisted, and `update_sync_cursors`
clobbered stored `UIDVALIDITY` to 0, disabling reconciliation. Remaining
for the phase-2 exit: the 7-day unattended token-refresh soak (#25) and the
non-CONDSTORE flag-pass fallback (#24).

## Definition of "phase done"

1. All child issues closed; exit criteria above verified in CI.
2. `docs/` updated to match reality (protocol, schema, threat model).
3. A retrospective issue filed with adjustments to standards/ADRs.

## Deferred by design

- Multiple profiles/identities UI beyond account list
- Plugin **host wiring** (loading sandboxed WASM plugins into the engine):
  ADR 0014 is accepted and the `kestrel-plugin` crate ships manifest +
  capability model + wasmtime runtime, but engine integration is deferred
  until plugin host APIs are stable.
- Telemetry of any kind (threat model §6)
