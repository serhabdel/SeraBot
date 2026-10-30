ALTER TABLE conversations ADD COLUMN last_read_at TEXT;

ALTER TABLE conversation_teams ADD COLUMN rules TEXT NOT NULL DEFAULT '';
