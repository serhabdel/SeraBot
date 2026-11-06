# Desktop client (Windows)

**Primary owner desktop:** `crates/serabot-desktop` — Rust with **Dioxus
Desktop** (WebView2). The UI layer is Windows-only; the API client, session,
credentials, updater and event stream are UI-independent Rust and are tested on
Linux. Mobile work lives under `apps/` separately; it does not replace this
console.

### Why Dioxus Desktop

The owner API client, endpoint/token validation, approval semantics, and
no-blind-resubmit rules are implemented once in Rust with integration tests.
Dioxus renders a WebView2 shell from Rust and CSS with no Node runtime, no
bundled web server, and no web admin UI on the server. The server binary remains
the default workspace member, so Linux releases and OVH deployments do not
include the desktop GUI.

A `ui-check` Cargo feature type-checks every Dioxus component on Linux without a
WebKit toolchain; CI runs it on Ubuntu. The Windows job builds and smoke-runs the
actual WebView2 binary.

## Build and launch (Windows, MSVC)

```bat
scripts\cargo-win.bat test -p serabot-desktop --locked
scripts\cargo-win.bat run -p serabot-desktop --locked
```

Release binary (optional):

```bat
scripts\cargo-win.bat build --release -p serabot-desktop --locked
target\release\serabot-desktop.exe
target\release\serabot-desktop.exe --version
```

`--version` (or `-V`) prints `serabot-desktop <version>` and exits without
opening a window; the release workflow uses it as a smoke test.

Prebuilt Windows clients (exe + zip) are attached to GitHub Releases on tag
pushes (`v*`); see [CI and secrets](ci-and-secrets.md#releases).

Build the server only with:

```bat
scripts\cargo-win.bat build --release -p serabot --locked
```

## First run and credentials

On first launch the client shows a centered connect card instead of the
console.

- **Server URL** — HTTPS origin, or exact loopback HTTP for an SSH tunnel
  (`http://127.0.0.1:8787`, `http://[::1]:8787`, `http://localhost:8787`).
  Credentials, paths, queries, fragments, redirects, and lookalike loopback
  hosts are rejected. Prefill order: a saved origin, else process environment
  `SERABOT_OWNER_API_URL`, else the value compiled from the GitHub Actions
  secret of the same name. Invalid values are ignored. See
  [CI and secrets](ci-and-secrets.md).
- **Sign in with Microsoft** - the only sign-in. The console starts a
  loopback listener on `http://127.0.0.1:<port>/auth-done`, asks the server for
  the Microsoft sign-in page (`POST /v1/auth/login/start` with a PKCE handoff
  challenge and a random state), and opens it in the system browser. When the
  browser returns, the console checks the state and trades the one-time handoff
  code for a session token (`POST /v1/auth/handoff`), then reads the user
  (`GET /v1/me`). The browser tab only shows "You can close this tab". See
  [the sign-in contract](specs/native-signin-contract.md). Nothing is pasted.
- **I have an invite code** - a link under the button opens an invite-code
  field for a first sign-in. The code travels with `login/start` and is only
  consumed by the server at the callback. Server error codes (`denied`,
  `invite_required`, `invite_invalid`, `disabled`, `max_users`, `server`) show
  as plain sentences.

Only after the first authenticated reply is the origin written to
`%APPDATA%\SeraBot\settings.json` (non-secret; `SERABOT_SETTINGS_DIR` overrides
the directory for development) and the session token stored in Windows
Credential Manager (service `SeraBot`, account `owner-api-token`; the name is
unchanged so a token saved by an earlier release keeps working). Later launches
reconnect from the saved origin and credential.

## Session lifecycle

- **Sign out** (Settings, Connection, or the account menu) calls
  `POST /v1/auth/logout`, then deletes the Credential Manager entry and clears
  chat text, history, directory lists and pending actions from memory. If the
  server cannot be reached the device is still signed out.
- Settings shows the signed-in name, email and role from `GET /v1/me`.
- Any HTTP 401 deletes the stored token and returns to the sign-in screen with
  "Your session ended. Sign in again."
- When the sign-in response carries `expires_at`, the console signs out with
  the same message once that time passes, without waiting for a 401. A token
  restored from the Credential Manager has no known expiry and relies on the
  401 path.
- Browser and Explorer launches run on a helper thread; a failure shows as a
  toast. The HTTP worker keeps one client (connection pool) per endpoint and
  token.
- An older install with a saved token stays signed in while the server still
  accepts it; otherwise it lands on the Microsoft screen.
- Operator device tokens (`serabot token mint`) are server-side break-glass
  only. The console has no field for them.

## Behaviour

The connected console is a Dioxus WebView2 shell with a three-pane, glass
layout: a 260px glass **sidebar**, the conversation column, and an optional
300px **inspector** (which becomes a sheet below 1100px; below 920px the
sidebar becomes an overlay drawer). All network work stays on the Rust
worker/SSE threads, never on the UI thread. The design system — tokens,
materials, motion, and the shared class contract — is specified in
[Desktop redesign](desktop-redesign.md) and implemented in
`crates/serabot-desktop/assets/app.css`.

- **Sidebar** — brand mark, **New chat**, a search field that filters labels
  and assistant names (`Cmd/Ctrl+K`), the nav list (Chats, Browser, Approvals,
  Memory, Connectors, Vault, AI keys, Settings), the connection
  status chip, and the owner chip. Thread rows reveal new-message, rename and
  archive actions on hover.
- **Appearance** — a Light/Dark switch sits on the connect card, in the
  topbar, and in **Settings → Appearance**. The palette is a true Apple-style
  pair: a deep neutral dark theme (`#0b0b0f` surfaces, `#0a84ff` accent) and a
  true light theme (`#f5f5f7` canvas, white surfaces, `#0071e3` accent), with
  glass materials, layered soft shadows, and spring easing. The choice is
  saved locally in `settings.json` and applies before the first connection.
- **Topbar** — the active conversation title and kind badge, the team avatar
  stack for a group, the model selector, the appearance switch, and refresh.
- **Chat** — flat avatar-led message rows, not heavy bubbles: avatar, display
  name, role and live state, timestamp, then the body. The owner's own
  messages align right in a capsule. A peer bot's message in a group carries
  a hairline accent border in that bot's hue. An empty conversation shows the
  bot roster, and a group shows its team roster with a one-tap add action.
  Chat history is capped at the server's recent window (100 messages).
- **Bot settings** — since 0.16 there is no separate Bots screen and no
  "Inspector". Open **Bot settings** from the chat header, or from the gear on a
  bot's row in the Chats sidebar. The panel slides in from the right: the
  header is the bot's picture and name; one form holds the picture
  (upload/remove, saved at once), name (1–48), description (up to 280),
  "How it should behave" (the instructions, multi-line), model (grouped by
  provider; **Default** follows the chat), browser profile (**Default profile**
  or one from the Browser page) and the plain-language abilities (saved at
  once). **Save** sends only what changed in one `PUT /v1/bots/{id}`; **Cancel**
  drops the edits. Problems show inline and keep Save off. Skills, the bot's id,
  status, routing and memory counts sit under **Advanced**. **Files** is the
  second tab (the old Library); in a group the first tab lists members and the
  group rules.
- **Composer** — a floating capsule with a gradient scrim so text scrolls
  under it. **Enter** sends, **Shift+Enter** adds a line, **Esc** closes the
  topmost sheet or menu. **Send** is enabled only when the catalog selection
  is saved and approved, no unknown outcome is pending, and the conversation
  accepts turns. Sections and groups are read-only structure.
- **The "+" menu** — in the chat column header and the composer row. It opens
  New direct chat, New group, New section and New bot. A new group can be
  created together with its initial team (1–5 bots); the room is created
  first and its team is set immediately after, so the two steps are one
  gesture.
- **Team sheet** — the group header shows the team as an avatar stack;
  clicking it opens the sheet with each member's role, live state and model, a
  remove action, an add-bot picker with search, and the room mode
  (**selected** — the orchestrator picks who answers — or **all** — every
  member answers every owner message). Changes are written through
  `PUT /v1/conversations/{id}/team` and are audited server-side.
- **Microphone** — the composer has a mic button. It records through the
  WebView2 `MediaRecorder` (opus/webm, with an mp4 fallback), posts the bytes
  to `POST /v1/audio/transcribe`, and drops the transcript into the composer
  as an editable draft. Nothing is sent until the owner sends it, and a
  recording failure never blocks the chat.
- **Conversations** — create sections, groups, direct threads, and rooms with a
  display name (stored as the conversation title) and an optional parent,
  rename a thread, and archive it (soft; messages stay on the server). Rooms are
  conversations with the orchestrator and the specialists it delegates to, not
  multi-person chats. The server assigns ids (`sec:`, `grp:`, `dir:`, `room:`).
  Pre-kind legacy rows are shown as turn-capable direct threads.
- **Memory** — list, search, add, and delete owner-scoped facts. Each write goes
  through `POST /v1/memories` and is audited. The screen states that the
  orchestrator receives the 20 most recently updated memories; specialists never
  list them on their own.
- **Providers** — provider cards show kind, models, catalog state, routing
  classes, and secret **names** with presence. **Fetch models** re-reads the
  provider catalog (requires `catalog_fetch = true` on the server). Model
  selection is saved per conversation and applies to the next turn; a running
  turn keeps its pinned route. Only egress-approved providers are selectable.
  **Add provider** offers presets for OpenAI-compatible, Mistral, Groq,
  OpenRouter-compatible, Ollama / local OpenAI-compatible, and a custom
  entry, so the owner normally only pastes the key. Anthropic's native
  Messages API is not a transport yet; an OpenAI-compatible gateway in front
  of Anthropic works.
- **Connectors** - "Let your bots use your accounts and tools." Microsoft
  comes first, with **OneDrive** on top: OneDrive is per user, so every member
  sees **Connect OneDrive** (`POST /v1/connectors/onedrive/start`, the
  returned `https` consent page opens in the system browser, never an
  embedded webview) and, once connected, **Disconnect**
  (`DELETE /v1/connectors/onedrive`). Every other card shows one main action
  that fits its state: **Connect** / **Reconnect** (opens the provider's
  authorization page), **Sign in** (ElevenLabs PKCE), or **Add key** /
  **Replace key** with a paste field and a **Get a key** link. Developer
  details (kind, enabled state, client id, redirect URI, secret names, auth
  mode, Turn on/off, Save settings, Test) sit behind an **Advanced**
  disclosure that only administrators see. Turning a connector on only makes
  its tools available; grant them to a bot in **Bots**.
- **Settings → Capabilities** — reads the authenticated health payload and
  shows Audio (on/off with the configured transcription and understanding
  models), Firecrawl, Telegram, and whether model catalogs are live or static.
  Audio can be enabled and its two Mistral model names edited here. Audio uses
  the provider credentials already in the age store; the console never asks for
  a second copy of the chat key.
- **Settings → Sandbox** — the live posture of the remote server: policy
  version, pending tasks and approvals, which tools require owner approval,
  data-egress consent per provider, and whether a restart is pending. Values
  that the server does not report are shown as unknown, never invented.
- **Live updates** — the Rust core opens `GET /v1/events` on a dedicated thread
  with its own HTTP client, reconnects with backoff, and resumes from the last
  event id (`after=` / `Last-Event-ID`). Events coalesce into at most one
  refreshed read per request gap; task polling remains as a stuck-stream guard
  and is the only path when the stream is unavailable. Team events
  (`team.member_set`, `team.asked`, `team.replied`, `team.turn_completed`)
  arrive on the same stream, so an open group sheet stays current.
- **Bots** — the roster loads with the Chats sidebar. New bot and New group
  are in the sidebar's **New chat** menu. A switched-off bot has **Finish
  setting up** on its row. Each row (and Bot settings) can **Switch off/on**
  (`PUT /v1/bots/{id}` with `enabled`) or **Delete** (`DELETE /v1/bots/{id}`,
  after a confirm step); the orchestrator can do neither. The instructions go
  to the server as `prompt`. Tools that require owner approval are listed under
  Advanced in the bot's settings.
- Approvals load from `GET /v1/approvals`. Once / Always / Deny call
  `POST /v1/approvals/{id}/decide` with the same semantics as Telegram. Decisions
  still bind canonical arguments, destination, artifact hash, policy version,
  and expiry.
- Submitted messages are tracked by task id. If a submit response is lost, the
  client reports an unknown outcome and refuses to resend automatically; review
  server state first.
- Late responses from a previous connection or conversation are discarded.

## Administration

Provider, connector, audio, and bot-profile writes use the owner token and land
in `admin-overrides.json` beside the SQLite database (on the standard host,
`/var/lib/serabot/admin-overrides.json`). That file holds non-secret settings
only; credentials go to the age store and are never returned by the API. The
overlay is merged over `serabot.toml` at startup, so provider, connector, and
audio changes take effect after **Restart server and apply** in the status
banner. Bot display profiles are read at request time and apply immediately.

- The restart endpoint refuses while a task is queued, running, or awaiting
  approval, and requires an explicit `confirm` in the request.
- The service exits with a failure status so `serabot.service`
  (`Restart=on-failure`) brings it back; the console reconnects automatically.
  A server started by hand will stop instead.
- Every write is audited (`config.provider_saved`, `config.connector_saved`,
  `config.audio_saved`, `config.restart_requested`). Rejection messages name the
  field or secret to fix, never a secret value.
- A provider id cannot be `default` or `auto`, and a new key cannot overwrite a
  secret that another provider still uses.
- `serabot.toml` remains the source for server bind, tunnels, OAuth client IDs,
  and the base provider set; the app never edits it.
- If an overlay cannot be applied at startup (for example a secret was removed
  out of band), the server logs it, keeps the base configuration, and stays
  reachable so the console can repair it.

## Bots and skills

The bot calls below are made from the Chats sidebar (**New chat** menu) and the
Bot settings panel; there is no separate Fleet screen on the desktop since 0.16
(the mobile client keeps its Fleet tab). Each submit uses the same 3-second gap
as other requests and is not retried if the outcome is unknown.

- **New specialist** — `POST /v1/bots` with a lowercase bot id, a description
  (stored in the prompt stub; there is no separate display-name field), tools
  chosen from the specialist allowlist, and optional skill ids. `delegate` is
  not offered. The server writes a disabled specialist. Restart before it can
  run, and add it to `main.delegates` yourself if `main` should hand it work.
  **Refresh bots** calls `GET /v1/bots` for on-disk summaries (`id`, `role`,
  `enabled`, `live`, tools, skills, approval-gated tools). Prompts and secrets
  are not returned. Tools that require owner approval are highlighted (for
  example Firecrawl search/crawl); hover shows the approval rule.
  `live: false` means the file is not loaded until you restart.
- **New skill** — `POST /v1/skills` with a lowercase skill id, a one-line
  description, and an optional markdown body. There is no path field. The
  server writes the pack under the configured skills directory and reports
  `live: false`. Restart, then add the id to a bot's skills list before that
  bot can load it. **Refresh skills** calls `GET /v1/skills` for on-disk
  summaries. Bodies are not returned.
- Conversation nodes are created in **Conversations**.

The mobile Fleet tab calls the specialist, skill, and conversation endpoints. See
[Mobile client](mobile-client.md).

## Updates

Release builds receive `SERABOT_RELEASES_REPO=owner/repo` and
`SERABOT_BUILD_SHA` at compile time. In the background (never blocking the UI)
the client reads the latest GitHub release, compares its semver tag with its
own version, and shows a banner: **Update vX.Y.Z available**, with asset size,
collapsible release notes, **Install and restart**, **Later**, and
**Check now**. Nothing installs silently.

- Only `serabot-desktop-latest-windows-x64.exe` is considered. It is downloaded
  with its `SHA256SUMS` entry to `%LOCALAPPDATA%\SeraBot\updates`, and the
  SHA-256 is verified before the file is kept or used. A mismatch aborts the
  update and deletes the partial download. Redirects are followed only over
  HTTPS, and older downloads and abandoned `.part` files are removed after a
  new update is verified.
- **Install and restart** launches the verified copy through a detached, hidden
  `cmd` helper that waits for this process to exit, replaces the running exe,
  restarts it, and removes itself. A failed replacement keeps the old exe and
  writes `update.log` in the updates directory.
- Offline, 403/429, and other network errors are non-fatal; the banner simply
  does not appear. The last verified update is cached and reused when GitHub is
  unreachable, and its hash is checked again before use.
- Source builds have no `SERABOT_RELEASES_REPO`, so updates are disabled; the
  footer and **Settings → Updates** show a developer build. Forks that publish
  their own releases can set the variable when building.

## Layout (0.14, design B)

A 72 px icon rail with a small text label under every icon (Chats, Browser, Approvals with a pending count, Memory, Connectors with a plug icon, Vault, AI keys; Settings with a gear and the account menu at the bottom, which holds Settings, All chats, the theme switch and Sign out), a 250 px conversation list (Chat only) whose head has a labelled **New chat** button that opens a "Choose a bot" list, and the chat column with its header (bot avatar, status pill, Bots in this chat, Reload, **Watch browser** only while a browser session is active, **Bot settings**). When the chat column is narrow the header buttons keep their icon, tooltip and accessible name and drop the text. The settings panel and the browser dock slide in from the right and leave the same way. The dock shows one control button at a time (**Take control** / **Give control back**), a status line ("You're in control: click and type in the page. The bot is paused." or "<Bot> is using the browser."), **Hide panel** (hides only; the session keeps running and Watch browser brings it back) and an overflow menu with **End browser session**. The screenshot fills the panel width at its own aspect ratio. The dock shows the browser of the bot you are talking to (its profile); on the Browser page it steps aside. Chat tool rows use plain sentences for known tools (for example "Signed in with your saved login "venue-portal". The bot never saw it." or "Opened example.com"). Every icon-only button has a tooltip and an accessible name. Motion follows `prefers-reduced-motion` (cross-fades) and `prefers-reduced-transparency` (solid surfaces).
## Browser page, profiles and Vault (0.16)

Contract: [`specs/browser-profiles-contract.md`](specs/browser-profiles-contract.md).

- **Browser** (rail) — a grid with one card per open browser: a live picture,
  the profile name, the site, the bot driving it and a pill (**Bot is driving**
  / **You're driving**). With nothing open it says "No browser open. Ask a bot
  to open a website." Pictures come from
  `GET /v1/browser/sessions/{id}/screenshot`, about once a second per card
  (never more than three requests a second), only while the page is on screen
  and the window has focus. A rate limit (429) pauses all background polling
  with the same Retry-After back-off as the rest of the app.
- **Focus view** — click a card. The browser is large on the left; the
  conversation of the bot that last drove it is on the right (the normal chat,
  with its composer). **Take control** / **Give control back**, **Hide**
  (back to the grid), **End session** (asks first) and a **full window** toggle
  (`Esc` leaves it). While you hold control, clicks, the wheel and typing on the
  picture go to `POST /v1/browser/sessions/{id}/input`.
- **Profiles** (below the grid) — list, add, rename, delete (asks first; the
  default stays) with the bots that use each. At most 8. A bot picks its
  profile in its settings.
- **Vault** (rail) — the passwords and keys list and form, moved out of the
  chat header. "Passwords and keys your bots can use. They never see the
  values."

## Known limitations

- The desktop client cannot cancel work already accepted by the server when it
  disconnects.
- The composer's attachment button stays disabled: the client has no inbox
  upload path, so there is nothing honest to send yet.
- A bot must be **enabled and loaded** before it can join a team, so a bot
  enabled in configuration needs a server restart before it can be added.
- A group room is a team of bots, not a multi-person chat: there are no guest
  users and no free-form agent-to-agent chatter. The peer exchange is capped at
  two replies per bot, three rounds, and eight messages per room turn.
- Deck artifact download / Italian→French approve flows remain server/Telegram
  oriented (`/v1/artifacts/...`); they are not yet a first-class desktop panel.
- Anthropic's native Messages API is not implemented; use an OpenAI-compatible
  gateway or the built-in providers.
- OAuth consent for Google, Microsoft, and Notion still happens outside the app;
  paste the resulting refresh token in **Connectors**.
- Admin saves require a service restart. If the unit is not managed by systemd
  with `Restart=on-failure`, restart it yourself. Bot display profiles are the
  exception: they apply immediately.
- Per-bot fallback models are not implemented. A bot's 1:1 conversation keeps
  its own pinned model through the Model bar.
- Routines are global and run as the Watcher; per-bot routines have no schema or
  UI yet.
- Per-bot fallback models are not implemented. A bot's 1:1 conversation keeps
  its own pinned model through the Model bar.
- Routines are global and run as the Watcher; per-bot routines have no schema or
  UI yet.
- There is no client route to switch a bot off or delete it: the server
  contract has no such endpoint yet (a bot is switched on by finishing its
  setup). The instructions field is sent as `instructions` on
  `PUT /v1/bots/{id}`; the 0.16 contract does not list it, so a server that
  ignores it will keep the old text.
- Generated decks and media are downloadable from approvals and artifact links,
  but there is no per-bot generation history index yet.
- No live provider or Telegram verification is implied by the synthetic tests.
