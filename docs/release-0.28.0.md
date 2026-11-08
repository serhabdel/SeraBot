# SeraBot 0.28.0: autonomous by default, subscription-only ElevenLabs, previews and logs

Spec: `docs/specs/release-0.28.0.md` (owner decisions recorded there).

## Harness policy (0.28)
- **Only destructive tools ask for approval.** Sends, uploads, PC writes,
  deletes (the `DESTRUCTIVE_TOOLS` list) always wait for the owner. Everything
  else — including ElevenLabs MCP generation and OpenRouter image generation —
  runs on the bot's judgement. A tool name the server marks as data-changing
  (`readOnlyHint: false`) is still refused, never run.
- **Bot self-YOLO:** a bot may call `yolo_toggle` to enable auto-approve for
  its conversation when its judgement says so. Every flip is audited
  (`yolo.self_enabled`), and YOLO **never** covers a destructive tool — the
  gateway refuses to auto-approve those even with YOLO on.
- **More warriors:** `main` can fan out up to 16 `delegate` handoffs per turn.

## Connectors
- **ElevenLabs API key connector removed entirely** — tools, config, secrets,
  docs. The subscription (hosted MCP, OAuth sign-in) is the only ElevenLabs
  connector.
- **OpenRouter image models:** `openrouter_generate_image` generates 1–4
  variants with any image-capable OpenRouter model, saved as artifacts.
- **Web search/crawl from settings:** Tavily and Firecrawl switches and key
  status are configurable in the desktop console's Settings → Web search.

## Server
- `GET /v1/artifacts/{hash}/preview` — server-rendered previews for pdf, pptx,
  xlsx, docx and md artifacts (owner-scoped).
- `GET /v1/jobs` — the owner's queued/running tasks and media jobs in one list.
- `GET /v1/logs?level=&q=` — redacted ring-buffer logs, owner only; rolling
  daily log files under the log dir (7 kept), secrets scrubbed.

## Clients
- Desktop console: **Jobs panel** in the main chat view (running tasks, media
  jobs, handoffs, with live states), **Logs tab** with level filter and search,
  chat view polish (stable streaming scroll, reply cards, WIP step feedback).
- Desktop + Flutter mobile: **preview panes** for pdf/pptx/xlsx/docx/md
  artifacts (server-rendered image/text, download fallback).

## Tests

Synthetic fixtures only: `scripts\cargo-win.bat test --workspace --locked`,
plus `flutter test` in `apps/serabot_mobile`.
