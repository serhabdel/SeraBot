-- Background media jobs (0.24): a provider render (image, video or audio) that
-- outlives the tool call that started it.
--
-- A row is created only after the provider accepted the work and returned an
-- external id. A job is never submitted again; the background loop only polls
-- it. `status` moves once from pending to done, failed or expired.
-- `artifact_hash` points at the artifact store; `error` is a short, truncated
-- reason. Every row is scoped by user_id, and every store method takes the
-- acting user's id.

BEGIN;

CREATE TABLE IF NOT EXISTS media_jobs (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    conversation_id TEXT NOT NULL,
    bot TEXT NOT NULL,
    provider TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('image', 'video', 'audio')),
    external_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'done', 'failed', 'expired')),
    artifact_hash TEXT,
    error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    deadline TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS media_jobs_pending
    ON media_jobs (status, created_at);

CREATE INDEX IF NOT EXISTS media_jobs_user_conversation
    ON media_jobs (user_id, conversation_id, created_at);

COMMIT;
