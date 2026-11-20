-- Alerts (0.68): where each member receives SeraBot's own alerts. Telegram
-- uses the member's linked chat; WhatsApp only the one allowed chat the
-- member picked. UID-scoped; no content.

BEGIN;

CREATE TABLE IF NOT EXISTS alert_settings (
    user_id TEXT PRIMARY KEY REFERENCES users(id),
    telegram INTEGER NOT NULL DEFAULT 1,
    whatsapp_conversation_id TEXT,
    updated_at TEXT NOT NULL
);

COMMIT;
