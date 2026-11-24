# Release 0.74.12

Streamlined desktop navigation with a grouped **Settings** hub, a dedicated
top-level **Mail** workspace across Server, Desktop, and Mobile, and full
interactive SMTP/IMAP management.

## Streamlined Desktop Sidebar & Grouped Settings Hub

- **Focused 7-item left rail (`crates/serabot-desktop/src/session.rs`, `ui/shell.rs`):**
  - Left navigation rail now focuses on daily work: **Chats**, **Mail** (new),
    **Tasks**, **Projects**, **Library**, **Workflows**, and **Browser**, with
    **Settings** at the bottom.
- **Grouped Settings Hub (`ui_settings_page.rs`, `ui/settings_view.rs`, `assets/system.css`):**
  - **Memory**, **Integrations & Connectors**, **AI Providers**, **Vault**,
    **Usage & Costs**, and **Logs** now live inside **Settings** under three
    clear groups: *Preferences*, *Connections & Data*, and *System*.
  - `Session::open_section` preserves 100% backwards-compatible deep-linking
    (e.g. `"Open Vault"` on secret cards, `"Manage accounts"`, and `Ctrl+K`
    Command Palette jumps land on the exact Settings tab and trigger loads).

## Interactive Mail API (`crates/serabot/src/api/mail.rs`, `mail.rs`)

- Added UID-scoped endpoints over the existing per-user `mail` / `mail.<label>`
  credentials:
  - `GET /v1/mail/search` (server-side IMAP search)
  - `POST /v1/mail/messages/{uid}/flag` (mark read/unread, star/unstar; audited)
  - `POST /v1/mail/messages/{uid}/move` (move to folder; audited)
  - `DELETE /v1/mail/messages/{uid}` (expunge; audited)
  - `POST /v1/mail/send` (blocklist-scanned, UID-scoped Library `artifact_hashes`
    attachments, automatic account signature, audited as `mail.send_ui`; bot
    `mail_send` remains strictly approval-gated)
  - `GET /v1/mail/messages/{uid}/attachments/{index}` (raw attachment download
    with `X-Content-Type-Options: nosniff`)
  - `POST /v1/mail/messages/{uid}/attachments/{index}/save-to-library` (saves
    email attachments directly into the member's Library + OneDrive mirror)

## Dedicated 3-Pane Desktop Mail Workspace (`crates/serabot-desktop/src/ui_mail.rs`)

- **Left pane:** Multi-account switcher, **Compose** button, smart-icon folder
  list, and **Manage accounts** link to Settings → Integrations.
- **Middle pane:** Server-side IMAP search bar, `All` / `Unread` / `Starred`
  segmented filter, unread indicator + bolding, star toggle, attachment badge.
- **Right pane:** Full message reader with **Reply**, **Forward**, **Star**,
  **Mark Read/Unread**, **Move**, **Delete** (inline confirm), **Ask bot**
  (opens Chat pre-filled with email context), and **Attachments** (**Save to
  Library** + **Save to disk**).
- **Compose / Reply / Forward modal:** Library attachment picker (no re-upload)
  and live signature note.
- Updated `Ctrl+K` Command Palette (`ui/sheets.rs`) and `ui_connectors.rs`
  (**Open Mail** button).

## Mobile Mail Interactive Parity (`apps/serabot_mobile/`)

- Upgraded `MailScreen` & `ApiClient` with server-side IMAP search, **Star** &
  **Mark Read/Unread**, **Delete** with confirmation, **Save to Library** on
  attachments, and a **Compose / Reply** bottom sheet.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo clippy -p serabot-desktop --features ui-check --all-targets -- -D warnings`
- `cargo test -p serabot-desktop --locked`
- `cargo test -p serabot --lib --locked mail`
- `cd apps/serabot_mobile && flutter analyze && flutter test` (242 passed)
- Pro security & code review pass: no security blockers found.
- Not verified live: real Hostinger IMAP search/flag/move/delete/send & Windows
  WebView2 visual pass on the 3-pane Mail view and grouped Settings hub.
