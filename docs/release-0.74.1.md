# Release 0.74.1

First patch batch on 0.74: full OneDrive browsing/search/recent/info and a new
`OneDrive connection` skill, browser download & PDF viewer fixes, integrated
native SMTP/IMAP mail connector (PR #134), desktop UI redesigns (Conversation
history switcher + sidebar recent chats, Workflows, Projects, Memory), and a
comprehensive `README.md` update.

## OneDrive: full drive browsing, search, recent, info & skill

- **Drive-root browsing & metadata (`crates/serabot/src/onedrive.rs`, `maker.rs`):**
  - `onedrive_list` now accepts an optional `path` from the member's drive root
    (defaults to `/`) in addition to legacy `subfolder` under the dossier
    folder, returning modified timestamps, web URLs, and child counts.
  - Added read-only tools `onedrive_search` (OData-escaped query across the
    member's own drive), `onedrive_recent` (`GET /me/drive/recent`, filtered to
    own-drive items), and `onedrive_info` (metadata, SHA-256 hash, MIME, child
    count, timestamps).
  - `onedrive_download` suggests the matching reader/editor tool by extension
    (`pdf_inspect` / `pdf_to_images` / `document_index`, `xlsx_import`,
    `docx_import`, `pptx_import`, `image_inspect`).
  - `onedrive_upload` remains approval-gated (`DESTRUCTIVE_TOOLS`) and now
    accepts an optional drive-root `folder` (e.g. `"Documents/Jobs"`) as well as
    `subfolder`, binding the exact destination path in the approval card.
- **Fleet & skill (`skills/onedrive/SKILL.md`, `bots/*.toml`):**
  - Expanded `skills/onedrive/SKILL.md` into the **OneDrive connection** skill
    (connection check, root browsing, search, read/edit workflow, approval-gated
    delivery, security limits).
  - Enabled read-only OneDrive tools and matching document readers on `main`,
    `analyst`, `researcher`, `maker`, and `jobfinder`.

## Browser downloads & PDF viewer fallback

- **PDF viewer & session-authenticated downloads (`crates/serabot/src/browser/`):**
  - When clicking a link opens a PDF or document directly in Chromium's built-in
    viewer (or a new tab) instead of firing `Browser.downloadWillBegin`,
    `click_download` falls back to fetching the resource inside the page's
    authenticated session via CDP `Network.loadNetworkResource` + `IO.read`
    stream with the 25 MB cap and public-URL validation.
  - `browser_download` now accepts `ref` (click element), `url` (direct download
    in the member's session), or `current: true` (save the active tab's
    document).
  - Download behavior is restored cleanly after each download attempt so PDF
    navigations are not left blocked.
  - `browser_download` and `browser_pdf` outputs now include the artifact hash,
    MIME type, recommended reader tool (`pdf_inspect`, `document_index`,
    `xlsx_import`, `docx_import`), and—for PDFs with extractable text—a bounded,
    untrusted-fenced first-page text preview.
  - Documented in `docs/browser-downloads.md`.

## Mail connector (SMTP/IMAP, integrated from PR #134)

- **Per-user SMTP/IMAP (`crates/serabot/src/mail.rs`, `api/mail.rs`):**
  - Hand-rolled SMTP (`EHLO`/`STARTTLS`/`AUTH LOGIN`/`MAIL`/`RCPT`/`DATA` with
    dot-stuffing/`QUIT`) and IMAP (`LOGIN`/`SELECT`/`UID SEARCH`/`UID FETCH
    BODY[]`) over `tokio-rustls` + `webpki-roots`; plain non-TLS connections
    are refused off-loopback.
  - Per-user credentials stored in `user_secrets` under provider `mail`.
  - Tools: `mail_send` (approval-gated in `DESTRUCTIVE_TOOLS`, MIME
    `multipart/mixed` with RFC 2047 subjects and base64 attachments),
    `mail_list`, `mail_read`.
  - API: `POST /v1/mail/connect` (with live SMTP + IMAP login test, audited as
    `mail.connect`), `GET /v1/mail`, `DELETE /v1/mail` (`mail.disconnect`).

## Desktop UI: Conversation history, Workflows, Projects & Memory

- **Conversation history (`ui/row_menu.rs`, `ui/shell.rs`, `bot_chats.rs`, `assets/system.css`):**
  - Solid, high-elevation popover card (no background chat bleed-through in
    light or dark mode) with an explicit **History** button in the chat header,
    instant search input, date-bucket grouping (*Today*, *Yesterday*, *This
    week*, *Older*), relative activity timestamps, unread badges, and keyboard
    navigation.
  - Added a collapsible **Recent chats** section under the active bot in the
    left sidebar with one-click switching and an *All chats…* trigger.
- **Workflows page (`ui_workflows.rs`, `workflows.rs`, `assets/pages-work.css`):**
  - Redesigned header, stat tiles (active routines, next scheduled run, active
    workflow checklists, attention needed), segmented view switcher (*All*,
    *Workflows*, *Routines*, *History*), human-readable schedule descriptions,
    vertical timeline steppers for workflow plans, and tidied creation/edit
    forms.
- **Projects page (`ui_projects.rs`, `projects.rs`, `assets/pages-work.css`):**
  - Redesigned project cards (file count, size, check status pill, relative
    updated time) and 3-pane workspace layout (file tree with extension badges
    and filter, tabbed code editor with dirty indicator and breadcrumb bar,
    Problems panel with severity filter chips and click-through to `file:line`).
- **Memory page (`ui/memory.rs`, `ui_memory_review.rs`, `assets/pages-data.css`):**
  - Stat tiles (saved facts, bots with memories, added this week, review queue),
    search + scope/category/sort filter bar, weekly memory review card with diff
    boxes, and rich fact cards with inline edit, pin, and delete actions.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace --locked`
- Not verified live: real OneDrive account browsing/search/upload, live PDF
  downloads on external websites, real Hostinger SMTP/IMAP mailbox login, and
  Windows WebView2 visual pass.
