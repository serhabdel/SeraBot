-- Scheduled helpers (0.70): morning brief, email triage and the weekly memory
-- review. The per-member switches are columns on alert_settings, added from
-- Rust (store.rs) only when missing, so this file can run again.
-- memory_reviews records which memories the member confirmed ("keep").

BEGIN;

CREATE TABLE IF NOT EXISTS memory_reviews (
    user_id TEXT NOT NULL REFERENCES users(id),
    memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    confirmed_at TEXT NOT NULL,
    PRIMARY KEY (user_id, memory_id)
);

COMMIT;
