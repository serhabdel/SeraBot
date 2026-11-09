-- Per-user watcher source overlay (0.30.0, Big Wave B3) and reliability
-- enqueue ledger (B4). Every row is UID-scoped.

BEGIN;

CREATE TABLE IF NOT EXISTS user_sources (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    name TEXT NOT NULL,
    origin TEXT NOT NULL CHECK (origin IN ('overlay', 'member')),
    url TEXT,
    kind TEXT NOT NULL DEFAULT 'rss' CHECK (kind IN ('html', 'rss')),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(user_id, name)
);

CREATE INDEX IF NOT EXISTS user_sources_user ON user_sources (user_id, name);

CREATE TABLE IF NOT EXISTS reliability_enqueues (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    enqueued_at TEXT NOT NULL,
    trigger_source TEXT
);

CREATE INDEX IF NOT EXISTS reliability_enqueues_user_day
    ON reliability_enqueues (user_id, enqueued_at);

COMMIT;
