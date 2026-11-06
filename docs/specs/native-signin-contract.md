# Contract: native Microsoft sign-in (no token paste)

Status: in progress (0.13.0). Shared by the server, desktop, and mobile work.

## Problem

1. `POST /v1/auth/login/start` returns 500 in production. With
   `tenant = "consumers"` (also `common` / `organizations`) Microsoft's
   discovery document reports the real tenant GUID as issuer
   (`https://login.microsoftonline.com/9188040d-6c67-4c5b-b112-36a304b66dad/v2.0`
   for consumers), and the `openidconnect` discovery check rejects it.
2. After sign-in the callback page shows a session token the user must paste
   into the desktop or mobile app. Users must never see or paste a token.

## Flow (authorization code + PKCE at the IdP, plus a PKCE-bound handoff)

1. Client makes `handoff_verifier` (43-128 chars, base64url of 32 random
   bytes) and `handoff_challenge = base64url(sha256(verifier))`, plus a random
   `client_state`.
2. `POST /v1/auth/login/start`

   ```json
   {"provider": "microsoft", "invite_code": null,
    "return_to": "http://127.0.0.1:51121/auth-done",
    "handoff_challenge": "<S256>", "client_state": "<random>"}
   ```

   Response `{"url": "https://login.microsoftonline.com/..."}`. All three new
   fields are optional only as a group: either all present or none (none = the
   legacy web page flow).
   `return_to` must be exactly one of:
   - `http://127.0.0.1:<port>/auth-done` (desktop loopback, port 1024-65535);
   - `serabot://auth` (mobile custom scheme).
   Anything else is a 400.
3. Client opens `url` in the system browser. The IdP redirects to the
   server's existing `/v1/auth/callback`.
4. On success the server stores a one-time handoff code (32 random bytes,
   only its SHA-256 hash persisted, TTL 120 s, single use, bound to the
   `handoff_challenge` and the new session) and answers `302` to
   `return_to?handoff=<code>&state=<client_state>`. On failure it answers
   `302` to `return_to?error=<short_code>&state=<client_state>` where
   `short_code` is one of `denied`, `invite_required`, `invite_invalid`,
   `disabled`, `max_users`, `server`.
5. Client checks `state`, then
   `POST /v1/auth/handoff {"handoff": "<code>", "verifier": "<handoff_verifier>"}`
   → `200 {"token": "<session token>", "expires_at": "<rfc3339>",
   "user": {"id", "email", "display_name", "role"}}`.
   Wrong verifier, reuse, or expiry → `400 {"error": "invalid handoff"}` and
   the handoff is burned.
6. Client stores the token in the OS secret store (Windows Credential
   Manager / Keychain / Android Keystore via secure storage), sends
   `Authorization: Bearer <token>` on every call, and on any `401` drops the
   token and returns to the sign-in screen.
7. `GET /v1/me` → the same `user` object (authenticated).
8. Sign-out: `POST /v1/auth/logout`, then delete the stored token.

## Defaults

- Sign-in with Microsoft is the first and only sign-in screen in desktop and
  mobile. The paste-a-token / pairing-token UI is removed from both clients.
- Operator CLI device tokens stay server-side as admin break-glass only; no
  client UI offers them.
- First sign-in of the admin: if the admin user has no `provider_subject` yet
  and the signed-in email equals `[auth] admin_email`, the account is claimed
  by the existing admin row (keeps all backfilled data). Otherwise a new user
  still needs a valid invite.

## Isolation (already in 0.12, unchanged)

Each user has their own conversations, memory, documents, schedules, bot
fleet copy, AI provider credentials, and connectors (migrations 0008-0012).
