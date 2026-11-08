# SeraBot 0.24.0

## ElevenLabs

Two independent connectors replace the single ElevenLabs connector with an auth-mode toggle.

- **ElevenLabs** (recommended): each account signs in with its own ElevenLabs account. The hosted MCP server exposes the full catalog (120+ tools). Three tools work through it: `elevenlabs_mcp_search_tools`, `elevenlabs_mcp_describe_tool`, `elevenlabs_mcp_call`. A call runs only a name the account's catalog returned; an unknown name is refused with the five closest names.
- **ElevenLabs API key** (legacy): the owner's API key and the native tools, renamed `elevenlabs_key_*`. They register only when the connector is enabled and a key is stored.

Changes:

- The sign-in is stored per account (provider `elevenlabs_mcp` in the per-user store). The pre-0.24 server-wide sign-in moves to the owner's account once on startup, then the server-wide names are removed.
- The tool catalog is cached per account for ten minutes; a 401 or a failed fetch drops it. Tool lists are paged up to 20 pages and 1000 tools.
- Search replies are at most 24 KiB, 40 tools a page, never cut mid-JSON; entries that do not fit are dropped whole and counted in `truncated`. Describe replies stay within about 16 KiB, with deeply nested schema left out and noted.
- Timeouts: 60 s for list, search, describe and read; 300 s (`mcp_generate_timeout_secs`) for calls that generate, create or spend.
- `elevenlabs_mcp_call` returns every saved media item in `data.artifacts` (`mime`, `artifact_path`, `artifact_hash`, `filename`, `bytes`). Recording all of them as artifacts is left to the media-jobs change.
- Config: `[elevenlabs] mcp_enabled` and `[elevenlabs.api_key] enabled`. The old `enabled` and `auth` keys are read once and folded in with a warning. Bot files, stored bot rows and console overrides that name old tools are renamed on load.
- Console: two connector cards with their own status, connect and disconnect. The mode toggle is removed. Sign-in takes effect without a restart; enabling a connector needs one.

Not done in this release:

- Members cannot sign in to the ElevenLabs connector (owner console only). Member sign-in needs a second redirect or a member consent flow.
- The API-key connector's key is still server-wide, not a per-user credential.
- Enabling or disabling either connector still needs a restart, because tools are registered at startup.
- Not verified against a live ElevenLabs account.

See `docs/elevenlabs.md`.
