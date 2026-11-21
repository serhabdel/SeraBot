# Release 0.74.2

Second patch batch on 0.74: native SMTP/IMAP Mail connector desktop card & skill,
UID-scoped full-text conversation history search (server + desktop), Tasks /
Kanban / Calendar UI polish, scanned-PDF OCR hint, and Cargo cleanup.

## Mail connector (SMTP/IMAP) — desktop UI, skill & bots

- **Desktop Integrations card (`crates/serabot-desktop/src/ui_connectors.rs`, `ui_integrations.rs`, `api.rs`, `assets/pages-connect.css`):**
  - Added the **Mail (SMTP / IMAP)** card on the Integrations / Connectors screen
    with Connected / Not connected badge, one-click **Hostinger** preset
    (`smtp.hostinger.com:587 STARTTLS`, `imap.hostinger.com:993 SSL`), editable
    SMTP/IMAP host, port, security, username, masked write-only password, and
    optional From address.
  - Displays live per-side SMTP and IMAP test results after saving, plus a
    Disconnect button wired to `/v1/mail/connect`, `GET /v1/mail`, and
    `DELETE /v1/mail`.
- **OneDrive card polish (`ui_connectors.rs`, `ui_integrations.rs`):**
  - Clarified full-drive capabilities (`0.74.1+`) and added a direct Reconnect
    action for expired grants (`invalid_grant`).
- **Skill, bots & activity labels (`skills/mail/SKILL.md`, `shipped.rs`, `bots/inbox.toml`, `bots/main.toml`, `runtime/activity.rs`):**
  - Added built-in `mail` skill (`Mail (SMTP/IMAP)`) with connection check,
    tool table, attachment/recipient limits, and untrusted-content rules.
  - Enabled `mail_list`, `mail_read`, and approval-gated `mail_send` on `inbox`
    and `main`, with human-readable activity labels.

## Full-text conversation history search (server + desktop)

- **Server (`crates/serabot/src/store.rs`, `api/conversations.rs`, `api/tests/chat_api.rs`):**
  - Added UID-scoped `Store::search_conversations` and
    `GET /v1/conversations/search?q=...&bot=...&limit=...`.
  - Searches both message content and conversation titles with escaped SQL
    `LIKE` wildcards, deduplicates by conversation (newest match first), and
    returns bounded, UTF-8-safe ~160-char snippets.
  - Verified with unit & API tests for UID isolation, bot filtering, and literal
    `%` / `_` handling.
- **Desktop History popover (`crates/serabot-desktop/src/ui/row_menu.rs`, `api.rs`, `assets/system.css`):**
  - Typing 2+ characters in the History popover keeps instant client-side title
    filtering and queries `/v1/conversations/search` for the active bot, showing
    a **Message matches** section with snippet previews that jump straight to
    the matching chat.

## Desktop Tasks / Kanban / Calendar UI polish

- **Design system alignment (`crates/serabot-desktop/src/ui_tasks.rs`, `assets/pages-work.css`):**
  - Upgraded the Tasks, Kanban, and Calendar views with a consistent page
    header, stat tiles (open, due soon/overdue, completed, upcoming events),
    segmented view/filter controls, crisp theme-aware cards, empty states, and
    `--fs-*` typography tokens.

## Browser & file-reading parity + Cargo cleanup

- **Scanned PDF hint (`crates/serabot/src/browser/tools_ext.rs`):**
  - When `browser_download` saves a PDF whose first-page text extraction is
    empty (scanned/image-only PDF), the tool output explicitly hints using
    `pdf_to_images` / OCR.
- **Bot reader parity (`bots/jobfinder.toml`, `bots/researcher.toml`):**
  - Ensured download-capable bots consistently carry `pdf_inspect` /
    `pdf_to_images` alongside Office readers.
- **Build hygiene (`crates/serabot/Cargo.toml`):**
  - Removed the unused `text-splitter` dependency.
  - Closed superseded draft PR #134 (`vibe/email-smtp-imap-2c32c0`).

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo clippy -p serabot-desktop --features ui-check --all-targets -- -D warnings`
- `cargo test --workspace --locked` (targeted + desktop suites locally, full matrix in CI)
- Not verified live: real Hostinger SMTP/IMAP login via desktop card, Windows
  WebView2 render pass on Tasks/Kanban/Calendar & History message search.
