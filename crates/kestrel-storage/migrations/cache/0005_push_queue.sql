-- Server-push queue (phase-3 daily-loop gate): flag/move mutations that
-- must be applied to the IMAP server (STORE/MOVE), drained at the start of
-- each sync cycle. Ephemeral (cache.db); rebuildable by design.
--
-- Distinct from `pending_ops` (0003): that journal replays *local* mutations
-- after offline mode; this queue exists because the local apply alone never
-- reaches the server — without it a locally archived message would
-- reappear on the next delta sync (server state is authoritative).

CREATE TABLE IF NOT EXISTS push_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id TEXT NOT NULL,
    op_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    retry_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT
);
CREATE INDEX IF NOT EXISTS idx_push_queue_account ON push_queue(account_id);
