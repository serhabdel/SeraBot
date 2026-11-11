# Desktop production gaps

Seen while moving the console to native Microsoft sign-in (0.13.0, branch
`feat/signin-desktop`) and not fixed there. The list is bounded: it covers what
the sign-in pass touched or read, not a full audit.

## Sign-in and session

- The sign-in path is tested against a scripted server that follows
  `specs/native-signin-contract.md`. It has not run against the real Microsoft
  tenant or the deployed server; treat it as unverified until it does.
- Fixed: `expires_at` from the handoff response now ends the session cleanly
  ("Your session ended. Sign in again.") when it passes. Still open: the expiry
  is held in memory only, so a token restored after a restart has no known
  expiry and relies on the 401 path; there is no "expires soon" hint or silent
  re-sign-in.
- A saved token from an older release that is an operator device token may not
  be accepted by `GET /v1/me`. The console tolerates a failed `/v1/me` and lets
  the next read (the model catalog) decide, but this was not checked against a
  real device token.
- Fixed (PR `feat/hardening-a8`): the loopback listener refuses any request
  whose `Host` is not `127.0.0.1` or `localhost` on its own port (DNS rebinding
  and forged requests), and the sign-in wait gives up after 50 ignored
  requests. Each sign-in already gets a fresh random `state`, and a wrong-state
  request is still ignored. Not changed: the listener still answers any local
  process that knows the port, so a local process can keep ignoring requests
  until the budget ends the wait.
- Fixed (PR `feat/hardening-a8`): the sign-in wait is configurable with
  `sign_in_timeout_secs` in the settings file, clamped to 60–900 seconds, default
  300. There is no settings control in the UI yet; edit the file and restart.
  If the browser does not open, the sign-in URL is shown with a read-only field
  and a "Copy link" button, and the wait continues.
- Fixed (PR `feat/hardening-a8`): if Windows Credential Manager refuses the
  write, the session still works for this run. A warning says so and tells the
  user to sign in again after a restart, with a "Try saving again" button.
- Open: `GET /v1/monitoring/sources` returns the configured source names and
  URLs to every member. Only the run status (last run, error) is per user.
  Whether the URL list should be admin-only is a decision for the Security
  Reviewer; it is not changed here.

## Panics, threads and I/O

- Non-test `unwrap()` and `expect()` were removed (three sites).
- Fixed: slicing in `session.rs`, `api.rs` and the markdown, mention and slash
  helpers in `ui.rs` now uses `.get()`/`strip_prefix`. The audit found no
  reachable panic; the old sites were guarded by ASCII or `find` results. Plain
  `lines[i]` and byte-scan indexing in the markdown block parser and
  `teams.rs` stay: each is bounded by an explicit length check.
- Fixed: `webbrowser::open`, `explorer` and `xdg-open` launches, including the
  connector "Connect" button, now run on a helper thread and report by toast.
- Fixed: `save_ui_settings` shows a toast when the write fails (the change
  still applies for this run).
- Fixed: the HTTP worker keeps one `ApiClient` per endpoint and token, so the
  connection pool and TLS sessions are reused. (Sign-out and sign-in still use
  their own short-lived clients.)
- Updater reviewed. No non-test `unwrap`/`expect`; downloads are size-capped,
  streamed to `.part`, hashed while writing and renamed only on a match, and
  the hash is re-checked before install. Fixed: redirects are followed only
  over HTTPS, and stale executables and `.part` files are pruned. Left open:
  the checksum comes from the same GitHub release as the binary, so there is no
  independent signature (Authenticode or a signed manifest) and the helper
  script cannot be tested without a real install; the update path has not run
  against a real release.

## Tests

- `ui.rs` is about 7,000 lines. It is type-checked (`--features ui-check`) but
  the connect screen, settings card and sign-out flow have no automated UI test.
  Session-level behaviour (reject, sign-out, offline sign-out) is unit tested.
  The copy-link field and the credential warning with its retry button are
  covered by the `ui-check` type-check only; no test drives them.
- Automated A8 smoke (`crates/serabot/src/api/tests/a8_smoke_api.rs`) drives
  steps 1–7 of `tasks/big-wave.md` against a seeded admin and member over the
  real router. It does not drive WebView2 or the owner's Microsoft sign-in.
- Unit tests that touch the token store now use the in-memory store on Windows
  as well, so they cannot read or delete a developer's real saved credential.
  There is no test that exercises the real Windows Credential Manager.

## Docs outside this crate

- `docs/ci-and-secrets.md`, `docs/mobile-client.md`, `docs/owner-wake.md` and
  `.github/workflows/*.yml` still describe `SERABOT_OWNER_TOKEN` as a name
  clients show for pasting a token. They were left alone because they belong to
  the CI and mobile work.
