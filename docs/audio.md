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
| Telegram | Optional intake saves attachments locally; transcription still goes through tools + approval. |
| Owner HTTP API | No dedicated audio upload endpoint (body limit 2 MiB). Use inbox + tools. |

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
