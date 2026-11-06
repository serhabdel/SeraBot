-- One-time sign-in handoff codes (native Microsoft sign-in, no token paste).
--
-- After a successful IdP callback for a client that presented a PKCE-style
-- `handoff_challenge`, the server stores a single-use code here instead of
-- showing a session token. Only the SHA-256 hash of the code is persisted.
-- The client redeems it with the matching verifier within the TTL; the row is
-- deleted on first use (success or failure), so a code can never be replayed.
-- The session itself is minted at redemption time, bound to `user_id`.
CREATE TABLE IF NOT EXISTS auth_handoffs (
    code_hash TEXT PRIMARY KEY,
    challenge TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS auth_handoffs_expires ON auth_handoffs (expires_at);
