-- Every artifact a tool produces inside a conversation, not only the ones that
-- went through an approval. The desktop Library reads this table alongside the
-- gateway `actions` rows so a built document is visible before it is delivered.
--
-- Multi-user: an artifact belongs to the user who owns the conversation. The
-- table is user-scoped like every other user-owned row (migration 0009), so a
-- member's Library can never list another member's artifacts.
CREATE TABLE IF NOT EXISTS conversation_artifacts (
    hash TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    turn_id TEXT,
    tool TEXT NOT NULL,
    filename TEXT NOT NULL,
    title TEXT,
    created_at TEXT NOT NULL,
    PRIMARY KEY (hash, conversation_id, user_id)
);

CREATE INDEX IF NOT EXISTS conversation_artifacts_conversation_created
    ON conversation_artifacts (conversation_id, created_at DESC);

CREATE INDEX IF NOT EXISTS conversation_artifacts_user
    ON conversation_artifacts (user_id, created_at DESC);
