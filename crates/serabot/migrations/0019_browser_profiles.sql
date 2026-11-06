-- Named per-user browser profiles and per-bot settings (0.16).
--
-- A browser profile is a Chrome user-data directory
-- `<browser base>/<user_id>/<profile_id>`. Every user has an implicit
-- `default` profile (row created on first use) that cannot be deleted.
-- `bot_settings` holds the owner-editable bot fields that are not part of the
-- bot's runtime config: display name, description and the assigned browser
-- profile (NULL = default). Deleting a profile resets the assignments to NULL.
-- Every row is scoped by user_id; there is no unscoped read path.

BEGIN;

CREATE TABLE IF NOT EXISTS browser_profiles (
    user_id TEXT NOT NULL REFERENCES users(id),
    id TEXT NOT NULL,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_used_at TEXT,
    PRIMARY KEY (user_id, id)
);

CREATE TABLE IF NOT EXISTS bot_settings (
    user_id TEXT NOT NULL REFERENCES users(id),
    bot TEXT NOT NULL,
    display_name TEXT,
    description TEXT,
    browser_profile TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, bot)
);

COMMIT;
