-- Per-user tool limits for one bot turn (0.24).
--
-- A row is an owner's override of the config defaults for how many tool calls
-- and model rounds a turn may use. No row means the config defaults apply.
-- Values are checked against the server ceiling when written and clamped again
-- when resolved. Keyed by user_id; there is no unscoped read path.

BEGIN;

CREATE TABLE IF NOT EXISTS user_turn_limits (
    user_id TEXT PRIMARY KEY REFERENCES users(id),
    max_tool_calls_per_turn INTEGER NOT NULL CHECK (max_tool_calls_per_turn >= 1),
    max_tool_rounds INTEGER NOT NULL CHECK (max_tool_rounds >= 1),
    updated_at TEXT NOT NULL
);

COMMIT;
