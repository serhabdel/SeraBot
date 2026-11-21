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
- [x] Next: embedding-based memory recall alongside FTS5 (done in 0.62.0).

## Release 0.62.0 — Semantic memory recall, contradiction check

- [x] `memory_vectors` (migration 0042): local E5 vector per fact; recall, `memory_search`/`memory_list` and console search use `max(lexical, semantic)`.
- [x] `runtime/conflicts.rs`: post-turn contradiction check (bounded judge, ids validated); retire the older fact, or hold an extracted guess that disagrees with the owner.
- [x] `turn.memory` saved covers `memory_save`; `replaces` shown in the reply trace.
- [x] Chat polish: no duplicate "Running" label; transcript padding clears the composer fade.
- [ ] Owner: run with the real E5 cache — check a paraphrase recall and one changed decision in a live turn.
- [ ] Later: private per-bot memory scope; a contradiction check for facts added from the console.

## Release 0.63.0 — WhatsApp bot, requested files, model chip, grouped sidebar, bot Library

- [x] WhatsApp tools scoped to the WhatsApp bot; Business tools only when connected; separate WhatsApp / WhatsApp Business cards.
- [x] `whatsapp_link_reply_file` and `telegram_reply_file` (requested files back into the same chat, not approval-gated); daily WhatsApp cap; device shows as Chrome on Windows.
- [x] Model picker moved to the chat header; Memory section inside bot settings; sidebar Pinned/Recent/All; bot Library with filters and item dialog.
- [ ] Owner: sign out and re-link WhatsApp (new device name); from WhatsApp, ask for a file another bot made; check the Library and the header chip on Windows.

## Release 0.64.0 — WhatsApp voice notes, visibility, no approval for allowed chats

- [x] Per-user transcription fallback (member's Mistral account) for WhatsApp, Telegram and console dictation; WhatsApp keeps an untranscribed voice note as an attachment and tells the bot.
- [x] WhatsApp chats listed under the WhatsApp bot ("WhatsApp · +number"); read-only `whatsapp_link_chats` tool.
- [x] `whatsapp_link_send` off the approval list (allowlist + per-minute/daily caps); spec, approvals doc, AGENTS.md updated.
- [ ] Owner: send a voice note on WhatsApp (with a Mistral account connected if server audio is off) and check the bot's answer.

## Release 0.65.0 — WhatsApp LID-addressed chats, desktop relay design

- [x] Inbound WhatsApp accepts LID-addressed chats via sender_alt / recipient_alt / LID-to-phone map (messages were silently dropped).
- [x] `docs/specs/desktop-network-relay.md`: proposed CONNECT-over-WebSocket relay through the desktop (TLS end to end, host allowlist, private-address refusal, opt-in).
- [ ] Owner: write to the WhatsApp number from an allowed phone; the bot should answer within seconds.
- [ ] Owner: answer the relay open questions; build in 0.66.

## Release 0.66.0 — Desktop network relay, WhatsApp idle presence

- [x] Desktop network relay: loopback CONNECT + long-poll exchange, per-provider opt-in, host list on both sides, public addresses only, audited; Settings card.
- [x] WhatsApp presence: offline when idle, online + "typing…" on a new message, back to offline after `presence_idle_secs`.
- [ ] Owner: turn the relay on for Antigravity with the desktop open and run one chat; check the card shows traffic.
- [ ] Owner: check the WhatsApp number shows offline when idle and "typing…" on a message.

## Release 0.67.0 — Usage insights

- [x] Turns tagged by client (`X-SeraBot-Client`, migration 0044); channels split desktop / phone / Telegram / WhatsApp / API / schedules.
- [x] `model.used` audit records `latency_ms` and `relayed`; per-model average and p95 latency, share via PC, failovers.
- [x] PC tunnel card: traffic per day and per host, share of model calls relayed, live status.
- [x] Server load: CPU and memory sampled every 5 minutes, 24 hours in memory, owner only.
- [ ] Owner: send a message from the desktop and the phone; check both channels appear on the Usage tab.

## Release 0.68.0 — Alerts, backups, voice replies, office steps, phone app catch-up

- [x] Alerts (`ops.rs`): failover streaks, expired sign-ins, PC tunnel drop during a turn, disk/memory (owner), failed backup (owner); cooldown, audited; Telegram and one chosen WhatsApp chat.
- [x] Nightly backup: VACUUM INTO, age-encrypted with the server key, test restore (integrity check), optional OneDrive copy, pruned; Usage tab card with "Back up and test now".
- [x] Voice replies: `/auto` mode (voice answer to a voice note), WhatsApp audio replies, member's own Mistral key as fallback.
- [x] Office steps: specific labels per document tool and change counts / ranges on the step card; file errors carry a next step (locked, moved, not allowed, too big, protected, damaged) for the card and the model; desktop local file errors say why.
- [x] Maker, analyst and comptable prompts: look before editing, report changes, act on file error hints.
- [x] Phone app 0.68: Usage insights, bot Library, memories in reply traces, approval alerts and badge.
- [ ] Owner: link Telegram, pick a WhatsApp alert chat, and run "Back up and test now" once; check the OneDrive copy.
- [ ] Owner: send `/auto` on WhatsApp, then a voice note; the answer should come back as audio plus text.

## Release 0.69.0 — Image tools, WhatsApp photos, own-chat sends, costs, digest

- [x] Image tools: image_crop (pixels, percent, aspect, trim), image_rotate/flip, image_adjust (brightness, contrast, grayscale, black-and-white), image_compress (target KB), images_to_pdf, pdf_to_images.
- [x] WhatsApp bot has the image and PDF tools; photos sent in a row reach it as one message (up to 10); it describes photos and offers crop / PDF / read.
- [x] `whatsapp_link_reply_file` takes an optional `to` (another allowed chat); `telegram_send_document` to the member's own chat runs without approval while "send files to my own chats without asking" is on (daily cap, audited).
- [x] Maker: PC images via local_import → edit → result back in the chat or OneDrive (bots never write to the PC).
- [x] Costs: owner price table, member monthly budget with alerts at 80/100%, "Costs this month" card; weekly digest (Monday 08:00 UTC) with a "send me now" preview.
- [x] Desktop: "Try another way" on failed steps.
- [ ] Owner: send 3 photos on WhatsApp, ask "crop them and make one PDF"; check the PDF comes back.
- [ ] Owner: add a few prices (Settings: Usage → Costs → Edit prices) and set a budget.

## Release 0.70.0 — Jobs, browser, brief/triage/review, research, bot health, costs, phone

- [x] Costs: cached tokens, cache price, suggested (unverified) prices, M-token totals, plans vs API (0047).
- [x] Job finder: interview, Moroccan site watching with robots.txt, matches to Telegram/WhatsApp, pipeline, OneDrive tracker via onedrive_download (0048).
- [x] Browser: actionability checks, banner dismissal, find/wait/select/check/scroll/hover/download/pdf/upload, snapshot modes, persistent logins.
- [x] Morning brief, email triage, memory review (0049); research reports; bot health; phone app 0.70.
- [ ] Owner: check the built-in job source URLs on the server; run "Check now".
- [ ] Owner: load suggested prices, verify, save; enter plan prices.
- [ ] Owner: try browser find/wait/banners on a real site.

## Release 0.71.0 — UI/UX audit: chat stylesheet consolidation, chat switcher, Usage page

- [x] Chat CSS single-owner: `.messages` / `.composer-wrap`(+`::before`) / `.msg-actions` defined once in the system.css chat block; app.css duplicates removed; palette on `--fs-*` tokens; dead `backdrop-filter` removed for `.menu`/`.connect-card`/`.composer`.
- [x] Chat switcher card: focus open chat on open, menu max-height+scroll, idempotent flip JS, caret-stable rename input.
- [x] Usage page: `stat-tile` unified (old `usage-tile*` deleted), section anchor chips + "Updated Ns ago", chart day labels.
- [x] Version bump 0.71.0, release notes, review-doc addendum.
- [ ] Owner: visual pass on Windows (chat light/dark + text sizes, switcher with several chats, Usage page scroll).

## Release 0.72.0 — audit follow-ups: font tokens, caret-stable renames, switcher unread badges

- [x] 23 hard-coded chat/sidebar/composer/switcher font sizes in app.css onto `--fs-*` tokens.
- [x] Sidebar + All-chats rename inputs uncontrolled (caret-stable), same pattern as the 0.71 switcher fix.
- [x] Chat switcher rows show unread badges.
- [x] Version bump 0.72.0, release notes, review-doc addendum.
- [ ] Owner: visual pass on Windows (text sizes in chat/sidebar, rename in place, switcher badges).
- [ ] Later: Tasks/Kanban/Calendar pages still have hard-coded px font sizes.

## Release 0.73.0 — project check harness, turn context, console-wide font tokens

- [x] Server: diagnostics harness (migration 0050, `diagnostics.rs`): owner-set allowlisted check command per project, contained cwd, capped output, 120s timeout, rustc-JSON parsing, UID-scoped storage, audit; API `check` get/put, `check/run`, `diagnostics`; approval-gated `project_check` bot tool (SHARED_TOOLS, DESTRUCTIVE_TOOLS, docs/approvals.md).
- [x] Desktop: Problems panel on Projects (counts, run button, command editor, diagnostics list); API client + helpers.
- [x] `turn.context` event at turn start (est chars, window, percent) + context meter in the model bar.
- [x] All remaining hard-coded px font-sizes in desktop CSS onto `--fs-*` tokens (0 left).
- [x] Version bump 0.73.0, release notes.
- [ ] Owner: run "Run checks" on a real project on the server; visual pass on Windows (Problems panel, context meter, text sizes).

## Queued after 0.74.0

- [ ] 0.75 LSP workspace (rust-analyzer, desktop-only): live diagnostics on save into the Problems panel, hover, go-to-definition; then references + rename with gated diff apply. Spec: `docs/specs/lsp-workspace.md` — **needs owner approval first** (widens the local-process exception).
- [ ] 0.76 Memory architecture v2: per-bot private memory scope; contradiction check for facts added from the console (deferred since 0.62).
- [ ] 0.77 Phone parity wave: Problems panel, context meter, switcher badges, Usage anchors on the Flutter app (`apps/serabot_mobile`).

## Release 0.74.0 — code workspace (editor + Problems click-through) + LSP spec

- [x] Server: `PUT /v1/projects/{slug}/files/{*path}` write endpoint (containment, 1 MiB cap, audit, policy-scanned path) + tests.
- [x] Desktop: workspace on Projects page — file tree, caret-stable editor with line gutter + synced scroll, Save, Problems click-through to file:line.
- [x] `docs/specs/lsp-workspace.md` (proposed; owner approval needed before 0.75 code).
- [x] Version bump 0.74.0, release notes.
- [ ] Owner: open a project on Windows, edit + save a file, run checks, click a problem to its line.

## Release 0.74.1 — OneDrive full drive + skill, browser PDF downloads, mail connector (SMTP/IMAP), UI redesigns & README

- [x] Release batch rule added to `AGENTS.md` (0.74.1, 0.74.2, … patch batches until owner is satisfied).
- [x] OneDrive (`onedrive.rs`, `maker.rs`, `skills/onedrive/SKILL.md`, `bots/*.toml`): drive-root `onedrive_list` (`path`), `onedrive_search`, `onedrive_recent`, `onedrive_info`, `onedrive_download` reader hints, approval-gated `onedrive_upload` to drive-root `folder` or dossier `subfolder`, `OneDrive connection` skill, and read tools on `main`, `analyst`, `researcher`, `maker`, `jobfinder`.
- [x] Browser downloads (`browser/chromium/actions.rs`, `browser/tools_ext.rs`, `docs/browser-downloads.md`): CDP `Network.loadNetworkResource` + `IO.read` fallback when clicks open Chromium's PDF viewer or a new tab, `url` and `current: true` modes on `browser_download`, reader hints + untrusted-fenced first-page PDF preview.
- [x] Native SMTP/IMAP mail connector integrated from PR #134 (`crates/serabot/src/mail.rs`, `api/mail.rs`): per-user credentials in `user_secrets` (`mail`), hand-rolled SMTP/IMAP over `tokio-rustls`, `mail_send` (approval-gated), `mail_list`, `mail_read`, `/v1/mail` connect/status/disconnect.
- [x] Desktop UI redesigns: solid searchable/grouped Conversation history popover + header History button + sidebar Recent chats (`ui/row_menu.rs`, `ui/shell.rs`, `bot_chats.rs`), Workflows page (`ui_workflows.rs`, `workflows.rs`), Projects page (`ui_projects.rs`, `projects.rs`), Memory page (`ui/memory.rs`, `ui_memory_review.rs`).
- [x] Comprehensive `README.md` update and `docs/release-0.74.1.md`.
- [x] Desktop Integrations card for mail connect/status/disconnect (shipped in `0.74.2`).
- [ ] Owner: live verification on Windows (OneDrive root browse/search/upload, browser PDF download, Hostinger SMTP/IMAP connect/send/read, visual pass on History/Workflows/Projects/Memory).

## Release 0.74.2 — Mail desktop card & skill, full-text conversation search, Tasks/Kanban/Calendar polish & cleanup

- [x] Mail connector completion: desktop Integrations card with Hostinger preset + custom SMTP/IMAP form + live test badges + disconnect (`ui_connectors.rs`, `ui_integrations.rs`, `api.rs`), `skills/mail/SKILL.md` (`shipped.rs`), `mail_list`/`mail_read`/`mail_send` on `inbox` & `main`, activity labels, and OneDrive card reconnect polish.
- [x] Full-text conversation search: UID-scoped `Store::search_conversations` + `GET /v1/conversations/search` (`store.rs`, `api/conversations.rs`, `api/tests/chat_api.rs`) wired into the desktop History popover (`ui/row_menu.rs`).
- [x] Desktop Tasks / Kanban / Calendar UI polish (`ui_tasks.rs`, `assets/pages-work.css`): stat tiles, segmented filters, theme-aware cards, `--fs-*` font tokens.
- [x] Scanned-PDF OCR hint in `browser_download`, reader-tool parity on download-capable bots, removed unused `text-splitter` dependency, closed superseded PR #134.
- [x] `docs/release-0.74.2.md` and `README.md` updated for `0.74.2`.
- [ ] Owner: live pass on Windows (Mail card connect/test with Hostinger, full-text History search, Tasks/Kanban/Calendar layout).


