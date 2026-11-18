-- WhatsApp linked device (0.58, docs/specs/whatsapp-linked-device.md).
-- One linked account per member. The session keys live in the library's own
-- store file under [whatsapp_link] dir, never here. UID-scoped.

BEGIN;

CREATE TABLE IF NOT EXISTS whatsapp_link_accounts (
    user_id TEXT PRIMARY KEY REFERENCES users(id),
    status TEXT NOT NULL CHECK (status IN ('pairing', 'linked', 'signed_out')),
    phone TEXT,
    allowlist_json TEXT NOT NULL DEFAULT '[]',
    updated_at TEXT NOT NULL
);

-- A WhatsApp chat and the SeraBot conversation that carries it.
CREATE TABLE IF NOT EXISTS whatsapp_link_chats (
    user_id TEXT NOT NULL REFERENCES users(id),
    conversation_id TEXT NOT NULL,
    jid TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (user_id, conversation_id),
    UNIQUE (user_id, jid)
);

COMMIT;
