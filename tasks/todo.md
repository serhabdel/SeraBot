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

- [x] T2.1 Migration `0009_user_scoping.sql`: `user_id` on all user-owned
      tables (backfill → NOT NULL → indexes). Verify with a v7 fixture.
- [x] T2.2 `store.rs`: scoped signatures for every user-owned read/write;
      no unscoped variants outside migrations.
- [x] T2.3 Call sites thread the acting user: `runtime.rs`, `gateway.rs`,
      `api.rs`, `tools.rs`, `memory.rs`, `watcher.rs`, `brief.rs`,
      `scheduler.rs`, `analyst.rs`, `documents.rs`, `retrieval.rs`;
      `audit_log.actor` = user id.
- [x] T2.4 Isolation suite: two users; cross-user reads return empty/fail
      closed on every path (store, API, tools).
- [x] T2.5 Phase gate + commit + push.

## Phase 3 — per-user fleet, skills, memory

- [x] T3.1 Migration `0010_user_fleet.sql`: `bots (user_id, name)` +
      `prompt` column; `user_skills` table.
- [x] T3.2 Fleet seeding on user create; per-user load + validation
      (orchestrator rule scoped per user).
- [x] T3.3 Skills: shipped catalog ∪ user rows; tools resolve per user.
- [x] T3.4 Memory injection limited to the acting user's rows.
- [x] T3.5 API: per-user bot CRUD + skill create (policy-scanned).
- [x] T3.6 Phase gate + commit + push.

## Phase 4 — user secrets, providers, Telegram

- [x] T4.1 `user_secrets.rs` (add `chacha20poly1305`): master key file,
      per-user DEK wrap, AEAD seal/open; migration `0011_user_secrets.sql`
      (`user_keys`, `user_credentials`, `provider_grants`).
- [x] T4.2 Per-user OneDrive: consent endpoints, DEK-stored refresh token,
      lazy client registry, rotation write-back.
- [x] T4.3 Per-user AI providers: credential CRUD, per-user
      `ModelRouter`, lossless admin import from the age store, rotation to
      the user store.
- [x] T4.4 Admin provider grants (auditable, revocable, secret never
      exposed); resolution order user → grant.
- [x] T4.5 Telegram linking: migration `0012_user_telegram.sql`, linking
      code `/start` flow, per-binding routing; admin pair migrates as the
      admin binding.
- [x] T4.6 Phase gate + commit + push.

## Phase 5 — gateway authority, quotas

- [x] T5.1 Approvals/grants decide within the owning user; gateway actor
      from context.
- [x] T5.2 Quotas: `tasks_per_user_per_day` at enqueue; per-session rate
      limit.
- [x] T5.3 Phase gate + commit + push.

## Phase 6 — consoles, docs

- [x] T6.1 Web + desktop sign-in via providers list (Microsoft enabled,
      others disabled).
- [x] T6.2 Desktop Telegram settings pane (link/unlink, status).
- [x] T6.3 Mobile login if the client source is in-repo.
- [x] T6.4 Docs updates (deployment, api, connectors, README, spec status).
- [x] T6.5 Final full gate (Windows + WSL), push, PR for owner review -
      merge only on the owner's OK. Merged as PR #9 (b863143) after the
      Windows, Linux, desktop, and mobile CI checks passed.

## Releases 0.39-0.48 — hardening follow-up

- [x] T7.1 Logging: noisy-crate caps, shared default filter, per-layer ring
      filter, `[logging]` durable owner-only directory (0.48.0).
- [x] T7.2 Approval decision refuses an expired approval before the sweep.
- [x] T7.3 Negative tests: job tools, workflows/routines, preview, vault
      autofill, approval binding (args/expiry/user/destination).
- [x] T7.4 Spot-check: workflow/routine rows are `user_id`-scoped; artifact
      preview is gated by `user_owns_artifact` and a 64-hex hash; jobfinder
      `require_approval = []` is consistent (no outbound-effect tools).

## Release 0.49.0 — providers, accounts, routes

- [x] P1 typed errors/classifier; P2 account store; P3 accounts API; P4 sign-in framework.
- [x] P5-P6c Antigravity member sign-in, wire parity, catalog, quota.
- [x] P7a/P7b Muse device sign-in and Responses transport.
- [x] P8 thinking catalog; P9a/P9b Anthropic Messages and thinking; P10 OpenCode/Nous.
- [x] P11 single-flight refresh; P12 bot routes; P13 failover engine.
- [x] P14-P16 desktop Providers page, route editor, composer chip.
- [x] P17 Claude plan OAuth (admin-only; default on since 0.50.1, `plan_oauth = false` turns it off).
- [x] P18 docs, release notes, version 0.49.0.
- [ ] P19 (optional) Mistral sign-in.
- [ ] Live verification against owner-approved accounts (none claimed).

## Release 0.50.0 — orchestration

- [x] B1 fleet mutation path; B2 management tools; B3 handoff ledger.
- [x] B4 `needs_review`; B5 `waiting_model`; B6 auto-resume; B8 compaction and continue.
- [x] B7 fairness caps and load fixture (passed at 2, 4, 8 workers locally); B9 orchestration events.
- [x] C1 virtualized feed; C2 event coalescing; C3 runs sidebar; C4 composer queue; C5 failure and wait UX.
- [x] C6 motion/accessibility pass (transition audit, focus, contrast computed, reduced-motion fix for remounted rows).
- [x] C7 docs, release notes, version 0.50.0.
- [ ] Visual screenshot review (light and dark) of chat, runs sidebar and composer on a device.
- [ ] Load fixture on the target host before raising workers above 2.
- [ ] Live verification against owner-approved accounts (none claimed).

## Release 0.51.0 — marketplace, GitHub, Codex, Copilot

- [x] M1 manifest schema v2; M2 MCP OAuth; M3 API and health; M4 runtime auth and annotations.
- [x] M5 native GitHub connector (device flow, hosted MCP) and M8 `suggest_marketplace_app`.
- [x] M6 real hosts: Linear, Notion, Sentry, Vercel, Stripe (read only), PostHog, Context7, Atlassian.
- [x] M7 desktop marketplace accounts, sign-in, tool switches, GitHub card.
- [x] Codex (ChatGPT plan) sign-in: browser PKCE and device code.
- [x] GitHub Copilot sign-in: device code, token exchange, several accounts.
- [x] M9 docs, release notes, version 0.51.0.
- [ ] Canva manifest (`mcp.canva.com/mcp`; not shipped).
- [ ] Live owner verification: each MCP host, GitHub App device flow and hosted MCP terms, Codex, Copilot (none claimed).
- [ ] Copilot `auto` model, Responses-only models, reasoning effort, quota meter; Codex reasoning-item replay and usage meter.
- [ ] Visual review of the desktop marketplace screens on a device.
- [ ] Mistral sign-in: not wanted (owner decision).

## Release 0.52.0 — sign-in fixes, AI providers page, chat redesign, marketplace bots

- [x] Claude: console save without a key while plan sign-in is on; live profile for sign-in gating; `plan_signin` reason.
- [x] Antigravity: refuse sign-in without a Google client secret (`signin_setup`), admin field to save it, CI build secret.
- [x] `pending_restart` and the restart prompt after a sign-in on a new provider.
- [x] Codex checked end to end.
- [x] AI providers page (rename and rebuild); chat sidebar, topbar, transcript, composer and All chats redesign.
- [x] Catalog-only bots: researcher, writer, translator, planner, support, product, tutor; Bots tab card grid.
- [ ] Owner: add the `SERABOT_AGY_CLIENT_SECRET` repository secret (or paste it on the AI providers page).
- [ ] Version bump to 0.52.0 when the owner wants the release.
- [ ] Live owner verification of Claude, Antigravity, Codex and Copilot sign-in (none claimed).
- [ ] Visual review of the redesigned screens on a Windows device.
- [ ] Optional: rebuild the model router when the console saves a provider, so no restart is needed.

