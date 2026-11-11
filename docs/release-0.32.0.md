# SeraBot 0.32.0: server stability, security, and mobile parity

This release closes the v0.32.0 batch: server hardening (PR #59) and Android
parity with the desktop polish (PR #60). Checklist: `tasks/big-wave.md`.

## Server stability and security (PR #59)

- **Feed URLs are admin-only.** `GET /v1/monitoring/sources` and `GET /v1/sources`
  return a feed's `url` to admins and `null` to members. Names, kinds, enabled
  flags and last-run status are unchanged for everyone.
- **Feed keys stay out of run errors.** Fetch errors name a URL without its query
  or fragment. Run errors are redacted when returned, which also covers rows
  stored by earlier builds.
- **Session expiry persists.** The desktop stores the server-reported expiry next
  to the saved token. An expired saved session is removed on load and is not
  restored. Sessions without a recorded expiry load as before.
- **Connector sign-in listener budget.** The loopback callback listener gives up
  after 50 ignored requests, the same limit as the sign-in listener.
- **Retries with backoff.** Connection failures, timeouts and connections dropped
  before a response are retried for GET requests (3 attempts). Source pages that
  return 429, 500, 502, 503 or 504 are retried (3 attempts). A run that gives up
  reports how many attempts it made.
- **A8 smoke test extended.** Member 403 checks on admin-only views, and checks
  that secrets and feed keys do not appear in member-readable responses.

## Android parity (PR #60)

- **Quota card** in Settings, with an OK / Near limit / At limit chip (icon and
  word) and a bar.
- **Retry** on Settings, chat and sign-in errors. A missing browser gets its own
  sign-in message.
- **Save failures no longer end the session.** If the phone refuses to store the
  sign-in, the session still works and a "Try saving again" banner appears.
- **Invite code** is masked, with a Show/Hide toggle.
- **Sign-out asks first.**
- **Chat "Jump to latest"** pill with an unseen count and a reply-in-progress marker.
- **Reduce motion.** With the system "Remove animations" setting on, page
  transitions and the chat scroll animation are off.
- **Theme-aware chip colours** for light and dark themes.

Sign-in is not offered as a copy-link fallback on Android. A sign-in must return
to the app through the app's own link, so a link opened in another browser cannot
finish it.

## Versions

- Server and desktop workspace: 0.32.0 (`Cargo.toml`, `Cargo.lock`). CI creates
  the `v0.32.0` tag from the workspace version.
- Mobile app: `pubspec.yaml` stays at `0.5.0+11`. The release flow does not read it.

## Verification

- Rust, local Linux gate: `cargo fmt --all -- --check`, `cargo clippy -p serabot
  --all-targets -- -D warnings`, `cargo test -p serabot`.
- Clients CI: `clients.yml` runs the Flutter analyze, test and APK builds, and the
  desktop checks on Ubuntu and Windows. Windows CI covers the server and desktop
  test runs.

## Not verified on real devices yet

- Windows: keyring behaviour with expiry, and a live Microsoft sign-in.
- Android: the eight-item phone checklist in PR #60, including a secure-storage
  refusal, a sign-in with no browser, and reduce-motion behaviour.
- Retries against a real flaky upstream.
