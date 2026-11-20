-- Desktop network relay (0.66, docs/specs/desktop-network-relay.md).
-- Per member: whether chosen provider traffic leaves through their own
-- desktop, and for which providers. UID-scoped; off by default.

BEGIN;

CREATE TABLE IF NOT EXISTS relay_settings (
    user_id TEXT PRIMARY KEY REFERENCES users(id),
    enabled INTEGER NOT NULL DEFAULT 0,
    providers_json TEXT NOT NULL DEFAULT '[]',
    updated_at TEXT NOT NULL
);

COMMIT;
