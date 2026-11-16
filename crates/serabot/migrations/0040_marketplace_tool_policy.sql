-- Per-tool owner override for marketplace plugin tools (v0.51, M4).
-- `ask` always asks, even for a tool the server calls read only. `allow` skips
-- the approval for a tool the server did not flag destructive. UID-scoped;
-- no secret is stored here.

BEGIN;

CREATE TABLE IF NOT EXISTS marketplace_tool_policy (
    user_id TEXT NOT NULL REFERENCES users(id),
    slug TEXT NOT NULL,
    tool TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('ask', 'allow')),
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, slug, tool)
);

COMMIT;
