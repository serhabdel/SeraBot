# Spec: SeraBot 0.28.0 — autonomous harness, subscription-only ElevenLabs, previews, logging

Owner decisions recorded 2026-11-08 (answers to the spec questions):

1. **Approvals:** only destructive tools ask the owner. Everything else —
   including ElevenLabs image/video generation and MCP write/generate calls —
   runs on the bot's judgement ("overall allow"). Destructive keeps approval.
2. **YOLO:** bots may self-enable YOLO for non-destructive work, based on their
   own judgement, instructions and memory. It is always audited and can never
   cover a destructive tool (owner decision 1 wins on conflict).
3. **ElevenLabs API key:** removed entirely (code, config, docs, tools).
   Subscription (hosted MCP) is the only ElevenLabs connector.
4. **Media preview:** all clients (desktop + Flutter mobile), best approach —
   server-rendered previews where the server can produce them, client-side
   where it cannot. Formats: pdf, pptx, xlsx, docx, md.
5. **Logging:** rolling log files on the server + a Logs viewer in the desktop
   console. Secrets stay redacted by the existing redactor.

## 1. Objective

Make the bots feel like a team of workers that just gets it done, and make the
console show the work in progress. 0.28.0 removes friction (approvals,
API-key ElevenLabs), adds visibility (jobs in the main view, WIP feedback,
media previews, logs), and extends reach (OpenRouter image models, configurable
web search/crawl).

## 2. Scope and phases

### Phase A — harness policy (server, `crates/serabot`)
- A1. Approval gate = `DESTRUCTIVE_TOOLS` only. The ElevenLabs MCP connector
  stops asking approval for generate/create/spend tools; it only refuses
  `readOnlyHint: false` names that *read* by spelling (unchanged fail-closed).
  `elevenlabs_key_*` tools disappear with the connector (B1).
- A2. Bot self-YOLO: a bot may turn on the conversation's YOLO switch itself
  when its instructions/memory say so; every auto-approval is audited with the
  existing `YOLO_AUDIT_REASON` and a `yolo.self_enabled` audit row. YOLO never
  applies to destructive tools — the gateway rejects that combination.
- A3. Subagents ("a lot of warriors"): keep the 0.27 fan-out (up to 8 handoffs
  per turn) and raise the ceiling to 16; the `delegate` spec text and prompts
  are updated. No recursive delegation (specialists still cannot delegate).

### Phase B — connectors
- B1. Remove the ElevenLabs API-key connector entirely: `elevenlabs_key_*`
  tools, `[elevenlabs.api_key]` config, secrets, docs, bot lists, rename logic
  for the legacy names. MCP subscription only.
- B2. OpenRouter image models: image generation through OpenRouter's image
  API for models advertised as image-capable in its catalog; new tools
  `openrouter_generate_image` (and list), approval-free per A1, artifacts
  recorded like other media.
- B3. Web search/crawl configurable from settings: Tavily + Firecrawl switches,
  keys, limits already exist in config — expose them in the desktop console
  Connectors/Settings UI and document in `serabot.toml` examples.

### Phase C — server features
- C1. Main view shows ongoing work: media jobs (pending/finishing/done),
  queued/running tasks, handoffs — via existing `media-jobs`, tasks and
  events endpoints, rendered in a Jobs panel.
- C2. Routines and workflows: routines = scheduled bot runs (scheduler exists);
  workflows = stored multi-step plans (`plans.rs`) the orchestrator can resume.
  Surface both in the console; no new engine.
- C3. Media preview endpoints: server-rendered previews for pdf (page 1
  raster/thumbnail), pptx (slide 1), xlsx (first rows grid), docx (first
  blocks text), md (rendered HTML/text). Served from the artifact store by
  hash: `GET /v1/artifacts/{hash}/preview`.
- C4. Logging: rolling `tracing` file logs (daily, capped retention) + ring
  buffer exposed as `GET /v1/logs?level=&q=` (owner only), secrets redacted.
  Desktop console gets a Logs tab with level filter + search.

### Phase D — clients (desktop Dioxus + Flutter mobile)
- D1. Desktop main view: jobs panel, logs tab, chat view polish — smoother
  streaming, stable scroll, reply cards, WIP/progress feedback (activity
  steps already stream over SSE; make them first-class with spinners,
  progress and clear done/failed states). Apple-design principles for motion:
  interruptible, 1:1, springs where gesture-driven.
- D2. Media preview panes in desktop + mobile apps for pdf/pptx/xlsx/docx/md,
  using C3 where the server can render, client-side fallback otherwise.

## 3. Commands

```bat
scripts\cargo-win.bat check --workspace --all-targets
scripts\cargo-win.bat test --workspace --locked
scripts\cargo-win.bat fmt --all -- --check
scripts\cargo-win.bat clippy --workspace --all-targets -- -D warnings
```
Flutter app: `flutter analyze` / `flutter test` in `apps/serabot_mobile` when
touched (not part of the cargo gate).

## 4. Project structure

- Server: `crates/serabot/src/...` (gateway, elevenlabs_mcp, openrouter,
  media_jobs, api, runtime). Desktop: `crates/serabot-desktop/src/...`.
  Mobile: `apps/serabot_mobile/lib/...`. Spec: this file. Tasks:
  `tasks/release-0.28.0.md`. Release notes: `docs/release-0.28.0.md`.

## 5. Boundaries

- Always: destructive tools require owner approval; YOLO (self or owner)
  never covers them and is audited; secrets never logged; per-user scoping
  untouched; tests synthetic only; fmt+clippy+tests green before commit.
- Ask first: changing `AGENTS.md` invariants (not needed for this spec as
  decided above), adding dependencies, deploy changes.
- Never: commit secrets, weaken per-user isolation, remove the blocklist,
  merge without owner OK.

## 6. Success criteria

- A bot can generate an ElevenLabs image and an OpenRouter image without any
  approval prompt; sending a file to Telegram still asks.
- A bot can self-enable YOLO in a conversation; the audit log shows it; a
  destructive call under YOLO is refused.
- No `elevenlabs_key_*` tool or `[elevenlabs.api_key]` config exists; MCP
  sign-in works.
- Console main view lists running media jobs/tasks; chat shows WIP steps and
  reply cards; Logs tab shows filtered logs.
- Desktop + mobile show previews for pdf/pptx/xlsx/docx/md artifacts.
- Full cargo gate green on Windows; CI green on ubuntu+windows; PR merged.

## 7. Open questions (non-blocking, defaults assumed)

- "Routines and workflows": assumed = existing scheduler routines + stored
  plan workflows surfaced in UI (C2). Correct if you meant something else.
- Mobile preview: Flutter app gets preview panes via the C3 endpoint;
  formats it cannot render fall back to download.
- Handoff ceiling 16 (A3): say the word if you want more.
