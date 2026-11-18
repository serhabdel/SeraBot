# Release 0.57.0

Official Anthropic & Claude logos, durable Logs recovery, immediate Stop-chat
cancellation, Chat UX starter quick-chips & polish, and native WhatsApp Business
Cloud API integration for the `whatsapp` bot.

## Official Anthropic & Claude branding

- **Distinct brand marks (`crates/serabot-desktop/src/branding.rs`):** added
  official geometric `A\` Anthropic SVG (`SVG_ANTHROPIC`), terracotta `#D97757`
  Claude starburst SVG (`SVG_CLAUDE` across `claude`, `claude-code`,
  `claude-agent`), and official green WhatsApp phone glyph (`SVG_WHATSAPP`).
- **Consistent surfaces:** replaced generic `"code"` icons in the Claude
  Agent banner (`ui_claude_code.rs`) and Command Palette (`ui/shell.rs`) with
  `BrandLogo { brand: "claude", .. }`.

## Logs tab reliability

- **Preserve in-memory ring buffer when log directory is unwritable
  (`crates/serabot/src/logging.rs`):** `logging::install` no longer aborts
  subscriber initialization when `create_private_dir(&dir)` fails (for
  example under hardened systemd `ProtectSystem=strict`), keeping `stdout`
  and `RingLayer` active so `/v1/logs` remains populated.
- **Explicit desktop error reporting (`crates/serabot-desktop/src/ui_logs.rs`):**
  tracks `logs_error` separately (for HTTP `403`/fetch failures) and displays a
  clear banner with a Retry button instead of a blank or misleadingly empty log
  list.

## Immediate Stop chat cancellation

- **Server-side immediate abort (`crates/serabot/src/runtime/` & `store.rs`):**
  per-turn `CancellationToken` registration wraps model failover calls,
  context compaction, memory recall, and `limit_answer` in `tokio::select!`,
  immediately dropping in-flight HTTP calls and finalizing `"Stopped."` without
  waiting for slow upstream responses.
- **Queued & early-claimed task cancellation:** `Store::cancel_pending_conversation_tasks`
  atomically cancels queued/waiting tasks and records `"Stopped."`; `stop_tasks`
  closes the pre-turn claim window between worker claim and turn creation.
- **Desktop immediate feedback (`session.rs`, `ui/chat.rs`):** Stop button stays
  active during optimistic `opt-` sends (`has_optimistic_pending()`), triggers
  `"Stopping…"` visual state + Escape-to-stop guard, and clears cleanly once
  stops settle.

## Chat UI/UX polish

- **Per-bot starter quick-chips (`crates/serabot-desktop/src/chat_feed.rs`):**
  added contextual empty-state starter chips via `quick_chips(bot_id)` so new
  conversations show focused 1-click prompts tailored to each bot (including
  `whatsapp`, `analyst`, `researcher`, `maker`, `jobfinder`, etc.).

## Native WhatsApp Business Cloud API (`whatsapp` bot)

- **Per-user token connector (`crates/serabot/src/token_connectors/whatsapp.rs`):**
  connects each user's Meta WhatsApp Phone Number ID, System User Access Token,
  and optional Business Account ID (`waba_id`) via Meta Graph API `v22.0`
  (`token_connectors.whatsapp_api_base`), sealed per user and redacted from all
  logs, outputs, and upstream errors.
- **Four gateway tools (`bots/whatsapp.toml`, `prompts/whatsapp.txt`):**
  `whatsapp_phone_info` and `whatsapp_list_templates` (read-only) plus
  `whatsapp_send_message` and `whatsapp_send_template` (approval-gated writes
  bound to recipient phone number and template name), keeping Composio as an
  optional fallback.
- **Desktop Integrations card (`connector_card.rs`, `ui_connectors.rs`):**
  shows Phone Number ID, Access Token, optional WABA ID, client validation, and
  a live Graph API **Test** probe under Integrations -> Messages -> **WhatsApp**.
  Synthetic fixtures and CI checks only; not yet verified against a live Meta
  WhatsApp Business account.
