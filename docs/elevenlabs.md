# ElevenLabs integration

SeraBot talks to the native [ElevenLabs API](https://elevenlabs.io/docs/api-reference/introduction) for speech synthesis, speech-to-text, sound effects, and Image & Video flow generations. All outbound calls use HTTPS and authenticate with **either** an API key (`xi-api-key` header, age-stored `elevenlabs_api_key`) **or** an OAuth bearer token from an ElevenLabs sign-in — never both, because the REST API rejects requests carrying both headers.

## Enable (API key)

1. Initialize the secret store if needed: `serabot secrets init`
2. Create an API key at [ElevenLabs](https://elevenlabs.io/app/settings/api-keys) and store it without printing it:
   ```bat
   set /p KEY= <nul & serabot secrets set elevenlabs_api_key
   ```
   (Paste the key, then end input with Ctrl+Z on Windows or use your shell’s stdin redirection. Keys are `sk_` + 48 hex characters; a truncated key is rejected at save time and reported as `Key rejected (400)` by the Test button.)
3. Set `[elevenlabs].enabled = true` (and `auth = "api_key"`, the default) in `serabot.toml`, optionally `default_voice_id`.
4. Allowlist tools on bots, for example:
   ```toml
   tools = ["elevenlabs_list_voices", "elevenlabs_text_to_speech", "elevenlabs_generate_image"]
   require_approval = ["elevenlabs_text_to_speech", "elevenlabs_generate_image", "elevenlabs_generate_video"]
   ```

Generation tools are marked `external_effect` and always require owner approval unless standing consent applies.

## Enable (OAuth sign-in)

`auth = "oauth"` switches the connector to `Authorization: Bearer <token>` using ElevenLabs OAuth 2.0 (authorization code + PKCE S256 + refresh token; authorization server `https://api.us.elevenlabs.io`, consent page `https://elevenlabs.io/app/oauth/authorize` — pinned in code, loopback allowed in tests only).

1. The server publishes its CIMD client metadata document (no dynamic client registration needed) at `https://bot.seradyx.tech/.well-known/serabot-oauth-client.json` — served by the public routes in `web.rs`. The `client_id` **is** that URL (`[elevenlabs].oauth_client_id`, overridable). The document declares exactly one redirect: `http://127.0.0.1:51122/oauth-callback`.
2. In the desktop console, open Connectors → ElevenLabs, choose auth mode `oauth`, and press **Sign in**. The console binds `127.0.0.1:51122` on your PC, opens the consent page in your browser, and sends the code to the server, which exchanges it at the pinned token endpoint. Tokens are persisted to the age store (`elevenlabs_access_token`, `elevenlabs_refresh_token`) and never returned to the console.
3. API: `POST /v1/admin/connectors/elevenlabs/oauth/start` → `{"flow":"pkce","url":…}`; `POST …/oauth/complete` with `{code, verifier, redirect_uri}`. Then `PUT /v1/admin/connectors/elevenlabs` with `{"enabled": true, "auth": "oauth"}` (the mode is stored in `admin-overrides.json`) and restart only `serabot.service`.
4. Scopes requested: `text_to_speech speech_history_read flows image_video_generation voice_generation convai_read` (`[elevenlabs].oauth_scope`). Verify each tool against the real account — scopes are product-scoped, and the authorization server is per-region (the code pins the global/US one).

At runtime the client refreshes the access token proactively before expiry and once more on a 401; a rotated refresh token is written back to the age store and a failed write fails the call. The connector Test button probes `GET /v2/voices?page_size=1` with whichever credential mode is active.

## Gateway tools

| Tool | Effect | Notes |
|------|--------|--------|
| `elevenlabs_list_voices` | read-only | `GET /v2/voices` |
| `elevenlabs_text_to_speech` | external | `POST /v1/text-to-speech/{voice_id}` → artifact |
| `elevenlabs_sound_effect` | external | `POST /v1/sound-generation` → artifact |
| `elevenlabs_transcribe` | external | `POST /v1/speech-to-text` from artifact hash or inbox path |
| `elevenlabs_generate_image` | external | `POST/GET /v1/flows/image` with polling → artifact |
| `elevenlabs_generate_video` | external | `POST/GET /v1/flows/video` with polling → artifact |

Prompts and TTS text are checked against the blocklist before leaving the host. Tool output is scanned again by the gateway.

## Mistral audio

`[audio]` remains the optional Mistral Voxtral path for inbox/Telegram transcription. ElevenLabs is independent; bots can use either or both when enabled.

## Tests

Wiremock unit tests live in `crates/serabot/src/elevenlabs.rs`. Run:

```bat
scripts\cargo-win.bat test -p serabot elevenlabs --locked
```
