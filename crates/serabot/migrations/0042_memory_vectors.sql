-- Semantic memory recall (0.62). One local embedding per memory, written by
-- the same multilingual E5 model as document retrieval. A row is stale when
-- its content hash or model no longer matches and is rebuilt lazily.
-- UID-scoped: every read filters on user_id; rows go with their memory.

BEGIN;

CREATE TABLE IF NOT EXISTS memory_vectors (
    memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    model TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    vector BLOB NOT NULL
);

CREATE INDEX IF NOT EXISTS memory_vectors_user ON memory_vectors(user_id);

COMMIT;
