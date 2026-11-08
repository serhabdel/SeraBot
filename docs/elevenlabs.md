# ElevenLabs

SeraBot has two independent ElevenLabs connectors. Each has its own switch, status, connect and disconnect in the console.

| Connector | What it is | Credential | Tools | Recommended |
|-----------|------------|------------|-------|-------------|
| **ElevenLabs** | Account sign-in through ElevenLabs' hosted MCP server | Each account's own OAuth sign-in, sealed in that account's credential store | `elevenlabs_mcp_search_tools`, `elevenlabs_mcp_describe_tool`, `elevenlabs_mcp_call` | Yes |
| **ElevenLabs API key** | Legacy native REST tools | The owner's API key, in the server secret store (`elevenlabs_api_key`) | `elevenlabs_key_*` | Only when the sign-in does not fit |

The two never share a credential and never run on the same request.

## ElevenLabs: account sign-in (hosted MCP)

The hosted server at `https://api.elevenlabs.io/v1/mcp` exposes the whole ElevenLabs catalog the account's plan allows: voices, speech, music, sound, image and video generation, agents, knowledge bases, phone numbers, flows, and more. It is 120+ tools, so the model does not get them all at once. It works through three tools:

| Tool | Arguments | Approval | What it does |
|------|-----------|----------|--------------|
| `elevenlabs_mcp_search_tools` | optional `query`, `category`, `page` (from 1) | no | Compact entries (`name`, `title`, `summary`, `read_only`), 40 per page, plus the categories with counts and `next_page`. Categories come from the name's first word (`agents`, `creative`, `voices`, …). |
| `elevenlabs_mcp_describe_tool` | `name` (exact) | no | The full description and input schema, bounded to about 16 KiB. Deeply nested schema parts are left out with a note. |
| `elevenlabs_mcp_call` | `tool` (exact name), `arguments` object | yes unless the tool is a read | Runs one tool. The name must be in the account's catalog; anything else is refused with the five closest names. |

Rules the tools enforce, and the guidance the model is given:

- Search the catalog before calling. Only call names returned by search or describe. If a tool is missing, say so; never invent tool names or model ids.
- The catalog is cached per account for ten minutes. A 401 or a failed fetch drops the cache.
- Reads (`list_`, `get_`, `search_`, `read_`, `view_`, `find_`, `fetch_`, `check_`, `describe_`, `estimate_`, `retrieve_`, `show_` names with no write word) run directly. Everything else asks the owner first: `create`, `generate`, `design`, `clone`, `transcribe`, `text_to_speech`, `call`, `delete`, and similar.
- Timeouts: list, search, describe and read requests use `mcp_timeout_secs` (60). Calls that generate, create or spend use `mcp_generate_timeout_secs` (300).
- Media in a result (inline or signed link, allowlisted types and hosts, at most 4 files) is saved as artifacts. `data.artifacts` lists every saved item with `mime`, `artifact_path`, `artifact_hash`, `filename` and `bytes`.
- Everything the server returns is untrusted data.

### Sign in

1. Open the desktop console → Connectors → **ElevenLabs** and press **Connect**. The console binds `127.0.0.1:51122`, opens the ElevenLabs consent page in your browser, and sends the code to the server. The server exchanges it at the pinned token endpoint and seals the tokens to your account. Tokens are never returned to the console.
2. The sign-in takes effect at once. No restart is needed for it.
3. **Disconnect** removes this account's sign-in. Tool calls fail with "not connected" until you sign in again.

API equivalents (owner only): `POST /v1/admin/connectors/elevenlabs/oauth/start` returns the consent URL; `POST /v1/admin/connectors/elevenlabs/oauth/complete` takes `{code, verifier, redirect_uri}`; `DELETE /v1/admin/connectors/elevenlabs/oauth` disconnects.

The authorization request is PKCE S256 with a CIMD client (`[elevenlabs].oauth_client_id`, the https URL of the metadata document this server publishes at `/.well-known/serabot-oauth-client.json`). The redirect is the fixed loopback `http://127.0.0.1:51122/oauth-callback`. The authorize and token endpoints are pinned in code. Scope: `[elevenlabs].oauth_scope`.

### Storage and per-account rules

- The sign-in is one sealed credential per account, under the provider id `elevenlabs_mcp` in the per-user store (`user_secrets`). Each account's tool calls use its own sign-in only.
- An access token is renewed from the refresh token on use. A rotated refresh token is written back to the same account's row; if that write fails, the call fails so the rotation is not lost.
- Before 0.24 the sign-in was server-wide (`elevenlabs_access_token`, `elevenlabs_refresh_token` in the age store). On the first start of 0.24 those tokens move to the **owner's** account credential and the server-wide names are removed. Nothing is kept server-wide.
- Members cannot sign in to this connector yet. The consent flow runs only in the owner's console, because the client metadata declares only the loopback redirect. See "Limits" below.

### Enable

```toml
[elevenlabs]
mcp_enabled = true
```

Enabling or disabling the connector in the console takes effect after a restart of `serabot.service`, because the tools are registered at startup. The sign-in itself does not.

## ElevenLabs API key (legacy native tools)

For an owner who prefers a key over the account sign-in. These are the native REST tools:

| Tool | Effect | Notes |
|------|--------|-------|
| `elevenlabs_key_list_voices` | read-only | `GET /v2/voices` |
| `elevenlabs_key_text_to_speech` | external, approval | `POST /v1/text-to-speech/{voice_id}` → artifact |
| `elevenlabs_key_sound_effect` | external, approval | `POST /v1/sound-generation` → artifact |
| `elevenlabs_key_transcribe` | external, approval | `POST /v1/speech-to-text` from an artifact hash or inbox path |
| `elevenlabs_key_generate_image` | external, approval | `POST/GET /v1/flows/image` with polling → artifact |
| `elevenlabs_key_generate_video` | external, approval | `POST/GET /v1/flows/video` with polling → artifact |

### Enable

1. Store the key without printing it: `serabot secrets set elevenlabs_api_key`. Keys are `sk_` followed by 48 hex characters. A truncated key is rejected at save time.
2. Set the switch in `serabot.toml` (or in the console, then restart):
   ```toml
   [elevenlabs.api_key]
   enabled = true
   ```
3. Allowlist the tools on the bots that should use them, for example `tools = ["elevenlabs_key_list_voices", "elevenlabs_key_text_to_speech"]` with `require_approval` for the generation tools.

The native tools are registered only when this connector is enabled **and** a key is stored. With the switch on and no key, the server logs a warning and registers nothing. The API key is server-wide, owned by the owner. It is not a per-account credential.

Prompts and text are checked against the blocklist before they leave the host. Tool output is scanned again by the gateway.

## Configuration

```toml
[elevenlabs]
mcp_enabled = false                 # the account sign-in connector
mcp_timeout_secs = 60               # list, search, describe, read
mcp_generate_timeout_secs = 300     # generate, create, spend
mcp_url = "https://api.elevenlabs.io/v1/mcp"
oauth_client_id = "https://bot.seradyx.tech/.well-known/serabot-oauth-client.json"
oauth_scope = "text_to_speech speech_history_read flows image_video_generation voice_generation convai_read"

[elevenlabs.api_key]
enabled = false                     # the API-key connector

api_base = "https://api.elevenlabs.io"
api_key_secret = "elevenlabs_api_key"
timeout_secs = 120                  # API-key connector REST calls
```

### Upgrading from 0.23

The old keys are still read, once, and folded into the new switches:

- `enabled = true` with `auth = "oauth"` becomes `mcp_enabled = true`.
- `enabled = true` with `auth = "api_key"`, or with no `auth` (the old default), becomes `[elevenlabs.api_key] enabled = true`.

A deprecation warning is logged. The console's old auth-mode toggle is gone, and an old `admin-overrides.json` that carries it still applies.

Bot files and stored bot rows that name the old native tools are renamed on load: `elevenlabs_list_voices` → `elevenlabs_key_list_voices` (and the other four native names), and `elevenlabs_mcp_list_tools` → `elevenlabs_mcp_search_tools`.

## Limits

- **Members cannot sign in to the ElevenLabs connector.** The sign-in is owner-console only. A member who calls an MCP tool is told to sign in. Member sign-in needs a second redirect on the metadata document or a member-facing consent flow; that is not built.
- The API-key connector's key is server-wide. It is not a per-user credential. Moving it to the per-user store is a separate change.
- Enabling or disabling either connector needs a restart; the tool list is built at startup.
- Some reads (for example `agents_list`) do not match the read prefixes, so they still ask for approval. The classification is deliberately conservative.
- Scopes are product-scoped and the authorization server is per-region. The code pins the global/US endpoints. Verify each tool against a real account before relying on it.

## Mistral audio

`[audio]` remains the optional Mistral Voxtral path for inbox and Telegram transcription. ElevenLabs is independent; bots can use either or both.

## Tests

Synthetic fixtures only (wiremock for the MCP server, the token endpoint and media hosts). They cover search paging and the byte budget, describe bounds, unknown-name suggestions, catalog paging across three pages with caching, approval classes, per-account sign-in, the one-time migration, connector-dependent registration, and config back-compat.

```bat
scripts\cargo-win.bat test -p serabot elevenlabs --locked
scripts\cargo-win.bat test -p serabot config --locked
```

Not verified against a live ElevenLabs account. Live verification requires the owner's approved account and is not part of these tests.
