-- Which client started a turn (0.67, Usage insights): desktop, mobile or
-- api. Telegram, WhatsApp and scheduled turns are told apart by their
-- conversation's origin instead. UID-scoped; no content.

BEGIN;

CREATE TABLE IF NOT EXISTS client_turns (
    task_id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    client TEXT NOT NULL CHECK (client IN ('desktop', 'mobile', 'api')),
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS client_turns_user ON client_turns(user_id, created_at);

COMMIT;
