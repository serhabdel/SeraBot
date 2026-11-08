# 0.28.0 task list

Spec: `docs/specs/release-0.28.0.md` (owner-approved decisions recorded there).
Branch: `release/0.28.0`. Gate per chunk: `scripts\cargo-win.bat check --workspace --all-targets`
+ `scripts\cargo-win.bat test --workspace --locked`, then fmt + clippy before commit.
PR to `main` at the end; never merge without the owner's OK.

## Phase A — harness policy
- [ ] A1 approval gate = DESTRUCTIVE_TOOLS only; MCP generate/create/spend no longer ask
  - Acceptance: `elevenlabs_mcp_call` on a generate tool runs without approval; destructive tools still ask; tests updated
  - Verify: `cargo-win test -p serabot --locked -- gateway elevenlabs`
  - Files: `gateway.rs`, `elevenlabs_mcp.rs`, `docs/approvals.md`
- [ ] A2 bot self-YOLO, audited, never for destructive tools
  - Acceptance: bot can enable yolo for the conversation; `yolo.self_enabled` audit row; destructive under yolo refused
  - Verify: gateway + cards tests
  - Files: `gateway.rs`, `cards.rs`, `runtime/turn_loop.rs`
- [ ] A3 handoff ceiling 8 → 16; delegate spec + prompts updated
  - Files: `delegate.rs`, `runtime/turn_loop.rs`, `prompts/main.txt`

## Phase B — connectors
- [ ] B1 remove ElevenLabs API-key connector entirely (tools, config, secrets, docs, bot lists, renames)
  - Files: `elevenlabs.rs`, `config.rs`, `secrets.rs`, `bots/*.toml`, `docs/elevenlabs.md`, `skills/elevenlabs`
- [ ] B2 OpenRouter image models (catalog image-capable models; `openrouter_generate_image`)
  - Files: `openrouter.rs`, `tools.rs`, `main.rs`
- [ ] B3 web search/crawl (Tavily/Firecrawl) configurable from desktop settings
  - Files: `crates/serabot-desktop/src/ui_connectors.rs`, `api/connectors.rs`

## Phase C — server features
- [ ] C1 main-view jobs data: media jobs + tasks + handoffs panel (API already exists; wire console)
- [ ] C2 routines & workflows surfaced in console
- [ ] C3 `GET /v1/artifacts/{hash}/preview` for pdf/pptx/xlsx/docx/md
- [ ] C4 rolling file logs + `GET /v1/logs` + console Logs tab

## Phase D — clients
- [ ] D1 desktop main view: jobs panel, chat smoothness, reply cards, WIP feedback (apple-design motion rules)
- [ ] D2 media preview panes desktop + Flutter mobile
