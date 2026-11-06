-- Bot and group profiles, and the bot onboarding interview (0.15).
--
-- `bot_avatars` and `conversation_profiles.avatar_*` hold the already
-- re-encoded 256x256 image (the server decodes the upload and re-encodes it, so
-- no original bytes or metadata are kept). `conversation_profiles.description`
-- is the optional group blurb. `bot_onboarding` is the small state machine
-- behind the "tell me what you need" interview that runs right after a bot is
-- created: the transcript and any pending proposal are stored as JSON.
-- Every row is scoped by user_id; there is no unscoped read path.

BEGIN;

CREATE TABLE IF NOT EXISTS bot_avatars (
    user_id TEXT NOT NULL REFERENCES users(id),
    bot TEXT NOT NULL,
    mime TEXT NOT NULL,
    data BLOB NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, bot)
);

CREATE TABLE IF NOT EXISTS conversation_profiles (
    user_id TEXT NOT NULL REFERENCES users(id),
    conversation_id TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    avatar_mime TEXT,
    avatar_data BLOB,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, conversation_id)
);

CREATE TABLE IF NOT EXISTS bot_onboarding (
    user_id TEXT NOT NULL REFERENCES users(id),
    bot TEXT NOT NULL,
    state TEXT NOT NULL,
    transcript_json TEXT NOT NULL DEFAULT '[]',
    proposal_json TEXT,
    rounds INTEGER NOT NULL DEFAULT 0,
    group_id TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, bot)
);

COMMIT;
