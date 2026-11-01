-- Multi-user identity foundations: accounts, invitations, token ownership.
--
-- Phase 1 of the multi-user reconstruction. The database stays one SQLite file
-- served by one process: `users` carries the identity the OIDC sign-in
-- upserts, `invites` gates first sign-in, and the existing `device_tokens`
-- rows become the sessions table by gaining the acting user's id. No
-- user-owned data rows are re-keyed yet; that is migration 0009.
--
-- The admin bootstrap is deterministic and config-independent: a fixed id
-- (uuidv7-shaped, but a literal so the migration never depends on config),
-- recorded in `kv` under `admin_user_id` so code can resolve the operator's
-- account without configuration. Existing device tokens backfill to that
-- user, so the operator's existing API and CLI tokens keep working as the
-- admin's sessions without re-issuing anything.
--
-- Timestamps follow the store's RFC3339 millisecond convention
-- (`2026-10-31T12:00:00.000Z`); SQLite's `strftime` emits that directly, so
-- rows written here parse with the same code path as rows written from Rust.

BEGIN;

CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    provider_subject TEXT NOT NULL UNIQUE,
    email TEXT NOT NULL,
    display_name TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('admin','member')),
    status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','disabled')),
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS invites (
    code_hash TEXT PRIMARY KEY,
    uses_left INTEGER NOT NULL,
    expires_at TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES users(id),
    created_at TEXT NOT NULL
);

ALTER TABLE device_tokens ADD COLUMN user_id TEXT REFERENCES users(id);

INSERT INTO users(id, provider_subject, email, display_name, role, status, created_at)
VALUES ('00000000-0000-7000-8000-000000000001', 'local:admin', '', 'Admin', 'admin', 'active',
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

INSERT INTO kv(key, value, updated_at)
VALUES ('admin_user_id', '00000000-0000-7000-8000-000000000001',
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at;

UPDATE device_tokens SET user_id = '00000000-0000-7000-8000-000000000001' WHERE user_id IS NULL;

COMMIT;
