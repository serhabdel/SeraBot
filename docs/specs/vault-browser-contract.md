# Contract: bot vault, server browser, desktop 0.14 (three-pane design)

Status: in progress (0.14.0). Shared by the vault, browser and desktop work.
Owner decisions: design **B · Three-pane workspace**; browser runs **on the
server**; **one vault per user**, shared by all of that user's bots.

## 1. Vault (per user)

Storage: reuse `UserSecrets` envelope encryption (`user_secrets.rs`), one
credential per vault entry under provider id `vault:<name>`. Metadata (kind,
allowed domains, note, timestamps) is stored next to it, never the value in
clear. `<name>`: `^[a-z0-9][a-z0-9-]{0,47}$`.

Entry kinds: `password` (username + password), `api_key`, `note` (free text).

API (authenticated, acting user only, values are write-only):

- `GET /v1/vault` → `{"entries":[{"name","kind","username"?,"domains":[...],
  "note"?,"created_at","updated_at","last_used_at"?}]}` — never a value.
- `PUT /v1/vault/{name}` body `{"kind","username"?,"value","domains":[...],
  "note"?}` → the entry view. `value` 1-8192 chars. Re-PUT replaces.
- `DELETE /v1/vault/{name}` → `{"deleted":true}`.

`domains`: optional host allow-list (`example.com` also matches
`*.example.com`). Empty list = any host. Fail closed on mismatch.

Model side:

- Bots see vault **names and kinds only** (a `vault_list` tool, read-only).
- Use is by reference only: the browser `type` tool takes
  `{"ref", "secret": "<name>", "field": "password"|"username"|"value"}`.
  The server resolves it through
  `vault::resolve_for_browser(users, user_id, name, host)` (signature fixed in
  `crates/serabot/src/vault.rs`) and types it. Gateway approval binds the
  secret **name** and destination host, never the value.
- Redaction: any vault value of the acting user that appears in tool output,
  page snapshots, or outgoing model requests is replaced by `[vault:<name>]`
  before it leaves the gateway. Audit records name + host only.

## 2. Server browser (per user, headless Chromium on the VPS)

- One headless Chromium per active user session, CDP on loopback only,
  separate profile dir per user under `/var/lib/serabot/browser/<user_id>`,
  idle timeout (default 10 min) then killed. Config `[browser]` keeps the
  existing provider shape; add `provider = "spawn"` support for a server
  `chromium` binary path, `max_sessions`, `idle_timeout_secs`.
- Navigation: public http(s) only; loopback, private, link-local and
  metadata addresses refused (resolve DNS and check every address).
- Bot tools (through the gateway, untrusted output): `browser_navigate{url}`,
  `browser_snapshot{}` (text outline with element refs `e1..eN`),
  `browser_click{ref}`, `browser_type{ref, text? | secret+field}`,
  `browser_press{key}`, `browser_screenshot{}` (stored as artifact, not sent
  to the model unless the model supports images and policy allows),
  `browser_close{}`.

Owner API for the live view (acting user's session only):

- `GET /v1/browser` → `{"active":bool,"url"?,"title"?,"controller":"bot"|"user",
  "updated_at"}`
- `GET /v1/browser/screenshot` → `image/jpeg` (max 1280px wide, ≤ 512 KiB),
  `404` when no session. Clients poll at most 2/s while visible.
- `POST /v1/browser/takeover` → controller=user (bot tools then wait/fail
  with "the user is in control"). `POST /v1/browser/handback` → bot.
- `POST /v1/browser/input` (only while controller=user)
  `{"type":"click","x","y"}` | `{"type":"text","text"}` |
  `{"type":"key","key"}` | `{"type":"scroll","dy"}` | `{"type":"navigate","url"}`.
  Coordinates are in screenshot pixels.
- `POST /v1/browser/close`.

## 3. Desktop (design B)

Layout: 58 px icon rail (Chat, Bots, Approvals, Memory, Connectors, AI
providers, Settings at bottom) · 250 px conversation list with search ·
chat column with a header (bot avatar, status pill, **Vault** button) ·
**browser dock** (400 px) that slides in only while the acting user has an
active browser session, showing the live screenshot, URL, controller, and
**Take over** / **Hand back** / **Close**. While taken over, clicks and keys
on the screenshot go to `/v1/browser/input`.

Vault: a floating sheet anchored to the Vault button: list entries, add /
replace (value field write-only, never displayed again), delete with
confirm, domains as chips.

Rules: tokens from `assets/app.css` only; system font; springs/ease-out,
interruptible; `prefers-reduced-motion` honoured; feedback on press; no
blocking calls on the UI thread.
