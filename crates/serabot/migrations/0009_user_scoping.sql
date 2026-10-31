-- UID scoping of user-owned data (phase 2 of the multi-user reconstruction).
--
-- Every table that holds user-owned rows gains `user_id TEXT NOT NULL
-- REFERENCES users(id)`; all pre-existing rows backfill to the bootstrap admin
-- (the fixed id written by migration 0008 and mirrored in
-- `kv['admin_user_id']`), so the single-operator history keeps working for the
-- operator without re-issuing anything. New rows always carry the acting
-- user's id: the store layer has no unscoped write path.
--
-- SQLite cannot add a NOT NULL foreign-key column with `ALTER TABLE ...
-- ADD COLUMN` (with foreign keys enabled such a column must default to NULL),
-- so each table is rebuilt with the documented twelve-step procedure
-- (https://sqlite.org/lang_altertable, "Making Other Kinds Of Table Schema
-- Changes"): a new table is created with the final shape, the rows are copied
-- under the admin id, the old table is dropped and the new one renamed.
-- Foreign keys are disabled for the surgery and `legacy_alter_table` is
-- enabled so renames behave as before SQLite 3.25 (pure renames: no
-- reference rewriting, no whole-schema sanity check). Both pragmas are
-- restored at the end.
--
-- Tables left alone on purpose: `bots` (phase 3 re-keys it), `kv` (server
-- configuration), `audit_log` (the `actor` column already carries identity;
-- new rows carry user ids), `telegram_updates` (transport bookkeeping),
-- `invites`, `users`, `device_tokens` (already scoped by migration 0008),
-- `chunk_vectors` and `chunks` (chunks are reachable only through their
-- user-scoped document), and `conversation_teams` (reachable only through
-- its user-scoped conversation).
--
-- `user_id` indexes are added where queries filter by user: conversations,
-- tasks, memories, events, actions.

PRAGMA foreign_keys = OFF;
PRAGMA legacy_alter_table = ON;

BEGIN;

-- conversations -------------------------------------------------------------
CREATE TABLE conversations_new (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    parent_id TEXT REFERENCES conversations(id),
    created_at TEXT NOT NULL,
    title TEXT,
    updated_at TEXT,
    archived_at TEXT,
    last_read_at TEXT,
    last_read_rowid INTEGER,
    user_id TEXT NOT NULL REFERENCES users(id)
);
INSERT INTO conversations_new(
    id, kind, parent_id, created_at, title, updated_at, archived_at,
    last_read_at, last_read_rowid, user_id
)
SELECT id, kind, parent_id, created_at, title, updated_at, archived_at,
       last_read_at, last_read_rowid,
       '00000000-0000-7000-8000-000000000001'
FROM conversations;
DROP TABLE conversations;
ALTER TABLE conversations_new RENAME TO conversations;
CREATE INDEX idx_conversations_parent ON conversations(parent_id);
CREATE INDEX idx_conversations_archived ON conversations(archived_at);
CREATE INDEX idx_conversations_user ON conversations(user_id);

-- tasks ----------------------------------------------------------------------
CREATE TABLE tasks_new (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    bot TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    status TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 5,
    run_after TEXT NOT NULL,
    lease_until TEXT,
    worker_id TEXT,
    fencing_token INTEGER NOT NULL DEFAULT 0,
    uncertain INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id)
);
INSERT INTO tasks_new(
    id, kind, bot, payload_json, status, priority, attempts, max_attempts,
    run_after, lease_until, worker_id, fencing_token, uncertain, last_error,
    created_at, updated_at, user_id
)
SELECT id, kind, bot, payload_json, status, priority, attempts, max_attempts,
       run_after, lease_until, worker_id, fencing_token, uncertain, last_error,
       created_at, updated_at,
       '00000000-0000-7000-8000-000000000001'
FROM tasks;
DROP TABLE tasks;
ALTER TABLE tasks_new RENAME TO tasks;
CREATE INDEX idx_tasks_claim ON tasks(status, run_after, priority, created_at);
CREATE INDEX idx_tasks_lease ON tasks(status, lease_until);
CREATE INDEX idx_tasks_user ON tasks(user_id);

-- turns ----------------------------------------------------------------------
CREATE TABLE turns_new (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    bot TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    status TEXT NOT NULL,
    state_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id)
);
INSERT INTO turns_new(
    id, task_id, bot, conversation_id, status, state_json, created_at,
    updated_at, user_id
)
SELECT id, task_id, bot, conversation_id, status, state_json, created_at,
       updated_at,
       '00000000-0000-7000-8000-000000000001'
FROM turns;
DROP TABLE turns;
ALTER TABLE turns_new RENAME TO turns;
CREATE INDEX idx_turns_task ON turns(task_id);

-- messages -------------------------------------------------------------------
CREATE TABLE messages_new (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    turn_id TEXT,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    name TEXT,
    tool_call_id TEXT,
    created_at TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id)
);
INSERT INTO messages_new(
    id, conversation_id, turn_id, role, content, name, tool_call_id,
    created_at, user_id
)
SELECT id, conversation_id, turn_id, role, content, name, tool_call_id,
       created_at,
       '00000000-0000-7000-8000-000000000001'
FROM messages;
DROP TABLE messages;
ALTER TABLE messages_new RENAME TO messages;
CREATE INDEX idx_messages_conversation ON messages(conversation_id, created_at);

-- actions --------------------------------------------------------------------
CREATE TABLE actions_new (
    id TEXT PRIMARY KEY,
    tool TEXT NOT NULL,
    task_id TEXT,
    turn_id TEXT,
    canonical_args TEXT NOT NULL,
    canonical_hash TEXT NOT NULL,
    destination TEXT,
    artifact_hash TEXT,
    policy_version TEXT NOT NULL,
    status TEXT NOT NULL,
    outcome_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id)
);
INSERT INTO actions_new(
    id, tool, task_id, turn_id, canonical_args, canonical_hash, destination,
    artifact_hash, policy_version, status, outcome_json, created_at,
    updated_at, user_id
)
SELECT id, tool, task_id, turn_id, canonical_args, canonical_hash, destination,
       artifact_hash, policy_version, status, outcome_json, created_at,
       updated_at,
       '00000000-0000-7000-8000-000000000001'
FROM actions;
DROP TABLE actions;
ALTER TABLE actions_new RENAME TO actions;
CREATE INDEX idx_actions_hash ON actions(canonical_hash);
CREATE INDEX idx_actions_user ON actions(user_id);

-- approvals ------------------------------------------------------------------
CREATE TABLE approvals_new (
    id TEXT PRIMARY KEY,
    action_id TEXT NOT NULL UNIQUE REFERENCES actions(id) ON DELETE CASCADE,
    tool TEXT NOT NULL,
    summary TEXT NOT NULL,
    state TEXT NOT NULL,
    scope TEXT,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    decided_at TEXT,
    note TEXT,
    user_id TEXT NOT NULL REFERENCES users(id)
);
INSERT INTO approvals_new(
    id, action_id, tool, summary, state, scope, expires_at, created_at,
    decided_at, note, user_id
)
SELECT id, action_id, tool, summary, state, scope, expires_at, created_at,
       decided_at, note,
       '00000000-0000-7000-8000-000000000001'
FROM approvals;
DROP TABLE approvals;
ALTER TABLE approvals_new RENAME TO approvals;
CREATE INDEX idx_approvals_pending ON approvals(state, expires_at);

-- standing_grants -------------------------------------------------------------
CREATE TABLE standing_grants_new (
    id TEXT PRIMARY KEY,
    tool TEXT NOT NULL,
    canonical_hash TEXT NOT NULL,
    destination TEXT,
    policy_version TEXT NOT NULL,
    approval_id TEXT NOT NULL,
    revoked_at TEXT,
    created_at TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id)
);
INSERT INTO standing_grants_new(
    id, tool, canonical_hash, destination, policy_version, approval_id,
    revoked_at, created_at, user_id
)
SELECT id, tool, canonical_hash, destination, policy_version, approval_id,
       revoked_at, created_at,
       '00000000-0000-7000-8000-000000000001'
FROM standing_grants;
DROP TABLE standing_grants;
ALTER TABLE standing_grants_new RENAME TO standing_grants;
CREATE INDEX idx_grants_match ON standing_grants(tool, canonical_hash, policy_version, revoked_at);

-- memories -------------------------------------------------------------------
CREATE TABLE memories_new (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    content TEXT NOT NULL,
    source TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id)
);
INSERT INTO memories_new(id, kind, content, source, created_at, updated_at, user_id)
SELECT id, kind, content, source, created_at, updated_at,
       '00000000-0000-7000-8000-000000000001'
FROM memories;
DROP TABLE memories;
ALTER TABLE memories_new RENAME TO memories;
CREATE INDEX idx_memories_user ON memories(user_id);

-- documents ------------------------------------------------------------------
-- `sha256` is the dedupe key for re-ingestion, so uniqueness is per owner:
-- two users may each index the same file without seeing (or colliding with)
-- each other's document row.
CREATE TABLE documents_new (
    id TEXT PRIMARY KEY,
    source_path TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    title TEXT NOT NULL,
    mime TEXT NOT NULL,
    pages INTEGER,
    status TEXT NOT NULL,
    error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    UNIQUE(user_id, sha256)
);
INSERT INTO documents_new(
    id, source_path, sha256, title, mime, pages, status, error, created_at,
    updated_at, user_id
)
SELECT id, source_path, sha256, title, mime, pages, status, error, created_at,
       updated_at,
       '00000000-0000-7000-8000-000000000001'
FROM documents;
DROP TABLE documents;
ALTER TABLE documents_new RENAME TO documents;

-- routines -------------------------------------------------------------------
-- `name` uniqueness is per owner: the configured schedule names are the
-- operator's, and a future member's own routine of the same name is a
-- different row.
CREATE TABLE routines_new (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    next_run_at TEXT NOT NULL,
    last_run_for TEXT,
    config_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    UNIQUE(user_id, name)
);
INSERT INTO routines_new(
    id, name, kind, enabled, next_run_at, last_run_for, config_json,
    created_at, updated_at, user_id
)
SELECT id, name, kind, enabled, next_run_at, last_run_for, config_json,
       created_at, updated_at,
       '00000000-0000-7000-8000-000000000001'
FROM routines;
DROP TABLE routines;
ALTER TABLE routines_new RENAME TO routines;

-- outbox ---------------------------------------------------------------------
CREATE TABLE outbox_new (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    destination TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id)
);
INSERT INTO outbox_new(
    id, kind, destination, payload_json, status, attempts, last_error,
    created_at, updated_at, user_id
)
SELECT id, kind, destination, payload_json, status, attempts, last_error,
       created_at, updated_at,
       '00000000-0000-7000-8000-000000000001'
FROM outbox;
DROP TABLE outbox;
ALTER TABLE outbox_new RENAME TO outbox;
CREATE INDEX idx_outbox_status ON outbox(status, created_at);

-- events ---------------------------------------------------------------------
CREATE TABLE events_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at TEXT NOT NULL,
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id)
);
INSERT INTO events_new(id, at, kind, payload_json, user_id)
SELECT id, at, kind, payload_json,
       '00000000-0000-7000-8000-000000000001'
FROM events;
DROP TABLE events;
ALTER TABLE events_new RENAME TO events;
CREATE INDEX idx_events_user ON events(user_id);

-- opportunities ---------------------------------------------------------------
-- Dedupe is keyed by (source, external_id) *per owner*: two users watching
-- the same feed maintain separate opportunity rows.
CREATE TABLE opportunities_new (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    external_id TEXT NOT NULL,
    title TEXT NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    url TEXT,
    status TEXT NOT NULL DEFAULT 'open',
    evidence TEXT NOT NULL DEFAULT '',
    revision INTEGER NOT NULL DEFAULT 1,
    first_seen TEXT NOT NULL,
    last_seen TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    UNIQUE(user_id, source, external_id)
);
INSERT INTO opportunities_new(
    id, source, external_id, title, summary, url, status, evidence, revision,
    first_seen, last_seen, user_id
)
SELECT id, source, external_id, title, summary, url, status, evidence,
       revision, first_seen, last_seen,
       '00000000-0000-7000-8000-000000000001'
FROM opportunities;
DROP TABLE opportunities;
ALTER TABLE opportunities_new RENAME TO opportunities;

-- deadlines ------------------------------------------------------------------
CREATE TABLE deadlines_new (
    id TEXT PRIMARY KEY,
    opportunity_id TEXT NOT NULL REFERENCES opportunities(id) ON DELETE CASCADE,
    due_date TEXT NOT NULL,
    due_time TEXT,
    timezone TEXT,
    precision TEXT NOT NULL,
    source_text TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'upcoming',
    observed_at TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id)
);
INSERT INTO deadlines_new(
    id, opportunity_id, due_date, due_time, timezone, precision, source_text,
    status, observed_at, user_id
)
SELECT id, opportunity_id, due_date, due_time, timezone, precision, source_text,
       status, observed_at,
       '00000000-0000-7000-8000-000000000001'
FROM deadlines;
DROP TABLE deadlines;
ALTER TABLE deadlines_new RENAME TO deadlines;
CREATE INDEX idx_deadlines_date ON deadlines(due_date, status);

-- source_runs -----------------------------------------------------------------
CREATE TABLE source_runs_new (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    ok INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    items_seen INTEGER NOT NULL DEFAULT 0,
    user_id TEXT NOT NULL REFERENCES users(id)
);
INSERT INTO source_runs_new(
    id, source, started_at, finished_at, ok, error, items_seen, user_id
)
SELECT id, source, started_at, finished_at, ok, error, items_seen,
       '00000000-0000-7000-8000-000000000001'
FROM source_runs;
DROP TABLE source_runs;
ALTER TABLE source_runs_new RENAME TO source_runs;
CREATE INDEX idx_source_runs_source ON source_runs(source, started_at);

COMMIT;

PRAGMA legacy_alter_table = OFF;
PRAGMA foreign_keys = ON;
