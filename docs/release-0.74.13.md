# Release `0.74.13`

## Summary

- **Native Mail Engine & Workspace Upgrade (Hostinger / Dovecot + HTML + Signatures + Send Later)**
  - **IMAP folder `LIST` fix (`mail.rs`)**: Fixed `parse_list_name` to parse the mailbox token after the hierarchy delimiter instead of `rfind('"')`, resolving `HTTP 502` on Hostinger/Dovecot responses such as `* LIST (\HasNoChildren \Sent) "." INBOX.Sent`.
  - **RFC 2047 multi-word header decoding (`mail.rs`)**: Added full `?B?` and `?Q?` decoding across multi-word subjects and senders with UTF-8, ISO-8859-1, and Windows-1252 support and encoded-word whitespace collapse.
  - **Recursive MIME + HTML Sanitizer + `cid:` inline images (`mail.rs`)**: Recursively parses nested `multipart/mixed`, `multipart/related`, and `multipart/alternative` parts with `quoted-printable` and `base64` decoding; strips scripts, forms, iframes, `on*` handlers, and unsafe URLs via `scraper`; rewrites `cid:` inline images (`image/png`, `jpeg`, `gif`, `webp`) to safe `data:` URLs; blocks remote images by default with opt-in `allow_remote_images=true`.
  - **Sandboxed HTML Reader (`ui_mail.rs`)**: Renders email HTML inside `<iframe "sandbox"="" srcdoc="...">` with a **HTML / Plain** toggle pill and a **"Remote images blocked · Load remote images"** banner.
  - **Rich HTML Composer & In-House Signature Builder (`mail.rs`, `api/mail.rs`, `ui_mail.rs`)**: Added a compose formatting toolbar (Bold, Italic, Underline, List, Link, Quote, Preview) emitting `multipart/mixed` -> `multipart/related` -> `multipart/alternative` with `cid:signature-logo@serabot`, plus a dedicated **Signature Builder** modal with Library image/logo picker, live preview, and `PUT /v1/mail/signature` (no password re-entry required).
  - **In-House Email Scheduler ("Send Later") (`0051_scheduled_mail.sql`, `store.rs`, `api/mail.rs`, `scheduler.rs`, `ui_mail.rs`)**: Added UID-scoped `scheduled_mail` persistence, `POST /v1/mail/schedule`, `GET /v1/mail/scheduled`, `PATCH /v1/mail/scheduled/{id}`, `DELETE /v1/mail/scheduled/{id}`, a 30-second background delivery loop (re-scanning the blocklist right before sending), **Send later** quick presets in the composer, and a **Scheduled** virtual folder with Reschedule/Cancel.

- **Instant, Smooth Agent Stop (`runtime/mod.rs`, `turn_loop.rs`, `session.rs`)**
  - **Desktop out-of-band Stop**: Clicking Stop immediately sets `stopping`, clears queued sends and `stop_queued`, and dispatches `POST /v1/conversations/{id}/stop` on a dedicated `serabot-stop` thread without waiting for `self.ready()`.
  - **Server cascading & mid-tool Stop**: `request_stop` cascades to active delegated child handoffs/turns, and `turn_loop` races tool executions against `CancellationToken` via `tokio::select!`, preserving `uncertain` / `needs_review` semantics when an effectful tool is cancelled mid-flight.

- **Browser DNS Resilience & Diagnostics (`browser/guard.rs`)**
  - Retries `tokio::net::lookup_host` once after 300 ms on transient failure and returns an actionable error naming the exact failing hostname (`DNS lookup failed for '{name}' ...`).

- **Redesigned 2-Column Chat Model Selector Card (`ui/chat.rs`, `system.css`)**
  - Fixed `.model-pop` upward clipping by anchoring it downward (`top: calc(100% + 6px); bottom: auto; right: 0;`).
  - Redesigned `HeaderModelChip` into a 2-column card with a connected Providers rail (brand icons + model counts + Local ACP) on the left and search, account switcher, scrollable model list, and pinned **Thinking** segmented bar (`Default | Off | Low | Medium | High | Max`) on the right.

## Verification

- Verified with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo clippy -p serabot-desktop --features ui-check --all-targets -- -D warnings`, unit/integration tests (`mail`, `mail_api`, `browser::guard`, `runtime::turn_loop`, `store` migration 51, `serabot-desktop`), and `flutter analyze` / `flutter test`. Live Hostinger IMAP/SMTP and Windows WebView2 rendering remain for owner verification.
