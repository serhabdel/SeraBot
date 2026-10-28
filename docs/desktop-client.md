# Desktop client (Windows)

`crates/serabot-desktop` is a native Rust client built with `eframe`/`egui`
(OpenGL). It adds no Node runtime, web server, or web admin UI. The server
binary is unchanged and remains the default workspace member, so server release
builds and Linux deployments do not include the desktop GUI.

> The desktop client is provisional and expected to change. Approvals and other
> sensitive actions remain in Telegram; the desktop client is a connection,
> provider/model selector, and chat surface.

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

- Chat history comes only from the server; there are no optimistic or demo
  messages.
- Provider and model are chosen separately. A change is not applied until
  confirmed, and the confirmation states that the provider will receive this
  conversation's history. Unapproved (`data_egress_approved = false`) profiles
  are visible but cannot be selected.
- Submitted messages are tracked by task id. If a submit response is lost, the
  client reports an unknown outcome and refuses to resend automatically; review
  server state first.
- Polling is conservative to stay inside the API rate limit.
- Late responses from a previous connection or conversation are discarded.
- `Ctrl+Enter` sends. `Message` is limited to 8,000 characters.

## Known limitations

- Visual/accessibility smoke testing exposed a layout defect in the current
  build: at some display scalings the bottom composer can be laid out below the
  visible window. Treat the client as a prototype until the UI is redesigned.
- The desktop client cannot cancel work already accepted by the server when it
  disconnects.
- Approval decisions and deck approvals are not exposed in the client.
- No live provider or Telegram verification is implied by the synthetic tests.
