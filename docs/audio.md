# Audio understanding (Mistral Voxtral)

SeraBot can send inbox audio to the configured LLM provider for **verbatim transcription** or **instruction-driven understanding**. Implementation is native Rust (`reqwest`); credentials are **secret names** in the age store, never logged.

## Provider APIs (Mistral, ~2026-10)

| Goal | HTTP | Model (defaults) |
|------|------|------------------|
| Verbatim transcript | `POST /v1/audio/transcriptions` (multipart `file` + `model`) | `voxtral-mini-latest` (offline/file mode; newer docs also list Voxtral Mini/Small Transcribe 3) |
| Summary, Q&A, translation, extraction | `POST /v1/chat/completions` with `input_audio` (base64) + text instruction | `voxtral-small-latest` |

Realtime WebSocket transcription and speech-to-speech agents are **not** wired in this release.

## Configuration

See `[audio]` in `serabot.toml` and `deploy/serabot.production.toml.example`.

- `enabled` — registers gateway tools at startup (requires the provider API key in the secret store).
- `provider` — `default` uses `[llm]` base URL, `api_key_secret`, and `data_egress_approved`. Other ids must exist under `[llm.providers.*]`.
- Optional `base_url` / `api_key_secret` override the chosen profile without changing `[llm]`.
- `max_bytes` — inbox file size cap before upload (default 50 MiB).
- `telegram_intake` — when `true` and Telegram is enabled, owner **voice** and **audio** messages are downloaded into `inbox/telegram/<message_id>.<ext>` (owner-only; no automatic egress).

## Gateway tools

| Tool | Effect | Approval |
|------|--------|------------|
| `audio_transcribe` | Sends file bytes to Mistral transcription | `external_effect`; binds file SHA-256 |
| `audio_understand` | Sends file + instruction via chat completions | same |

Both tools:

- Resolve paths **only** under `documents.watch_dir` (same inbox as documents).
- Allowed extensions: wav, mp3, mpeg, ogg, oga, opus, m4a, aac, flac, webm.
- **Fail closed** if provider `data_egress_approved` is false or the API key is missing.
- Return provider text as **untrusted data** (blocklist applies on egress from the agent).

Add tool names to bot `tools = [...]` (for example `bots/analyst.toml`).

## Intake paths

| Path | Behavior |
|------|----------|
| Inbox file | Owner copies audio into `data/inbox` (or production path); bots call tools with a relative `path`. |
| Telegram | **Native.** A voice note is downloaded, transcribed immediately, and the transcript is enqueued as the owner's own message in the same chat, so the normal turn pipeline answers it. The original file is still saved under `inbox/telegram/`. |
| Owner HTTP API | `POST /v1/audio/transcribe` (owner token) transcribes raw bytes and returns the text. Used by the desktop console's microphone button. |

### Telegram voice notes

When `audio.enabled` is on and the audio provider is configured (provider
resolvable, credential present in the age store, and `data_egress_approved`
set), the server:

1. downloads the Telegram attachment (size limit `audio.telegram_max_bytes`),
2. saves it to `inbox/telegram/<message_id>.<ext>` (unchanged intake),
3. transcribes it with the configured transcription model, and
4. enqueues the transcript through the same `submit_text` path used for a typed
   owner message — so it is policy-scanned and blocklist-checked like any other
   incoming message.

The owner is told the result in one line. When audio is off or unconfigured,
the owner gets one line saying voice notes are off and where to turn them on —
never a list of tool names or inbox paths.

| Situation | Owner sees |
|-----------|-----------|
| Transcribed | `Voice note transcribed — answering now. The original recording is saved as inbox/telegram/<message_id>.<ext>.` |
| Provider unavailable | `I couldn't transcribe that voice note — the audio provider is unavailable right now, so please try again or send it as text.` |
| Too large | `That voice note is too large to transcribe — please send a shorter clip or type your message.` |
| Unsupported format | `That audio format can't be transcribed — please send a WAV, MP3, OGG, M4A, FLAC, or WebM file.` |
| Download failed | `I couldn't download that voice note — please send it again.` |
| Nothing heard | `I couldn't hear anything in that voice note — please try again or type your message.` |
| Audio off | `Voice notes are off — turn on audio in Settings (and store its provider key) to have them transcribed automatically.` |

Transcripts are owner content: they are never written to the server log, only
the failure reason is.

### `POST /v1/audio/transcribe`

Owner token required.

| | |
|---|---|
| Request | Raw audio bytes as the body, `X-Audio-Filename: note.webm` header (≤128 chars, no separators or control characters) |
| Body limit | 20 MiB (`DEFAULT_TELEGRAM_AUDIO_MAX_BYTES`); the JSON endpoints keep the smaller `server.body_limit_bytes` |
| 200 | `{"text":"transcribed text","filename":"note.webm","bytes":1234}` |
| 400 | Validation (bad filename, empty body) |
| 413 | Over the size limit |
| 502 | Upstream provider failure (sanitized; never echoes a key) |
| 503 | Audio provider not configured — names the missing setting |

### Telegram limits

- Only the configured `owner_user_id` / `owner_chat_id` are accepted.
- Default `telegram_max_bytes` = 20 MiB (configurable).
- Voice notes are stored as `.ogg`; audio documents use the filename extension when present.
- Telegram Bot API file download applies; very large files may be rejected by Telegram before SeraBot sees them.

## Secrets

Store the Mistral key under the name referenced by `[llm].api_key_secret` (default `llm_api_key`) or under `[audio].api_key_secret` when overridden:

```bash
printf '%s' "$MISTRAL_API_KEY" | serabot secrets set llm_api_key
```

Enable egress only after explicit owner review:

```toml
[llm]
data_egress_approved = true
```

## Tests

Unit/integration tests in `crates/serabot/src/audio.rs` use **wiremock** fixtures (no live Mistral calls).
