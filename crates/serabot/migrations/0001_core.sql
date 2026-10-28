PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS bots (
    name TEXT PRIMARY KEY,
    config_json TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
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
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tasks_claim ON tasks(status, run_after, priority, created_at);
CREATE INDEX IF NOT EXISTS idx_tasks_lease ON tasks(status, lease_until);

CREATE TABLE IF NOT EXISTS turns (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    bot TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    status TEXT NOT NULL,
    state_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_turns_task ON turns(task_id);

CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    turn_id TEXT,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    name TEXT,
    tool_call_id TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_conversation ON messages(conversation_id, created_at);

CREATE TABLE IF NOT EXISTS actions (
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
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_actions_hash ON actions(canonical_hash);

CREATE TABLE IF NOT EXISTS approvals (
    id TEXT PRIMARY KEY,
    action_id TEXT NOT NULL UNIQUE REFERENCES actions(id) ON DELETE CASCADE,
    tool TEXT NOT NULL,
    summary TEXT NOT NULL,
    state TEXT NOT NULL,
    scope TEXT,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    decided_at TEXT,
    note TEXT
);
CREATE INDEX IF NOT EXISTS idx_approvals_pending ON approvals(state, expires_at);

CREATE TABLE IF NOT EXISTS standing_grants (
    id TEXT PRIMARY KEY,
    tool TEXT NOT NULL,
    canonical_hash TEXT NOT NULL,
    destination TEXT,
    policy_version TEXT NOT NULL,
    approval_id TEXT NOT NULL,
    revoked_at TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_grants_match ON standing_grants(tool, canonical_hash, policy_version, revoked_at);

CREATE TABLE IF NOT EXISTS audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at TEXT NOT NULL,
    actor TEXT NOT NULL,
    event TEXT NOT NULL,
    tool TEXT,
    task_id TEXT,
    approval_id TEXT,
    action_id TEXT,
    policy_version TEXT,
    details_json TEXT
);
CREATE INDEX IF NOT EXISTS idx_audit_at ON audit_log(at);

CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at TEXT NOT NULL,
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS telegram_updates (
    update_id INTEGER PRIMARY KEY,
    received_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS documents (
    id TEXT PRIMARY KEY,
    source_path TEXT NOT NULL,
    sha256 TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    mime TEXT NOT NULL,
    pages INTEGER,
    status TEXT NOT NULL,
    error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS chunks (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    page INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,
    text TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_chunks_document ON chunks(document_id, ordinal);

CREATE TABLE IF NOT EXISTS opportunities (
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
    UNIQUE(source, external_id)
);

CREATE TABLE IF NOT EXISTS deadlines (
    id TEXT PRIMARY KEY,
    opportunity_id TEXT NOT NULL REFERENCES opportunities(id) ON DELETE CASCADE,
    due_date TEXT NOT NULL,
    due_time TEXT,
    timezone TEXT,
    precision TEXT NOT NULL,
    source_text TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'upcoming',
    observed_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_deadlines_date ON deadlines(due_date, status);

CREATE TABLE IF NOT EXISTS source_runs (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    ok INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    items_seen INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_source_runs_source ON source_runs(source, started_at);

CREATE TABLE IF NOT EXISTS routines (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    next_run_at TEXT NOT NULL,
    last_run_for TEXT,
    config_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS memories (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    content TEXT NOT NULL,
    source TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS outbox (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    destination TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_outbox_status ON outbox(status, created_at);
