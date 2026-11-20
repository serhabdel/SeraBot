-- Job watcher (0.70): each member's job profile, extra and switched job
-- sources, the postings already seen, the last watch run and the
-- application pipeline. Every row carries the member's user_id.

BEGIN;

CREATE TABLE IF NOT EXISTS job_profiles (
    user_id TEXT PRIMARY KEY REFERENCES users(id),
    profile TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Member-added sources (builtin = 0) and per-member switches for the
-- built-in ones (builtin = 1, only the auto_watch flag is used).
CREATE TABLE IF NOT EXISTS job_sources (
    user_id TEXT NOT NULL REFERENCES users(id),
    source_key TEXT NOT NULL,
    builtin INTEGER NOT NULL DEFAULT 0,
    url TEXT NOT NULL DEFAULT '',
    label TEXT NOT NULL DEFAULT '',
    auto_watch INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    PRIMARY KEY (user_id, source_key)
);

CREATE TABLE IF NOT EXISTS job_seen (
    user_id TEXT NOT NULL REFERENCES users(id),
    url_hash TEXT NOT NULL,
    title_hash TEXT NOT NULL,
    source_key TEXT NOT NULL,
    title TEXT NOT NULL,
    employer TEXT NOT NULL DEFAULT '',
    city TEXT NOT NULL DEFAULT '',
    deadline TEXT NOT NULL DEFAULT '',
    url TEXT NOT NULL,
    matched INTEGER NOT NULL DEFAULT 0,
    first_seen TEXT NOT NULL,
    PRIMARY KEY (user_id, url_hash)
);
CREATE INDEX IF NOT EXISTS job_seen_title ON job_seen (user_id, title_hash);
CREATE INDEX IF NOT EXISTS job_seen_recent ON job_seen (user_id, first_seen);

CREATE TABLE IF NOT EXISTS job_watch_state (
    user_id TEXT PRIMARY KEY REFERENCES users(id),
    last_run_at TEXT NOT NULL,
    last_mode TEXT NOT NULL DEFAULT 'scheduled',
    last_report TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS job_applications (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    title TEXT NOT NULL,
    employer TEXT NOT NULL DEFAULT '',
    city TEXT NOT NULL DEFAULT '',
    url TEXT NOT NULL DEFAULT '',
    deadline TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL,
    notes TEXT NOT NULL DEFAULT '',
    applied_on TEXT NOT NULL DEFAULT '',
    interview_on TEXT NOT NULL DEFAULT '',
    artifacts TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS job_applications_user ON job_applications (user_id, status, updated_at);

COMMIT;
