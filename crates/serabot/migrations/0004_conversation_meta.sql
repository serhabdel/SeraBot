BEGIN;
ALTER TABLE conversations ADD COLUMN title TEXT;
ALTER TABLE conversations ADD COLUMN updated_at TEXT;
ALTER TABLE conversations ADD COLUMN archived_at TEXT;
UPDATE conversations SET updated_at = created_at WHERE updated_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_conversations_archived ON conversations(archived_at);
COMMIT;
