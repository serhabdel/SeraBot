# Release `0.74.16`

## Summary

- **AI Providers — Remove Claude Agents (Local ACP) & Improve Bot Routing**
  - **Integrated server models everywhere (`main.rs`, `ui/shell.rs`, `ui/chat.rs`, `ui_bots.rs`)**: Removed Claude Code (Local ACP) from the Desktop UI (topbar toggle, 2-column model picker rail/list, chat view override, and bot execution-engine toggle) so every bot cleanly runs on integrated server model routes.
  - **Bot Routing overview in Providers (`ui/providers.rs`, `pages-connect.css`)**: Added a **Bot routing** section directly below **Default for new chats** in **Settings → Providers** listing every enabled bot's current model, thinking badge, inline model selector (grouped by provider + Default/inherit), and a **Fallbacks** button that opens the bot's full route & failover editor right over Providers.

- **WhatsApp Connector — Live Connected Profile & Cleaner Allowlist**
  - **Server live profile (`whatsapp_link/client.rs`, `whatsapp_link/mod.rs`, `api/whatsapp_link.rs`)**: Added `WhatsappLink::profile_view` reading live `push_name`, `pn()` (phone), and `is_connected()` from `whatsapp-rust`, automatically backfilling `phone` in SQLite when discovered on reconnect, and exposing `push_name`, `phone`, `updated_at`, `connected`, and structured `profile` on `GET /v1/whatsapp-link`.
  - **Desktop Connected Profile Card (`ui_whatsapp_link.rs`, `system.css`)**: Replaced the bare phone line with a rich connected profile card (initials avatar, live green/amber status dot, push name, formatted phone, linked timestamp, Refresh button, compact risk footnote when linked) and chip-based Allowed Chats with inline `+ Add` / `×` removal plus Bulk edit toggle.

- **Custom Email Connector — Multi-Account Management & Password-Optional Updates**
  - **Server password-optional updates & full account metadata (`api/mail.rs`, `mail.rs`)**: `POST /v1/mail/connect` now accepts an empty/omitted `password` when updating an existing mailbox and reuses its sealed password from `UserSecrets` (while still requiring a password for brand-new mailboxes) and preserves existing HTML signature/logo when signature fields are omitted. `GET /v1/mail` returns full non-secret settings (`smtp_host`, `smtp_port`, `smtp_security`, `imap_host`, `imap_port`, `imap_security`, `from`, `signature*`) per account.
  - **Desktop Connected Mailboxes UI (`api.rs`, `ui_connectors.rs`)**: Upgraded `MailConnectorCard` with a **Connected Mailboxes** list (per-account **Edit**, **Open**, and **Disconnect** with confirmation), a **+ Add another mailbox** flow, and password-optional editing (`Leave blank to keep saved password`) with form auto-collapse when connected.

## Verification

- Verified with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo clippy -p serabot-desktop --features ui-check --all-targets -- -D warnings`, `pro` security & code review, server unit/integration tests (`mail`, `mail_api`, `whatsapp_link`), and `serabot-desktop` unit tests (`--lib` + `--features ui-check`). Live WhatsApp linking and Hostinger multi-account updates remain for owner verification.
