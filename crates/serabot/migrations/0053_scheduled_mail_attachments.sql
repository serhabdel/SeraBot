-- 0.74.15: files attached to a scheduled or undo-hold message, by artifact hash.
-- Kept apart from `scheduled_mail` so that table's row shape does not change.
-- Ownership of each hash is checked when the message is queued and again when it is sent.

BEGIN;

CREATE TABLE IF NOT EXISTS scheduled_mail_attachments (
    mail_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    hash TEXT NOT NULL,
    position INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (mail_id, hash)
);

CREATE INDEX IF NOT EXISTS idx_scheduled_mail_attachments_user
    ON scheduled_mail_attachments(user_id, mail_id);

COMMIT;
