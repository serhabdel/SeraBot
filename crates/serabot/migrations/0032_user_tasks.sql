-- Personal tasks (0.25.0): the Tasks tab's per-user todo list, calendar and
-- Kanban. Every row is UID-scoped; the remote columns link a local row to its
-- Google Tasks / Microsoft To Do counterpart for sync. The prefs table holds
-- the user's chosen sync provider.

BEGIN;

CREATE TABLE IF NOT EXISTS user_tasks (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    title TEXT NOT NULL,
    notes TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'todo'
        CHECK (status IN ('todo', 'doing', 'done')),
    due TEXT,
    position INTEGER NOT NULL DEFAULT 0,
    remote_provider TEXT,
    remote_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS user_tasks_user
    ON user_tasks (user_id, status, position);

CREATE INDEX IF NOT EXISTS user_tasks_due
    ON user_tasks (user_id, due);

-- One local row per remote task per provider.
CREATE UNIQUE INDEX IF NOT EXISTS user_tasks_remote
    ON user_tasks (user_id, remote_provider, remote_id)
    WHERE remote_provider IS NOT NULL AND remote_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS user_task_prefs (
    user_id TEXT PRIMARY KEY REFERENCES users(id),
    sync_provider TEXT NOT NULL DEFAULT 'none'
        CHECK (sync_provider IN ('none', 'google_tasks', 'microsoft_todo',
                                 'google_calendar', 'outlook_calendar')),
    updated_at TEXT NOT NULL
);

COMMIT;
