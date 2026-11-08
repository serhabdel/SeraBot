# 0.28.0 task list

Spec: `docs/specs/release-0.28.0.md` (owner-approved decisions recorded there).
Branch: `release/0.28.0`. Gate per chunk: `scripts\cargo-win.bat check --workspace --all-targets`
+ `scripts\cargo-win.bat test --workspace --locked`, then fmt + clippy before commit.
PR to `main` at the end; never merge without the owner's OK.

## Phase A — harness policy (done)
- [x] A1 approval gate = DESTRUCTIVE_TOOLS only; MCP generate/create/spend no longer ask
  - Files: `gateway.rs`, `elevenlabs_mcp.rs`, `docs/approvals.md`
- [x] A2 bot self-YOLO via `yolo_toggle`, audited (`yolo.self_enabled`), never for destructive tools
  - Files: `gateway.rs`, `plans.rs`, `runtime/prompts.rs`, `tools.rs`, `config.rs`
- [x] A3 handoff ceiling 8 → 16
  - Files: `delegate.rs`, `prompts/main.txt`

## Phase B — connectors (done)
- [x] B1 ElevenLabs API-key connector removed entirely; OAuth/MCP subscription kept
  - Files: `elevenlabs.rs` (deleted), `elevenlabs_oauth.rs` (new shared module), `config.rs`, `admin.rs`, `connectors.rs`, `web.rs`, `main.rs`, `tools.rs`, `bots/maker.toml`, `skills/elevenlabs`, `docs/elevenlabs.md`, `serabot.toml`
- [x] B2 `openrouter_generate_image` (1–4 variants, image-capable catalog models, artifacts)
  - Files: `openrouter.rs`, `main.rs`, `tools.rs`, `config.rs`
- [x] B3 web search/crawl settings UI (Tavily/Firecrawl) in desktop console
  - Files: `crates/serabot-desktop/src/ui_settings_page.rs` etc.

## Phase C — server features (done)
- [x] C1 `GET /v1/jobs` (tasks + media jobs across conversations)
- [ ] C2 routines & workflows surfaced in console (scheduler/plans already exist; UI pass pending)
- [x] C3 `GET /v1/artifacts/{hash}/preview` for pdf/pptx/xlsx/docx/md — `api/preview.rs`
- [x] C4 rolling file logs + `GET /v1/logs` (redacted ring buffer) — `logging.rs`, `api/logs.rs`

## Phase D — clients (done)
- [x] D1 desktop main view: jobs panel, Logs tab, chat polish/WIP feedback
- [x] D2 preview panes desktop + Flutter mobile (`apps/serabot_mobile/lib/preview_pane.dart`)
