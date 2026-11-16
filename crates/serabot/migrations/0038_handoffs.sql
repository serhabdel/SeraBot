-- Handoff ledger (v0.50). Every row is UID-scoped.
--
-- One row per subagent handoff the orchestrator queued. `key_hash` is
-- sha256(parent_turn_id, to_bot, objective): a repeated `delegate` call in the
-- same parent task finds its existing row instead of queuing a second child.
-- The row is written in the same transaction as the child task.
--
-- `tasks.status` has no CHECK constraint (0009), so the new task statuses
-- `waiting_model` and `needs_review` need no table rebuild.

BEGIN;

CREATE TABLE IF NOT EXISTS handoffs (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    parent_task_id TEXT NOT NULL,
    parent_turn_id TEXT,
    child_task_id TEXT NOT NULL,
    to_bot TEXT NOT NULL,
    key_hash TEXT NOT NULL,
    status TEXT NOT NULL
        CHECK (status IN ('queued', 'running', 'completed', 'failed_resumable', 'failed', 'resumed')),
    outcome_code TEXT,
    effects_unknown INTEGER NOT NULL DEFAULT 0,
    resume_count INTEGER NOT NULL DEFAULT 0,
    route_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (user_id, parent_task_id, key_hash)
);

CREATE INDEX IF NOT EXISTS idx_handoffs_user_created
    ON handoffs(user_id, created_at);
CREATE INDEX IF NOT EXISTS idx_handoffs_child
    ON handoffs(user_id, child_task_id);

COMMIT;
