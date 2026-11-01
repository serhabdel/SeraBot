-- Per-user credential store (phase 4 of the multi-user reconstruction).
--
-- Two secret tiers. The age store keeps server-level secrets (the Telegram
-- bot token, auth/OneDrive app client ids, the existing global connectors).
-- Everything a user owns — OneDrive refresh tokens, AI provider API keys and
-- OAuth tokens — lives here, envelope-encrypted:
--
--   master key (private/master.key, 32 bytes, 0600, never in the database)
--     └─ wraps each user's random 32-byte DEK (`user_keys.wrapped_dek`)
--          └─ seals that user's credential values (`user_credentials`)
--
-- The ciphertext is ChaCha20-Poly1305 with a fresh 12-byte nonce per write.
-- The associated data binds a blob to `user_id|provider`, so a row copied to
-- another user or another provider fails to decrypt instead of silently
-- opening. `wrapped_dek` is bound to `user_id` the same way.
--
-- `provider_grants` is the admin's explicit, revocable permission for another
-- account to use an admin-owned provider. It stores no secret: resolution
-- reads the grantor's credential at call time and the grantee never learns
-- the value or the grantor's identity.
--
-- No rows are backfilled: the startup import copies the operator's existing
-- age-store provider credentials into the admin's rows exactly once (see
-- `providers::import_age_provider_credentials`), and the OneDrive global
-- refresh token likewise (see `onedrive::import_global_refresh_token`).

BEGIN;

CREATE TABLE IF NOT EXISTS user_keys (
    user_id TEXT PRIMARY KEY REFERENCES users(id),
    wrapped_dek BLOB NOT NULL,
    nonce BLOB NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS user_credentials (
    user_id TEXT NOT NULL REFERENCES users(id),
    provider TEXT NOT NULL,
    ciphertext BLOB NOT NULL,
    nonce BLOB NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, provider)
);

CREATE TABLE IF NOT EXISTS provider_grants (
    grantor TEXT NOT NULL REFERENCES users(id),
    grantee TEXT NOT NULL REFERENCES users(id),
    provider TEXT NOT NULL,
    created_at TEXT NOT NULL,
    revoked_at TEXT,
    PRIMARY KEY (grantor, grantee, provider)
);

CREATE INDEX IF NOT EXISTS idx_provider_grants_grantee
    ON provider_grants(grantee, provider);

COMMIT;
