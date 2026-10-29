# Desktop client (Windows)

`crates/serabot-desktop` is a native Rust owner console built with `eframe`/`egui`
(OpenGL). It adds no Node runtime, web server, or web admin UI. The server
binary is unchanged and remains the default workspace member, so server release
builds and Linux deployments do not include the desktop GUI.

## Build and launch (Windows, MSVC)

```bat
scripts\cargo-win.bat test -p serabot-desktop --locked
scripts\cargo-win.bat run -p serabot-desktop --locked
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
