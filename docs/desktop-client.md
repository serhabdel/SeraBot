# Desktop client (Windows)

**Primary owner desktop:** `crates/serabot-desktop` — native Rust with
`eframe`/`egui` (OpenGL). There is no second desktop shell (no Tauri app in this
repo). Mobile work lives under `apps/` separately; it does not replace this
console.

### Why egui instead of Tauri

The owner API client, endpoint/token validation, approval semantics, and
no-blind-resubmit rules are implemented once in Rust with integration tests.
Shipping a Tauri webview tonight would duplicate that surface, add a Node
frontend toolchain, and split documentation without improving security or
features beyond what the native console already covers (chat, conversations,
memory, providers, connectors, approvals, fleet and updates).

The stack adds no Node runtime, web server, or web admin UI on the server. The
server binary remains the default workspace member, so Linux release builds and
OVH deployments do not include the desktop GUI.

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

The connected console is a native three-part shell: an icon rail, a
conversation column in **Chat**, and a content pane. It is responsive down to
860×560 and never blocks the UI thread on network I/O.

- **Rail** — Chat, Conversations, Memory, Providers, Connectors, Approvals,
  Fleet, and Settings. Icons are painted with the GPU painter; the build ships
  no image assets.
- **Top bar** — the active conversation title and kind, connection state, the
  per-conversation model chip, an update badge, and the command palette
  (`Ctrl+K`).
- **Chat** — the conversation column lists server metadata (title, kind,
  last-message time) and supports search, open, and new direct threads. Messages
  are bubbles by bot with timestamps; clicking a bubble copies its text. The
  message list is virtualized: only visible rows are drawn, and wrapped galleys
  are cached by message id and width, so scrolling and resizing do not re-wrap
  the whole history. `stick_to_bottom` keeps the newest message in view unless
  you scroll away; **Latest** returns to it.
- **Composer** — `Ctrl+Enter` sends; **Send** is enabled only when the catalog
  selection is saved and approved, no unknown outcome is pending, and the
  conversation accepts turns. Sections and groups are read-only structure.
- **Conversations** — create sections, groups, direct threads, and rooms, rename
  a thread, and archive it (soft; messages stay on the server). Rooms are
  conversations with the orchestrator and the specialists it delegates to, not
  multi-person chats. The server assigns ids (`sec:`, `grp:`, `dir:`, `room:`).
- **Memory** — list, search, add, and delete owner-scoped facts. Each write goes
  through `POST /v1/memories` and is audited. The screen states that the
  orchestrator receives the 20 most recently updated memories; specialists never
  list them on their own.
- **Providers** — read-only cards for each configured provider: kind, models,
  routing classes, catalog status, egress approval, and secret **names** with a
  presence dot. Values never appear. Model selection is saved per conversation
  through the existing endpoint and still requires the egress confirmation.
- **Connectors** — enabled/disabled state, redirect URI, API base, default inbox,
  and secret names for Google, Microsoft, Notion, and AgentMail. Missing secrets
  offer a copyable `serabot secrets set NAME` command; OAuth login and secret
  writes stay on the server CLI.
- **Live updates** — the client opens `GET /v1/events` on a dedicated thread
  with its own HTTP client, reconnects with backoff, and resumes from the last
  event id (`after=` / `Last-Event-ID`). Events coalesce into at most one
  refreshed read per request gap; task polling remains as a stuck-stream guard
  and is the only path when the stream is unavailable. All network work stays
  off the UI thread.
- Approvals load from `GET /v1/approvals`. Once / Always / Deny call
  `POST /v1/approvals/{id}/decide` with the same semantics as Telegram. Decisions
  still bind canonical arguments, destination, artifact hash, policy version,
  and expiry.
- Submitted messages are tracked by task id. If a submit response is lost, the
  client reports an unknown outcome and refuses to resend automatically; review
  server state first.
- Late responses from a previous connection or conversation are discarded.

### Shortcuts

| Keys | Action |
| --- | --- |
| `Ctrl+K` | Command palette |
| `Ctrl+1…7` | Jump to a rail section |
| `Ctrl+R` | Refresh reads |
| `Ctrl+Enter` | Send message |
| `Esc` | Close the palette or model window |

## Fleet

**Fleet** calls the owner API on demand. Each submit uses the same 3-second gap
as other requests and is not retried if the outcome is unknown.

- **New specialist** — `POST /v1/bots` with a lowercase bot id, a description
  (stored in the prompt stub; there is no separate display-name field), tools
  chosen from the specialist allowlist, and optional skill ids. `delegate` is
  not offered. The server writes a disabled specialist. Restart before it can
  run, and add it to `main.delegates` yourself if `main` should hand it work.
  **Refresh bots** calls `GET /v1/bots` for on-disk summaries (`id`, `role`,
  `enabled`, `live`, tools, skills). Prompts and secrets are not returned.
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
- No live provider or Telegram verification is implied by the synthetic tests.
