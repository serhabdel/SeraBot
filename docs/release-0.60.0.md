# Release 0.60.0

Production reliability and desktop UX release fixing WhatsApp linked-device
and file logging on read-only config mounts, scanned PDF reading & per-user
OCR, Antigravity (Cloud Code) pure Gemini requests, per-provider thinking
levels, default bot tool access, and high-visibility desktop chat & Command
Palette layout bugs. Synthetic fixtures only; nothing here was verified
against a live WhatsApp, Mistral, or Google Antigravity account.

## Writable data-dir fallback (WhatsApp Link HTTP 500 & file logging)

- `config.rs`: added `Config::ensure_writable_dir(configured, fallback_subdir)`,
  which tries `self.path(configured)` first and falls back to
  `<parent(database_path)>/<fallback_subdir>` when the config directory is
  read-only (such as `/etc/serabot` under systemd `ProtectSystem=strict`).
- `whatsapp_link/client.rs`: `WhatsappLink::session_path` uses
  `ensure_writable_dir(..., "whatsapp")` and returns a descriptive validation
  error instead of an opaque HTTP 500 if the directory is still unwritable.
- `main.rs`: file logging resolves `[logging] dir` through
  `ensure_writable_dir(..., "logs")` so daily log files write beside the
  database instead of failing on a read-only `/etc/serabot`.

## Scanned PDF reading & per-user Mistral OCR

- `ocr.rs` & `documents.rs`: added per-user OCR credential resolution
  (`resolve_user_ocr_credential` / `recognize_bytes_for_user`). Members who
  have a connected Mistral provider account automatically get Mistral OCR on
  scanned PDFs and images in `document_index` even when server-wide `[ocr]`
  is not enabled, falling back to the server age store when `[ocr]` is on.
- `documents.rs` & `attachments.rs`: added pure-Rust embedded JPEG/PNG image
  extraction (`extract_pdf_images`) for scanned/image-only PDFs attached in
  chat, feeding up to 4 page images inline to vision-capable models even when
  no OCR key is configured. Updated misleading `"OCR is not supported"`
  messages in `documents.rs` and `analyst.rs`.

## Antigravity (Cloud Code) pure Gemini & thinking levels

- `cloud_code.rs`: `antigravity_thinking` no longer forces a default
  `thinkingLevel` on Gemini 3/4 when no level was requested (avoiding HTTP 400
  rejections), and adds `thinkingBudget` support for `gemini-2.5-*`.
- `llm.rs`: on Cloud Code HTTP 400 with a thinking level set, retries once
  without `thinkingConfig` and falls through to the next production host
  before failing; prevents sandbox host 403s from masking primary errors; logs
  sanitized Google error status/reason at `warn` without echoing bodies into
  user-facing errors.
- `thinking.rs`: added `github-copilot` thinking rules, expanded `mistral`
  (`mistral-small-26*`, `mistral-medium-3*`, `deepseek-v4*`, `zai-glm*`, and
  `reasoning`-tagged models), and aligned `google-antigravity` Gemini levels
  (`gemini-2.5-*` and `gemini-3*/4*` without unsupported `max`).

## Default bot tools & Desktop UI/UX fixes

- `collaboration.rs`: added browser automation (`browser_*`), `watch_now`,
  `firecrawl_crawl`, and `timesheet_analyze` to `SHARED_TOOLS` so every bot
  has all non-admin tools available by default (destructive actions stay
  approval-gated in `gateway`).
- `ui_settings.rs`: moved the verbose "What it can do" capability toggles out
  of the main right-sidebar Bot Settings form into the collapsed `Advanced`
  section.
- `assets/system.css` & `ui/chat.rs`:
  - Fixed Jump (`Ctrl+K`) Command Palette being shifted off the top of the
    screen under reduced-motion and rendered on an opaque white scrim under
    reduced-transparency.
  - Moved `.msg-actions` cleanly in-flow below every message so older messages
    never clip at the top edge and actions are always visible and clickable.
  - Fixed white-on-white attachment pills inside user bubbles.
  - Positioned `Jump to latest` cleanly above the composer, aligned the jobs
    panel with chat width, tightened toasts into compact pills, and made the
    composer thinking-level picker update immediately on model switch.
