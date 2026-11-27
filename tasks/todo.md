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

## Release 0.74.3 — LiveCard, chat stability, handwriting, mobile catch-up

- [x] Server: re-emit turn.context after compaction/failover; strip
  :::hand fences in Telegram/WhatsApp; Cargo.toml 0.74.3.
- [x] CI fixes: IMAP greeting full-prefix match in mail.rs; pinned MCP
  clients never use an environment proxy (mcp_client.rs .no_proxy());
  dropped the misplaced `key` in the handwriting block (ui/markdown.rs)
  that failed Windows clippy under -D warnings.
- [x] Desktop: row-id scroll anchor; fingerprint incl. ledger revision +
  steps; generation-counted composer; latest_context newest-or-None;
  LiveCard; :::hand block + Caveat font + Appearance toggle.
- [x] Mobile: history full-text search; Mail card; Tasks stat tiles +
  Kanban Board; hand block; pubspec 0.74.3+77.
- [x] Release notes docs/release-0.74.3.md; README 0.74.3.
- [ ] CI green on PR; merge only if all checks pass.
- [ ] Owner: live verification on Windows (LiveCard states, scroll/caret,
  handwriting), Hostinger mail via desktop/mobile, mobile search jump.
- [ ] Follow-up (not in 0.74.3): bundle Caveat TTF on mobile via a
  binary-capable route; inline hand spans; PDF/DOCX handwriting.

## Release 0.74.4 — Hostinger IMAP fix, multi-account mail, hostinger & mailbox bots

- [x] Server: IMAP `expect_tagged` after LOGIN and SELECT (skip untagged
  `* FLAGS (...)` / `* n EXISTS` lines; accept `* PREAUTH` greeting) plus
  literal-folding fix in `read_logical_line` (buffered read, one literal per
  line) with Hostinger-style fake-server tests (`mail.rs`).
- [x] Server: named mail accounts (`mail.<label>`) — `account` on
  `mail_list`/`mail_read`/`mail_send`, `/v1/mail` connect/status(`accounts`)/disconnect(`?account=`), `providers_for` filter.
- [x] Desktop: mail card account-name field, mailboxes summary, per-account
  disconnect (`ui_connectors.rs`, `api.rs`); mobile models/api client
  (`mail_models.dart`, `api_client.dart`).
- [x] Bots: `hostinger` (full Hostinger mailbox) + `mailbox` (multi-account)
  in `bots/`, `prompts/`, `shipped.rs` (fleet 22), default avatars
  (`assets/bots/hostinger.png`, `mailbox.png`), desktop starter prompts
  (`chat_feed.rs`); fleet-count tests updated (22 templates / 15 seeded);
  `skills/mail/SKILL.md` updated.
- [x] Version bump 0.74.4 (Cargo.toml), mobile 0.74.4+78, `docs/release-0.74.4.md`, README, AGENTS.md.
- [ ] Owner: live verification on Windows — Hostinger preset connect in
  Integrations → Mail (the `* FLAGS` error should be gone), second named
  account, `hostinger`/`mailbox` bots triaging.

## Release 0.74.5 — mail management depth, signatures, mail window

- [x] Server: `mail_folders`/`mail_search` (read-only), approval-gated
  `mail_flag`/`mail_move`/`mail_delete` (DESTRUCTIVE_TOOLS + docs/approvals.md),
  `mail_read` attachment listing (multipart parse), per-account `signature`
  appended on `mail_send`; tools registered in `tools.rs`.
- [x] Server API: `/v1/mail/folders`, `/v1/mail/messages`,
  `/v1/mail/messages/{uid}` (read-only, account-aware); `signature` in connect.
- [x] Desktop: mail window (`MailWindow` in `ui_connectors.rs`, only rendered
  while connected) with account/folder selectors, list, reading pane; signature
  field on the mail card; api.rs models + client methods.
- [x] Mobile: account-name field on the mail screen (connect/disconnect),
  mailboxes list in the status card.
- [x] Bots: `hostinger`/`mailbox` get the 5 new mail tools (flag/move/delete
  approval-gated), `inbox` gets folders/search; prompts + `skills/mail`
  updated (account rule, signature auto-append, new tools).
- [x] Version bump 0.74.5 (Cargo.toml), mobile 0.74.5+79,
  `docs/release-0.74.5.md`, README, AGENTS.md.
- [ ] Owner: live pass on Windows — Hostinger preset, Open inbox window,
  flag/move/delete with approval cards, signature on sent mail.

## Release 0.74.6 — social media extractor (`social_extract`), fleet-wide

- [x] Server: `social_extract.rs` — platform detect, OG/Twitter cards, JSON-LD
  (VideoObject/SocialMediaPosting/…), `<img>`/`<video>` scan (largest srcset),
  metrics; optional artifact download (image/* only, ≤20 MiB each, ≤50 MiB
  per call, UID-scoped conversation artifacts); `fetch.rs` gained
  `fetch_bytes`/`FetchedBytes`; registered in `main.rs` with its own
  SSRF-safe public fetcher.
- [x] Fleet-wide grant: `SHARED_TOOLS` + `known_tool_names`; skill
  `skills/social-extract` (+ `shipped.rs` 39, `docs/skills.md`, README).
- [x] Tests: unit (platform/meta/JSON-LD/srcset/filename) + wiremock end-to-end
  (extract + download + caps + no-download default).
- [x] Version bump 0.74.6 (Cargo.toml), mobile 0.74.6+80,
  `docs/release-0.74.6.md`, README, AGENTS.md.
- [ ] Owner: try real posts (Instagram/X may need login for full media —
  extraction is best-effort); check artifacts land in the Library.
- [ ] 0.75 on-demand tool packs: spec drafted in `docs/specs/tool-packs.md`
  (0.74.7) — **needs owner approval before any code**, like the LSP spec.
  Phase 1: `TOOL_PACKS` + `pack_list`/`pack_load` + prompt hint + migrate
  `hostinger`/`mailbox`/`inbox` to `packs = ["mail"]`; phase 2: desktop
  editor checkboxes + optional per-conversation persistence.

## Release 0.74.7 — proposed tool-packs spec + doc polish (no runtime changes)

- [x] `docs/specs/tool-packs.md` (proposed, not approved): pack registry,
  `pack_list`/`pack_load`, manifest `packs`, prompt/UX, security invariants,
  phases, risks.
- [x] `docs/skills.md`: `mail` row; `hostinger`/`mailbox` added to
  `email-triage`/`messaging-etiquette` rows. README note next to the LSP one.
- [x] Version bump 0.74.7 (Cargo.toml), mobile 0.74.7+81,
  `docs/release-0.74.7.md`, README, AGENTS.md.
- [ ] Owner: approve or amend `docs/specs/tool-packs.md` before 0.75 code.

## Release 0.74.8 — Library page (cross-chat files + uploads) with OneDrive mirror

- [x] Server: `library.rs` (`LIBRARY_SCOPE`, `LibraryMirror` best-effort
  OneDrive mirror), `[library]` config (`enabled`, `onedrive_mirror`,
  `onedrive_folder` = "SeraBot Library"); `store.list_library_items`
  (generated + uploaded, UID-scoped, conversation titles);
  `ArtifactStore::size`; `api/library.rs` (`GET /v1/library`,
  `POST /v1/library/upload` raw body + X-Filename, blocklist on the name,
  audit `library.upload`, mirror status in the reply).
- [x] `social_extract` downloads mirrored through `LibraryMirror`
  (`main.rs` registers the tool once the OneDrive registry exists); tool
  output reports the mirror outcome.
- [x] Desktop: `api.rs` `global_library()`/`library_upload()` +
  `LibraryStatus`/`LibraryUploadReply`/`LibraryOnedriveStatus`; new
  `ui_library.rs` Library page (nav rail, tabs/search/date/sort, upload via
  rfd, item dialog) reusing `bot_library` view rules; `Section::Library`;
  icons `library`/`upload`; CSS.
- [x] Tests: `api/tests/library_api.rs` (validation, scoping, audit, mirror
  status string) + `library.rs` mirror unit tests + store scoping test.
- [x] Version bump 0.74.8 (Cargo.toml), mobile 0.74.8+82,
  `docs/release-0.74.8.md`, README, AGENTS.md.
- [ ] Owner: live pass on Windows — Library page, upload a file, connect
  OneDrive and confirm the `SeraBot Library` mirror, `social_extract` image
  download mirroring.

## Release 0.74.9 — Library big upgrade (image cards + viewer, PDF/PPTX/XLSX previews, Open with default app)

- [x] Server: `api/library.rs` `GET /v1/library/thumbnail?hash=&w=`
  (UID-scoped, w clamps 16–1024, never upscales, JPEG q82 / PNG on alpha,
  EXIF dropped, GIF/BMP/SVG as-is, `private, max-age=86400`) and
  `GET /v1/library/preview-doc?hash=&kind=pptx|xlsx` (UID-scoped, 404 when
  `[library]` off / foreign hash, 400 on kind mismatch; PPTX max 50 slides,
  2000 chars/slide via new `pptx_ops::slide_preview()`; XLSX max 5 sheets,
  20×20 rows/cols via `workbooks`, formulas never evaluated).
- [x] Desktop: `ui_library.rs` Grid/List toggle, image cards with real
  thumbnails (authenticated fetch, data URLs, 48-image cap, fallback to
  artifact preview), full image viewer (zoom 25–400%, prev/next, keyboard),
  in-app previews in the item dialog (PDF via PreviewDock, PPTX slides,
  XLSX sheet tables, DOCX/MD/TXT/CSV text — Dioxus text nodes only),
  "Open with default app" as the primary button (existing
  `session.open_artifact` flow; Windows `explorer` launches the default
  app) + Show in Explorer / Save to Downloads; CSS in `system.css`.
- [x] Mobile: per-bot Library screen (no separate main Library screen
  exists) — thumbnail tiles via authenticated preview, full-screen
  `InteractiveViewer` image viewer, PPTX/XLSX preview sheet over
  `preview-doc` with a friendly unavailable state on 404, PDF via the
  existing preview pane, share via `DeviceActions.shareFile` (no
  `url_launcher`/`open_file` in pubspec — direct open needs that dep).
- [x] Tests: server `library_api.rs` (5 new), desktop
  `library_doc_preview_parses_and_bounds`, mobile `bot_library_test.dart`,
  `api_client_test.dart`, `bot_library_screen_test.dart`.
- [x] Version bump 0.74.9 (Cargo.toml), mobile 0.74.9+83,
  `docs/release-0.74.9.md`, README, AGENTS.md.
- [ ] Owner: live pass on Windows — Library grid + image viewer, open a
  real PDF/PPTX/XLSX in-app, Open with default app, and on mobile open an
  image and a PPTX/XLSX preview against a 0.74.9 server.

## Release 0.74.10 — Chat, composer & core (scroll restore, streaming perf, Delegating card, needs_review, mobile composer catch-up, persisted drafts, compaction persistence, heartbeat, duplicate-send guard, tool digest, honest context meter)

- [x] Chat view: scroll restore (no first-load force-pin, feed window keeps
  last start, uncapped bridge restore wait), slim transcript fingerprint +
  cached haystacks + streaming parse-cache bypass + memoised tokenize,
  Delegating card from the handoff ledger with parallel count, `NeedsReview`
  TaskStatus + LiveCard state (non-terminal, never auto-retried), streaming
  markdown (unclosed fence/`:::hand`/table header as plain text, Copy hidden
  while streaming), search next/prev jump-to-match, in-chat context meter.
- [x] Composer: mobile queue while running + @// pickers + draft restore +
  send action, files/uploads/held draft stashed per conversation, Enter
  falls through on empty picker, submit() reason toasts, drafts+queue
  persisted to `composer_drafts.json` (text only, secrets dropped), voice
  cancel + timer + transcribing state + one retry, Up/Down history cycling,
  queue with already-uploaded files, over-limit "Attach as file", picker
  a11y, paste/drop gating, auto-grow JS fallback. Server 8000-char cap
  already existed in `runtime/routing.rs` — no change needed.
- [x] Core: chunked compaction summaries stored regardless of folded Tool
  messages, durable folded boundary, fail-fast when compaction cannot shrink
  the prompt, summary cap 800→2000, FACTS_MAX_CHARS 1200→2500, `turn.progress`
  heartbeat every 15 s during model calls, uncertain flag before every
  effectful tool call + completed-effects replay skip + batch validation,
  per-turn tool digest persisted for `history_for_llm`, history char cap
  (60,000), `turn.context` after full prompt build + re-emit per round +
  no 100% clamp + tool-arg/image-aware estimator, `ContextTooLong` tries
  larger-window candidates, expired-approval recovery notice, `ask_teammate`
  ledger row + quota + child-failure summary to parent, pinned recall (up
  to 2 outside recall_k).
- [x] Tests: chat, composer (desktop + mobile), and core slices as listed in
  `docs/release-0.74.10.md`.
- [x] Version bump 0.74.10 (Cargo.toml), mobile 0.74.10+84,
  `docs/release-0.74.10.md`, README, AGENTS.md.
- [ ] Owner: live pass on Windows — scroll restore in a long chat, streaming
  reply smoothness, Delegating card on a multi-bot handoff, mobile queue +
  pickers + draft restore, and a long conversation against a 0.74.10 server
  (compaction keeps earlier facts, heartbeat shows while the model thinks).

## Release 0.74.11 — Heartbeat & needs_review UX, native IMAP in brief/triage, unified ApprovalCard & Library actionability

- [x] Live heartbeat (`turn.progress`) & `needs_review` recovery: wired on Desktop (`working.rs`, `activity.rs`, `session.rs`, `ui/wait_cards.rs`, `ui/chat.rs` with Mark reviewed + confirmed Send again) and Mobile (`models.dart`, `chat_controller.dart`, `chat_screen.dart`).
- [x] Morning Brief & Daily Email Triage (`brief/morning.rs`, `ops.rs`): reads up to 5 native multi-account IMAP mailboxes per user (10 s timeout, silent degradation) and qualifies native IMAP accounts for daily `inbox` triage.
- [x] Unified `ApprovalCard` (`ui_cards.rs`, `ui/approvals.rs`, `assets/system.css`): shared card across inline chat and Approvals page with tool badge, destination chip, expiry countdown, artifact link, and strict `tool_allows_standing_grant` gating.
- [x] Library actionability: Desktop **Attach to chat** (`ui_library.rs`) and Mobile global `LibraryScreen` with `GET /v1/library` + `POST /v1/library/upload` (`library_screen.dart`, `api_client.dart`, `device.dart`, `home_screen.dart`).
- [x] Version bump `0.74.11` (`Cargo.toml`), mobile `0.74.11+85`, `docs/release-0.74.11.md`, `README.md`, `tasks/todo.md`.
- [ ] Owner: live pass on Windows & Android (slow reasoning heartbeat, Hostinger IMAP in morning brief/triage, unified ApprovalCard, Library Attach to chat & Mobile Library upload).

## Release 0.74.12 — Streamlined sidebar + Settings hub, 3-pane Desktop Mail workspace, interactive Mail API & Mobile parity

- [x] Streamlined Desktop sidebar & grouped Settings hub (`session.rs`, `ui/shell.rs`, `ui/settings_view.rs`, `ui_settings_page.rs`, `ui/sheets.rs`, `system.css`): 7-item main rail (`Chats`, `Mail`, `Tasks`, `Projects`, `Library`, `Workflows`, `Browser`) + Settings housing `Memory`, `Integrations`, `Providers`, `Vault`, `Usage`, `Logs` with deep-linking and `Ctrl+K` support.
- [x] Server Interactive Mail API (`api/mail.rs`, `mail.rs`, `api/tests/mail_api.rs`): `GET /v1/mail/search`, `POST /v1/mail/messages/{uid}/flag`, `POST /v1/mail/messages/{uid}/move`, `DELETE /v1/mail/messages/{uid}`, `POST /v1/mail/send` (blocklist + UID-scoped Library attachments + signature), `GET .../attachments/{index}`, `POST .../save-to-library`.
- [x] Desktop 3-pane Mail workspace (`ui_mail.rs`, `api.rs`, `ui_connectors.rs`, `pages-work.css`): multi-account & folder pane, IMAP search & unread/starred filters, full reader (Reply/Forward/Star/Read/Move/Delete/Ask bot/Save to Library), Compose modal with Library picker.
- [x] Mobile Mail interactive parity (`apps/serabot_mobile/`): server-side search, Star / Mark Read-Unread, Delete, Save attachment to Library, Compose / Reply bottom sheet (`flutter analyze` + `242` tests).
- [x] Pro security & code review pass, `docs/release-0.74.12.md`, `README.md`, `Cargo.toml` (`0.74.12`), mobile `0.74.12+86`.
- [ ] Owner: live pass on Windows & Android (7-item rail + Settings hub, 3-pane Mail workspace, Hostinger IMAP search/flag/move/delete/send, Save attachment to Library).

## Release 0.74.13 — Native Mail Upgrade (HTML + Signatures + Send Later), Instant Stop, Browser DNS & 2-Column Model Selector Card

- [x] Native Mail Engine fixes (`mail.rs`): Dovecot/Hostinger `parse_list_name` fix, multi-word RFC 2047 (`?B?` & `?Q?` + UTF-8/ISO-8859-1/Windows-1252), recursive MIME + `quoted-printable`, strict `scraper` HTML sanitizer + `cid:` inline image rewriting, remote images blocked by default.
- [x] In-House Signature Builder & HTML Sending (`mail.rs`, `api/mail.rs`, `ui_mail.rs`): `PUT /v1/mail/signature` (no password re-entry), `cid:signature-logo@serabot` inline logo from UID-scoped Library, live preview, `multipart/mixed` -> `multipart/related` -> `multipart/alternative` builder.
- [x] In-House Email Scheduler ("Send Later") (`0051_scheduled_mail.sql`, `store.rs`, `api/mail.rs`, `scheduler.rs`, `ui_mail.rs`): UID-scoped `scheduled_mail` table, `/v1/mail/schedule` + `/v1/mail/scheduled` CRUD, 30s background delivery loop with blocklist re-scan, **Send later** quick presets, and **Scheduled** folder with Reschedule/Cancel.
- [x] Instant, smooth Agent Stop (`runtime/mod.rs`, `turn_loop.rs`, `session.rs`): Desktop out-of-band `serabot-stop` dispatch without waiting for `self.ready()`, server `request_stop` cascading to active delegated handoffs, and `tokio::select!` mid-tool cancellation preserving `uncertain` / `needs_review` for effectful tools.
- [x] Browser DNS Resilience (`browser/guard.rs`): 300ms DNS retry + exact-hostname diagnostic error message.
- [x] 2-Column Chat Model Selector Card (`ui/chat.rs`, `system.css`): downward-anchored `.model-pop` with Providers rail + icons on the left and search, account switcher, model list, and pinned Thinking Level segmented bar on the right.
- [x] Version bump `0.74.13` (`Cargo.toml`), mobile `0.74.13+87`, `docs/release-0.74.13.md`, `README.md`, `tasks/todo.md`.
- [ ] Owner: live pass on Hostinger IMAP/SMTP (folders, HTML emails, Signature logo, Send later), instant Stop, browser DNS errors, and 2-column Model Selector card.

## Release 0.74.14 — Mail workspace, model picker, Settings search & shared UX layer

- [x] Scheduled-send reliability: retry with backoff, stale `sending` rows failed (never resent), checked completion writes, 5 s loop (`store_mail.rs`, `scheduler.rs`, migration `0052`).
- [x] Mail server: list/search metadata + threading, search operators, snooze, drafts (IMAP APPEND), Undo send hold, contacts, saved searches, unsubscribe (`mail_ux.rs`, `mail.rs`, `api/mail*.rs`).
- [x] Desktop mail window: resizable panes + density, avatar/snippet rows, day headings, conversations, bulk bar, deferred archive/delete/move with Undo, snooze dialog + Snoozed folder, reader toolbar + Ask-bot strip + unsubscribe, compose suggestions/autosave/Undo send, shortcuts (`ui_mail*.rs`, `mail_view.rs`, `mail.css`).
- [x] Model picker fuzzy search + favorites/recents (`picker_search.rs`, `ui/chat.rs`); code-block Wrap toggle.
- [x] Settings search + descriptions, palette Settings/Compose entries + combobox a11y, approvals badge on Chats, shared focus ring/spacing tokens (`ux.css`).
- [x] Mobile: snippet in the mail list, new fields in `MailMessageInfo`.
- [x] Version bump `0.74.14` (`Cargo.toml`, `Cargo.lock`), mobile `0.74.14+88`, `docs/release-0.74.14.md`, `README.md`, `AGENTS.md`.
- [ ] Owner: live pass on Hostinger (Drafts APPEND, search operators, threading, snooze wake, Undo send, unsubscribe) and on Windows WebView2 (pane resize, density, shortcuts, Undo bar, snooze dialog, picker star).
- [ ] Deferred: branch/pin chat messages, Send/Stop morph, composer mode chips, picker cost/speed hints and per-chat default toggle, Settings Save/Discard bar, Activity feed, mobile threads/snooze/drafts.

## Release 0.74.15 (proposed) — harness, stability, composer, recall

Plan: `docs/specs/0.74.15-proposal.md`. Version stays `0.74.14` until the batch is cut.

- [x] Mail composer: sticky footer, recipient chips (Enter/comma/Tab/paste, Backspace removes, suggestions with arrows), attach from PC + drag and drop + Library, size/count limits, Ctrl+Enter, address validation.
- [x] Chat: message action bar below the message instead of floating over its neighbour; a retried message with no reply between shows once.
- [ ] Owner: try the composer on Windows (drop files, Enter in To, footer in a short window) and the chat bar.
- [x] Track 1: fake IMAP server + client tests, owner checklist script. Track 2: batched list fetch, task tracing span. Track 3: unread badge, attachments on scheduled/undo sends. Track 6: hostile HTML and header corpora.
- [ ] Still open: file splits, durable job runner, turn-loop invariant tests, back-pressure audit, restore drill, Settings Save/Discard bar, Activity feed, shared components, audit-coverage test, mobile mail parity.
- [ ] Track 7b recall and retry-by-reference: **needs owner approval** before code.

## Release 0.74.16 — Providers & Bot Routing (no Local ACP), WhatsApp Live Profile, Multi-Account Mail Updates

- [x] Remove Claude Code (Local ACP) from Desktop UI (`main.rs`, `ui/shell.rs`, `ui/chat.rs`, `ui_bots.rs`) and add **Bot routing** overview directly in **Settings → Providers** (`ui/providers.rs`, `pages-connect.css`).
- [x] WhatsApp Connector Live Profile (`whatsapp_link/client.rs`, `whatsapp_link/mod.rs`, `api/whatsapp_link.rs`, `ui_whatsapp_link.rs`, `system.css`): live `push_name`, `phone` backfill, connected profile card with avatar/dot/refresh, and chip-based Allowed Chats.
- [x] Custom Email Connector Multi-Account & Password-Optional Updates (`api/mail.rs`, `mail.rs`, `api/tests/mail_api.rs`, desktop `api.rs`, `ui_connectors.rs`): preserve sealed password & HTML signature when updating an existing mailbox, full non-secret metadata per account, **Connected Mailboxes** list with per-account Edit/Open/Disconnect and **+ Add another mailbox**.
- [x] Version bump `0.74.16` (`Cargo.toml`), mobile `0.74.16+90`, `docs/release-0.74.16.md`, `README.md`, `tasks/todo.md`.
- [ ] Owner: live pass on Providers bot routing, WhatsApp connected profile display, and editing/adding custom email accounts without re-entering password.

## Release 0.74.17 — Browser-Backed Social Accounts Hub, Providers Routing Polish & Saved-Mailbox Test

- [x] Social Accounts Hub (`browser/signin.rs`, `api/browser.rs`, `api/mod.rs`, `bots/marketer.toml`, `prompts/marketer.txt`, desktop `api.rs`, `ui_connectors.rs`, `ui_integrations.rs`, `pages-connect.css`): persistent `"social"` browser profile endpoints (`/v1/browser/social/status`, `/v1/browser/social/open`), LinkedIn/Instagram/X/Facebook checkpoint refusal rules, `marketer` browser tools + confirmation-gated prompt, and 1-click **Social Accounts** card with **Assign `marketer` to `social` profile**.
- [x] Providers Bot Routing Polish & Dead ACP Cleanup (`session.rs`, `session/bot_settings.rs`, `ui/providers.rs`, `ui.rs`, removed `ui_claude_code.rs`): startup `ListBots` hydration, `BotDetail` refresh on save, explicit reset label when fallbacks exist.
- [x] Integrations & Mail Test (`api/mail.rs`, `api/tests/mail_api.rs`, desktop `api.rs`, `ui_connectors.rs`, `ui_integrations.rs`): `POST /v1/mail/test` + per-row **Test** button without password re-entry, and named-mailbox summary in Integrations.
- [x] Version bump `0.74.17` (`Cargo.toml`), mobile `0.74.17+91`, `docs/release-0.74.17.md`, `README.md`, `tasks/todo.md`.
- [ ] Owner: live pass on signing into LinkedIn/Instagram in the `social` browser profile, assigning `marketer`, and testing a saved Hostinger mailbox.

## Release 0.74.18 — Antigravity Reliability, Claude & Mistral Sign-In, Inception Labs

- [x] Antigravity Connector Reliability (`cloud_code.rs`, `llm.rs`): 6-hour TTL `LIVE_VERSION` cache (never caching `None`), `IDE_VERSION` bumped to `2.31.4`, and automatic `invalidate_live_version` + negative project-discovery retry on initial `400`/`403`.
- [x] Claude Sign-In Upgrade (`auth_flows/claude.rs`, `api/connectors.rs`, `provider_presets.rs`, `provider_accounts.rs`, `ui/providers.rs`, `ui/account_views.rs`): `"OAuth / API Key"` preset badge, inline `"Paste callback URL or code#state"` fallback for port `54545`, and `sk-ant-oat01-...` `setup-token` support.
- [x] Inception Labs Native Provider + Logo (`providers.rs`, `relay_net.rs`, `costs.rs`, `provider_presets.rs`, `branding.rs`): `https://api.inceptionlabs.ai/v1`, Mercury models & pricing, relay allowlist, and custom SVG logo.
- [x] Mistral Vibe Browser Sign-In (`auth_flows/mistral.rs`, `auth_flows/mod.rs`, `provider_presets.rs`, `provider_flow.rs`, `ui/account_views.rs`, `provider_accounts.rs`, `ui/providers.rs`): PKCE-bound `console.mistral.ai/api/vibe/sign-in` polling & API-key exchange under `FlowKind::Device` (no `user_code`).
- [x] Version bump `0.74.18` (`Cargo.toml`), mobile `0.74.18+92`, `docs/release-0.74.18.md`, `README.md`, `tasks/todo.md`.
- [ ] Owner: live pass on Claude sign-in, Mistral browser sign-in, Google Antigravity chat, and Inception Labs `mercury-3`.

## Release 0.74.19 — Antigravity & Claude Fixes, History Sanitization, Settings & Providers UI/UX

- [x] Antigravity Connector & Catalog Reliability (`cloud_code.rs`, `llm.rs`, `model_router.rs`): fallback across all Cloud Code origins on `CloudPost::Failed`, 5-minute `CachedVersion::Failed` cache, and `FetchFailure::RequestFailed` for non-auth 4xx responses.
- [x] Claude Plan & `setup-token` Transport (`providers.rs`, `llm/wire_calls.rs`, `auth_flows/claude.rs`, `provider_presets.rs`): route `sk-ant-oat*` through `Wire::AnthropicPlan` (`Bearer` + `oauth-2025-04-20`), avoid duplicate `User-Agent` on wire GETs, surface clear OAuth gate errors, and reorder `anthropic` fallback models.
- [x] Tool Limit & Conversation Poisoning (`config.rs`, `store.rs`, `llm.rs`, desktop `api.rs`): `MAX_SCAFFOLD_TOOLS` 32 → 64; `sanitize_llm_history` + `messages_to_json` drop empty-id / orphaned `Role::Tool` results.
- [x] Chat `"Retry on..."` & Settings / AI Providers Overhaul (`wait_cards.rs`, `chat_failure.rs`, `runs.css`, `ui_settings_page.rs`, `ui/settings_view.rs`, `ui/providers.rs`, `pages-connect.css`, `settings.css`): bounded scrollable `.failed-alt`, diversified alternatives, structured Settings cards, and two-column per-bot routing & fallback chain editor (`bot-chain-shell`).
- [x] Version bump `0.74.19` (`Cargo.toml`), mobile `0.74.19+93`, `docs/release-0.74.19.md`, `README.md`, `tasks/todo.md`.
- [ ] Owner: live pass on Antigravity, Claude, and the updated Settings / AI Providers UI.

## Release 0.74.20 — Claude Agent SDK Gateway, Settings Scroll & Loading Fix, Vendor PNG Icons

- [x] Python `claude-agent-sdk` Loopback Gateway (`services/claude_gateway/`, `AGENTS.md`): loopback-only HTTP server with constant-time Bearer auth, `tools=[]` / `setting_sources=[]`, and `mcp__serabot__*` tool capture back to SeraBot's Rust `gateway`.
- [x] Rust `[claude_gateway]` & `claude-agent-sdk` Wire (`config.rs`, `providers.rs`, `llm.rs`, `llm/wire_calls.rs`, `wire/mod.rs`, `provider_presets.rs`): loopback-pinned `Wire::ClaudeGateway` + `"Claude Agent SDK (Gateway)"` preset.
- [x] Settings Independent Scroll & Lazy-Loading Fix (`settings.css`, `pages-data.css`, `ui/settings_view.rs`, `session.rs`, `ui/providers.rs`, `ui_integrations.rs`, `ui_logs.rs`, `ui/memory.rs`): independent sidebar/content scrolling, scroll-to-top on switch, `poll_paused` clear on navigation, and `section_load_error` + inline Retry button.
- [x] Real Vendor PNG Icons (`assets/brands/{opencode,muse,nous,inception}.png`, `branding.rs`, `SOURCES.md`): 64×64 RGBA PNGs replacing generic SVG placeholders.
- [x] Version bump `0.74.20` (`Cargo.toml`), mobile `0.74.20+94`, `docs/release-0.74.20.md`, `README.md`, `tasks/todo.md`.
- [ ] Owner: live pass on `services/claude_gateway`, Settings independent scroll, lazy-loaded tabs, and new vendor icons.

## Release 0.74.21 — Windows CI Fix, Animated Bot Avatars, Live Step Timeline & Memory Pills

- [x] Windows CI Test Fix (`session.rs`): `failed_providers_load_sets_section_load_error` uses non-retryable `403 Forbidden` mock response so `desktop (windows-latest)` passes without retry-backoff flake.
- [x] Animated Bot Avatars & Living Presence (`working.rs`, `ui.rs`, `ui/shell.rs`, `ui/chat.rs`, `ui/wait_cards.rs`, `assets/system.css`): `ActivityKind` (`thinking`, `searching`, `browsing`, `coding`, `delegating`, `waiting_approval`, `done`), activity-keyed `.avatar[data-activity]` CSS animations (respecting `data-motion="reduce"`), and `from_bot → to_bot` `.handoff-pill` in `LiveCardData::Delegating`.
- [x] Live Step Timeline & Interactive Memory Pills (`turn_trace.rs`, `ui/chat_feed_view.rs`, `ui/chat.rs`, `assets/system.css`): `LiveTimeline` (`Recalled N memories → Tool step(s) → Drafting reply`), interactive `.memory-pill` chips, and **"Pin to memory"** quick action in assistant `.msg-actions`.
- [x] Version bump `0.74.21` (`Cargo.toml`), mobile `0.74.21+95`, `docs/release-0.74.21.md`, `README.md`, `tasks/todo.md`.
- [ ] Owner: live visual pass on animated bot avatars, `LiveTimeline`, and memory pills in desktop WebView2.

## Release 0.74.22 — AWS Kiro & Devin CLI OAuth, Thinking Level Accuracy, Usage Page Redesign

- [x] AWS Kiro OAuth & EventStream Wire (`auth_flows/kiro.rs`, `wire/kiro.rs`, `providers.rs`, `llm.rs`, `llm/wire_calls.rs`, `relay_net.rs`, `provider_presets.rs`, `branding.rs`): AWS SSO OIDC device flow, strict `KiroRegion` allowlist, EventStream binary decoder (4 MiB frame cap), real `kiro.png` icon, and `PLAN_RISK_KIRO`.
- [x] Devin CLI OAuth & Connect-RPC Wire (`auth_flows/devin.rs`, `wire/devin.rs`, `providers.rs`, `llm.rs`, `llm/wire_calls.rs`, `relay_net.rs`, `oauth.rs`, `provider_presets.rs`, `provider_flow.rs`, `branding.rs`): PKCE (`127.0.0.1:59653/callback`), `devin-session-token$...` paste fallback, Connect-RPC JSON wire (`GetCliModelConfigs` / `GetChatMessage`), real `devin.png` icon, and `PLAN_RISK_DEVIN`.
- [x] Per-Model Thinking Level Accuracy (`thinking.rs`, `cloud_code.rs`): tightened Antigravity Gemini/Claude rules and added accurate `levels_for()` + `wire_fields()` for `claude-agent-sdk`, `kiro`, `devin`, `moonshot`, `inception`, `ollama`, `cerebras`, `together`, `fireworks`, `minimax`, `nebius`, and `sambanova`.
- [x] Usage Page & Card Redesign (`ui_usage.rs`, `ui_usage_insights.rs`, `pages-data.css`): 4-card KPI Hero Strip, 5 segmented sub-view tabs, `.usage-card` containers, and `.usage-share-bar` proportion bars.
- [x] Version bump `0.74.22` (`Cargo.toml`), mobile `0.74.22+96`, `docs/release-0.74.22.md`, `README.md`, `tasks/todo.md`.
- [ ] Owner: live pass on Kiro OAuth, Devin CLI OAuth, thinking-level selectors, and redesigned Usage page.

## Release 0.74.23 — Modal Sheet Stacking Fix & Proton Pass Vault Connector Overhaul

- [x] Desktop AI Provider Modal Sheet Fix (`assets/system.css`, `ui.rs`): stacked `.sheet, .sheet.open` at `z-index: 220` above `.scrim.open` (`z-index: 210`) with `backdrop-filter: none`, preventing blurry overlays and click-through dismissals; added `modal_sheets_stack_above_scrim_and_stay_crisp` regression test.
- [x] Proton Pass Vault Connector Overhaul (`vault_providers.rs`, `api/vault.rs`, `vault_import.rs`, `ui_vault.rs`): native `pass://` `item view` resolution (fixing ID-vs-name mismatch), headless `PROTON_PASS_KEY_PROVIDER=fs` + `PROTON_PASS_SESSION_DIR`, `PROTON_PASS_AGENT_REASON` audit logging for AI Agent Access Tokens, serialized single-session `login`/`run_batch`/`logout --force`, token normalization/validation (`pst_...::...`), bounded stderr error classification, Login-only item filtering, and desktop per-vault local import + setup guide.
- [x] Version bump `0.74.23` (`Cargo.toml`), mobile `0.74.23+97`, `docs/release-0.74.23.md`, `README.md`, `tasks/todo.md`.
- [ ] Owner: live verification of AI Provider modal sheet and Proton Pass AI Agent token probe/sync/autofill.

## Release 0.74.24 — Windows CI Test Reliability Fix

- [x] Windows CI Test Fix (`session.rs`): hardened `failed_providers_load_sets_section_load_error` mock TCP server (`flush` + `shutdown(Shutdown::Write)` to avoid Windows loopback `RST`, `AtomicBool` stop flag, and 15 s deadline).
- [x] Version bump `0.74.24` (`Cargo.toml`), mobile `0.74.24+98`, `docs/release-0.74.24.md`, `README.md`, `tasks/todo.md`.

## Release 0.74.25 — Rich HTML Mail, Embedded-Logo Signatures, AI Mail Tools & UI Upgrade

- [x] Server HTML Mail & Inline `cid:` Pipeline (`mail.rs`): raised `MAX_SIGNATURE_HTML_CHARS` to 64 KiB, preserved safe `<style>` blocks in `sanitize_email_html` (`sanitize_style_sheet`), added `extract_inline_data_images` (`data:image/*` → `multipart/related` `cid:inline-N@serabot`), and added `In-Reply-To`/`References` header support in `build_message_with_inline`.
- [x] AI Mail Management Tools (`mail.rs`, `main.rs`, `gateway.rs`, `bots/hostinger.toml`, `bots/mailbox.toml`): upgraded `mail_send` with `body_html`, `cc`, `in_reply_to`, `references`, and automatic HTML signature + inline logo attachment; added `mail_reply` (approval-gated in `DESTRUCTIVE_TOOLS`), `mail_draft` (IMAP `Drafts`), and `mail_triage` (read-only inbox/signature/unread overview).
- [x] Desktop Rich HTML Editor, Reader CSP Fix & UI Upgrade (`ui_mail_reader.rs`, `ui_mail.rs`, `mail.css`): fixed `MailReader` to pass `*ctx.remote_loaded.read()` to `wrap_email_html_srcdoc_with_remote`, framed HTML bodies in `.mail-paper-card`, added **Visual / Paste** `contenteditable` editors in `SignatureBuilderModal` and `MailComposeModal` with formatting toolbar + live signature preview (`cid:` → `data:` resolution) + **AI Assist**.
- [x] Compliant Claude Setup (`provider_presets.rs`, `ui/providers.rs`, `provider_flow.rs`, `api/accounts.rs`, `auth_flows/claude.rs`): removed shut-down third-party `claude.ai` OAuth PKCE; `anthropic` is API Key only, paired with `claude-agent-sdk` (loopback Python SDK gateway) and Desktop ACP.
- [x] Antigravity `pi-antigravity` v0.15.0 Overhaul (`cloud_code.rs`, `thinking.rs`, `relay_net.rs`, `provider_presets.rs`): added `aicode` OAuth scope, `antigravity/cli/1.5.5` UA, 126-char `systemInstruction` auto-folding (`ANTIGRAVITY_INLINE_SYSTEM`), public→runtime tier & integer `thinkingBudget` routing, `businessaicode*.googleapis.com` enterprise hosts, capacity backoff, and `403 VALIDATION_REQUIRED` diagnostics.
- [x] Version bump `0.74.25` (`Cargo.toml`), mobile `0.74.25+99`, `docs/release-0.74.25.md`, `README.md`, `tasks/todo.md`.
- [ ] Owner: live pass pasting a rich HTML signature with embedded logo, sending HTML mail, loading remote images, testing `mail_triage`/`mail_draft`/`mail_reply`, and re-signing into Antigravity (`aicode` scope).




## Release 0.74.26 — Provider Sign-in & Antigravity Routing Fixes

- [x] Retired Claude plan sign-in shows a terminal notice instead of a dead "Turn on Claude sign-in" button (`serabot-desktop/src/provider_accounts.rs`): `anthropic` + `plan_signin = "off"` maps to `SigninBlocker::Retired` (no fix button), since the server hard-rejects third-party claude.ai OAuth since 0.74.25.
- [x] Kiro / Devin OAuth accounts gated on the raw `plan_oauth` flag (`providers.rs`): `plan_oauth_enabled()` is kind-restricted to anthropic/codex/copilot, so kiro/devin sign-in accounts were always refused with `forbidden: … sign-in is turned off on this server`.
- [x] Cloud Code (Antigravity) envelope truncates beyond `MAX_WIRE_TOOLS` (128) with a `warn` instead of failing the task with `validation error: too many tools` (`cloud_code.rs`).
- [x] Version bump `0.74.26` (`Cargo.toml`), mobile `0.74.26+100`, `docs/release-0.74.26.md`, `README.md`, `tasks/todo.md`.
- [ ] Owner: re-pin any route still on provider id `kiro` (`unknown_provider` in logs) to the current provider id; live pass on Kiro/Devin OAuth accounts and an Antigravity turn with a large tool set.
## Release 0.74.27 — Proton Pass Sync Errors & Agent SDK Multi-Account Docs

- [x] Proton Pass sync/read failures surface the classified reason instead of a bare "HTTP 502" (`vault_providers.rs`): pass-cli stderr with 502/503/504/bad-gateway/upstream maps to "Proton Pass servers answered with an outage (HTTP 5xx)"; CLI exit failures are now `SeraError::Rejected` (422) carrying the safe sentence.
- [x] Clients show the server's `error` body beyond 422: desktop `api.rs` parses `{error, code}` for all non-success statuses except 401/403/409/429 (`ApiError::Coded`); mobile `serverErrorText` also parses 500/502.
- [x] Agent SDK documented (`docs/specs/claude-agent-sdk-gateway.md`, AGENTS.md, preset note): no third-party claude.ai OAuth (retired 0.74.25); SDK auth = `claude auth login`/API key inside the gateway; multi-account = one loopback gateway per login on its own port, one `claude-agent-sdk` provider each (config test `multiple_claude_agent_sdk_gateways_each_on_their_own_loopback_port`); Rust already speaks the gateway natively via `Wire::ClaudeGateway`.
- [x] Version bump `0.74.27` (`Cargo.toml`), mobile `0.74.27+101`, `docs/release-0.74.27.md`, `README.md`, `tasks/todo.md`.
- [ ] Owner: re-run Proton Pass sync (previously "Server returned HTTP 502") and try a second gateway on `127.0.0.1:8792` as provider `claude-agent-sdk-2`.
## Release 0.74.28 — "Sign in with browser" for the Claude Agent SDK gateway

- [x] Gateway auth endpoints (`services/claude_gateway/server.py`): `POST /v1/auth/start`, `POST /v1/auth/complete`, `GET /v1/auth/status` — first-party claude.ai OAuth PKCE (the `claude auth login` client id), verifier stays in the gateway, pinned token URL, redirect_uri validated loopback, tokens written to the CLI credentials file 0600.
- [x] Server relays the flow for kind `claude-agent-sdk` (`auth_flows/gateway.rs`, `auth_flows/mod.rs` `start_with_gateway`/`complete_with_gateway`, `api/provider_accounts.rs`, registry in `api/mod.rs`); account credential = gateway bearer token, identity from the gateway.
- [x] Desktop: preset `sign_in = "pkce"` + "Sign in with browser" tile label, so the gateway card offers browser sign-in next to the API key.
- [x] Docs: `docs/specs/claude-agent-sdk-gateway.md` browser sign-in section; `docs/release-0.74.28.md`; version bump `0.74.28` (`Cargo.toml`), mobile `0.74.28+102`, `README.md`, `tasks/todo.md`.
- [ ] Owner: on the `claude-agent-sdk` card press "Sign in with browser" and complete the claude.ai login; then run a turn and `GET /v1/auth/status` on the gateway.
## Release 0.74.29 — Gateway provider can be saved keyless (both ways on the card)

- [x] `admin.rs`: `claude-agent-sdk` providers may save without an API key (browser sign-in available), like OpenRouter — no more "secret claude_gateway_token is empty; provide an API key" forcing a key.
- [x] `api/provider_accounts.rs` `gateway_endpoint`: browser sign-in relay token falls back to the server-level `[claude_gateway] token_secret` age secret when the provider has no saved key; plain error when neither is set.
- [x] Desktop `provider_accounts.rs`: "secret … is empty; provide an API key" maps to a friendly "Sign in to Claude Agent SDK Gateway…" (kind `claude-agent-sdk`), never the raw secret name.
- [x] Version bump `0.74.29` (`Cargo.toml`), mobile `0.74.29+103`, `docs/release-0.74.29.md`, `README.md`, `tasks/todo.md`.
- [ ] Owner: save the gateway provider with no key, press "Sign in with browser"; also try "Add a key".
