-- Per-user marketplace plugin installs (v0.36). Every row is UID-scoped.
-- Secrets are not stored here: they live in the per-user secrets store.

BEGIN;

CREATE TABLE IF NOT EXISTS marketplace_installs (
    user_id TEXT NOT NULL REFERENCES users(id),
    slug TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    installed_version TEXT NOT NULL,
    installed_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, slug)
);

COMMIT;
