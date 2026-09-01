-- Snooze table for deferred message visibility (cache.db, rebuildable per ADR 0009).
CREATE TABLE snooze (
    id           TEXT NOT NULL PRIMARY KEY,  -- SnoozeId (uuid v7)
    message_id   TEXT NOT NULL,
    account_id   TEXT NOT NULL,
    folder_id    TEXT NOT NULL,
    snoozed_until INTEGER NOT NULL,          -- unix ms
    created_at   INTEGER NOT NULL            -- unix ms
);
CREATE INDEX idx_snooze_until ON snooze(snoozed_until);
