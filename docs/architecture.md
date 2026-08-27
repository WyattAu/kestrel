# Kestrel Architecture

Status: **v1.0 — binding** · Derived from `requirements.md` · Decisions recorded
as ADRs (see `docs/adr/`).

---

## 1. System Overview

Kestrel is a modular, offline-first email client written in Rust. A single
asynchronous **core engine** owns all protocol, storage, indexing, and crypto
work. Two frontends — a terminal UI (`kestrel-tui`) and a native desktop GUI
(`kestrel-gui`) — are pure clients of the engine. They share no state with it
except through the typed message protocol (`docs/message-protocol.md`).

```
┌─────────────────┐        ┌─────────────────┐
│   kestrel-tui   │        │   kestrel-gui   │
│ ratatui loop    │        │ Slint shell +   │
│ (own thread)    │        │ wry body view   │
└───────┬─────────┘        └────────┬────────┘
        │  Command mpsc            │ Command mpsc
        │  Event broadcast (shared, cloned receivers)
┌───────▼──────────────────────────▼─────────┐
│                CORE ENGINE                  │
│  ┌──────────┐ ┌──────────┐ ┌────────────┐  │
│  │ Account/ │ │  Sync    │ │  Outbox    │  │
│  │ Config   │ │ Services │ │  Service   │  │
│  └──────────┘ └────┬─────┘ └────────────┘  │
│  ┌──────────┐ ┌────▼─────┐ ┌────────────┐  │
│  │Credential│ │ Storage  │ │  Index     │  │
│  │ Service  │ │ Service  │ │ Service    │  │
│  └──────────┘ └──────────┘ └────────────┘  │
└──────┬───────────────┬──────────────┬──────┘
       │               │              │
   IMAP/JMAP        SQLite WAL     Tantivy
   SMTP (rustls)    + blob CAS     full-text
```

## 2. Crate Graph & Dependency Rules

```
kestrel-tui ──┐                    ┌── kestrel-gui
              ├──> kestrel-core <──┤
   kestrel-sync ──> kestrel-core
   kestrel-storage ──> kestrel-core
   kestrel-crypto ──> kestrel-core
```

| Crate | Owns | May depend on |
|-------|------|---------------|
| `kestrel-core` | Domain types, config (ADR 0006), error taxonomy (ADR 0007), message protocol types, `MimeParser` trait (ADR 0002) | external crates only |
| `kestrel-sync` | IMAP/JMAP/SMTP engines, sync state machine, IDLE loops | `kestrel-core` |
| `kestrel-storage` | SQLite (ADR 0003), Tantivy index, blob CAS | `kestrel-core` |
| `kestrel-crypto` | keyring/GPG credential store, OpenPGP (Phase 5), TLS config, SASL/OAuth2 flows | `kestrel-core` |
| `kestrel-tui` | ratatui frontend, `$EDITOR` composition | `kestrel-core` |
| `kestrel-gui` | Slint shell (ADR 0001), wry viewport, tray/notifications | `kestrel-core` |

**Binding rules** (enforced in review, see `docs/engineering-standards.md`):

1. Core crates never import frontend crates; frontends never import each
   other's crates; neither frontend is imported by anything.
2. `kestrel-core` is dependency-light: no UI, no async runtime beyond trait
   bounds, no storage backends. It is the vocabulary, not the engine.
3. Lateral communication between `kestrel-sync`/`kestrel-storage`/
   `kestrel-crypto` happens **only** through the core's service protocol at
   runtime — no direct crate-level calls between them.
4. Any rule exception requires an ADR.

## 3. Execution & Concurrency Model

Runtime: **tokio multi-threaded scheduler**. One process hosts the engine and
the frontend (both binaries; see §7).

### 3.1 Threads & tasks

- **UI thread (frontend-owned):** the ratatui event loop / Slint render loop.
  It performs **no** blocking calls and **no** awaits on service futures with
  unbounded latency; it polls a filled mailbox or receives on a dedicated
  channel and repaints.
- **Core supervisor task:** owns service lifecycle (start, health, restart,
  shutdown) per ADR 0004.
- **One service task per responsibility:** `SyncService` (per account),
  `StorageService`, `IndexService`, `SearchService`, `OutboxService`,
  `CredentialService`, `ConfigWatcher`.
- **Blocking-pool usage only** for genuinely blocking FFI (keyring, webview
  glue) via `tokio::task::spawn_blocking`.

### 3.2 Non-blocking guarantee (HFT-style rules)

1. UI render paths may allocate bounded, predictable memory; they may not
   touch locks held across `await`s, perform disk/network I/O, or parse MIME.
2. Every channel crossing is bounded (§4, `message-protocol.md`); backpressure
   is a designed state, not an OOM.
3. Storage writes are batched in transactions; ingestion never holds a lock
   the read path needs (WAL readers never block on the writer).
4. Frame pacing (60/120 FPS scroll SLA) is protected by pre-materialized,
   windowed message-list models owned by the frontend.

### 3.3 Shutdown & crash semantics

- **Graceful shutdown order:** frontends detach → services cancel (CancellationToken)
  → OutboxService performs a final bounded flush (≤ 5 s) → storage checkpoint
  → process exit. SIGINT/SIGTERM trigger the same path.
- **Service crash:** a panicking service task is caught by the supervisor,
  which emits `ServiceDegraded` on the event bus and restarts the service with
  exponential backoff + jitter (cap 5 min). Sync services re-enter the
  Disconnected state and revalidate cache integrity (`UIDVALIDITY` check)
  before resuming.
- **Process crash:** SQLite WAL guarantees consistency; the blob store is
  content-addressed and therefore append-safe; on next start the engine
  performs a cheap integrity pass and a Tantivy `validate`/repair.

## 4. Data Flow Walkthroughs

### 4.1 Inbound sync (IMAP)

```
IDLE wake / interval ─> SyncService: UID FETCH deltas (QRESYNC/CONDSTORE)
  ─> raw RFC822 (lazy blobs) ─> blob CAS write (kestrel-storage)
  ─> MimeParser (kestrel-core trait) ─> envelope + part metadata
  ─> StorageService: batched UPSERT (messages, parts, threads)
  ─> IndexService: Tantivy add (body extracted, not stored)
  ─> EventBus: MailArrived / FolderCountsChanged
  ─> frontends update lists/notifications
```

### 4.2 Compose & send

```
Frontend composer ─> Command::ComposeSubmit(draft)
  ─> OutboxService: persist outbox row (raw RFC822 after Markdown ─> MIME build)
  ─> event: OutboxEnqueued
  ─> on connectivity: SMTP submit (kestrel-sync) with rustls
       ├─ success ─> mark sent, move to Sent via APPEND, event MailSent
       └─ failure ─> retry_count++, exponential backoff (jittered), event OutboxRetry
```

### 4.3 Search

```
Frontend ─> Command::Search(query)
  ─> SearchService: parse query ─> Tantivy (facets: account/folder, fast date)
  ─> top-50 IDs + snippets ─> StorageService hydrate envelopes
  ─> Reply::SearchResult
```

## 5. State Ownership

| State | Owner | Location |
|-------|-------|----------|
| Account/config snapshot | `ConfigWatcher` (ADR 0006) | `Arc<Config>` + event bus |
| Connection state machines | `SyncService` (per account) | in-task |
| Sync cursors (`uidvalidity`, `highestmodseq`) | `SyncService`, persisted | SQLite `folders` row |
| Metadata | `StorageService` | SQLite (WAL) |
| Raw bodies/attachments | blob CAS (content-addressed) | `$XDG_DATA_HOME/kestrel/blobs/` |
| Search index | `IndexService` | Tantivy dir |
| Credentials/tokens | `CredentialService` | OS keyring / GPG file |
| UI view state (selection, focus) | frontend only | in-process |

Single-writer rule: SQLite is written by `StorageService` only (one writer
task); Tantivy is written by `IndexService` only.

## 6. Security Architecture (summary)

Detailed in `docs/threat-model.md`. Key invariants:

- HTML bodies render only inside the sandboxed `wry` viewport with the
  mandated CSP, JS disabled, `file://` denied, `cid:` via in-memory protocol.
- Remote content blocked by default; per-sender opt-in is user policy.
- Credentials never in plaintext, never in SQLite, never in logs (ADR 0008
  privacy rules).
- All parsers treat input as hostile: no panics, bounded depth/size (fuzzed
  per `docs/engineering-standards.md`).

## 7. Process & Binary Layout

Two binaries share the engine:

- `kestrel-tui`: engine spawned in-process; terminal restored on exit.
- `kestrel-gui`: engine spawned in-process; Slint event loop owns the main
  thread (OS requirement); engine runs on the shared tokio runtime.

If frontends ever split into separate processes, the message protocol is the
seam: ADR 0004's alternatives cover the IPC migration path.

## 8. Cross-Cutting Conventions

- **Errors:** ADR 0007 taxonomy; services never `unwrap`.
- **Observability:** ADR 0008 span conventions (`account`, `folder`, `uid`).
- **Paths:** XDG per requirements §7; all path resolution via `kestrel-core`
  `Paths` type (testable, overridable for tests).
- **Time:** `kestrel-core` clock abstraction; no direct `SystemTime::now`
  outside it (deterministic tests).
- **IDs:** typed `AccountId`, `FolderId`, `MessageId`, `BlobHash` newtypes —
  no bare `u64`/`String` crossing crate boundaries.
