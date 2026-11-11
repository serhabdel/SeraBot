-- Several chats per bot, and the per-bot memory refresh interval (v0.35.0).
-- Every row is UID-scoped.

BEGIN;

-- An extra chat with a bot. The primary chat `dir:<bot>` needs no row.
CREATE TABLE IF NOT EXISTS bot_chats (
    conversation_id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    bot_id TEXT NOT NULL,
    carried_from TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS bot_chats_user_bot ON bot_chats (user_id, bot_id);

-- How many of the owner's user turns pass between memory refreshes for a bot.
CREATE TABLE IF NOT EXISTS bot_chat_settings (
    user_id TEXT NOT NULL REFERENCES users(id),
    bot_id TEXT NOT NULL,
    refresh_every_turns INTEGER NOT NULL CHECK (refresh_every_turns BETWEEN 5 AND 500),
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, bot_id)
);

COMMIT;
