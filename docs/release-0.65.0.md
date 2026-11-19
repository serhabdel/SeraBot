# Release 0.65.0

Fixes WhatsApp messages never reaching the bot, and proposes the desktop
network relay. Tests use synthetic fixtures only. Nothing here ran against a
live WhatsApp account.

## WhatsApp: the bot now answers when you write

- **Cause:** WhatsApp now addresses most one-to-one chats by a private id
  (`…@lid`) rather than the phone number. SeraBot only accepted
  phone-number chat ids, so every LID-addressed message was silently dropped,
  texts and voice notes alike. That is why the bot said "no incoming
  messages" and never answered on its own.
- **Fix:** the inbound check now uses every address on the message (chat,
  sender, `sender_alt`, `recipient_alt`, and the client's LID-to-phone map).
  An allowed number is accepted, and the turn starts right away: the bot
  answers in the same chat, and voice notes use the 0.64 transcription
  fallback.
- Unchanged: groups, your own messages and numbers that are not on the
  allowlist stay out. An ignored message is logged with no number.

## Desktop network relay: design for review

`docs/specs/desktop-network-relay.md` describes how to send chosen model
traffic, for example Antigravity, through your own desktop:

- The server tunnels it to your PC. TLS stays end to end, so the desktop only
  moves encrypted bytes and never sees keys or prompts.
- It covers a fixed list of hosts per provider, and the desktop refuses
  private addresses.
- It is off by default, audited, and works only while the desktop is open.

Building it is planned for 0.66 once the open questions are answered.
