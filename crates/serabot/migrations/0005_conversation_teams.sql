CREATE TABLE IF NOT EXISTS conversation_teams (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
    members TEXT NOT NULL,
    mode TEXT NOT NULL DEFAULT 'selected',
    updated_at TEXT NOT NULL
);