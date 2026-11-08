-- Media jobs (0.24): the `finishing` status. A pass claims a pending job
-- (pending -> finishing) before it saves and posts the finished file, then
-- marks it done. A second pass cannot claim a job that is already finishing,
-- so the file is never posted twice.
--
-- SQLite cannot alter a CHECK constraint, so the table is rebuilt with every
-- row and both indexes kept.

BEGIN;

CREATE TABLE media_jobs_next (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    conversation_id TEXT NOT NULL,
    bot TEXT NOT NULL,
    provider TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('image', 'video', 'audio')),
    external_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'finishing', 'done', 'failed', 'expired')),
    artifact_hash TEXT,
    error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    deadline TEXT NOT NULL
);

INSERT INTO media_jobs_next (
    id, user_id, conversation_id, bot, provider, kind, external_id, status,
    artifact_hash, error, created_at, updated_at, deadline
)
SELECT
    id, user_id, conversation_id, bot, provider, kind, external_id, status,
    artifact_hash, error, created_at, updated_at, deadline
FROM media_jobs;

DROP TABLE media_jobs;

ALTER TABLE media_jobs_next RENAME TO media_jobs;

CREATE INDEX IF NOT EXISTS media_jobs_pending
    ON media_jobs (status, created_at);

CREATE INDEX IF NOT EXISTS media_jobs_user_conversation
    ON media_jobs (user_id, conversation_id, created_at);

COMMIT;
