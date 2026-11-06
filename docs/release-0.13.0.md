# SeraBot 0.13.0: native Microsoft sign-in

## Fixed
- Microsoft sign-in failed with HTTP 500 for `tenant = "consumers"`, `common`
  and `organizations`: Microsoft reports the tenant GUID (or a `{tenantid}`
  template) as issuer. The server now maps `consumers` to its GUID and
  validates `iss` against the token's `tid` claim for templated tenants.
- `deploy.sh` preflight exited 141 (SIGPIPE on `ldd --version | head -1`),
  so v0.12.1 never reached the server.

## Changed
- Desktop and mobile open on "Sign in with Microsoft". The token-paste and
  pairing screens are gone. The app receives its session through a one-time,
  PKCE-bound handoff (`docs/specs/native-signin-contract.md`); users never see
  a token.
- The web callback page no longer shows a session token.
- Sign out in both clients. Any 401 returns to the sign-in screen.
- `deploy.sh` keeps one rollback binary, `/opt/serabot/serabot.prev`.
- On start the server sends `SeraBot <version> started` to the admin's
  Telegram chat.

- Desktop: session expiry ends the session cleanly, browser/Explorer
  launches run off the UI thread, one pooled HTTP client, HTTPS-only update
  redirects, stale update files pruned.
- Code layout: `api.rs` and `runtime.rs` are now the `api/` and `runtime/`
  module directories (pure moves, no behaviour change).
- Deploy targets the new VPS through the `OVH_VPS_IP` and
  `OVH_DEPLOY_KNOWN_HOSTS` repository variables; no server address or host
  key in the tree. `deploy.sh` creates its release dirs and keeps 3 staged
  binaries and 5 DB backups.

## New
- `POST /v1/auth/handoff`, `GET /v1/me`, migration `0014_auth_handoffs.sql`.
- `[auth] admin_email`: the first Microsoft sign-in with this email claims the
  existing admin account and its data.

## Operator steps before deploy
1. Set `[auth] admin_email` in `/etc/serabot/serabot.toml`.
2. Reinstall `deploy/deploy.sh` to `/opt/serabot-deploy/` (`deploy/install.sh`).
