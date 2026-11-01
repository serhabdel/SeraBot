Adds OCR, voice replies, and a settings surface for both, on the server plus the desktop and mobile consoles.

## OCR fallback

New `crates/serabot/src/ocr.rs` speaks the Mistral `/v1/ocr` contract (`mistral-ocr-latest` by default) and is wired into document ingestion as a fallback:

- A PDF whose text layer is empty, or a PNG/JPG/WebP scan, is sent for OCR **only** when the extracted text is below `[ocr].min_text_chars` (default 200), so a normal text PDF never costs a call.
- The provider resolves through the same `[llm]` profile the audio pipeline uses, so one Mistral key serves chat, audio, and OCR.
- Fail closed: a document with no text and a failed OCR stays `failed` with the reason recorded rather than indexing nothing.
- Payload capped by `ocr.max_bytes`, response by 32 MiB, pages by 500.

## Voice replies

`[audio]` gains `speech_model` (`voxtral-mini-tts-2603`), `voice_id`, `reply_mode`, and `max_speech_chars`.

- The runtime delivers the final reply through a new `deliver_reply`: in voice mode it synthesizes speech and sends a Telegram **voice note** (`sendVoice`).
- Every failure path — provider down, no voice id, reply over the cap, transport that cannot carry audio — delivers the same text plus one line saying why. Content is never lost.
- `/voice` and `/text` switch the mode per chat. The setting is stored **per user and per conversation**, so two accounts using `dir:main` never share it, and a corrupt value reads as `text` rather than starting audio.

## Settings surface

| Method | Path | Access |
| --- | --- | --- |
| `GET` | `/v1/settings/voice` | any signed-in user — readiness for STT, TTS, OCR + why a capability is off (never a secret) |
| `PUT` | `/v1/settings/voice` | admin only |
| `GET`/`PUT` | `/v1/conversations/{id}/reply-mode` | per user |

Settings persist through the existing admin overlay (new audio fields + an OCR overlay), so the console changes them without editing TOML.

**Clients:** a Voice and OCR card in the desktop Settings pane and a Voice tab in the mobile app, both showing readiness chips, the reason a capability is off, editable speech model / voice id / OCR model, an OCR toggle, and the default reply mode.

## Also

`admin_error` now maps `SeraError::Forbidden` to 403; it previously fell through to 500.

## Verification

- Windows: `fmt` clean, `clippy -D warnings` clean, **729 tests passing**.
- Linux (WSL gate script): `fmt` clean, `clippy` clean, **715 tests passing**, release build green — `gate status: 0`.
- Mobile: `flutter analyze` clean, **24 tests passing**.
- New tests: OCR client (PDF and image payloads, unsupported mime, size cap, provider error redaction, egress consent, `Debug` redaction, https-only), reply-mode store semantics (per user, per conversation, corrupt value, invalid mode), and the API surface (readiness payload, member 403, invalid mode 422, reply-mode round trip).

Models confirmed against the current Mistral documentation via Context7: `mistral-ocr-latest`, `voxtral-mini-tts-2603`, `voxtral-mini-latest`.
