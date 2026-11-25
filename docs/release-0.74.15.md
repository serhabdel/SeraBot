# Release `0.74.15` (in progress)

Plan: `docs/specs/0.74.15-proposal.md`. This file grows as pieces land; the version is bumped when the batch is cut.

## Landed

- **Mail composer**: sticky footer (Send is always visible), wider drawer, chip fields for To/Cc/Bcc (Enter, comma, semicolon, Tab, a space after a finished address, or a paste commits; Backspace removes the last chip; arrow keys pick suggestions; invalid addresses are marked and block Send), attach from this PC, by drag and drop, or from the Library (10 files, 18 MB in total), Ctrl+Enter sends.
- **Chat message bar**: Copy, Quote, Create task and Edit sit in a row under their message instead of floating over the neighbouring one.
- **Retried messages**: a message sent again with no reply in between shows once. Transcript only; the server still stores both copies. Real recall and retry-by-reference are proposed (Track 7b) and need approval.

- **New-mail badge**: `GET /v1/mail/unread` (one read-only IMAP search, nothing fetched), polled every 90 s, shown on the Mail rail button.
- **Attachments on Send later and Undo send**: files now travel with queued messages (migration 0053); ownership is checked when queued and at send, and a failed lookup retries instead of sending without files.
- **Faster mail list**: the API list and search make one batched request for the first 8 KB of each message instead of fetching every message whole; they fall back to the old way if a server refuses.
- **Tracing**: every task runs inside a span with its id, kind, bot and attempt.
- **Tests**: a reusable fake IMAP server, client round trips against it, 29 hostile HTML payloads and header edge cases.
- **Owner checklist**: `scripts/owner-checklist.py` and `docs/owner-checklist.md`.

## Owner verification

- Hostinger: the unread badge, a scheduled message with an attachment, and list speed on a large inbox.
- Windows: drop files on the composer, press Enter in To, check the footer in a short window, check the chat bar.
