# SeraBot 0.74.28

"Sign in with browser" for the Claude Agent SDK gateway.

## Highlights

- **Gateway browser sign-in** (`services/claude_gateway/server.py`): new
  Bearer-protected endpoints `POST /v1/auth/start`, `POST /v1/auth/complete`,
  `GET /v1/auth/status`. The gateway runs the **first-party** claude.ai
  OAuth PKCE flow (the `claude auth login` client id) — not the third-party
  claude.ai OAuth Anthropic shut down in 2026. The PKCE verifier stays in
  the gateway process; the desktop loopback `redirect_uri` is validated
  (http loopback, port ≥ 1024, no credentials/query/fragment); the token
  exchange is pinned to `https://api.anthropic.com/api/oauth/token` with
  redirects refused and a 64 KiB cap; tokens are written where the CLI/SDK
  reads its login (`~/.claude/.credentials.json`, mode 0600, overridable
  via `SERABOT_CLAUDE_GATEWAY_CREDS_PATH`). Pending flows are keyed by
  `state` with a 600 s TTL and are single-use.
- **Server relays the flow** (`auth_flows/gateway.rs`, `auth_flows/mod.rs`,
  `api/provider_accounts.rs`, `api/mod.rs`): kind `claude-agent-sdk` is
  registered with a `pkce` flow; `signin/start` and `signin/complete` call
  `start_with_gateway`/`complete_with_gateway`, which relay to
  `{gateway}/v1/auth/*` over loopback (validated by
  `validate_loopback_gateway_url`) with the provider's stored gateway
  bearer token. SeraBot never sees a claude.ai URL, code or verifier. The
  stored account's credential is the gateway bearer token; identity comes
  from the gateway's non-secret account summary.
- **Desktop**: the `claude-agent-sdk` preset documents `sign_in = "pkce"`,
  so the provider card offers **Sign in with browser** next to the API-key
  field; the directory tile says "Sign in with browser".
- Docs: `docs/specs/claude-agent-sdk-gateway.md` now describes the browser
  sign-in step by step.

## Verification

- Gateway: `python3 -m unittest discover -s services/claude_gateway/tests` — 8 passed
  (auth required, redirect validation, PKCE URL, code swap with a mocked
  token endpoint, credentials file 0600 without the verifier, single-use
  state, status).
- `cargo fmt --all -- --check`, `cargo clippy -p serabot --locked --all-targets -- -D warnings`,
  `cargo clippy -p serabot-desktop --features ui-check --locked --all-targets -- -D warnings`,
  desktop `cargo test --lib` + `--test http` — all green locally.
- New `auth_flows::gateway` unit tests run in CI's `cargo test --workspace --locked`
  (the lib test binary does not link in a 6 GB sandbox).
