-- Per-user Telegram linking (phase 4 of the multi-user reconstruction).
--
-- The server keeps one bot token (age store, admin-managed). A member asks
-- their console for a short-lived linking code and sends `/start <code>` to
-- the bot; the polling loop resolves the code to that member and records the
-- binding here. An inbound message whose (telegram_user_id, telegram_chat_id)
-- pair has an active row acts as the bound user; every other chat is ignored,
-- exactly as the single-operator build ignored non-owner chats.
--
-- `revoked_at` is a soft revoke so the audit trail keeps the pair. A revoked
-- pair can be re-linked by the same user; the UNIQUE constraint keeps a pair
-- from ever being claimed by a second account without an explicit unbind.
--
-- The operator's configured owner pair is *not* backfilled here: the owner
-- ids live in config, not the database. `TelegramClient::bootstrap_admin_binding`
-- inserts it for the admin idempotently at startup when no binding exists.

BEGIN;

CREATE TABLE IF NOT EXISTS user_telegram (
    user_id TEXT PRIMARY KEY REFERENCES users(id),
    telegram_user_id INTEGER NOT NULL,
    telegram_chat_id INTEGER NOT NULL,
    linked_at TEXT NOT NULL,
    revoked_at TEXT,
    UNIQUE(telegram_user_id, telegram_chat_id)
);

COMMIT;
