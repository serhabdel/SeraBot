# Claude Agent SDK gateway — auth model, multi-account, native Rust integration

This page answers the three recurring questions about the
`claude-agent-sdk` provider kind. The product boundary in `AGENTS.md` is
unchanged: the opt-in Python loopback gateway (`services/claude_gateway/`)
is the one approved bridge, and Desktop ACP is the other. Do not add a new
local runtime or a third-party OAuth client.

## 1. There is no claude.ai OAuth to support in SeraBot itself

Anthropic shut down third-party `claude.ai` OAuth in 2026; SeraBot retired
it in 0.74.25 (`CLAUDE_OAUTH_SHUTDOWN_MESSAGE`, hard `plan_signin = "off"`
for `anthropic`, and the desktop shows a terminal "retired" notice since
0.74.26). The Claude Agent SDK does not use third-party OAuth either: it
authenticates with an **API key** (`ANTHROPIC_API_KEY`) or the **Claude CLI
login** (`claude auth login`, a setup token stored by the CLI) — and that
authentication happens *inside the gateway process*, on the member's own
machine, never inside SeraBot. SeraBot only ever holds the gateway's
shared bearer token (`SERABOT_CLAUDE_GATEWAY_TOKEN`), which is not a
Claude credential.

So the supported Claude paths are:

- **Anthropic (API key)** — `anthropic` provider, Console key.
- **Claude Agent SDK (Gateway)** — `claude-agent-sdk` provider → loopback
  gateway → the member's own CLI login or API key.
- **Claude Code (Desktop ACP)** — Windows desktop only, pinned
  `@agentclientprotocol/claude-agent-acp`, per `docs/specs/claude-code-acp.md`.

### Paste a `claude setup-token` token (0.74.34) — Pro/Max plan billing

`claude setup-token`, run on any computer signed in to the Claude plan,
prints a long-lived `sk-ant-oat01-…` token. Pasted in the gateway card's
**Claude token or API key** tab, it is relayed to the gateway
(`POST {gateway}/v1/auth/token {token, profile}`), which stores it in that
account's profile (`serabot-claude-token.json`, mode 0600) and never returns
or logs it. SeraBot does not store the Claude token: the account's credential
stays the gateway bearer token, identity `{profile, billing}`. The profile is
`tok_` + the first 12 hex digits of the token's SHA-256, so pasting the same
token again lands on the same account.

Which credential a turn uses (`_apply_claude_credential`):

- stored `sk-ant-oat…` → `CLAUDE_CODE_OAUTH_TOKEN`, and `ANTHROPIC_API_KEY` /
  `ANTHROPIC_AUTH_TOKEN` removed — usage counts against the plan's limits;
- stored `sk-ant-api…` → `ANTHROPIC_API_KEY` (pay-as-you-go);
- otherwise a plan login (`claude auth login` credentials in the profile, or a
  host `CLAUDE_CODE_OAUTH_TOKEN` on the default account) removes any
  `ANTHROPIC_API_KEY`, because the SDK would prefer it and silently bill the
  API instead of the plan;
- a named account never inherits the host's `CLAUDE_CODE_OAUTH_TOKEN`.

Before 0.74.34 that tab saved whatever was pasted as the *gateway bearer
token*, so a pasted Claude token only produced 401s from the gateway.

### "Sign in with browser" on the gateway card (0.74.28, reworked 0.74.33)

The provider card offers **Sign in with browser**. It is *not* SeraBot doing
claude.ai OAuth: the gateway runs the member's own `claude auth login
--claudeai` (first-party CLI, its own client id), in a per-account
`CLAUDE_CONFIG_DIR` profile, and SeraBot only relays.

`claude auth login` offers two ways to finish, and the gateway uses both:

- **Automatic URL** — handed to `$BROWSER`; its redirect is the CLI's own
  listener, `http://localhost:<port>/callback`. The gateway sets `$BROWSER` to
  a one-shot capture script (POSIX) to learn that URL, its port and its
  `state`. (Before 0.74.33 `$BROWSER` was `true`, so this URL was thrown away
  and every sign-in ended on a page with a code nobody was told to paste.)
- **Manual URL** — printed to the terminal; its last page shows a
  `code#state` that the CLI's `Paste code here if prompted >` prompt accepts.

Flow:

1. Desktop → `POST /v1/providers/{id}/signin/start` (flow `pkce`) → server →
   gateway `POST /v1/auth/start`. The reply carries `url` (the automatic URL
   when captured, else the manual one), `manual_url`, and — when captured —
   `callback {port, path}` and `state`. The server passes them to the desktop
   as `callback_port` / `callback_path` / `state` / `manual_url` (a callback
   on a port below 1024 or with an odd path is dropped).
2. The desktop opens `url` and tries to listen on the callback port on its
   own loopback:
   - **Gateway on this computer**: the CLI already holds the port, the
     browser reaches it directly, and the CLI exits 0 by itself. The
     desktop's status poll (`GET .../signin/status/{flow}` → gateway
     `GET /v1/auth/flow/{id}`) sees `ok`.
   - **Gateway on the server**: the port is free on the desktop, so the
     desktop catches the redirect (state checked) and posts
     `POST .../signin/complete {flow_id, code, state}`. The gateway checks
     `state` in constant time and hands the redirect to the CLI's own
     listener on its loopback (`127.0.0.1`, then `::1`; only the port and
     path the CLI announced; never through a proxy). The CLI swaps the code
     with its own PKCE verifier.
3. Fallback, folded in the sheet ("Browser shows a code, or cannot reach
   localhost?"): **Open code page** opens `manual_url`; the member pastes the
   `code#state` it shows, or the whole `http://localhost:…/callback?…`
   address if the browser stopped there (the gateway relays that one like a
   caught redirect). A paste without the `#` makes the CLI ask again; the
   gateway answers `code_incomplete` and the flow **stays open**.
4. On success the gateway reads the stored login (`claude auth status
   --json`, or the profile's `.credentials.json`) and returns only the
   non-secret account summary (email, plan). SeraBot stores the provider
   account: credential = the gateway bearer token, identity = that summary
   plus the gateway profile, `AuthKind::Oauth`. Turns send the profile as
   `X-Claude-Profile`.

A finished gateway flow keeps answering `GET /v1/auth/flow/{id}` with its
result until it expires, so a poll that races a complete still sees how it
ended; a flow the gateway forgot (restart) reads as `expired`. A refused code
reports the CLI's own reason (the pasted code is masked). Codes are capped at
4096 characters and control characters are refused before anything reaches
the CLI's terminal.

Windows gateways cannot capture `$BROWSER` (no POSIX helper), so they fall
back to the manual URL and a pasted code.

`GET {gateway}/v1/auth/status` reports whether a login is stored. All
`/v1/auth/*` endpoints require the gateway bearer token like every other
`/v1/*` route.

## 2. Multiple accounts = one gateway per Claude login, one provider each

The gateway URL is per **provider**, validated as a strict loopback HTTP
URL (`validate_loopback_gateway_url`: `http://127.0.0.1[:port]`,
`http://[::1][:port]`, `http://localhost[:port]`, no credentials/query/
fragment). So to use several Claude logins at once:

1. Start one gateway per login, each on its own loopback port, each with
   its own `SERABOT_CLAUDE_GATEWAY_TOKEN`:

   ```bash
   # login A
   claude auth login            # or: export ANTHROPIC_API_KEY=...
   SERABOT_CLAUDE_GATEWAY_TOKEN="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')" \
     uvicorn server:app --host 127.0.0.1 --port 8791   # from services/claude_gateway
   # login B, separate shell/profile with its own CLI credentials
   SERABOT_CLAUDE_GATEWAY_TOKEN="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')" \
     uvicorn server:app --host 127.0.0.1 --port 8792
   ```

2. Register each as its own provider in `serabot.toml` (kind
   `claude-agent-sdk`, distinct id, that gateway's loopback URL, that
   token's secret name):

   ```toml
   [llm.providers.claude-agent-sdk]
   kind = "claude-agent-sdk"
   base_url = "http://127.0.0.1:8791"
   api_key_secret = "claude_gateway_token"
   catalog_fetch = true
   data_egress_approved = true

   [llm.providers.claude-agent-sdk-2]
   kind = "claude-agent-sdk"
   base_url = "http://127.0.0.1:8792"
   api_key_secret = "claude_gateway_token_2"
   catalog_fetch = true
   data_egress_approved = true
   ```

   The desktop console can do the same from Add provider (the preset's
   base URL field is editable; keep it loopback).

3. Bots/chats pin `provider = "claude-agent-sdk-2"` etc.; the provider
   account system gives per-account status, priority/weight, failover and
   health as for any other provider. `check_profile` accepts any number of
   `claude-agent-sdk` providers on distinct loopback ports (locked in by
   `multiple_claude_agent_sdk_gateways_each_on_their_own_loopback_port` in
   `config.rs`).

## 3. "Native in my Rust app" = the loopback HTTP contract, already wired

SeraBot's Rust server already speaks to the gateway natively — no Python
is called from Rust at turn time. `LlmClient::with_claude_gateway()`
(`crates/serabot/src/llm.rs`) pins the endpoint with
`validate_loopback_gateway_url` and selects `Wire::ClaudeGateway`, which
speaks plain HTTP/JSON over loopback with the bearer token:

- `GET {base}/v1/models` — model catalog (`resolve_catalog` for the kind).
- `POST {base}/v1/turn` — one turn: `{model, system, messages, tools, max_tokens}`
  → `{content, tool_calls, usage, finish_reason}`.
- `GET {base}/health` — used by probes.

Tool calls the SDK makes (`mcp__serabot__<name>`) are captured by the
gateway (`tools=[]`, `setting_sources=[]`) and executed by SeraBot's own
Rust `gateway` with SeraBot's approvals, policy scan and audit log — the
Rust app stays the only place tools run.

Writing a *Rust-native reimplementation* of the Agent SDK (a second local
runtime speaking ACP to the CLI) is out of scope by design: `AGENTS.md`
forbids new local runtimes without a new security design. If you want
Rust-native Claude access without the bridge, the supported path is the
Anthropic Messages API with a Console API key (`anthropic` provider,
`wire/anthropic.rs`).
