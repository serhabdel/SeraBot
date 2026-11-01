-- Every artifact a tool produces inside a conversation, not only the ones that
-- went through an approval. The desktop Library reads this table alongside the
-- gateway `actions` rows so a built document is visible before it is delivered.
CREATE TABLE IF NOT EXISTS conversation_artifacts (
    hash TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    turn_id TEXT,
    tool TEXT NOT NULL,
    filename TEXT NOT NULL,
    title TEXT,
    created_at TEXT NOT NULL,
    PRIMARY KEY (hash, conversation_id)
);

CREATE INDEX IF NOT EXISTS conversation_artifacts_conversation_created
    ON conversation_artifacts (conversation_id, created_at DESC);
