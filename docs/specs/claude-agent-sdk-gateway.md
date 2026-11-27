# Claude Agent SDK gateway — auth model, multi-account, native Rust integration

This page answers the three recurring questions about the
`claude-agent-sdk` provider kind. The product boundary in `AGENTS.md` is
unchanged: the opt-in Python loopback gateway (`services/claude_gateway/`)
is the one approved bridge, and Desktop ACP is the other. Do not add a new
local runtime or a third-party OAuth client.

## 1. There is no claude.ai OAuth to support

Anthropic shut down third-party `claude.ai` OAuth in 2026; SeraBot retired
it in 0.74.25 (`CLAUDE_OAUTH_SHUTDOWN_MESSAGE`, hard `plan_signin = "off"`
for `anthropic`, and the desktop shows a terminal "retired" notice since
0.74.26). The Claude Agent SDK does not use OAuth either: it authenticates
with an **API key** (`ANTHROPIC_API_KEY`) or the **Claude CLI login**
(`claude auth login`, a setup token stored by the CLI) — and that
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
