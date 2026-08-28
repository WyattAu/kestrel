-- cache.db initial schema (docs/schema.md §3.2, ADR 0009: rebuildable).
-- Syncable metadata only; durable records live in data.db.

CREATE TABLE folders (
    id             TEXT NOT NULL PRIMARY KEY,           -- FolderId (uuid v7)
    account_id     TEXT NOT NULL,              -- FK into data.db (code-enforced)
    remote_name    TEXT NOT NULL,
    attributes     TEXT NOT NULL,              -- JSON array: \HasNoChildren etc.
    role           TEXT,                       -- inbox|sent|drafts|trash|archive|junk
    delimiter      TEXT NOT NULL DEFAULT '/',
    uid_validity   INTEGER NOT NULL DEFAULT 0, -- IMAP UIDVALIDITY (uint32)
    highest_modseq INTEGER NOT NULL DEFAULT 0, -- CONDSTORE/QRESYNC cursor
    last_seen      INTEGER NOT NULL,           -- discovery epoch (unix ms)
    UNIQUE(account_id, remote_name)
);

CREATE TABLE messages (
    id              TEXT NOT NULL PRIMARY KEY,          -- MessageId
    folder_id       TEXT NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    uid             INTEGER NOT NULL,          -- IMAP UID (uint32)
    internal_date   INTEGER NOT NULL,          -- unix ms
    flags           TEXT NOT NULL,             -- JSON array; server is truth
    message_id      TEXT,                      -- RFC 5322 Message-ID (normalized)
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
    id          TEXT NOT NULL PRIMARY KEY,              -- PartId
    message_id  TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,              -- traversal order
    mime_type   TEXT NOT NULL,                 -- lowercased type/subtype
    content_id  TEXT,                          -- Content-ID for cid: resolution
    disposition TEXT,                          -- inline|attachment|null
    filename    TEXT,
    encoding    TEXT,                          -- transfer encoding
    byte_size   INTEGER NOT NULL,
    blob_sha256 TEXT NOT NULL,                 -- CAS reference
    UNIQUE(message_id, seq)
);

-- CAS registry for GC (docs/schema.md §4.3). refcount covers references
-- from cache.db (parts, messages.raw_blob) via triggers below; data.db
-- (outbox) references are adjusted in code at the StorageService boundary.
CREATE TABLE blobs (
    sha256     TEXT NOT NULL PRIMARY KEY,
    byte_size  INTEGER NOT NULL,
    refcount   INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    last_gc_at INTEGER
);

-- Refcount triggers: parts.
CREATE TRIGGER parts_refcount_insert
AFTER INSERT ON parts BEGIN
    INSERT INTO blobs (sha256, byte_size, refcount, created_at)
    VALUES (NEW.blob_sha256, NEW.byte_size, 1, unixepoch() * 1000)
    ON CONFLICT(sha256) DO UPDATE SET refcount = refcount + 1;
END;
CREATE TRIGGER parts_refcount_delete
AFTER DELETE ON parts BEGIN
    UPDATE blobs SET refcount = refcount - 1 WHERE sha256 = OLD.blob_sha256;
END;

-- Refcount triggers: messages.raw_blob (nullable, may transition).
CREATE TRIGGER messages_refcount_insert
AFTER INSERT ON messages WHEN NEW.raw_blob IS NOT NULL BEGIN
    INSERT INTO blobs (sha256, byte_size, refcount, created_at)
    VALUES (NEW.raw_blob, 0, 1, unixepoch() * 1000)
    ON CONFLICT(sha256) DO UPDATE SET refcount = refcount + 1;
END;
CREATE TRIGGER messages_refcount_delete
AFTER DELETE ON messages WHEN OLD.raw_blob IS NOT NULL BEGIN
    UPDATE blobs SET refcount = refcount - 1 WHERE sha256 = OLD.raw_blob;
END;
CREATE TRIGGER messages_refcount_update
AFTER UPDATE OF raw_blob ON messages BEGIN
    UPDATE blobs SET refcount = refcount - 1
     WHERE sha256 = OLD.raw_blob AND OLD.raw_blob IS NOT NULL;
    INSERT INTO blobs (sha256, byte_size, refcount, created_at)
    VALUES (NEW.raw_blob, 0, 1, unixepoch() * 1000)
    ON CONFLICT(sha256) DO UPDATE SET refcount = refcount + 1;
END;
