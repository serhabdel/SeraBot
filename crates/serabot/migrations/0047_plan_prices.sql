-- Costs (0.70): what each member pays for their plan sign-ins (ChatGPT /
-- Codex, Copilot, Antigravity, Claude plan, Muse, OpenCode), to compare the
-- subscription with what the same usage would cost at API prices.
-- model_prices.cached_per_m is added from Rust when missing.

BEGIN;

CREATE TABLE IF NOT EXISTS plan_prices (
    user_id TEXT NOT NULL REFERENCES users(id),
    provider TEXT NOT NULL,
    monthly_usd REAL NOT NULL CHECK (monthly_usd >= 0),
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, provider)
);

COMMIT;
