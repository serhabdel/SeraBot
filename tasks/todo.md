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
- [x] Version bump to 0.52.0 (owner approved the release).
- [ ] Live owner verification of Claude, Antigravity, Codex and Copilot sign-in (none claimed).
- [ ] Visual review of the redesigned screens on a Windows device.
- [ ] Optional: rebuild the model router when the console saves a provider, so no restart is needed.

## Release 0.53.0 — consistency, motion, live providers

- [x] Shared, swappable model router (`RouterHandle`) and `ModelRouter::rebuilt`; console provider saves and removals apply at once.
- [x] `assets/system.css`: cards, stat tiles, segmented controls, skeletons, list motion, focus rings, reduced motion.
- [x] Page polish: Tasks, Workflows, Logs, Memory, Vault, Usage, Settings, Integrations, Marketplace, Browser, sheets.
- [ ] Visual review on a Windows device (light and dark), including Kanban and Calendar views.
- [ ] Settings panes beyond Account and Appearance, the Vault form and import picker: markup polish.
- [x] Marketplace "Coming soon" filter label updated to "Available" now that live plugins run.

## Release 0.53.1 — Antigravity parity with HielCowork

- [x] Redirect `http://localhost:<port>/oauth-callback`; desktop and CLI listen on `127.0.0.1` and `[::1]`.
- [x] `AGY_CLIENT_ID` override for sign-in and refresh.
- [x] Live Hub version (`client_version`, `AGY_VERSION`, updater, pin).
- [x] One retry of plain `claude-opus-5-5` as `claude-opus-5-5-thinking` on 400/404.
- [x] Configured Antigravity models join the verified live list.
- [x] Idle read timeout for model calls (long thinking answers no longer cut at 120 s); 15-minute cap per call.
- [x] Chat says why a model call failed (status and type, never the body) and when a parked turn resumes.
- [x] No `anthropic-beta` header to Cloud Code.
- [ ] Owner: sign in to Antigravity once from the desktop and send one chat (none claimed).
- [x] Owner decision: Claude Code through ACP on the desktop (approved; shipped in 0.54.0).
- [ ] Owner: add the `SERABOT_AGY_CLIENT_SECRET` repository secret (or paste it on the AI providers page).

## Release 0.54.0 — Claude Code through ACP

- [x] ACP client (protocol, process, file fence, permission cards, transcript) with synthetic-agent tests.
- [x] Setup: Node.js check and winget install, folder choice, Claude Code's own sign-in.
- [x] Claude Code page in the desktop; the agent keeps running across pages.
- [ ] Owner: run it once on Windows against the real adapter (none claimed).
- [ ] Later: keep transcripts across restarts; images and @-file mentions in prompts.

## Release 0.55.0 — bot-integrated Claude Agent, Antigravity parity, chat responsiveness, visual polish

- [x] Remove standalone `Section::ClaudeCode` tab from `IconRail` and `Shell`; integrate local Claude Code ACP mode directly into bot chats (`ClaudeAgentBotView`, Topbar toggle, `ModelBar`, and `RouteEditor`), with per-bot persisted transcripts (`acp-transcripts/<bot_id>.json`), per-bot preferred desktop folders, cached Node probe, and one-time persona context injection.
- [x] Antigravity provider parity with Hiel-Cowork: incremental `CloudCodeStreamReader` SSE streaming via `response.bytes_stream()`, runtime `loadCodeAssist`/`onboardUser` project discovery when `project_id` is empty, catalog fallback on HTTP 400, consistent `AGY_CLIENT_ID`, empty-turns fallback, and `cachedContentTokenCount` subtraction.
- [x] Chat UI freeze & speed fixes: keep `ChatView` mounted across conversation switches (no destructive key remount), coordinate `CHAT_SCROLL_BRIDGE_JS` and `feed_window_js` resets on `data-conv` changes, replace owner-scoped clones with `std::mem::take` in `State::new_scope`, and offload blocking disk/keyring writes + `LocalAgent::start_with_credentials` construction onto background `PersistWorker`.
- [x] Studio workspace redesign: grouped `IconRail` (Work / Data / Connect / System), `Ctrl+K` `CommandPaletteSheet`, updated `Marketplace` `Available` filter + `"Not live yet"` copy, and elevated glassmorphic surfaces in `assets/system.css`, `assets/claude-code.css`, and `ConnectView`.
- [ ] Owner: live verification of Antigravity provider and bot-integrated Claude Agent (ACP) on Windows.

## Release 0.56.0 — composer & sidebar fixes, crisp UI, built-in 20-bot / 37-skill fallback

- [x] Fix `#composer-input` draft wipe in `ChatView` via `should_sync_external_composer` and harden caret restoration across streaming repaints with `window.__serabotComposerGen`.
- [x] Right-align sidebar `NewMenu`, add explicit `r#type: "button"` attributes, raise `.sidebar` above `.main`, and remove WebView2 `backdrop-filter` blur fuzziness in `assets/system.css`.
- [x] Compile all 20 shipped bots/prompts and 37 shipped skills into `crates/serabot/src/shipped.rs` and merge them behind disk overrides in `load_bots_with_builtin` and `SkillCatalog::load_with_builtin`.
- [ ] Owner: verify composer input, sidebar `+ New chat` crispness/menu, and 20-bot Marketplace catalog on Windows + upgraded server.

## Release 0.58.0 — basic tools, readable attachments, projects, WhatsApp linked device

- [x] Every bot gets document, OCR, audio, image, local-file and connector tools (`SHARED_TOOLS`).
- [x] Scanned PDF attachments point to `document_index` (OCR); handoffs inherit attachments.
- [x] Per-user project folders (`project_list`, `project_create`, `project_file`).
- [x] rusqlite 0.39 upgrade; WhatsApp linked device (server, API, desktop card).
- [ ] Owner: pair a spare number and verify WhatsApp live; turn on `[ocr]` and `[logging] dir`.

## Release 0.57.0 — logos, Logs fix, immediate Stop, Chat UX polish & WhatsApp Cloud API

- [x] Official Anthropic (`SVG_ANTHROPIC`), Claude (`SVG_CLAUDE`), and WhatsApp (`SVG_WHATSAPP`) brand SVGs; `BrandLogo` used in Claude Agent banner and Command Palette.
- [x] Fix empty desktop Logs tab: preserve `RingLayer` + stdout in `logging::install` when `log_dir` cannot be created, and distinguish `logs_error` from empty logs with Retry in `ui_logs.rs`.
- [x] Immediate Stop chat cancellation: server `CancellationToken` + `tokio::select!` aborts slow model/compaction/`limit_answer` calls, cancels queued/waiting tasks atomically, and closes pre-turn claim races; desktop enables Stop during optimistic `opt-` sends with `"Stopping…"` feedback.
- [x] Chat UX polish: per-bot `quick_chips(bot_id)` starter prompts and `"Stopping…"` / Escape-to-stop responsiveness.
- [x] Native WhatsApp Business Cloud API connector (`token_connectors/whatsapp.rs`, `whatsapp_*` gateway tools, `bots/whatsapp.toml`, `prompts/whatsapp.txt`, live `Test` endpoint, and 2-field + optional WABA ID Integrations UI card).
- [ ] Owner: verify WhatsApp Cloud API connector with a real Meta Phone Number ID + System User token and review desktop UI/logs/stop behavior on Windows.

## Release 0.59.0 — Projects UI, multimodal vision, drag-and-drop, Copilot/Codex parity, ACP prompts & WhatsApp link enabled

- [x] Projects end-to-end: fix `project_file` attachment relocation updating `chat_attachments.rel_path`, add user-scoped `/v1/projects/*` HTTP API (`api/projects.rs`), and add desktop `ProjectsView` (`Section::Projects`).
- [x] Real multimodal image vision (`LlmImage`, `load_images_for_model`) across OpenAI-compat, Anthropic, Responses, and Antigravity/Cloud Code transports, plus bounded inline Office excerpts (`.docx`/`.xlsx`/`.pptx`).
- [x] Desktop chat drag-and-drop upload bridge (`ui_files.rs`), 48h session-expiry warning banner (`SessionExpiryBanner`), and conversation Markdown export (`Topbar`).
- [x] Provider quota readers for `github-copilot` and `openai-codex`, Copilot `auto` model + Responses-only routing + `reasoning_effort`, and Responses reasoning-item capture/replay (`resp:`).
- [x] Local Claude Code (ACP) `@`-file mentions scoped to the allowed folder and base64 image prompt blocks (`PromptImage`).
- [x] Enable `[whatsapp_link] enabled = true` by default in `config.rs`, `serabot.toml`, and `deploy/serabot.production.toml.example` (still opt-out via `enabled = false`, no widening).
- [ ] Owner: live verification of WhatsApp linked-device pairing, image vision, Copilot/Codex quotas, and ACP `@`-file / image prompts on Windows + upgraded server.

## Release 0.60.0 — Read-only FS fallback, scanned PDF OCR/vision, Antigravity Gemini & thinking levels, default bot tools & Chat/Jump UX

- [x] Writable runtime data-dir fallback (`Config::ensure_writable_dir`) beside `database_path` so WhatsApp Link (`/v1/whatsapp-link/pair`) and daily file logging work under systemd `ProtectSystem=strict` instead of failing with `os error 30` / HTTP 500.
- [x] Per-user Mistral OCR credential resolution (`ocr::recognize_bytes_for_user`) for `document_index` plus pure-Rust embedded JPEG/PNG page extraction (`documents::extract_pdf_images`) for inline scanned-PDF vision in chat.
- [x] Antigravity (Cloud Code) pure Gemini fix (`antigravity_thinking` omits forced `thinkingLevel` when `None`, adds `gemini-2.5-*` `thinkingBudget`, retries once without `thinkingConfig` on HTTP 400, preserves primary errors over sandbox 403s, and logs sanitized upstream error details at `warn`).
- [x] Thinking levels per model/provider (`thinking::levels_for`: added `github-copilot`, expanded `mistral`, refined `google-antigravity` Gemini levels) + instant desktop composer thinking-level refresh on model switch.
- [x] All built-in non-admin tools enabled by default for every bot (`SHARED_TOOLS` includes browser automation, `watch_now`, `firecrawl_crawl`, `timesheet_analyze`) and verbose bot ability toggles moved under collapsed `Advanced` in right-sidebar Bot Settings.
- [x] Desktop UX fixes: Command Palette (`Ctrl+K`) transform-free centering & dark scrim under reduced-motion/transparency, unclipped in-flow `.msg-actions` on older messages, crisp user attachment pills, floating `Jump to latest`, aligned jobs panel, and compact toast pills.
- [ ] Owner: live verification of WhatsApp pairing, scanned PDF OCR/vision, and Antigravity pure Gemini models against the upgraded server.



## Release 0.61.0 — Chat window: per-reply turn traces, visible memory, token type, one chat stylesheet

- [x] Per-reply turn traces: desktop `turn_trace.rs` ledger keyed by `turn_id`, fed live and from `/v1/conversations/{id}/activity`; collapsed steps + memory line inside the reply.
- [x] `turn.memory` event (recalled at turn start, saved after extraction; bounded, scanned, user-scoped) and dated, sourced injected fact lines.
- [x] Chat chrome font sizes on `--fs-*` tokens; Small floor 11px.
- [x] Chat transcript CSS merged into one block in `system.css`; light-theme user bubble contrast fixed (1.41:1 → 12.25:1).
- [x] Reply measure capped at 72ch.
- [ ] Owner: look at the chat on Windows (light and dark, Small/Large text) and run one turn that uses and saves a memory.
- [ ] Next: embedding-based memory recall alongside FTS5 (keyword-only recall misses paraphrases).
