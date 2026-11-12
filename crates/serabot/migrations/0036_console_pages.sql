-- v0.37 console pages: pinned memories, task-sync health, connector checks.
--
-- New columns default so existing rows stay valid. Connector checks store
-- only a fixed, secret-free message from the test endpoint.

BEGIN;

ALTER TABLE memories ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_memories_user_pinned ON memories(user_id, pinned, updated_at);

ALTER TABLE user_task_prefs ADD COLUMN last_sync_at TEXT;
ALTER TABLE user_task_prefs ADD COLUMN last_sync_ok INTEGER;
ALTER TABLE user_task_prefs ADD COLUMN last_sync_error TEXT;

CREATE TABLE IF NOT EXISTS connector_checks (
    user_id TEXT NOT NULL,
    connector_id TEXT NOT NULL,
    ok INTEGER NOT NULL,
    message TEXT NOT NULL,
    checked_at TEXT NOT NULL,
    PRIMARY KEY (user_id, connector_id)
);

COMMIT;
