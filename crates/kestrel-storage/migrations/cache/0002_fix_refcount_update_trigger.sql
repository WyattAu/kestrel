-- Fix: messages_refcount_update trigger fires when raw_blob is in the
-- SET clause even if the new value is NULL (e.g. COALESCE(NULL, NULL)
-- on re-ingest without a raw blob). The INSERT must be guarded the same
-- way as messages_refcount_insert/delete.

DROP TRIGGER IF EXISTS messages_refcount_update;

CREATE TRIGGER messages_refcount_update
AFTER UPDATE OF raw_blob ON messages
WHEN NEW.raw_blob IS NOT NULL
BEGIN
    UPDATE blobs SET refcount = refcount - 1
     WHERE sha256 = OLD.raw_blob AND OLD.raw_blob IS NOT NULL;
    INSERT INTO blobs (sha256, byte_size, refcount, created_at)
    VALUES (NEW.raw_blob, 0, 1, unixepoch() * 1000)
    ON CONFLICT(sha256) DO UPDATE SET refcount = refcount + 1;
END;
