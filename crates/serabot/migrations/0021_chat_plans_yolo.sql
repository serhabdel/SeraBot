-- Per-conversation goal and todo plan, and the per-conversation auto-approve
-- ("YOLO") setting (0.18).
--
-- `conversation_plans` holds at most one plan per (user, conversation): a
-- one-line goal and a JSON array of {text, status} items written by the
-- `plan_update` tool. `conversation_yolo` holds the owner's auto-approve
-- switch for one conversation; a missing row means off. Every row is scoped
-- by user_id and every store method takes the acting user's id, so one
-- user's plan or switch is never visible to, or applied in, another user's
-- conversation even when the conversation ids collide (for example dir:main).

BEGIN;

CREATE TABLE IF NOT EXISTS conversation_plans (
    user_id TEXT NOT NULL REFERENCES users(id),
    conversation_id TEXT NOT NULL,
    goal TEXT NOT NULL,
    items TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, conversation_id)
);

CREATE TABLE IF NOT EXISTS conversation_yolo (
    user_id TEXT NOT NULL REFERENCES users(id),
    conversation_id TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, conversation_id)
);

COMMIT;
