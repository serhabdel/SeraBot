-- Per-user bot vault (0.14): metadata only.
--
-- The secret value itself is never stored here. It lives in
-- `user_credentials` under provider id `vault:<name>`, sealed by the per-user
-- envelope encryption (see `user_secrets.rs`). This table carries what the UI
-- and the `vault_list` tool may show: kind, username, the host allow-list,
-- a note, and timestamps. Rows are scoped by user_id; there is no unscoped
-- read path.

BEGIN;

CREATE TABLE IF NOT EXISTS vault_entries (
    user_id TEXT NOT NULL REFERENCES users(id),
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('password', 'api_key', 'note')),
    username TEXT,
    domains_json TEXT NOT NULL DEFAULT '[]',
    note TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_used_at TEXT,
    PRIMARY KEY (user_id, name)
);

COMMIT;
