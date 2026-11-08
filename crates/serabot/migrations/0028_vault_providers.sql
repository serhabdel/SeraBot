-- Vault sources (0.23): an entry may point at an item in an external password
-- manager instead of holding a sealed value.
--
-- `source` says where the value comes from: `local` (the sealed value in
-- `user_credentials`, the default for every existing row) or a password
-- manager. For a password manager `reference` holds the item pointer
-- (`op://vault/item/field` or `pass://vault/item/field`), never a value. The
-- value is read at fill time and is not stored. Both columns are metadata;
-- no secret is written here.

BEGIN;

ALTER TABLE vault_entries ADD COLUMN source TEXT NOT NULL DEFAULT 'local'
    CHECK (source IN ('local', 'onepassword', 'protonpass'));
ALTER TABLE vault_entries ADD COLUMN reference TEXT NULL;

COMMIT;
