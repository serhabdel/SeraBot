# SeraBot Claude Agent SDK Gateway (Opt-in)

Loopback-only HTTP bridge wrapping Python `claude-agent-sdk` for SeraBot.

- **Loopback-only**: Refuses any non-loopback bind host (`127.0.0.1` / `::1` only).
- **Token-authenticated**: Requires `Authorization: Bearer <SERABOT_CLAUDE_GATEWAY_TOKEN>` on `/v1/*` (fail-closed `503`/`401` if unset or mismatched).
- **Tool execution**: Disables built-in Claude Code tools (`tools=[]`, `setting_sources=[]`) and captures `mcp__serabot__<name>` tool calls so SeraBot's Rust `gateway` executes them with SeraBot's own permissions, approvals, and audit log.

## Setup

1. Install the package in a Python 3.12+ environment:

```bash
pip install -e services/claude_gateway
```

2. Authenticate Claude Agent SDK (either API key or Claude CLI login):

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
# or: claude auth login
```

3. Set a strong shared bearer token:

```bash
export SERABOT_CLAUDE_GATEWAY_TOKEN="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')"
```

4. Start the loopback server:

```bash
uvicorn server:app --host 127.0.0.1 --port 8791
```

(Or from `services/claude_gateway/`: `serabot-claude-gateway --host 127.0.0.1 --port 8791`.)

## Endpoints

- `GET /health` — `{"status": "ok", "sdk": "claude-agent-sdk", "auth_configured": <bool>}`
- `GET /v1/models` — Lists supported Claude model IDs (Bearer token required).
- `POST /v1/turn` — Runs a single model turn with `{model, system, messages, tools, max_tokens}` and returns `{content, tool_calls, usage, finish_reason}` (Bearer token required).
