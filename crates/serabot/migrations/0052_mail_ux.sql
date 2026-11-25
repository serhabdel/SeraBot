-- 0.74.14 mail workspace: send retries and undo-send, snooze, contacts, saved searches.
-- Every row is UID-scoped; store methods always take the acting user's id.

BEGIN;

-- Retry bookkeeping for scheduled sends. `claimed_at` lets a restart find rows
-- that were left in `sending`. `kind` is 'later' (Send later) or 'undo' (the
-- short hold behind Undo send; hidden from the Scheduled folder unless it failed).
ALTER TABLE scheduled_mail ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE scheduled_mail ADD COLUMN claimed_at INTEGER;
ALTER TABLE scheduled_mail ADD COLUMN kind TEXT NOT NULL DEFAULT 'later';

-- A snoozed message stays on the server. It is hidden from its folder list until
-- `wake_at`, then the wake loop deletes the row and marks the message unread.
CREATE TABLE IF NOT EXISTS mail_snooze (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    account_label TEXT NOT NULL DEFAULT '',
    folder TEXT NOT NULL,
    uid INTEGER NOT NULL,
    subject TEXT NOT NULL DEFAULT '',
    from_addr TEXT NOT NULL DEFAULT '',
    wake_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_mail_snooze_msg
    ON mail_snooze(user_id, account_label, folder, uid);
CREATE INDEX IF NOT EXISTS idx_mail_snooze_due
    ON mail_snooze(wake_at);

-- Address book for recipient autocomplete, filled from sent mail and from the
-- senders of listed messages. `uses` counts sends only.
CREATE TABLE IF NOT EXISTS mail_contacts (
    user_id TEXT NOT NULL,
    email TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    uses INTEGER NOT NULL DEFAULT 0,
    last_used INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, email)
);

CREATE TABLE IF NOT EXISTS mail_saved_searches (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    name TEXT NOT NULL,
    query TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_mail_saved_searches_user
    ON mail_saved_searches(user_id, created_at);

COMMIT;
