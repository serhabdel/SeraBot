# SeraBot 0.31.0: Big Wave C — deep ops and mobile Workflows

Spec: `docs/specs/big-wave.md`. Checklist: `tasks/big-wave.md`.

## Wave C (merged on `main`)

- **C1** — Desktop browser console: profile picker, last host, idle TTL, sign-in
  helper status ([PR #38](https://github.com/serhabdel/SeraBot/pull/38)).
- **C2** — Vault accounts view: local + external refs + card-backed entries;
  domain filter; last-used from audit ([PR #45](https://github.com/serhabdel/SeraBot/pull/45)).
- **C3** — Autonomy presets: watcher/analyst prompts reference Workflows,
  `watch_now`, and `plan_update` ([PR #44](https://github.com/serhabdel/SeraBot/pull/44)).
- **C4** — Admin reliability dashboard on Usage (scheduler lag, claim failures,
  source staleness — counts only) ([PR #46](https://github.com/serhabdel/SeraBot/pull/46)).
- **C5** — Flutter read-only Workflows + traces parity with desktop
  ([PR #48](https://github.com/serhabdel/SeraBot/pull/48)).

## Verification

- Rust: `scripts\cargo-win.bat check --workspace --all-targets` and
  `scripts\cargo-win.bat test --workspace --locked` (or WSL fmt/clippy/test).
- Clients CI (`.github/workflows/clients.yml`): desktop check/test on Ubuntu and
  Windows; mobile `flutter analyze`, `flutter test`, debug APK, and release-mode
  compile check on every PR and `main` push — see `docs/ci-and-secrets.md` and
  `docs/mobile-client.md`.

## Still open (not in 0.31.0)

- **A8** — Owner desktop smoke checklist in `tasks/big-wave.md`.
- **E1** — Quota near-limit surfacing in console.
- **E2** — Mem0 provider console toggle when enabled in config.
