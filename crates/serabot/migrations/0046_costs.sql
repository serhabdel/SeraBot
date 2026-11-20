-- Cost tracking (0.69): the owner's price table per model (USD per million
-- tokens), each member's monthly budget, and the weekly digest switch.

BEGIN;

CREATE TABLE IF NOT EXISTS model_prices (
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    input_per_m REAL NOT NULL CHECK (input_per_m >= 0),
    output_per_m REAL NOT NULL CHECK (output_per_m >= 0),
    updated_at TEXT NOT NULL,
    PRIMARY KEY (provider, model)
);

CREATE TABLE IF NOT EXISTS usage_budgets (
    user_id TEXT PRIMARY KEY REFERENCES users(id),
    monthly_usd REAL NOT NULL CHECK (monthly_usd >= 0),
    updated_at TEXT NOT NULL
);

-- alert_settings.weekly_digest is added from Rust (store.rs) only when it is
-- missing, so the migration can run again on a database that has it.

COMMIT;
