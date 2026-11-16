-- Per-user AI provider accounts (v0.49). Every row is UID-scoped.
--
-- A user can hold several accounts per provider (API keys, OAuth sign-ins).
-- No secret is stored here: each account's credential is sealed in
-- `user_credentials` under the reserved provider id `acct:<provider>:<id12>`,
-- bound to the user and that id by the AEAD associated data.
--
-- An existing `(user, provider)` row in `user_credentials` is not touched. It
-- reads as the virtual account `default` and moves into this scheme on its
-- first edit (see `accounts.rs`), so old credentials keep working.
--
-- `fingerprint` is derived from a stable identity (an account id or email, or a
-- keyed hash for API keys), never from a bearer token.

BEGIN;

CREATE TABLE IF NOT EXISTS provider_accounts (
    user_id TEXT NOT NULL REFERENCES users(id),
    provider TEXT NOT NULL,
    account_id TEXT NOT NULL,
    label TEXT NOT NULL,
    auth_kind TEXT NOT NULL CHECK (auth_kind IN ('api_key', 'oauth', 'device', 'minted')),
    fingerprint TEXT NOT NULL,
    identity_json TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'ok'
        CHECK (status IN ('ok', 'needs_reauth', 'rate_limited', 'quota_exhausted', 'disabled')),
    priority INTEGER NOT NULL DEFAULT 0,
    weight INTEGER NOT NULL DEFAULT 1,
    cooldown_until TEXT,
    quota_json TEXT,
    last_error TEXT,
    last_used_at TEXT,
    session_generation INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, provider, account_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_provider_accounts_fp
    ON provider_accounts(user_id, provider, fingerprint);

CREATE TABLE IF NOT EXISTS route_events (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    conversation_id TEXT,
    task_id TEXT,
    bot TEXT,
    from_provider TEXT,
    from_model TEXT,
    from_account TEXT,
    to_provider TEXT,
    to_model TEXT,
    to_account TEXT,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_route_events_conv
    ON route_events(user_id, conversation_id, created_at);

CREATE TABLE IF NOT EXISTS provider_holds (
    user_id TEXT NOT NULL REFERENCES users(id),
    provider TEXT NOT NULL,
    account_id TEXT NOT NULL,
    model TEXT NOT NULL,
    kind TEXT NOT NULL,
    until TEXT NOT NULL,
    reason TEXT NOT NULL,
    PRIMARY KEY (user_id, provider, account_id, model, kind)
);

COMMIT;
