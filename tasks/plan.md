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

## Status after 0.47.0 (hardening follow-up, 0.48.0)

Phases 1-6 shipped; releases 0.39-0.47 added desktop/mobile UX, marketplace
base (`RUNTIME_CALLS_PLUGINS = true` for live `http` MCP plugins), OAuth
wave 8, vault providers, workflow studio, and the `jobfinder` bot. 0.48.0
hardens logging (`[logging]`, noisy-crate caps), rejects expired approvals at
decision time, and adds negative isolation/approval tests. Future releases
should split server/security changes from UI churn and add wrong-user,
missing-approval and stale-approval tests alongside each new surface.

## Status after 0.48.0 (providers, 0.49.0)

0.49.0 adds per-user provider accounts (migration 0037), the sign-in flow
framework, Antigravity and Muse member sign-in, Anthropic/OpenCode backends,
single-flight refresh, bot routes with a failover engine, and the matching
desktop pages. Claude plan OAuth is implemented, admin-only, on by default since 0.50.1 (owner decision, supersedes O1; `plan_oauth = false` turns it off).
Optional P19 (Mistral sign-in) is not done. No live provider was used.

## Status after 0.49.0 (orchestration, 0.50.0)

0.50.0 adds the single fleet mutation path and `main`'s management tools,
the handoff ledger (migration 0038) with auto-resume, `waiting_model` parking,
context compaction, `needs_review` for unknown effects, per-user fairness
(`max_turns_per_user`, `max_active_children`, per-account inflight caps), and
the desktop virtualized feed, event coalescing, runs sidebar, composer queue,
failure and wait cards, plus a motion/accessibility pass. No live provider was
used and the desktop was not visually verified on a device. B7 worker counts
above 2 are gated on running the load fixture on the target host.

## Status after 0.58.0 (basic tools, readable attachments, projects, WhatsApp linked device)

0.58.0 gives every bot the shared document, OCR, audio, image, local-file and
connector tools, makes attached PDFs readable (OCR hint, attachments passed to
handoffs), adds per-user project folders, and adds WhatsApp as a linked device
of a spare number (`docs/specs/whatsapp-linked-device.md`), off by default and
not yet verified live.

## Status after 0.57.0 (official logos, Logs fix, Immediate Stop, Chat UX polish & WhatsApp Cloud API)

0.57.0 adds official Anthropic, Claude, and WhatsApp SVG logos in
`crates/serabot-desktop/src/branding.rs`; fixes empty desktop Logs by keeping
`RingLayer` active when the log directory is unwritable and surfacing
`logs_error` explicitly in `ui_logs.rs`; makes Stop chat abort in-flight model
calls, compaction, and queued/early-claimed tasks immediately via
`CancellationToken` + `tokio::select!`; adds per-bot starter quick-chips in
`chat_feed.rs`; and connects the `whatsapp` bot to a native per-user WhatsApp
Business Cloud API connector (`token_connectors/whatsapp.rs`) with
approval-gated send tools and a dedicated multi-field Integrations card.
Tested against synthetic fixtures and `ui-check`; live Meta Graph API and
desktop visual verification remain pending owner testing.

## Status after 0.56.0 (composer, sidebar New chat & crisp chrome, built-in 20-bot fallback)

0.56.0 fixes the desktop composer self-wipe regression via generation-checked
caret restoration and `should_sync_external_composer`, aligns the sidebar
`NewMenu` right (`right: true`) above `.main` with explicit button types, removes
WebView2 `backdrop-filter` blur fuzziness across static chrome in `system.css`,
and compiles all 20 shipped bots/prompts and 37 skills into
`crates/serabot/src/shipped.rs` as a disk-override-friendly fallback so binary-only
server upgrades still serve the complete 20-bot Marketplace Bots catalog.

## Status after 0.55.0 (bot-integrated Claude Agent, Antigravity SSE, chat stability & Studio UI)

0.55.0 removes the standalone Claude Code rail tab and integrates local Claude
Code (ACP) directly into per-bot chat and route configuration with per-bot
persisted transcripts, folder preferences, and persona context; adds incremental
SSE streaming (`CloudCodeStreamReader`), `loadCodeAssist`/`onboardUser` project
discovery, and parity fixes for `google-antigravity`; stabilizes chat switching
by keeping `ChatView` mounted with `data-conv`-coordinated scroll/feed resets and
background `PersistWorker` writes; and introduces the grouped `IconRail`,
`Ctrl+K` `CommandPaletteSheet`, Marketplace `Available` filter, and Studio
surface styling. Tested against synthetic fixtures and `ui-check`; live Windows
ACP and live Antigravity verification stay pending owner testing.

## Status after 0.54.0 (Claude Code through ACP)

0.54.0 adds a desktop Claude Code page that runs the real Claude Code through
the Agent Client Protocol with the person's own Claude login, one allowed
folder and a permission card for every edit or command, and finishes the
0.53.1 model-call fixes (idle timeouts, visible failures). Synthetic agent
tests and a static render only; not yet run against the real adapter.

## Status after 0.53.1 (Antigravity parity with HielCowork)

0.53.1 moves the Antigravity redirect to `localhost` with IPv4 and IPv6
loopback listeners, adds `AGY_CLIENT_ID`, presents the live Hub version, retries
plain Claude Opus once under its wire id, and keeps configured models in the
verified live list. Synthetic fixtures only; the owner signs in once to verify.

## Status after 0.59.0 (Projects UI, vision, drag-and-drop, Copilot/Codex, ACP, WhatsApp enabled)

0.59.0 completes Projects end-to-end (relocation fix, `/v1/projects/*` API,
desktop `ProjectsView`), adds real multimodal image vision across all four wire
transports plus inline Office excerpts, adds chat drag-and-drop uploads,
session-expiry warnings, conversation Markdown export, Copilot/Codex quota
readers & Responses reasoning replay, ACP `@`-file mentions & image blocks, and
enables `[whatsapp_link]` by default per owner approval (`enabled = false`
turns it off). Synthetic fixtures only; no live provider or WhatsApp account
was used.

## Status after 0.53.0 (consistency, motion, live providers)

0.53.0 shares one swappable model router across the runtime and the tools that
read it, so console provider saves and removals apply without a restart, adds
the shared surface system (`assets/system.css`), and polishes every remaining
console page. Checked against synthetic fixtures and a local render only.

## Status after 0.52.0 (sign-in fixes, design, marketplace bots)

0.52.0 fixes Claude plan sign-in from the console (no key needed while plan
sign-in is on), makes Antigravity refuse to start without a Google client
secret and lets the admin save one, reports `plan_signin`, `signin_setup` and
`pending_restart` on provider listings, rebuilds the AI providers page, redesigns
the chat sidebar, topbar, transcript, composer and All chats list, and adds seven
catalog-only bots to the marketplace. Synthetic fixtures only; the owner adds the
`SERABOT_AGY_CLIENT_SECRET` repository secret for the deploy build.

## Status after 0.50.0 (marketplace, providers, 0.51.0)

0.51.0 adds marketplace manifest schema v2, MCP OAuth (`mcp_oauth.rs`) with
per-member accounts (migration 0039) and per-tool policy (migration 0040),
runtime bearer injection and read-only annotations, eight real remote MCP host
manifests, a native GitHub connector (device flow), `suggest_marketplace_app`,
the desktop accounts, sign-in and tool-switch UI, and ChatGPT (Codex) and GitHub
Copilot plan sign-in (on by default with a risk banner, per owner decision;
Mistral sign-in is not wanted). Canva is not shipped. Everything was checked
against synthetic fixtures; no live host, GitHub App, ChatGPT or Copilot account
was used, so the owner must run each before it is called working.

## Status after 0.70.0 (UI/UX audit, 0.71.0)

0.71.0 consolidates the desktop chat stylesheet into one owner per rule in
the `system.css` chat block (feed container, composer wrap, message actions),
moves the command palette onto the `--fs-*` text-size tokens, removes
`backdrop-filter` rules that `system.css` already overrides, fixes the chat
switcher card (focus on the open chat, capped scrollable menu, idempotent
overflow flip, caret-stable rename), and polishes the Usage page (shared
`stat-tile`, section anchors with an updated-ago stamp, chart day labels).
Source-level audit only; no render was taken, so the owner does a visual pass
on Windows. No server changes, no new dependencies.

## Status after 0.71.0 (audit follow-ups, 0.72.0)

0.72.0 finishes the 0.71.0 audit: the remaining hard-coded font sizes in the
chat, sidebar, composer and chat-switcher chrome are on the `--fs-*` tokens,
the sidebar and All-chats rename inputs are caret-stable (uncontrolled, like
the switcher), and switcher rows show unread badges. Desktop-only, no server
changes. Source-level only; owner visual pass pending. Tasks/Kanban/Calendar
pages still carry px font sizes — noted for a later pass.

## Status after 0.72.0 (coding harness + context + font sweep, 0.73.0)

0.73.0 adds the project check harness: per-project owner-configured check
commands (allowlisted `cargo`/`rustc`, contained project cwd, no shell, capped
output, hard timeout) whose rustc-JSON diagnostics are stored UID-scoped and
shown in a desktop Problems panel; the `project_check` bot tool is
approval-gated. It also emits `turn.context` (estimated prompt size, model
window, percent) at turn start, shown as a context meter in the model bar, and
moves every remaining hard-coded px font-size in the desktop CSS onto the
`--fs-*` tokens. Synthetic fixtures only; no real compiler ran and no render
was taken — owner verification pending.

## Status after 0.73.0 (code workspace, 0.74.0)

0.74.0 adds a code workspace to the Projects page: file tree, caret-stable
editor with line gutter, save through a new contained `PUT
/v1/projects/{slug}/files/{*path}` endpoint (audit-logged), and Problems
click-through that opens the file at the diagnostic's line. It also ships
`docs/specs/lsp-workspace.md`, a proposed rust-analyzer LSP design for 0.75
that widens the local-process exception and therefore waits for owner
approval. Queued: 0.76 memory v2, 0.77 phone parity.
