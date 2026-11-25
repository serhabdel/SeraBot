-- Scheduled outgoing emails ("Send Later", 0.74.13).
-- All rows are UID-scoped; the background loop claims due pending rows
-- atomically and sends them through the user's configured mail account.

BEGIN;

CREATE TABLE IF NOT EXISTS scheduled_mail (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    account_label TEXT NOT NULL,
    to_json TEXT NOT NULL,
    cc_json TEXT NOT NULL,
    bcc_json TEXT NOT NULL,
    subject TEXT NOT NULL,
    body TEXT NOT NULL,
    body_html TEXT,
    send_at INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    last_error TEXT,
    created_at INTEGER NOT NULL,
    sent_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_scheduled_mail_due
    ON scheduled_mail(status, send_at);

CREATE INDEX IF NOT EXISTS idx_scheduled_mail_user
    ON scheduled_mail(user_id, send_at);

COMMIT;
