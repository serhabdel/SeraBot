-- Memory v2 (0.19): hybrid recall, provenance, supersession, session summaries.
--
-- Existing rows keep their meaning: they are explicit, active memories with
-- importance 0.5. `content_hash` (SHA-256 of the normalized content) cannot be
-- computed in SQL, so the store fills it for existing rows right after this
-- batch (`memory::backfill_content_hashes`).
--
-- `memories_fts` is an external-content FTS5 index over `memories.content`.
-- Its rowids are the memories' implicit rowids, which VACUUM may renumber, so
-- the index is rebuilt on every open (`memory::rebuild_fts`), not only here.
--
-- Every new column defaults so the old single-operator writers keep working.
-- `status = 'pending'` is reserved for a review queue; nothing writes it yet.

BEGIN;

ALTER TABLE memories ADD COLUMN bot TEXT;
ALTER TABLE memories ADD COLUMN conversation_id TEXT;
ALTER TABLE memories ADD COLUMN origin TEXT NOT NULL DEFAULT 'explicit';
ALTER TABLE memories ADD COLUMN status TEXT NOT NULL DEFAULT 'active';
ALTER TABLE memories ADD COLUMN superseded_by TEXT;
ALTER TABLE memories ADD COLUMN importance REAL NOT NULL DEFAULT 0.5;
ALTER TABLE memories ADD COLUMN last_recalled_at TEXT;
ALTER TABLE memories ADD COLUMN recall_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memories ADD COLUMN content_hash TEXT;

CREATE INDEX idx_memories_user_status ON memories(user_id, status, updated_at);
CREATE INDEX idx_memories_user_hash ON memories(user_id, content_hash);

CREATE VIRTUAL TABLE memories_fts USING fts5(
    content,
    content='memories',
    content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER memories_fts_ai AFTER INSERT ON memories BEGIN
    INSERT INTO memories_fts(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER memories_fts_ad AFTER DELETE ON memories BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, content)
    VALUES ('delete', old.rowid, old.content);
END;

CREATE TRIGGER memories_fts_au AFTER UPDATE OF content ON memories BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, content)
    VALUES ('delete', old.rowid, old.content);
    INSERT INTO memories_fts(rowid, content) VALUES (new.rowid, new.content);
END;

INSERT INTO memories_fts(memories_fts) VALUES ('rebuild');

-- Rolling conversation summaries. `through_message_id` is the last message
-- folded into the summary; the next summary starts after it.
CREATE TABLE session_summaries (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    conversation_id TEXT NOT NULL,
    bot TEXT,
    summary TEXT NOT NULL,
    through_message_id TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_session_summaries_conversation
    ON session_summaries(user_id, conversation_id, created_at);

COMMIT;
