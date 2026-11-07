-- Chat attachments: files a user uploads from the composer. Stored on disk in
-- the user's own inbox area (<watch_dir>/<user_id>/chat/...); this table is the
-- per-user index. Self-contained: no other table references it.
CREATE TABLE IF NOT EXISTS chat_attachments (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    conversation_id TEXT NOT NULL,
    filename TEXT NOT NULL,
    rel_path TEXT NOT NULL,
    mime TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    turn_id TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS chat_attachments_user_conversation
    ON chat_attachments (user_id, conversation_id, created_at);
CREATE INDEX IF NOT EXISTS chat_attachments_turn
    ON chat_attachments (user_id, turn_id);
