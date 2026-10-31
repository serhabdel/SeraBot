# Spec: SeraBot multi-user reconstruction (small trusted circle)

Status: **draft, awaiting owner approval.** Approving this spec deliberately
rewrites the product boundary in `AGENTS.md` (which today forbids multi-user
auth). Nothing in this spec is implemented until the owner approves it and the
`AGENTS.md` rewrite lands as the first change.

## Objective

Turn SeraBot from a single-operator agent into a small-circle multi-user
system, keeping the existing pipeline character: deterministic Rust around
bounded LLM calls, one process, one SQLite database, no new services.

Who uses it: the owner plus a handful of invited, personally known people.
There is no open sign-up. Each user gets:

- the **default bot fleet** (today's `main`/`analyst`/`maker`/`watcher`) as a
  personal copy, editable by them;
- the ability to **create their own bots** from the existing scaffold;
- **UID-scoped** conversations, memory, skills, documents, and schedules;
- **their own connectors**, starting with OneDrive (their Microsoft account,
  their drive), and **their own AI provider credentials** (API keys and
  OAuth logins), with credentials no other user can reach;
- **approval authority over their own tool calls**, with the admin retaining
  visibility.

Success at a glance: two users sign in, each chats with their own bots, saves
memory, connects their own OneDrive, and provably cannot read each other's
data or credentials — while the operator keeps admin visibility and the server
secrets stay server-only.

## Current-state facts (what the build starts from)

Single-operator assumptions baked into the code today (file:line evidence from
the 0.11.0 tree):

1. **No user entity anywhere in the schema.** `audit_log.actor` is free text
   (`"owner"`, `"system"`); `ToolContext.actor` is hardcoded `"owner"`
   (`runtime.rs:1747`, `gateway.rs:177`).
2. **One Telegram owner** — exactly one `owner_user_id` + `owner_chat_id`
   pair (`telegram.rs:79-129`).
3. **One API token hash** (`config.api.token_hash`) plus `device_tokens` rows
   that carry a label but no identity (`api.rs:2584-2627`).
4. **File-based bot fleet** — `bots/*.toml` + `prompts/*.txt`, validated at
   startup, with a fleet-wide rule of exactly one orchestrator named `main`
   (`orchestration.rs:247-292`).
5. **Global memory pool** — the 20 most recent `memories` rows are injected
   into every bot's prompt (`runtime.rs:1911-1923`).
6. **Global skill catalog** — `skills/` dir, allowlisted per bot only
   (`skills.rs`).
7. **One age store with one identity** for all secrets (`secrets.rs`);
   provider clients (OneDrive, ElevenLabs, Tavily, Firecrawl, connectors) are
   constructed once at startup and shared (`main.rs::run`).
8. **Approvals bind** canonical args, hash, destination, artifact hash, policy
   version, expiry — but no user (`gateway.rs:274-467`).
9. **Migrations** are sequential `user_version`-gated embedded SQL files
   `migrations/0001..0007` (`store.rs:186-251`).

These are the seams the reconstruction works along; nothing else about the
architecture (gateway-first tool execution, durable turn loop, policy
scanning, WAL store) changes.

## Target architecture

### Identity and authentication — thin OIDC passthrough

- **Sign-in = Microsoft only**, via a single Azure app registration owned by
  the operator, configured as a public client with PKCE (same style as the
  existing OneDrive registration). No passwords, emails, or MFA are handled
  by SeraBot; the identity provider does all of it.
- New dependency: the `openidconnect` crate. Flow: authorization code + PKCE
  → id_token verified via the provider's JWKS → extract stable subject
  (`sub`) and email → upsert `users` row.
- **Provider registry, not a hardwired IdP.** `users.provider_subject` is
  namespaced (`microsoft:<sub>`, later `google:<sub>`, `github:<sub>`), so
  adding Google and GitHub later is configuration plus a client-id secret —
  no schema change. `GET /v1/auth/providers` reports which providers are
  enabled; web, desktop, and mobile render the others as visible but
  disabled ("coming soon"). v1 ships Microsoft enabled only.
- **Sessions stay opaque and server-side** (no JWT sessions): the existing
  bearer-token + SHA-256-hash pattern is kept. `device_tokens` grows a
  `user_id` and becomes the sessions table. Every API request resolves to a
  user; the authorize middleware returns `UserId` + role.
- **Invitation gate**: the admin creates a single-use invite (random code,
  TTL, max uses) via an admin API. A user can only reach the OIDC flow with a
  valid invite. First sign-in creates the `users` row.
- **Bootstrap**: on migration, the operator's user is created (`role=admin`)
  and all existing rows backfill to that user id. The admin account is
  linked to the existing Telegram pair; the existing API token and device
  tokens keep working unchanged as the admin's sessions (nothing to
  re-issue).
- Config sketch (synthetic):

  ```toml
  [auth]
  enabled = true
  providers = ["microsoft"]         # google + github ship shown-but-disabled
  tenant = "consumers"
  client_id_secret = "auth_client_id"   # in the age store, like onedrive_client_id
  base_url = "https://bot.example.example"  # public base URL for the redirect
  session_ttl_hours = 720
  invite_ttl_hours = 48
  max_users = 10

  [users]
  tasks_per_user_per_day = 500
  ```

### Data model — UID-scoped SQLite (migrations 0008+)

- New tables: `users` (id, `provider_subject` UNIQUE, email, display_name,
  role `admin|member`, status `active|disabled`, created_at),
  `invites` (code_hash, created_at, expires_at, uses_left),
  `user_credentials` (user_id, provider, ciphertext, nonce, updated_at),
  `user_keys` (user_id, wrapped_dek, nonce).
- `user_id` is added (NOT NULL, backfilled to the admin) to every user-owned
  table: `conversations`, `tasks`, `turns`, `messages`, `actions`,
  `approvals`, `standing_grants`, `memories`, `documents`, `routines`,
  `outbox`, `events`, `opportunities`, `deadlines`, `source_runs`. `chunks`
  is reached only through its `documents` FK.
- **Deny-by-default store layer**: every store method that reads or writes
  user-owned rows takes the acting `user_id` and filters on it. There is no
  unscoped variant of these methods outside migration/maintenance code paths.
  Cross-user access is a type error, not a runtime check.
- `audit_log.actor` carries the user id (`"system"` retained for maintenance).
- The `bot id → (user_id, name)` remap is below, under the fleet.

### Bot fleet — per-user, DB-backed

- `bots` table re-keys from `name` to `(user_id, name)`. Prompt text moves
  into the row (`prompt TEXT`, still capped at 100 KB). The file-based
  `bots/` + `prompts/` directories remain in the repo **only as seed
  templates** applied when a user is created.
- Fleet validation becomes per-user: exactly one enabled orchestrator per
  user (default name `main`), delegates resolve inside that user's fleet
  (`orchestration.rs` rule, scoped).
- The existing scaffold endpoint (`POST /v1/bots`) becomes per-user; the
  admin overlay (`AdminOverlay.bots`) applies to the admin user only.
- Bot enable/disable and prompt edits become DB writes on the owner-of-record
  user. The restart-to-apply model is kept (no hot reload in scope).

### Skills — server catalog + per-user creations

- `skills/` stays the shipped, server-managed catalog every user can
  allowlist.
- User-created skills become DB rows (`user_skills`: user_id, id, name,
  description, body) written through the existing policy-scanned `create_skill`
  path; a user's effective catalog is the shipped catalog ∪ their own rows,
  and bot allowlists resolve within that union.

### Memory — per-user pool

- `memories` gains `user_id`; the injection list becomes the 20 most recent
  **for the acting user**; the `memory_*` tools read/write only the calling
  user's rows. Same kinds, limits, and provenance rules as today.

### Connectors and secrets — envelope encryption, OneDrive first

- **Two secret tiers.** Server tier: the age store stays exactly as-is for
  server-level secrets (LLM provider keys, Telegram bot token, auth client
  id, existing global connectors). User tier: new envelope-encrypted store —
  a server master key (`private/master.key`, 32 bytes, 0600, generated on
  first run, **not** the age identity) wraps a per-user random DEK
  (`user_keys`), which encrypts that user's provider tokens
  (`user_credentials`) with an AEAD (preference: `chacha20poly1305`; final
  crate choice is ask-first). Tokens are never logged, never echoed; the
  `Debug` redaction convention applies to the new types.
- **Per-user OneDrive**: the existing `[onedrive]` section becomes the
  *availability + defaults* section (server app registration, tenant, folder
  template). A user opts in from their console: authorization code + PKCE
  consent (`Files.ReadWrite`, `offline_access`) with **their** Microsoft
  account → refresh token encrypted under their DEK → uploads land in **their
  drive** under the configured folder. The existing `OneDriveClient` is
  reused unchanged (it already takes plain values); instances are built
  lazily per user and cached in a per-user registry sharing one
  `reqwest::Client`. Rotation writes back through the user-credential store.
  The old global `onedrive_refresh_token` flow remains for the admin account
  only (backward compatible).
- **AI providers are user-owned too.** LLM API keys and OAuth-based provider
  logins (Antigravity, Muse Code, ElevenLabs OAuth) move into the same
  `user_credentials` store. Migration is lossless: on upgrade the operator's
  existing age-store values are imported into the operator's own credential
  rows, and rotation writes back there (the flows' persist target moves from
  `SharedSecrets` to the user store). The age store keeps only server-level
  secrets: the Telegram bot token and auth client ids; provider *endpoints*
  stay in config. Model routing resolves per user: `ModelRouter` is built
  from the acting user's credentials against the admin-defined model
  catalog; a user without a provider's credentials simply cannot route to
  that provider (fail closed, explicit error). Members add API keys or run
  OAuth logins from their console.
- Other connectors (Google, Notion, AgentMail) keep their global
  single-operator form in v1; per-user expansion follows the OneDrive
  pattern once it proves out.

### Gateway and approvals — per-user authority

- `ToolContext.actor` carries the acting `UserId`; the `Gateway`'s hardcoded
  actor field is removed. Allowlist, `require_approval`, policy scanning, and
  canonical-args binding are unchanged — they gain the user dimension:
  `actions`, `approvals`, and `standing_grants` bind the user id, a user
  decides **only their own** pending approvals, and grants match only within
  the granting user's rows.
- The policy blocklist stays a **global server policy** (one version hash for
  everyone), so its invalidation semantics are unchanged.
- Per-user quotas: task/day cap and the existing per-minute rate limit become
  per-session. Quota exhaustion is a clear validation error, never a silent
  drop.

### Transports

- **Owner API → User API.** The authorize middleware resolves sessions to
  `(user, role)`; all list/get/decide endpoints scope to the acting user.
  Admin-only endpoints (users, invites, global config, overlay) gate on
  `role=admin`. SSE streams filter by user.
- **Console login**: the existing `web.rs` public pages gain `/login` — a
  minimal page with a "Sign in with Microsoft" link that starts the OIDC
  flow; after the callback the page shows the new session token once for
  pasting into the desktop/mobile clients (same UX as `serabot token` today).
  No new frontend framework.
- **Telegram is per-user linkable, configured from the desktop app.** The
  server keeps one bot token (age store, admin-managed, unchanged). A
  member opens settings → Telegram in the desktop app, requests a linking
  code, and sends `/start <code>` to the bot; the binding
  (`user_telegram`: user_id, telegram_user_id, telegram_chat_id, linked_at,
  revoked_at) routes that chat's messages to the member's user scope —
  their bots, memory, and approvals. The desktop app shows binding status
  and unlink. The operator's existing owner pair becomes simply the
  admin's binding (migrated, not re-linked). Unbound chats stay ignored,
  as today.

### Security model (isolation)

The threat model adds one actor: a **malicious or curious member**. They must
not be able to (a) read another user's rows, (b) use another user's
credentials or quota, (c) touch server secrets or config, (d) escalate to
admin, (e) exhaust the host.

Enforcement, in order:

1. Store layer: user-scoped methods only (see above) — no code path can
   construct a cross-user read.
2. Credentials: `user_credentials`/`user_keys` are only addressed by the
   acting user's id; the master key never leaves the process and is never
   exposed via any API.
3. Gateway: approvals/grants bound to user; tool destinations validated per
   user (per-user inbox roots `private/inboxes/<user_id>/`; OneDrive folder
   inside the acting user's own drive).
4. API: role gates + per-session rate limits; session tokens stored hashed
   (existing constant-time compare), revocable per user.
5. Audit: every event carries the acting user id; the admin sees all, users
   see their own.
6. Deployment: unchanged — one binary, SQLite/WAL, loopback-first API. The
   public `base_url` requirement (OIDC redirect) means the owner must
   consciously expose the API endpoint (existing `allow_non_loopback` +
   reverse-proxy pattern, already documented).

## Tech stack

Unchanged: Rust, axum, SQLite (r2d2, WAL), sqlite-vec, reqwest, tokio, age.
New dependencies (each ask-first before adding): `openidconnect` (OIDC code
flow + JWKS verification), an AEAD crate (preference `chacha20poly1305`),
`rand` if not already a transitive dependency. No Node, no Docker, no
PostgreSQL, no extra daemon — the OIDC provider is Microsoft's, not ours.

## Commands

Unchanged from `AGENTS.md` (Windows dev box, Ubuntu WSL gate):

```bat
scripts\cargo-win.bat check --workspace --all-targets
scripts\cargo-win.bat test --workspace --locked
```

```bash
source ~/.cargo/env
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
cargo build --release --locked
```

## Project structure

No new crates. New/changed modules inside `crates/serabot/src`:

| Path | Role |
| --- | --- |
| `auth.rs` (new) | OIDC client, login/callback handlers, invite redemption, session issue/verify |
| `user_secrets.rs` (new) | Master key, per-user DEKs, `user_credentials` encrypt/decrypt |
| `store.rs` | Users, invites, sessions; user-scoped variants of user-owned queries |
| `migrations/0008_users_and_sessions.sql` … | Sequential migrations incl. backfill |
| `config.rs` | `[auth]`, `[users]` sections; per-user overlay semantics |
| `gateway.rs` | User-bound actions/approvals/grants |
| `runtime.rs` | Per-user routing, memory injection, fleet resolution |
| `onedrive.rs` (unchanged struct) + `maker.rs` | Per-user client registry and tools |
| `api.rs` / `web.rs` | Session auth, `/login`, admin endpoints, scoped SSE |

Tests stay inline `#[cfg(test)] mod tests` per module — no `tests/` dir.

## Code style

Existing conventions carry over verbatim: module-level `//!` docs stating
permissions and failure semantics, deny-by-default validation that errors
early with explicit messages, bounded everything, redacted `Debug` on
anything secret-adjacent. The one new pattern this spec adds — user scoping
as a type-level property — looks like:

```rust
/// List the acting user's memories, newest first, bounded.
///
/// Rows are always filtered by `user_id`; there is no unscoped list, and
/// an unknown user yields an empty list, never another user's rows.
pub fn list_memories(&self, user_id: &UserId, limit: usize) -> Result<Vec<Memory>> {
    debug_assert!(limit <= MAX_MEMORY_LIST);
    // SELECT ... WHERE user_id = ?1 ORDER BY updated_at DESC LIMIT ?2
    ...
}
```

## Testing strategy

Synthetic fixtures only; no live provider is ever contacted from tests
(existing rule). Levels:

- **Unit** (per module, inline): user_scoping, envelope-encryption roundtrip
  (tempdir master key), invite/session state machines, per-user fleet
  validation.
- **Isolation suite** (new, `store.rs` + `gateway.rs` tests): two synthetic
  users; every read/write path proven one-directional — user A's memory,
  documents, approvals, credentials, and OneDrive client are unreachable from
  user B's scope, including empty-result and error cases.
- **OIDC flow**: wiremock-style mock identity provider with synthetic JWKS
  and signed test tokens (same pattern as the existing provider mocks).
- **Migration**: a fixture DB at `user_version = 7` is migrated and asserted:
  all rows land under the admin user, no data loss, second user starts empty.
- **Full gate**: `cargo fmt --check`, `clippy -D warnings`, `cargo test
  --workspace --locked` green on both Windows and WSL before any merge.

## Boundaries

- **Always**: run the fmt/clippy/test gate before commits; synthetic fixtures
  only; every new user-owned store method takes the acting user id; every
  audit row carries the user; secrets redacted in `Debug`, logs, and errors;
  approvals still bind canonical args + hash + destination + policy version
  + expiry (now + user).
- **Ask first**: adding any dependency; new migrations beyond this spec's
  outline; changing the OIDC provider set; exposing the API beyond loopback;
  any `AGENTS.md` edit; any OVH touch (still read-only without explicit
  owner authorization).
- **Never**: open self-signup; cross-user data access; logging or printing
  secrets; storing plaintext provider tokens; bypassing the gateway/policy;
  committing private data (dossier, blocklist terms, real credentials, real
  user emails) to the public repo; Docker/PostgreSQL/Node.

## Assumptions (owner: correct any of these)

1. Users are invited by the admin; no self-signup; ≤ 10 users.
2. The operator becomes the first user with `admin` role; existing data
   backfills to that account (nothing is lost, nothing re-keyed).
3. Members approve their own tool actions; the admin sees everything in audit
   but is not in the approval path for members.
4. AI provider credentials (LLM API keys and OAuth logins: Antigravity,
   Muse Code, ElevenLabs OAuth) become **user-owned**: the operator's
   existing tokens migrate into the operator's user-credential store with
   nothing lost, members bring their own keys/logins, and credential
   resolution is strictly per user (fail closed — no silent fallback to
   another user's key).
5. Telegram is linkable per user from the desktop app (linking-code flow);
   the single server bot token stays admin-managed in the age store.
6. Bot fleets become per-user DB copies seeded from today's default fleet;
   the shipped `skills/` catalog stays server-managed; user-created skills
   are per-user DB rows.
7. One Microsoft app registration for sign-in (public client + PKCE); the
   OneDrive consent is a second, connector-specific flow — whether it reuses
   the same registration is an open question (below).
8. The server keeps the restart-to-apply model (no hot reload) in v1.
9. The sign-in screen shows three providers — Microsoft enabled, Google
   and GitHub visible but disabled ("coming soon") — on web, desktop, and
   mobile; enabling them later is additive configuration, no schema
   change.

## Success criteria

1. A second user can be invited, sign in with a Microsoft account (mocked in
   tests), receive the default fleet, chat, save memory, and create a bot.
2. Isolation suite proves: user B cannot read user A's conversations, memory,
   documents, actions, approvals, or credentials — by scoped query, by API,
   and by tool call — and vice versa.
3. Two users connect two different OneDrive accounts; each `onedrive_upload`
   lands in the connecting user's own drive with their own token (wiremock
   fixtures); a cross-user token request fails closed.
4. Existing single-operator data migrates fully to the admin account
   (migration fixture at `user_version = 7` → new schema, zero row loss).
5. No new services on the host; idle memory increase within ~50 MB; the
   full build/test gate is green.
6. `AGENTS.md` product boundary rewritten and merged together with phase 1.
7. The operator's existing AI-provider credentials (API keys and OAuth
   tokens) keep working after migration, now stored as the admin user's
   credentials; token rotation writes back to the user store (wiremock +
   migration fixtures).
8. A member links Telegram from the desktop app and chats with their own
   bots; the admin's existing Telegram pair and API/device tokens keep
   working without re-linking or re-issuing.

## Open questions

1. **App registration** — one Azure app for both sign-in and OneDrive consent
   (incremental consent) or two registrations?
2. **Provider sharing** — credentials resolve strictly per user (assumed);
   should the admin also be able to *grant* a member access to an
   admin-owned provider, or is BYO the only path?
3. **Data deletion** — admin-disable user keeps data (assumed); do we also
   need full delete/export of a user's rows?
4. **Deployment** — the OVH VPS is at 97% disk; enlargement/cleanup is a
   prerequisite for shipping. Owner decision required.

## Phasing (detail comes in the plan, not this spec)

- **Phase 0**: this spec approved; `AGENTS.md` boundary rewritten.
- **Phase 1**: identity foundations — users/invites/sessions, OIDC, auth
  middleware, migrations + admin backfill.
- **Phase 2**: store scoping — `user_id` across the data model, scoped
  access layer, isolation suite.
- **Phase 3**: per-user fleet/skills/memory (DB-backed bots, seeded defaults).
- **Phase 4**: user secrets — envelope encryption + lossless credential
  migration (OneDrive + AI providers) + per-user model routing.
- **Phase 5**: per-user gateway authority, quotas, per-session rate limits.
- **Phase 6**: console login flow (Microsoft enabled, Google/GitHub shown
  disabled), desktop Telegram linking, docs updates, deployment manifest
  (OVH remains untouched until the owner approves it explicitly).
