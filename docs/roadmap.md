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
| **1 — Core storage & parsing** | `phase-1` | **In progress** | `kestrel-core` types & protocol types, SQLite schema + migrations (ADR 0003, `docs/schema.md`), `MimeParser` adapter (ADR 0002), Tantivy indexing pipeline, threading, blob CAS + GC | Ingestion benchmark ≥ 800 msgs/sec (target 1,500); fuzz corpus green; schema + parser crates reviewed against threat model §4 |
| **2 — Sync engine** | `phase-2` | Planned | IMAP `FETCH`/`IDLE`/`STORE` via `imap-flow` (ADR 0005), QRESYNC/CONDSTORE deltas, `UIDVALIDITY` reconciliation, SMTP sender, OAuth2 loopback + PKCE, outbox with backoff, credential service (`docs/sync-engine.md`) | Offline-first flows pass integration suite (Dovecot/Greenmail); outbox survives restart; token refresh unattended for 7 days |
| **3 — TUI MVP** | `phase-3` | Planned | `kestrel-tui`: 3-pane + focus mode, vi keys, OSC 8, `$EDITOR` compose, Markdown → `multipart/alternative`, fuzzy search | Cold start < 50 ms; full read/ reply/ archive loop usable daily; memory < 25 MB idle |
| **4 — GUI MVP** | `phase-4` | Planned | `kestrel-gui`: Slint shell (ADR 0001), sandboxed `wry` viewport + `kestrel-cid://`, composer, tray, notifications, theme | Threat-model §7 webview test matrix green; cold start < 200 ms; CSP verified on every load |
| **5 — Hardening** | `phase-5` | Planned | Broken-MIME stress corpora, JMAP (RFC 8620/8621), OpenPGP via Sequoia (sign/encrypt), performance polish to SLA targets | All SLA benchmarks at target (not just hard limit); JMAP account E2E; PGP round-trip interop tests |

## Definition of "phase done"

1. All child issues closed; exit criteria above verified in CI.
2. `docs/` updated to match reality (protocol, schema, threat model).
3. A retrospective issue filed with adjustments to standards/ADRs.

## Deferred by design

- Multiple profiles/identities UI beyond account list
- Plugins/extension API (needs its own ADR; protocol §7 keeps the seam)
- Telemetry of any kind (threat model §6)
