-- data.db initial schema (docs/schema.md §3.1, ADR 0009: durable records,
-- never wiped by migrations).

CREATE TABLE accounts (
    id           TEXT NOT NULL PRIMARY KEY,             -- AccountId (uuid v7)
    name         TEXT NOT NULL,
    email        TEXT NOT NULL,
    provider     TEXT NOT NULL,                -- generic|gmail|outlook|fastmail|jmap
    protocol     TEXT NOT NULL DEFAULT 'imap', -- imap|jmap
    auth_kind    TEXT NOT NULL,                -- password|oauth2
    sync_state   TEXT NOT NULL DEFAULT 'disconnected',
    created_at   INTEGER NOT NULL,             -- unix ms
    updated_at   INTEGER NOT NULL
);

-- JWZ-lite threading roots (schema.md §3.4).
CREATE TABLE threads (
    id           TEXT NOT NULL PRIMARY KEY,             -- ThreadId (uuid v7)
    thread_key   TEXT NOT NULL UNIQUE,         -- threading algorithm key
    subject_norm TEXT NOT NULL,                -- lowercased, prefixes stripped
    first_seen   INTEGER NOT NULL              -- unix ms
);
CREATE INDEX idx_threads_subject ON threads(subject_norm);

CREATE TABLE outbox (
    id               TEXT NOT NULL PRIMARY KEY,          -- OutboxId
    account_id       TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    raw_rfc822_blob  TEXT NOT NULL,             -- sha256 into blob CAS
    raw_inline       BLOB,                      -- durability copy (< 64 KiB)
    envelope         TEXT NOT NULL,             -- JSON: from/to/cc/bcc/subject for UI
    retry_count      INTEGER NOT NULL DEFAULT 0,
    next_attempt_at  INTEGER,                   -- null = due now
    last_error       TEXT,
    created_at       INTEGER NOT NULL,          -- unix ms
    sent_at          INTEGER                    -- null while queued
);
CREATE INDEX idx_outbox_due ON outbox(next_attempt_at) WHERE sent_at IS NULL;

CREATE TABLE settings (
    key   TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
);
