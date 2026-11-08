-- Interactive chat cards (0.23): a question a bot puts to its owner, or a
-- request for a secret that the vault resolves. One row per card.
--
-- Every row is scoped by user_id and every store method takes the acting
-- user's id, so a card id alone never selects another user's card. `payload`
-- holds the bounded card content as JSON. `response` records the owner's
-- reply; a secret request never stores a secret value in it.

BEGIN;

CREATE TABLE IF NOT EXISTS chat_cards (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    conversation_id TEXT NOT NULL,
    bot TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('question', 'secret_request')),
    payload TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'answered', 'declined', 'expired')),
    response TEXT,
    created_at TEXT NOT NULL,
    resolved_at TEXT
);

CREATE INDEX IF NOT EXISTS chat_cards_user_conversation_status
    ON chat_cards (user_id, conversation_id, status);

COMMIT;
