# Release 0.64.0

WhatsApp follow-up: voice notes are understood, the WhatsApp bot can see its
WhatsApp chats, and sending to allowed chats no longer asks for approval.
Tests use synthetic fixtures only. Nothing here ran against a live WhatsApp,
Telegram or Mistral account.

## Voice notes

- **Why they were missed:** WhatsApp voice notes were transcribed only with the
  server's audio provider. When server audio is off, the transcription failed
  and the whole message was dropped silently.
- **Now:** `audio::transcribe_audio_bytes_for_user` tries the server's audio
  provider first. If none is set up, it uses the member's own Mistral account
  (AI providers), the same rule as the per-user OCR fallback.
- **If neither works:** the voice note is kept as an attachment and the bot is
  told why ("no transcription is set up…"), so it can say so instead of
  claiming nothing arrived.
- Telegram voice notes and the console's dictation use the same fallback.

## Visibility for the WhatsApp bot

- Each WhatsApp chat is now listed under the **WhatsApp bot** in the sidebar,
  titled "WhatsApp · +number".
- New read-only tool `whatsapp_link_chats`: the WhatsApp bot lists your allowed
  chats with their latest messages, voice notes included. From the console you
  can ask "did you get my voice note?" and it checks. It covers only the acting
  member's allowed chats.

## No approval for allowed chats

- `whatsapp_link_send` is no longer approval-gated. This was the owner's
  decision.
- It still refuses any number that is not on your allowlist, never reaches
  groups, and counts toward the 6-per-minute and 100-per-day caps.
- The bot may send to several allowed chats when you ask.
- Spec, `docs/approvals.md` and AGENTS.md are updated.

## To try

1. If server audio is off, connect a Mistral account under AI providers.
2. Send a voice note from an allowed WhatsApp chat.
3. Open the WhatsApp bot and look for "WhatsApp · +number" under it.
4. Ask the bot in the console what arrived.
