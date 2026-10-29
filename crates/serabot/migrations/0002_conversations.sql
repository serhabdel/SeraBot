CREATE TABLE IF NOT EXISTS conversations (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    parent_id TEXT REFERENCES conversations(id),
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_conversations_parent ON conversations(parent_id);
