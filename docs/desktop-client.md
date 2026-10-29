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
- **API token** — mint a device token on the server host:

  ```bash
  serabot token mint --label desktop-win
  ```

  The plaintext token is printed once; the server stores only its hash. Move it
  over a secure channel (for example type it from an SSH session on the server,
  or copy it through a password manager). Do not send it over chat or email and
  do not commit it. Paste it into the masked field.
- **Connect** — **Connect** or **Enter** (with a valid server URL and token)
  starts the authenticated session read. The card stays up with
  **Connecting…** until the first `/v1/health` reply succeeds.

Only after that first authenticated reply is the origin written to
`%APPDATA%\SeraBot\settings.json` (non-secret; `SERABOT_SETTINGS_DIR` overrides
the directory for development) and the token stored in Windows Credential
Manager (service `SeraBot`, account `owner-api-token`). A rejected token or an
unreachable server persists nothing. Later launches auto-connect from the saved
origin and credential, so the card appears only when no credential exists or a
token was cleared.

## Token lifecycle

- **Disconnect** (Settings → Connection) ends the session and clears the
  token, chat text, history, directory lists, and pending actions from memory.
  The saved credential remains for the next launch.
- **Forget token** deletes the Windows Credential Manager entry and clears the
  masked field, keeping the server URL. Mint a new token to reconnect.
- A **rejected or revoked** token (HTTP 401) is handled like **Forget token**:
  the stored credential is deleted and the card reports "Token rejected or
  revoked. Mint a new token on the server." A token the server no longer
  accepts cannot stay cached.
- Revoke unused tokens on the server with `serabot token list` and
  `serabot token revoke <id>`.
- **Debug builds only:** when the credential store has no token,
  `SERABOT_OWNER_TOKEN` in the process environment seeds the masked field.
  Release builds contain no code that reads it, and
  `SERABOT_OWNER_TOKEN` is never compiled into the binary.

## Behaviour

The connected console is a Dioxus WebView2 shell: a nav rail, a conversation
column in **Chat**, and a content pane. It is responsive down to 920×620 and all
network work stays on the Rust worker/SSE threads, never on the UI thread.

- **Rail** — Chat, Conversations, Memory, Providers, Connectors, Approvals,
  Fleet, and Settings. Icons are inline SVG; the build ships no image assets.
- **Appearance** — a Light/Dark switch sits on the connect card, in the header,
  and in **Settings → Appearance**. The CSS palette switches between a deep
  teal-black dark theme and a warm paper light theme. The choice is saved
  locally in `settings.json` and applies before the first connection. Typography
  uses Segoe UI Variable / Segoe UI with gradient backdrops, elevated cards, and
  soft status chips.
- **Header** — the active conversation title and kind, connection state, a
  **Bots** count, the appearance switch, and refresh.
- **Chat** — the conversation column lists server metadata (title, kind, and
  id) and supports search, open, and new direct threads. Messages render as
  bubbles with avatars, speaker, timestamp, and text. An empty conversation
  shows the specialist roster. Chat history is capped at the server's recent
  window (100 messages).
- **Composer** — **Send** is enabled only when the catalog selection is saved and
  approved, no unknown outcome is pending, and the conversation accepts turns.
  Sections and groups are read-only structure.
- **Conversations** — create sections, groups, direct threads, and rooms, rename
  a thread, and archive it (soft; messages stay on the server). Rooms are
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
  turn keeps its pinned route. Only egress-approved providers are selectable;
  the separate confirmation checkbox was removed. The form adds or updates an
  OpenAI-compatible or built-in provider, stores the key in the age store, and
  can delete providers created here. Anthropic's native Messages API is not a
  transport yet; an OpenAI-compatible gateway in front of Anthropic works.
- **Connectors** — all configured connectors are listed with enabled state,
  redirect URI, and secret names. Toggle a connector, paste its client id,
  client secret, refresh token, or AgentMail key, then save. OAuth authorization
  itself still happens out of band; the app stores the refresh token. Enabling a
  connector only makes its tools available; grant them to a bot in **Fleet**.
- **Settings → Capabilities** — reads the authenticated health payload and
  shows Audio (on/off with the configured transcription and understanding
  models), Firecrawl, Telegram, and whether model catalogs are live or static.
  Audio can be enabled and its two Mistral model names edited here. Audio uses
  the provider credentials already in the age store; the console never asks for
  a second copy of the chat key.
- **Live updates** — the Rust core opens `GET /v1/events` on a dedicated thread
  with its own HTTP client, reconnects with backoff, and resumes from the last
  event id (`after=` / `Last-Event-ID`). Events coalesce into at most one
  refreshed read per request gap; task polling remains as a stuck-stream guard
  and is the only path when the stream is unavailable.
- **Fleet** — opening the screen loads the bot roster and skill packs
  automatically. Every tool is shown; tools that require owner approval are
  highlighted amber (for example Firecrawl search/crawl).
- Approvals load from `GET /v1/approvals`. Once / Always / Deny call
  `POST /v1/approvals/{id}/decide` with the same semantics as Telegram. Decisions
  still bind canonical arguments, destination, artifact hash, policy version,
  and expiry.
- Submitted messages are tracked by task id. If a submit response is lost, the
  client reports an unknown outcome and refuses to resend automatically; review
  server state first.
- Late responses from a previous connection or conversation are discarded.

## Administration

Provider, connector, and audio writes use the owner token and land in
`admin-overrides.json` beside the SQLite database (on the standard host,
`/var/lib/serabot/admin-overrides.json`). That file holds non-secret settings
only; credentials go to the age store and are never returned by the API. The
overlay is merged over `serabot.toml` at startup, so saved changes take effect
after **Restart server and apply** in the status banner.

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

## Fleet

**Fleet** calls the owner API on demand. Each submit uses the same 3-second gap
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
- Conversation nodes are created in **Conversations**; the Fleet header links
  there.

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
  update and deletes the partial download.
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

## Known limitations

- The desktop client cannot cancel work already accepted by the server when it
  disconnects.
- Deck artifact download / Italian→French approve flows remain server/Telegram
  oriented (`/v1/artifacts/...`); they are not yet a first-class desktop panel.
- Anthropic's native Messages API is not implemented; use an OpenAI-compatible
  gateway or the built-in providers.
- OAuth consent for Google, Microsoft, and Notion still happens outside the app;
  paste the resulting refresh token in **Connectors**.
- Admin saves require a service restart. If the unit is not managed by systemd
  with `Restart=on-failure`, restart it yourself.
- No live provider or Telegram verification is implied by the synthetic tests.
