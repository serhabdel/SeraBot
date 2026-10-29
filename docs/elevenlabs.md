# ElevenLabs integration

SeraBot talks to the native [ElevenLabs API](https://elevenlabs.io/docs/api-reference/introduction) for speech synthesis, speech-to-text, sound effects, and Image & Video flow generations. All outbound calls use HTTPS and the age-encrypted secret `elevenlabs_api_key` (configurable via `[elevenlabs].api_key_secret`).

## Enable

1. Initialize the secret store if needed: `serabot secrets init`
2. Create an API key at [ElevenLabs](https://elevenlabs.io/app/settings/api-keys) and store it without printing it:
   ```bat
   set /p KEY= <nul & serabot secrets set elevenlabs_api_key
   ```
   (Paste the key, then end input with Ctrl+Z on Windows or use your shell’s stdin redirection.)
3. Set `[elevenlabs].enabled = true` in `serabot.toml` and optionally `default_voice_id`.
4. Allowlist tools on bots, for example:
   ```toml
   tools = ["elevenlabs_list_voices", "elevenlabs_text_to_speech", "elevenlabs_generate_image"]
   require_approval = ["elevenlabs_text_to_speech", "elevenlabs_generate_image", "elevenlabs_generate_video"]
   ```

Generation tools are marked `external_effect` and always require owner approval unless standing consent applies.

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
