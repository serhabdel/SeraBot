# Desktop production gaps

Seen while moving the console to native Microsoft sign-in (0.13.0, branch
`feat/signin-desktop`) and not fixed there. The list is bounded: it covers what
the sign-in pass touched or read, not a full audit.

## Sign-in and session

- The sign-in path is tested against a scripted server that follows
  `specs/native-signin-contract.md`. It has not run against the real Microsoft
  tenant or the deployed server; treat it as unverified until it does.
- `expires_at` from the handoff response is validated but not used. The console
  finds an expired session only when a request answers 401. A "session expires
  soon" hint or a silent re-sign-in is not built.
- A saved token from an older release that is an operator device token may not
  be accepted by `GET /v1/me`. The console tolerates a failed `/v1/me` and lets
  the next read (the model catalog) decide, but this was not checked against a
  real device token.
- The loopback listener answers any local process that knows the port. The
  `state` check blocks a forged redirect from being used, and a wrong-state
  request is ignored, but there is no `Host` header check and no rate limit.
- The sign-in wait is fixed at 5 minutes and has no "copy the link" fallback
  for a machine whose default browser cannot be opened.
- If Windows Credential Manager refuses the write, the session lives for this
  run only. The user sees "Connected, but the credential could not be saved."
  but gets no retry button.

## Panics, threads and I/O

- Non-test `unwrap()` and `expect()` were removed (three sites). Indexing and
  slicing (`[..]`, `[i]`) was not audited.
- `webbrowser::open`, `explorer` and `xdg-open` spawns, and the connector
  "Connect" button's browser open, run on the UI thread. They return quickly but
  are not off-thread.
- `save_ui_settings` ignores write errors (pins and theme silently fail to
  persist).
- `ApiClient::new` builds a fresh `reqwest` client on every worker job, so TLS
  setup repeats per request.
- The updater (`updater.rs`) was not reviewed in this pass.

## Tests

- `ui.rs` is about 7,000 lines. It is type-checked (`--features ui-check`) but
  the connect screen, settings card and sign-out flow have no automated UI test.
  Session-level behaviour (reject, sign-out, offline sign-out) is unit tested.
- Unit tests that touch the token store now use the in-memory store on Windows
  as well, so they cannot read or delete a developer's real saved credential.
  There is no test that exercises the real Windows Credential Manager.

## Docs outside this crate

- `docs/ci-and-secrets.md`, `docs/mobile-client.md`, `docs/owner-wake.md` and
  `.github/workflows/*.yml` still describe `SERABOT_OWNER_TOKEN` as a name
  clients show for pasting a token. They were left alone because they belong to
  the CI and mobile work.
