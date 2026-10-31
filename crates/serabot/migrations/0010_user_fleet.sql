-- Per-user bot fleets and user-created skills (phase 3 of the multi-user
-- reconstruction).
--
-- `bots` re-keys from `name` to `(user_id, name)`: every account owns its own
-- copy of the fleet, seeded from the repository templates (`bots/` +
-- `prompts/`) when the account is created. The system prompt moves into the
-- row (`prompt TEXT`, capped at 100 KB by the API and template loaders) so a
-- user's prompt edits are ordinary scoped database writes.
--
-- Existing `bots` rows backfill to the bootstrap admin (the fixed id written
-- by migration 0008 and mirrored in `kv['admin_user_id']`) so the operator's
-- fleet keeps working. SQL cannot read the prompt files, so the backfilled
-- `prompt` column is the empty string; the startup fleet seeding
-- (`fleet::FleetRegistry::seed`) fills it from the matching repository
-- template on the first load after the upgrade. `config_json` is carried
-- over unchanged and keeps parsing as before.
--
-- `user_skills` holds skills a user created from their own console. The
-- shipped `skills/` directory stays the server-managed catalog; a user's
-- effective catalog is the shipped catalog union their own rows, and bot
-- allowlists resolve inside that union.
--
-- SQLite cannot re-key a table in place, so `bots` is rebuilt with the
-- documented twelve-step procedure (https://sqlite.org/lang_altertable,
-- "Making Other Kinds Of Table Schema Changes"): a new table is created with
-- the final shape, the rows are copied under the admin id, the old table is
-- dropped and the new one renamed. Foreign keys are disabled for the surgery
-- and `legacy_alter_table` is enabled so renames behave as before SQLite
-- 3.25 (pure renames: no reference rewriting, no whole-schema sanity check).
-- Both pragmas are restored at the end.

PRAGMA foreign_keys = OFF;
PRAGMA legacy_alter_table = ON;

BEGIN;

-- bots -----------------------------------------------------------------------
CREATE TABLE bots_new (
    user_id TEXT NOT NULL REFERENCES users(id),
    name TEXT NOT NULL,
    config_json TEXT NOT NULL,
    prompt TEXT NOT NULL,
    enabled INTEGER NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(user_id, name)
);
INSERT INTO bots_new(user_id, name, config_json, prompt, enabled, updated_at)
SELECT '00000000-0000-7000-8000-000000000001', name, config_json, '', enabled, updated_at
FROM bots;
DROP TABLE bots;
ALTER TABLE bots_new RENAME TO bots;
CREATE INDEX idx_bots_user ON bots(user_id);

-- user_skills ----------------------------------------------------------------
CREATE TABLE user_skills (
    user_id TEXT NOT NULL REFERENCES users(id),
    id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(user_id, id)
);

COMMIT;

PRAGMA legacy_alter_table = OFF;
PRAGMA foreign_keys = ON;
