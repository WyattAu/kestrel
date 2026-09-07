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

- **Sync trigger:** per-account IMAP/JMAP services run autonomously;
  `Command::TriggerSync` is a documented no-op until wiring lands (issue #1).
  Startup IMAP sync *resume* for stored accounts and the ordered
  `EngineHandle::shutdown` path (bounded outbox flush, storage checkpoint)
  are implemented.
- **Phase 4 security matrix:** sanitizer/link structural tests are in place;
  the webview *network-isolation* runtime test (`unshare -n`) described in
  `docs/testing-strategy.md` §5 is not yet implemented (issue #6).
- **SLA gates:** the CI bench job enforces absolute SLA thresholds; the
  baseline-comparison (> 10 % regression) gate and pinned-runner settings from
  `docs/engineering-standards.md` §5 are not yet wired into CI (issue #8).

Issue numbers refer to the GitHub backlog (phase/epic labels). The
`protocol_surface_matches_documentation` test in `kestrel-core` guards
`docs/message-protocol.md` drift at compile time.

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
