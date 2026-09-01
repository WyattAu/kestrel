-- Offline mutation journal (sync-engine.md §6): ops enqueued while offline,
-- replayed FIFO on reconnect. Ephemeral (cache.db); rebuildable.

CREATE TABLE IF NOT EXISTS pending_ops (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id TEXT NOT NULL,
    op_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    retry_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT
);
CREATE INDEX IF NOT EXISTS idx_pending_ops_account ON pending_ops(account_id);
