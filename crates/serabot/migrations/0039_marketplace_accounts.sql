-- Per-user marketplace accounts and the native GitHub connector (v0.51).
-- Every row is UID-scoped. No secret is stored here: OAuth tokens and the
-- client registration are sealed in `user_credentials` under the reserved
-- provider id `marketplace.<slug>.<account>.oauth`, and the GitHub connector's
-- tokens under `connector:github`.

BEGIN;

CREATE TABLE IF NOT EXISTS marketplace_accounts (
    user_id TEXT NOT NULL REFERENCES users(id),
    slug TEXT NOT NULL,
    account_id TEXT NOT NULL,
    label TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'needs_sign_in'
        CHECK (state IN ('needs_setup', 'needs_sign_in', 'ready', 'unreachable')),
    last_error TEXT,
    last_ok_at TEXT,
    created_at TEXT NOT NULL,
    PRIMARY KEY (user_id, slug, account_id)
);

CREATE TABLE IF NOT EXISTS github_connector (
    user_id TEXT PRIMARY KEY REFERENCES users(id),
    login TEXT NOT NULL,
    gh_user_id INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'connected'
        CHECK (state IN ('connected', 'expired', 'needs_sign_in')),
    access_expires_at TEXT,
    refresh_expires_at TEXT,
    updated_at TEXT NOT NULL
);

COMMIT;
