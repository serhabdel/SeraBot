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
features beyond what the current UI already covers (chat, model routing,
approvals, connection).

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
```

Build the server only with:

```bat
scripts\cargo-win.bat build --release -p serabot --locked
```

## Connection

- **Server URL** — HTTPS origin, or exact loopback HTTP for an SSH tunnel
  (`http://127.0.0.1:8787`, `http://[::1]:8787`, `http://localhost:8787`).
  Credentials, paths, queries, fragments, redirects, and lookalike loopback
  hosts are rejected.
- **API token** — the owner token whose SHA-256 hash is in `[api].token_hash`.
  It is held only in memory for the session (masked input); it is not written to
  disk. Disconnecting clears the token, chat text, history, and pending actions.
- **Connect** — **Connect** or **Enter** (with valid server URL, token, and
  conversation id) starts the session read.

## Behaviour

- Sidebar navigation: **Chat** and **Approvals**, plus conversation id, model
  routing, and connection controls.
- Chat history comes only from the server; there are no optimistic or demo
  messages. The composer stays in a reserved bottom region so it is not clipped
  at common window sizes.
- Provider and model are chosen separately. A change is not applied until
  confirmed, and the confirmation states that the provider will receive this
  conversation's history. Unapproved (`data_egress_approved = false`) profiles
  are visible but cannot be selected.
- Approvals load from `GET /v1/approvals`. Once / Always / Deny call
  `POST /v1/approvals/{id}/decide` with the same semantics as Telegram. Decisions
  still bind canonical arguments, destination, artifact hash, policy version,
  and expiry.
- Submitted messages are tracked by task id. If a submit response is lost, the
  client reports an unknown outcome and refuses to resend automatically; review
  server state first.
- Polling is conservative to stay inside the API rate limit.
- Late responses from a previous connection or conversation are discarded.
- `Ctrl+Enter` sends. `Message` is limited to 8,000 characters.

## Known limitations

- The desktop client cannot cancel work already accepted by the server when it
  disconnects.
- Deck artifact download / Italian→French approve flows remain server/Telegram
  oriented (`/v1/artifacts/...`); they are not yet a first-class desktop panel.
- No live provider or Telegram verification is implied by the synthetic tests.
