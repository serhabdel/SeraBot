# Release 0.74.35 — Claude Agent (Pro / Max / Ultra): Reliable Server Lifecycle, Live Verification & Browser-First UX

## Fixed & Improved

- **Always-on, self-healing gateway lifecycle (`main.rs`, `claude_gateway_proc.rs`, `llm/wire_calls.rs`)**:
  - `serabot run` now shares one `ClaudeGatewaySupervisor` across the server runtime and API, auto-bootstraps the loopback gateway token, and starts any configured `claude-agent-sdk` gateway at boot before model catalog refresh.
  - Chat turns (`complete_claude_gateway`) automatically revive the loopback gateway if it stopped or crashed mid-run.
  - `ClaudeGatewaySupervisor::ensure_running` now probes authenticated `GET /v1/auth/status` when `/health` is up and automatically restarts a stale supervised child if the bearer token was rotated (while refusing to touch unmanaged external processes).
- **Bundled & user-local `claude` CLI discovery (`services/claude_gateway/server.py`)**:
  - `_find_claude_cli()` now checks `~/.local/bin/claude`, `~/.claude/local/claude`, `~/.npm-global/bin/claude`, `/usr/local/bin/claude`, and the native `claude` binary bundled inside `claude_agent_sdk/_bundled/` — fixing `503 claude_cli_missing` on `systemd` servers with minimal `PATH`.
  - Passes resolved `cli_path` into `ClaudeAgentOptions` and orders `claude-sonnet-4-6` / `claude-opus-4-6` first in `SUPPORTED_MODELS` (`DEFAULT_MODEL = "claude-sonnet-4-6"`).
- **Credential routing & live account verification (`api/admin.rs`, `api/provider_accounts.rs`, `auth_flows/gateway.rs`)**:
  - `POST /v1/admin/providers` ("Add a key" on the Provider Directory tile) no longer overwrites `claude_gateway_token` when a user pastes an `sk-ant-oat01-…` setup-token or `sk-ant-api…` key; instead it preserves the gateway bearer token and registers the Claude credential as a gateway account.
  - Added `POST /v1/auth/verify` on the gateway and wired `POST /v1/providers/{id}/accounts/{aid}/test` to verify the stored Claude subscription / API key live against Anthropic instead of returning a fake OK from static `/v1/models`.
- **Desktop UX & Branding (`serabot-desktop`)**:
  - Renamed provider display label to **Claude Agent (Pro / Max / Ultra)** (keeping `id = "claude-agent-sdk"` unchanged for full backwards compatibility).
  - `AddAccountSheet` for `claude-agent-sdk` now puts **Sign in with browser** first, with a dedicated **Setup token** (`claude setup-token`, 1-year) tab and a separate **API key** (`sk-ant-api…`) tab, supporting multiple accounts.
  - Un-hid the remote `code#state` / callback URL paste panel in `FlowPanel` so remote server sign-ins never bury the manual completion box, and streamlined `GatewayStatusStrip` to show a clean **Claude Agent Ready** chip when healthy.

## Verification note
- Verified with synthetic unit tests across Python (`pytest services/claude_gateway/tests`) and Rust (`cargo test --workspace --locked`); live Anthropic/Claude plan accounts were not exercised in CI.
