# WhatsApp as a linked device (proposal, 0.58)

Status: implemented in 0.58 (server, API, desktop card). Not verified on a
live WhatsApp account. Approved by the owner on 2026-11-18 ("go ahead, I'll use a
spare number"). Nothing here is verified against a live WhatsApp account.

## Why

The current WhatsApp connector uses the Meta Cloud API (`token_connectors/whatsapp`),
which needs a Meta Business account and verification. The owner wants the
`whatsapp` bot to work from an ordinary number, and to read voice notes and
files sent to it.

[`whatsapp-rust`](https://github.com/oxidezap/whatsapp-rust) (MIT, Rust, Tokio,
0.7.0) signs in as a **linked device** of a normal WhatsApp account, like
WhatsApp Web.

## Risks the owner accepted

- The client is unofficial. Its README says use "may violate Meta's Terms of
  Service and could result in account suspension". The owner uses a **spare
  number**, never their main one.
- The linked session can read and send everything on that account. It is a
  credential and is handled like one (below).
- Protocol changes on WhatsApp's side can break the client until the library
  updates. The cloud API connector stays as the fallback.

## Scope

In:
- One linked WhatsApp account per member, opt-in per member; enabled by default
  on the server in 0.59 (`[whatsapp_link] enabled = true`, set `false` to turn off).
- Pairing from the desktop console: QR code, or pair code by phone number.
- Incoming messages from **allowed chats only** go to the member's `whatsapp`
  bot as turns in a `whatsapp:<chat>` conversation.
- Voice notes are downloaded and transcribed with the existing audio pipeline
  (`audio_transcribe`); the transcript is the turn text.
- Documents and images are saved as chat attachments of the WhatsApp
  conversation (`<inbox>/<user>/chat/<conversation>/`) with the chat-attachment checks (size,
  extension allowlist, content sniff, filename blocklist), so every bot reads
  them with the usual tools and can file them into a project.
- Replies: text, and documents from the artifact store.

Out (needs a new design): groups the owner did not allow, calls/VoIP, status,
contact sync, reading history from before pairing, sending to arbitrary numbers
without approval.

## Security

- **Session keys**: the library's own SQLite store, one file per member at
  `[whatsapp_link] dir/<user>.db` (production: `/var/lib/serabot/whatsapp`),
  folder 0700 and file 0600, owned by the service user, never in the main
  database. It is **not** envelope-encrypted: the library opens it directly.
  It has the same protection as the main database file; encrypting it needs a
  custom `Backend` (option 2 below) and is left for later. The admin API never
  returns it.
- **Sign out** removes the device on WhatsApp's side (logout), then deletes the
  file.
- **Allowlist**: messages from chats not on the member's allowlist are dropped
  before any model call and never stored. Default allowlist: the member's own
  number ("note to self").
- **Untrusted input**: message text, transcripts and file content are untrusted
  data, never instructions (same labelling as chat attachments).
- **Replies**: the bot's answer to a message from an allowed chat goes back to
  that chat (the `WhatsappLink` notifier), like Telegram. Allowing a chat is
  the member's consent for replies there.
- **Outbound**: a message the bot starts itself goes through
  `whatsapp_link_send`, which is on `DESTRUCTIVE_TOOLS`. Approval binds the
  user, the chat (destination `whatsapp:<jid>`) and the canonical text. An
  approval notice in a WhatsApp chat only says an approval is waiting; it is
  decided in SeraBot, never over WhatsApp.
- **Blocklist** scan on every outgoing text, filename and document.
- **Rate limits**: at most N sends per chat per minute (default 6) to keep
  the number from looking like spam.
- No phone numbers, message bodies or JIDs in logs at `info`; IDs are hashed.

## Storage: the one build blocker

`whatsapp-rust`'s default store is `whatsapp-rust-sqlite-storage`, built on
diesel with `libsqlite3-sys ^0.37`. SeraBot uses `rusqlite 0.31`
(`libsqlite3-sys 0.28`). Both link the native `sqlite3`, and Cargo allows only
one: adding the crate with default features **fails to resolve** (checked).
With `default-features = false` (no sqlite storage) it resolves.

Options:

1. **Upgrade rusqlite** (and `r2d2_sqlite`) to the release on
   `libsqlite3-sys 0.37`, then use the library's `SqliteStore` in its own file.
   Least new code, uses the library's tested store. Cost: one dependency
   upgrade across `store.rs` and friends, and a full test run.
2. **Implement `wacore::store::Backend` over rusqlite** (`SignalStore`,
   `AppSyncStore`, `ProtocolStore`, `MsgSecretStore`, `DeviceStore`, about 88
   methods). No dependency upgrade, and encryption can be done per row, but it
   is a lot of protocol-sensitive code to get right.

**Chosen: option 1.** Done in its own change (rusqlite 0.31 to 0.39, r2d2_sqlite
0.24 to 0.34, libsqlite3-sys 0.37): whatsapp-rust with default features now
resolves. The full test suite passed unchanged. Kept separate so a
problem in the upgrade is easy to see and revert.

## Build

- `rust-version` 1.95 matches the workspace.
- Feature flag `whatsapp-link` on `serabot` (default on for release builds,
  off in a minimal build), features
  `tokio-transport, tokio-runtime, ureq-client, tokio-native, signal`.

## Pieces

- `whatsapp_link/mod.rs`: per-member client supervisor (start, reconnect with
  backoff, stop on sign-out, one client per member).
- `whatsapp_link/inbound.rs`: allowlist, media download, transcription, inbox
  write, turn submission.
- `whatsapp_link/tools.rs`: `whatsapp_link_send` (text or artifact).
- `api/whatsapp_link.rs`: `POST /v1/whatsapp-link/pair` (QR or pair code,
  streamed), `GET` status, `DELETE` (sign out), `PUT /allowlist`.
- Desktop: a card in Integrations: Link a spare number, QR / code view, status,
  allowed chats, Sign out, and the ban-risk banner.

## Tests (synthetic only)

Allowlist drop, media size/type refusal, voice note to transcript to turn (mock
transcriber), approval binding for sends, sign-out deletes the store, a
member's session never visible to another member. Nothing is reported as
working on WhatsApp until the owner pairs a spare number and runs it.

## Build note

`whatsapp-rust` is used with `default-features = false` and
`sqlite-storage, tokio-transport, tokio-runtime, ureq-client, tokio-native`:
its default `simd` feature needs nightly Rust (`portable_simd`).


## 0.63: the WhatsApp bot, requested files, and limits

Approved by the owner on 2026-11-19.

- **One bot speaks on WhatsApp.** Only the bot named by `[whatsapp_link] bot`
  (default `whatsapp`) gets WhatsApp tools. Every other bot loses them on
  every turn, whatever its stored tool list says (main's `tools = ["all"]`
  included). Other bots reach WhatsApp by being asked by the WhatsApp bot
  (`ask_teammate`), never directly.
- **Business is separate.** The WhatsApp Business (Cloud API) tools are given
  to the WhatsApp bot only when the member saved Business credentials
  (`conn.whatsapp`). A linked personal number never needs them.
- **Conversations stay with the WhatsApp bot.** Messages from an allowed chat
  open or continue that chat's conversation with the WhatsApp bot. It may ask
  any of the member's bots for help with `ask_teammate` (the existing
  handoff ledger: one tracked request and one answer, not free chatter) and
  passes the answer back.
- **Requested files go back without a prompt.** `whatsapp_link_reply_file`
  sends one of the member's own files (artifact ownership checked) into the
  chat the current conversation came from. It takes no recipient, so it can
  only answer the person who asked, like the automatic text reply. It is not
  on `DESTRUCTIVE_TOOLS`. Telegram has the same rule with
  `telegram_reply_file`, which refuses outside a Telegram-origin conversation.
- **Bot-started messages still need approval.** `whatsapp_link_send` (to any
  allowed chat, on the bot's own initiative) stays approval-gated, as do
  `telegram_send_document` from other conversations and the Business sends.
- **Limits.** Allowlist only; no groups. At most `sends_per_minute` (6) per
  chat and `sends_per_day` (100) per member across all chats, counting text
  replies and files. Filenames and captions pass the blocklist. Every tool
  call is audited by the gateway.
- **Device name.** The linked device registers as a Chrome browser on
  `device_os` (default `Windows`) instead of the library default. It is set
  at pairing: sign out and link again to change it. It only changes the
  Linked devices label; it does not lower the ban risk of an unofficial
  client.
