# Multi-user reconstruction — implementation plan

Source of truth: `docs/specs/multi-user-reconstruction.md` (owner-approved
2026-10-31). Task status: `tasks/todo.md`. Branch:
`spec/multi-user-reconstruction` (push after each phase gate; **never merge to
`main` without the owner's explicit OK**).

## Execution model

- Phases run **sequentially** (each builds on the previous). Inside a phase,
  tasks may parallelize only when they touch disjoint files.
- Gate after every task chunk: `scripts\cargo-win.bat check --workspace
  --all-targets`, then `scripts\cargo-win.bat test --workspace --locked`.
  The WSL gate (fmt, clippy `-D warnings`, full test) runs before each phase
  commit.
- Commit per phase with its tests in the same commit, then push to origin.
- Agents implement; the coordinator reviews diffs, runs the gate, commits,
  pushes.

## Phase graph

```
P0 boundary ─→ P1 identity ─→ P2 scoping ─→ P3 fleet/skills/memory ─→
P4 user secrets/providers ─→ P5 gateway authority ─→ P6 consoles/docs
```

## Phase 1 — identity foundations

Goal: users, invites, sessions, Microsoft OIDC sign-in, login page; existing
single-operator credentials keep working as the admin's sessions.

- Migration `0008_users_and_sessions.sql`: `users`, `invites`,
  `device_tokens.user_id`; deterministic admin bootstrap (fixed admin user
  id, `kv['admin_user_id']`, token backfill).
- `store.rs`: user/invite/session state machines. Opaque session tokens,
  stored sha256-hashed (existing `device_tokens` pattern), constant-time
  compare.
- `config.rs`: `[auth]` (enabled, providers `["microsoft"]`, tenant,
  `client_id_secret`, `base_url`, TTLs, `max_users`) and `[users]`
  (`tasks_per_user_per_day`).
- New `auth.rs` with the `openidconnect` crate (owner-approved dependency):
  provider registry, PKCE + state + nonce, JWKS-verified id_token, subject
  namespaced `microsoft:<sub>`, invite redemption at login start, session
  issuance at callback. Endpoints overridable for tests (pattern:
  `onedrive.rs::set_token_url`).
- `api.rs`: authorize middleware resolves `(User, role)`; existing token and
  device tokens authorize as admin. New: `GET /v1/auth/providers` (public),
  `POST /v1/auth/login/start`, `GET /v1/auth/callback` (shows the session
  token once), `POST /v1/auth/logout`, `POST /v1/auth/invites` (admin),
  `GET /v1/users` + enable/disable (admin).
- `web.rs`: public `/login` — Microsoft enabled; Google/GitHub shown
  disabled.
- Tests: wiremock IdP with synthetic JWKS; `jsonwebtoken` (dev-dep,
  owner-approved) signs test id_tokens; migration-fixture test at
  `user_version = 7`.

Checkpoint: two synthetic users can exist; member session is rejected on
admin endpoints; auth disabled ⇒ everything fails closed.

## Phase 2 — store scoping

Goal: `user_id` on every user-owned table and a scoped access layer.

- Migration `0009_user_scoping.sql`: add `user_id` (backfilled to the admin,
  then NOT NULL, indexed) to `conversations`, `tasks`, `turns`, `messages`,
  `actions`, `approvals`, `standing_grants`, `memories`, `documents`,
  `routines`, `outbox`, `events`, `opportunities`, `deadlines`,
  `source_runs`.
- `store.rs`: every user-owned read/write takes the acting user id; no
  unscoped variant exists outside migrations/maintenance.
- Call sites: `runtime.rs`, `gateway.rs`, `api.rs`, `tools.rs`, `memory.rs`,
  `watcher.rs`, `brief.rs`, `scheduler.rs`, `analyst.rs`, `documents.rs`,
  `retrieval.rs` thread the acting user end-to-end. `audit_log.actor`
  becomes the user id (`"system"` retained).
- Isolation suite: two users; every read path proven one-directional,
  including empty-result and error cases.

Checkpoint: isolation suite green; full workspace test green.

## Phase 3 — per-user fleet, skills, memory

- Migration `0010_user_fleet.sql`: `bots` re-keyed `(user_id, name)` with
  `prompt TEXT` (≤100 KB); `user_skills(user_id, id, name, description,
  body, created_at)`.
- Fleet seeding: on user creation, copy the default fleet (repo
  `bots/`+`prompts/` become seed templates) into the user's rows.
  Per-user validation: exactly one enabled orchestrator, delegates resolve
  within the acting user's fleet (`orchestration.rs` rules, scoped).
- `skills.rs`: effective catalog = shipped catalog ∪ acting user's rows;
  `skill_load`/`skill_search` resolve per user.
- Memory injection: 20 most recent rows **of the acting user**.
- API: per-user bot CRUD (existing scaffold endpoint scoped), per-user
  skill creation (policy-scanned, unchanged scan rules).

## Phase 4 — user secrets, per-user providers, Telegram linking

- New `user_secrets.rs`: master key `private/master.key` (32 bytes, 0600,
  generated on first run, **not** the age identity); per-user random DEK
  wrapped by the master key; AEAD `chacha20poly1305` (owner-approved dep)
  for `user_credentials(user_id, provider, ciphertext, nonce, updated_at)`.
- Migration `0011_user_secrets.sql` (+ `user_keys`, `provider_grants`).
- Per-user OneDrive: consent flow (auth-code + PKCE, `Files.ReadWrite
  offline_access`), refresh token under the user's DEK; lazy per-user
  `OneDriveClient` registry (struct unchanged); rotation writes back to the
  user store.
- Per-user AI providers: `ModelRouter` resolves from the acting user's
  credentials against the admin model catalog (fail closed); admin's
  existing age-store keys/OAuth tokens imported losslessly into the admin's
  credential rows; rotation targets move to the user store. Admin grants:
  `provider_grants(grantor, grantee, provider)` — auditable, revoc, never
  exposing the secret.
- Telegram linking: migration `0012_user_telegram.sql`; member requests a
  linking code from their console, sends `/start <code>`; the binding routes
  that chat to the member's scope. The admin pair migrates as the admin's
  binding. Unbound chats stay ignored.

## Phase 5 — per-user gateway authority, quotas

- `Gateway` drops its hardcoded actor; `ToolContext.actor` = acting user.
  Actions/approvals/standing_grants resolve and decide only within the
  owning user; admin sees all in audit, decides only their own (spec
  decision: members approve their own).
- Quotas: `tasks_per_user_per_day` enforced at enqueue (explicit error);
  the existing per-minute rate limit becomes per-session.

## Phase 6 — consoles, docs

- Web/desktop sign-in via the providers list (Microsoft enabled;
  Google/GitHub disabled with "coming soon").
- Desktop (Dioxus): Telegram settings pane — link/unlink, binding status.
- Mobile login if the client source is in-repo; otherwise document it.
- Docs: `deployment.md` (auth env/secret bootstrap), `api.md`, `connectors.md`
  (per-user OneDrive), `README.md` index, spec status → implemented.
- Final full gate (Windows + WSL), push, present the PR for the owner's OK.

## Binding technical decisions

- Migrations: sequential `user_version` if-blocks; new files `0008+`
  (`store.rs:18-251` pattern). IDs: uuidv7 (`domain.rs::new_id`), except the
  deterministic admin bootstrap id.
- Sessions: opaque random tokens, sha256-hex stored, constant-time compare
  (existing `api.rs` pattern). No JWT sessions.
- OIDC: `openidconnect` crate; discovery from
  `https://login.microsoftonline.com/{tenant}/v2.0/.well-known/openid-configuration`;
  scopes `openid profile email`; single app registration with incremental
  OneDrive consent later.
- Tests never touch live providers: wiremock IdP + `jsonwebtoken`-signed
  synthetic tokens; `Store::open_in_memory`; synthetic fixtures only.
- Envelope: `chacha20poly1305` AEAD; master key file 0600; per-user DEK.
- No unscoped store path for user-owned rows — enforced by method
  signatures, not runtime checks.
