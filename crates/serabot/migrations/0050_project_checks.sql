-- Project check harness (0.73): per-project check command, check runs and
-- the compiler diagnostics they produced. All rows are UID-scoped; the check
-- command is owner-configured (allowlisted program, no absolute paths) and
-- runs with the project folder as its working directory.

BEGIN;

CREATE TABLE IF NOT EXISTS project_checks (
    user_id TEXT NOT NULL REFERENCES users(id),
    project TEXT NOT NULL,
    command TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, project)
);

CREATE TABLE IF NOT EXISTS project_check_runs (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    project TEXT NOT NULL,
    status TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    errors INTEGER NOT NULL DEFAULT 0,
    warnings INTEGER NOT NULL DEFAULT 0,
    notes INTEGER NOT NULL DEFAULT 0,
    error TEXT
);

CREATE INDEX IF NOT EXISTS idx_project_check_runs
    ON project_check_runs(user_id, project, started_at);

CREATE TABLE IF NOT EXISTS project_diagnostics (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    project TEXT NOT NULL,
    run_id TEXT NOT NULL,
    file TEXT NOT NULL,
    line INTEGER,
    column INTEGER,
    severity TEXT NOT NULL,
    code TEXT,
    message TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_project_diagnostics
    ON project_diagnostics(user_id, project, run_id);

COMMIT;
