# Kestrel Storage Schema & Persistence Design

Status: **v1.0** · Implements `requirements.md` §3 · Access layer per ADR 0003,
layout per `docs/architecture.md` §5.

---

## 1. On-Disk Layout (XDG)

| Path | Contents | Durability class |
|------|----------|------------------|
| `$XDG_CONFIG_HOME/kestrel/config.toml` | user configuration (ADR 0006) | durable |
| `$XDG_CACHE_HOME/kestrel/cache.db` (+ `-wal`, `-shm`) | syncable metadata: `folders`, `messages`, `parts` | **rebuildable** — full resync recovers it |
| `$XDG_DATA_HOME/kestrel/data.db` | durable records: `accounts`, `outbox`, `threads`, `settings`, sync cursors | durable |
| `$XDG_DATA_HOME/kestrel/blobs/ab/cd/<sha256>` | content-addressed raw bodies & attachments (§4) | durable |
| `$XDG_DATA_HOME/kestrel/index/` | Tantivy full-text index | **rebuildable** from cache.db + blobs |

> **Resolved by ADR 0009:** requirements place SQLite at `XDG_CACHE_HOME`,
> but the `outbox` (unsent mail) must survive a cache wipe. The split above
> (cache.db vs data.db) is the accepted resolution: syncable metadata lives
> in the wipeable cache; anything not re-fetchable from a server lives in
> `data.db`. Both honor the required pragmas and both are sqlx-migrated.

## 2. SQLite Pragmas (both databases)

```sql
PRAGMA journal_mode = WAL;        -- mandated (§3.1)
PRAGMA synchronous = NORMAL;      -- mandated (§3.1)
PRAGMA foreign_keys = ON;         -- mandated (§3.1)
PRAGMA busy_timeout = 5000;
PRAGMA wal_autocheckpoint = 1000; -- keep WAL bounded
```

Single-writer rule: each database is written only by its owning service task
(StorageService); readers use pooled read connections (never block on the
writer under WAL).

## 3. Schema (DDL)

### 3.1 Durable records — `data.db`

```sql
CREATE TABLE accounts (
    id           TEXT PRIMARY KEY,           -- AccountId (uuid v7, text)
    name         TEXT NOT NULL,
    email        TEXT NOT NULL,
    provider     TEXT NOT NULL,              -- generic|gmail|outlook|fastmail|jmap
    protocol     TEXT NOT NULL DEFAULT 'imap', -- imap|jmap
    auth_kind    TEXT NOT NULL,              -- password|oauth2
    host         TEXT NOT NULL DEFAULT '',   -- IMAP/JMAP server hostname
    sync_state   TEXT NOT NULL DEFAULT 'disconnected', -- mirror of ConnectionState
    created_at   INTEGER NOT NULL,           -- unix ms
    updated_at   INTEGER NOT NULL
);

CREATE TABLE threads (               -- JWZ-lite threading roots
    id           TEXT PRIMARY KEY,
    thread_key   TEXT NOT NULL UNIQUE, -- deterministic dedup key
    subject_norm TEXT NOT NULL,       -- lowercased, 're:'/'fwd:' stripped
    first_seen   INTEGER NOT NULL
);
CREATE INDEX idx_threads_subject ON threads(subject_norm);

CREATE TABLE outbox (
    id               TEXT PRIMARY KEY,          -- OutboxId
    account_id       TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    raw_rfc822_blob  TEXT NOT NULL,             -- sha256 into blob CAS (see note)
    envelope         TEXT NOT NULL,             -- JSON: from/to/cc/bcc/subject for UI
    retry_count      INTEGER NOT NULL DEFAULT 0,
    next_attempt_at  INTEGER,                   -- null = due now
    last_error       TEXT,
    created_at       INTEGER NOT NULL,
    sent_at          INTEGER                    -- null while queued
);
CREATE INDEX idx_outbox_due ON outbox(next_attempt_at) WHERE sent_at IS NULL;

CREATE TABLE settings (              -- engine-level key/value (UI prefs stay in config.toml)
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

> `raw_rfc822_blob` stores the **SHA-256 hash** of the raw message in the CAS,
> not inline bytes — requirements §3.2 forbids large blobs in SQLite; the
> column name from §3.1 is preserved, its type reinterpreted as a reference.
> Outbox drafts below 64 KiB MAY additionally be inlined in a
> `raw_inline BLOB` column as a durability belt-and-suspenders for cache-less
> recovery; the CAS copy remains authoritative.

### 3.2 Syncable metadata — `cache.db`

```sql
CREATE TABLE folders (
    id             TEXT PRIMARY KEY,           -- FolderId
    account_id     TEXT NOT NULL,              -- FK into data.db (enforced in code;
    remote_name    TEXT NOT NULL,              --  cross-db: no SQL FK possible)
    attributes     TEXT NOT NULL,              -- JSON array: \HasNoChildren etc.
    role           TEXT,                       -- inbox|sent|drafts|trash|archive|junk|null
    delimiter      TEXT NOT NULL DEFAULT '/',
    uid_validity   INTEGER NOT NULL DEFAULT 0, -- IMAP UIDVALIDITY (uint32)
    highest_modseq INTEGER NOT NULL DEFAULT 0, -- CONDSTORE/QRESYNC cursor
    last_seen      INTEGER NOT NULL,           -- discovery epoch
    UNIQUE(account_id, remote_name)
);

CREATE TABLE messages (
    id              TEXT PRIMARY KEY,          -- MessageId
    folder_id       TEXT NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    uid             INTEGER NOT NULL,          -- IMAP UID (uint32)
    internal_date   INTEGER NOT NULL,          -- unix ms
    flags           TEXT NOT NULL,             -- JSON array; source of truth = server
    message_id      TEXT,                      -- RFC 5322 Message-ID (normalized <>)
    in_reply_to     TEXT,
    subject         TEXT,
    from_addr       TEXT,                      -- canonical JSON: [{name?, email}]
    to_addrs        TEXT NOT NULL DEFAULT '[]',
    cc_addrs        TEXT NOT NULL DEFAULT '[]',
    size            INTEGER NOT NULL,
    is_read         INTEGER NOT NULL DEFAULT 0,
    is_flagged      INTEGER NOT NULL DEFAULT 0,
    is_answered     INTEGER NOT NULL DEFAULT 0,
    has_attachments INTEGER NOT NULL DEFAULT 0,
    thread_id       TEXT NOT NULL,             -- FK into data.db threads (code-enforced)
    raw_blob        TEXT,                      -- sha256 of raw RFC822 in CAS; null if not fetched
    indexed_at      INTEGER,                   -- Tantivy sync cursor; null = pending
    UNIQUE(folder_id, uid)
);
CREATE INDEX idx_messages_folder_date ON messages(folder_id, internal_date DESC);
CREATE INDEX idx_messages_thread      ON messages(thread_id);
CREATE INDEX idx_messages_msgid       ON messages(message_id);
CREATE INDEX idx_messages_pending_idx ON messages(indexed_at) WHERE indexed_at IS NULL;

CREATE TABLE parts (
    id          TEXT PRIMARY KEY,              -- PartId
    message_id  TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,              -- traversal order
    mime_type   TEXT NOT NULL,                 -- lowercased type/subtype
    content_id  TEXT,                          -- Content-ID for cid: resolution
    disposition TEXT,                          -- inline|attachment|null
    filename    TEXT,
    encoding    TEXT,                          -- transfer encoding
    byte_size   INTEGER NOT NULL,
    blob_sha256 TEXT NOT NULL,                 -- CAS reference (content or raw slice)
    UNIQUE(message_id, seq)
);

CREATE TABLE blobs (                 -- CAS registry for GC (§4.3)
    sha256     TEXT PRIMARY KEY,
    byte_size  INTEGER NOT NULL,
    refcount   INTEGER NOT NULL DEFAULT 0,     -- maintained by triggers; GC uses it
    created_at INTEGER NOT NULL,
    last_gc_at INTEGER
);
```

Design notes:

- Addresses are stored as canonical JSON; matching/normalization happens in
  `kestrel-core` types — SQLite comparisons never parse addresses.
- `flags` mirrors the server; optimistic local changes are journaled through
  the sync engine, never edited directly by the UI.
- Cross-database references (`folders.account_id`, `messages.thread_id`) are
  enforced at the StorageService boundary because SQLite cannot express
  foreign keys across files; violations are a `Bug`-class error (ADR 0007).

### 3.4 Threading

`threads` + `messages.thread_id` implement JWZ-lite:

1. Group by normalized `message_id` chain (`in_reply_to` + `References`
   captured at parse time into `in_reply_to`), fall back to
   `subject_norm` grouping within a ± 7-day window.
2. Threading runs inside the ingestion transaction; a message's thread
   assignment is immutable once written (re-threading only on
   `UIDVALIDITY` reconciliation).
3. The algorithm is pure and table-driven → property-tested with generated
   reply graphs (`docs/testing-strategy.md`).

## 4. Blob CAS (Content-Addressed Storage)

### 4.1 Layout & write path

- Path: `$XDG_DATA_HOME/kestrel/blobs/ab/cd/<sha256>` where `abcd` are the
  first four hex chars of the digest (requirements §3.2).
- Write protocol: temp file in `blobs/tmp/` → fsync → verify hash while
  writing → atomic rename to final path → registry row upsert. Crashes leave
  only orphan temp files (swept at startup).

### 4.2 Read path

- Reads go through `StorageService::open_blob(hash)` returning a streaming
  reader; the HTML viewport resolves `cid:` parts via the in-memory protocol
  (threat model §5) — no direct filesystem access from any frontend or the
  webview.

### 4.3 Garbage collection

- `refcount` = number of `parts.blob_sha256` + `messages.raw_blob` +
  `outbox.raw_rfc822_blob` rows referencing the hash (triggers keep it exact).
- Two-phase mark-sweep: after each sync checkpoint, hashes with `refcount = 0`
  get `last_gc_at = now`; a later pass (grace 24 h) unlinks files and registry
  rows. Grace covers in-flight fetches and crash recovery.
- CAS deduplicates identical bodies/attachments across accounts and folders
  for free.

## 5. Tantivy Index Schema (`$XDG_DATA_HOME/kestrel/index/`)

| Field | Type | Options | Notes |
|-------|------|---------|-------|
| `message_id` | Str | `STORED` | join key back to SQLite |
| `account_id` | U64 (faceted) | fast | filter |
| `folder_id` | U64 | fast | filter; folder→u64 map in `settings` |
| `subject` | Text | `STORED`, English stemmer, `TextField` | |
| `body_plain` | Text | not stored | extracted text only, never HTML |
| `from` | Text | tokenized | |
| `from_exact` | Str | raw | exact address match |
| `to`, `cc`, `bcc` | Text | tokenized | `to_exact`/`cc_exact` Str variants |
| `attachment_names` | Text | tokenized | |
| `date` | I64 | fast (range) | internal_date ms |

- Writer owned solely by `IndexService` (single writer); commits batched
  (≤ 1 commit / 500 ms or 500 docs) — commit latency, not indexing, is the
  cost center at the > 1,500 msgs/sec SLA.
- `messages.indexed_at` is the truth for index freshness; on mismatch
  (crash between DB write and index commit) the pending-index cursor drives
  catch-up; index is always **rebuildable** from cache.db + blobs.
- 500k-message / < 30 ms budget is CI-benchmarked (`docs/engineering-standards.md`
  §Benchmarks).

## 6. Migration Policy

- `sqlx migrate` (ADR 0003): forward-only, append-only under
  `kestrel-storage/migrations/cache/` and `kestrel-storage/migrations/data/`;
  every PR adding a migration must state cache-rebuild impact.
- **Breaking migrations** on `cache.db` are permitted and cheap: the engine
  may wipe + resync cache.db (offline metadata). **`data.db` and blobs are
  never wiped by migrations** — destructive `data.db` change requires an ADR
  and an export/import path.
- Offline `.sqlx` query metadata is committed and CI-verified fresh
  (`cargo sqlx prepare --check`).

## 7. Integrity & Recovery

| Failure | Detection | Recovery |
|---------|-----------|----------|
| cache.db corruption | startup quick-check | wipe cache.db, full resync |
| data.db corruption | startup quick-check | fail with user action prompt (backup exists?); outbox export offered |
| Orphan blobs / temp files | startup sweep | mark-sweep GC (§4.3) |
| Tantivy corruption | startup validate | rebuild from cache.db + blobs |
| UIDVALIDITY change | sync engine | purge folder rows, re-thread, re-index (requirements §2.2) |
