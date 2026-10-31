# Multi-user reconstruction — task list

Plan: `tasks/plan.md`. Spec: `docs/specs/multi-user-reconstruction.md`.
Gate per chunk: `scripts\cargo-win.bat check --workspace --all-targets` +
`scripts\cargo-win.bat test --workspace --locked`. Push after each phase;
never merge without the owner's OK.

## Phase 0 — boundary

- [x] T0.1 Rewrite `AGENTS.md` product boundary to the approved
      multi-user, invitation-only model; keep every other invariant.

## Phase 1 — identity foundations

- [x] T1.1 Migration `0008_users_and_sessions.sql`: `users`, `invites`,
      `device_tokens.user_id`; deterministic admin bootstrap (fixed id,
      `kv['admin_user_id']`, token backfill). Register `MIGRATION_8`.
      - Acceptance: a DB at `user_version=7` migrates; admin user exists;
        all existing device tokens resolve to the admin.
      - Verify: `cargo-win test store` (new migration fixture test).
- [x] T1.2 `store.rs`: `User`, invite mint/consume (transactional),
      session mint/resolve/revoke (hash-only storage, constant-time
      compare), user disable/enable.
      - Acceptance: state machines incl. expired invite, exhausted invite,
        revoked session; inline tests green.
- [x] T1.3 `config.rs`: `[auth]` + `[users]` sections, fail-fast validation;
      synthetic examples in example configs only.
      - Acceptance: invalid base_url/tenant/providers rejected at load;
        defaults documented in struct docs.
- [x] T1.4 New `auth.rs` (add `openidconnect`): provider registry
      (microsoft), PKCE + state + nonce, JWKS-verified id_token, namespaced
      subject, invite redemption at start, session issue at callback;
      overridable endpoints for tests; secrets never logged.
      - Acceptance: happy path + each failure path (bad state, expired
        invite, unknown provider, auth disabled) unit-tested against a
        wiremock IdP with a `jsonwebtoken`-signed synthetic token.
- [x] T1.5 `api.rs`: authorize middleware yields `(User, role)`; existing
      tokens keep working as admin; add `GET /v1/auth/providers` (public),
      `POST /v1/auth/login/start`, `GET /v1/auth/callback` (token shown
      once), `POST /v1/auth/logout`, `POST /v1/auth/invites` + user
      list/enable/disable (admin-only).
      - Acceptance: member session ⇒ 403 on admin endpoints; 401
        unauthenticated; providers list shows google/github disabled.
- [x] T1.6 `web.rs`: public `/login` page; Microsoft enabled, Google and
      GitHub visibly disabled.
      - Acceptance: page renders without JS frameworks; matches landing
        style; covered by an axum oneshot test.
- [x] T1.7 Phase gate: check + full workspace test green; clippy clean on
      WSL; commit; push.

## Phase 2 — store scoping

- [ ] T2.1 Migration `0009_user_scoping.sql`: `user_id` on all user-owned
      tables (backfill → NOT NULL → indexes). Verify with a v7 fixture.
- [ ] T2.2 `store.rs`: scoped signatures for every user-owned read/write;
      no unscoped variants outside migrations.
- [ ] T2.3 Call sites thread the acting user: `runtime.rs`, `gateway.rs`,
      `api.rs`, `tools.rs`, `memory.rs`, `watcher.rs`, `brief.rs`,
      `scheduler.rs`, `analyst.rs`, `documents.rs`, `retrieval.rs`;
      `audit_log.actor` = user id.
- [ ] T2.4 Isolation suite: two users; cross-user reads return empty/fail
      closed on every path (store, API, tools).
- [ ] T2.5 Phase gate + commit + push.

## Phase 3 — per-user fleet, skills, memory

- [ ] T3.1 Migration `0010_user_fleet.sql`: `bots (user_id, name)` +
      `prompt` column; `user_skills` table.
- [ ] T3.2 Fleet seeding on user create; per-user load + validation
      (orchestrator rule scoped per user).
- [ ] T3.3 Skills: shipped catalog ∪ user rows; tools resolve per user.
- [ ] T3.4 Memory injection limited to the acting user's rows.
- [ ] T3.5 API: per-user bot CRUD + skill create (policy-scanned).
- [ ] T3.6 Phase gate + commit + push.

## Phase 4 — user secrets, providers, Telegram

- [ ] T4.1 `user_secrets.rs` (add `chacha20poly1305`): master key file,
      per-user DEK wrap, AEAD seal/open; migration `0011_user_secrets.sql`
      (`user_keys`, `user_credentials`, `provider_grants`).
- [ ] T4.2 Per-user OneDrive: consent endpoints, DEK-stored refresh token,
      lazy client registry, rotation write-back.
- [ ] T4.3 Per-user AI providers: credential CRUD, per-user
      `ModelRouter`, lossless admin import from the age store, rotation to
      the user store.
- [ ] T4.4 Admin provider grants (auditable, revocable, secret never
      exposed); resolution order user → grant.
- [ ] T4.5 Telegram linking: migration `0012_user_telegram.sql`, linking
      code `/start` flow, per-binding routing; admin pair migrates as the
      admin binding.
- [ ] T4.6 Phase gate + commit + push.

## Phase 5 — gateway authority, quotas

- [ ] T5.1 Approvals/grants decide within the owning user; gateway actor
      from context.
- [ ] T5.2 Quotas: `tasks_per_user_per_day` at enqueue; per-session rate
      limit.
- [ ] T5.3 Phase gate + commit + push.

## Phase 6 — consoles, docs

- [ ] T6.1 Web + desktop sign-in via providers list (Microsoft enabled,
      others disabled).
- [ ] T6.2 Desktop Telegram settings pane (link/unlink, status).
- [ ] T6.3 Mobile login if the client source is in-repo.
- [ ] T6.4 Docs updates (deployment, api, connectors, README, spec status).
- [ ] T6.5 Final full gate (Windows + WSL), push, PR for owner review —
      merge only on the owner's OK.
