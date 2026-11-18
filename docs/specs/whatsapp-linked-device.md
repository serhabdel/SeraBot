# WhatsApp as a linked device (proposal, 0.58)

Status: proposal. Approved by the owner on 2026-11-18 ("go ahead, I'll use a
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
- One linked WhatsApp account per member, opt-in, off by default
  (`[whatsapp_link] enabled = false`).
- Pairing from the desktop console: QR code, or pair code by phone number.
- Incoming messages from **allowed chats only** go to the member's `whatsapp`
  bot as turns in a `whatsapp:<chat>` conversation.
- Voice notes are downloaded and transcribed with the existing audio pipeline
  (`audio_transcribe`); the transcript is the turn text.
- Documents and images are downloaded into the member's inbox
  (`<inbox>/<user>/whatsapp/<chat>/`) with the chat-attachment checks (size,
  extension allowlist, content sniff, filename blocklist), so every bot reads
  them with the usual tools and can file them into a project.
- Replies: text, and documents from the artifact store.

Out (needs a new design): groups the owner did not allow, calls/VoIP, status,
contact sync, reading history from before pairing, sending to arbitrary numbers
without approval.

## Security

- **Session keys**: never in plain SQLite. The library's store goes in its own
  database file under `/var/lib/serabot/whatsapp/<user>.db`, mode 0600, owned
  by the service user. The file is encrypted at rest with the member's
  envelope key from `user_secrets` (decrypted into memory at start, or via a
  `Backend` wrapper, see Storage). The admin cannot read it.
- **Sign out** removes the device on WhatsApp's side (logout), then deletes the
  file.
- **Allowlist**: messages from chats not on the member's allowlist are dropped
  before any model call and never stored. Default allowlist: the member's own
  number ("note to self").
- **Untrusted input**: message text, transcripts and file content are untrusted
  data, never instructions (same labelling as chat attachments).
- **Outbound**: `whatsapp_link_send` goes through the gateway and is on
  `DESTRUCTIVE_TOOLS`. Approval binds user, chat JID (destination), canonical
  text and artifact hash. A reply in the same chat the message came from can
  use a per-chat standing grant the member turns on; nothing else can.
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
