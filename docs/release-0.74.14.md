# Release `0.74.14`

A UX wave over the mail window, the model picker, Settings and the shared design layer,
plus the 0.74.13 loose ends. Everything below ran through fmt, clippy `-D warnings`, the
full workspace tests and the Windows UI compile in CI. Nothing was run against a live
mailbox or on a real Windows desktop; see **Owner verification**.

## Summary

- **Scheduled-send reliability (0.74.13 follow-up)** (`store_mail.rs`, `scheduler.rs`, migration `0052`)
  - A transient SMTP/network failure retries after 1 minute, then 5 minutes, then fails. A blocklist or validation refusal fails at once.
  - A row left in `sending` by a crash or restart is marked `failed` after 10 minutes and **never resent**, because it may already have been delivered. The Scheduled folder shows why.
  - The `complete`/`fail` writes are checked; a send that went out but could not be marked sent is logged as an error.
  - The background loop now ticks every 5 seconds (it was 30), which Undo send needs.

- **Mail server** (`mail_ux.rs`, `mail.rs`, `api/mail.rs`, `api/mail_extras.rs`)
  - List and search results carry `to`, `cc`, `message_id`, `in_reply_to`, `references`, `snippet`, `has_attachments`, `thread_key` and `unsubscribe`. Conversations group by `References`/`In-Reply-To`; messages with the same subject group only when one carries a `Re:`/`Fwd:` prefix.
  - Search operators: `from:`, `to:`, `subject:`, `is:unread|read|starred`, `has:attachment`, `before:`, `after:`. A bare word keeps the old subject/sender/body search. `has:attachment` is checked after the fetch.
  - Snooze (`POST/DELETE /v1/mail/messages/{uid}/snooze`, `GET /v1/mail/snoozed`): the message stays in the mailbox, is hidden from its folder list until the wake time, then returns unread.
  - Drafts (`POST /v1/mail/drafts`): IMAP `APPEND` to the Drafts folder, replacing the previous autosave.
  - Undo send: `POST /v1/mail/schedule` with `kind: "undo"` holds a message 3 to 120 seconds, hidden from the Scheduled folder unless it failed. Messages with attachments skip the hold.
  - Recipient suggestions (`GET /v1/mail/contacts`) from sent mail and the senders of listed mail. Saved searches (`/v1/mail/saved-searches`, 25 per member).
  - Unsubscribe (`POST /v1/mail/unsubscribe`): sends the `mailto:` request of a `List-Unsubscribe` header after the usual blocklist scan. `https:` links are never fetched by the server; the desktop opens them in the member's browser.
  - Migration `0052`: `scheduled_mail.attempts/claimed_at/kind`, `mail_snooze`, `mail_contacts`, `mail_saved_searches`. All rows are UID-scoped.

- **Desktop mail window** (`ui_mail*.rs`, `mail_view.rs`, `assets/mail.css`)
  - Resizable panes and a Comfortable/Compact density switch, remembered per device.
  - Rows with avatar, sender, snippet, time and attachment/star icons; day headings; conversation grouping with a count and expand; hover quick actions; multi-select (Ctrl/Shift-click, checkboxes) with a bulk bar.
  - Archive, delete and move leave the list at once and reach the server after 6 seconds unless **Undo** is pressed. Leaving Mail does not lose them. Flag changes roll back if the server refuses them. Snooze, bulk mark read/unread/star and send have an Undo toast.
  - Delete moves to Trash when the mailbox has one; in Trash, or with no Trash folder, it deletes for good (still with the Undo window).
  - Snooze dialog (later today, tomorrow morning, this weekend, next week, custom), Snoozed folder with Unsnooze, saved searches in the sidebar, search tips, skeleton loaders and real empty states.
  - Reader: sender header with avatar and full date, Reply / Reply all / Forward, archive, delete, snooze, star, read toggle, move, an Ask-bot strip (summarize, draft a reply, extract tasks, suggest replies; each opens a chat with the message filled in and sends nothing), unsubscribe banner, remote-image banner.
  - Compose: recipient suggestions, draft autosave to the Drafts folder after 3 quiet seconds, Undo send (about 10 seconds), Esc to close.
  - Keyboard: `j/k`, `Enter`, `c`, `r`/`a`, `f`, `e`, `#`, `s`, `u`, `z`, `x`, `Ctrl+A`, `Ctrl+Z`, `/`, `g`, `Esc`, `?` (cheat sheet). Shortcuts are ignored while typing in a field or while a dialog is open.
  - Fixed: "Mark unread" and un-star did not update rows loaded from the server, because the server reports flags without the backslash.

- **Mail composer** (`ui_mail_compose.rs`, `assets/ux.css`)
  - The Send buttons are always visible (sticky footer, wider drawer); they used to fall out of view in a short window.
  - To, Cc and Bcc are chip fields: Enter, comma, semicolon, Tab, a space after a finished address, or a paste of several addresses makes chips; Backspace removes the last one; arrow keys pick suggestions. Invalid addresses are marked and block Send with a message.
  - Attach files from this PC, by drag and drop onto the composer, or from the Library. Limits (10 files, 18 MB in total) are checked before upload. Ctrl+Enter sends.

- **Chat message bar and retries** (`ui/chat.rs`, `chat_feed.rs`)
  - Copy, Quote, Create task and Edit sit in a row under their message instead of floating over the neighbouring one.
  - A message sent again with no reply in between shows once. This hides the copy in the transcript only; the server still stores both. A real recall needs a server change, proposed in `docs/specs/0.74.15-proposal.md`.

- **Model picker** (`picker_search.rs`, `ui/chat.rs`)
  - Fuzzy search ("son5" finds "claude-sonnet-5"), ranked by match quality.
  - Favorites (star) and Recent, kept on this device; with an empty search box favorites lead, then recents.

- **Chat** (`ui/markdown.rs`): a Wrap toggle on code blocks.

- **Settings, palette, shell** (`session.rs`, `ui_settings_page.rs`, `ui/sheets.rs`, `ui/shell.rs`, `assets/ux.css`)
  - Settings search with a one-line description under every section.
  - Command palette: every Settings section, and "Compose email". The input is now a proper combobox (`aria-activedescendant`, option ids) and the active row scrolls into view.
  - The Chats button in the rail shows how many approvals are waiting.
  - One keyboard focus ring for every control, a spacing scale (`--sp-1`..`--sp-6`) and OS reduced-motion support.

- **Mobile** (`apps/serabot_mobile`): list rows show the snippet; the message model reads the new fields.

## Not in this release

These were proposed and left out on purpose, so the notes do not overstate:

- Branch-from-here and pin on chat messages (need server support for forking a conversation), and recalling a sent message (see the 0.74.15 proposal).
- A single Send/Stop morph button, mode chips in the composer, per-model cost and speed hints (the catalog has no price or latency data) and a per-chat versus default model toggle.
- A shared Save/Discard bar for Settings (each card still saves itself) and an Activity feed (no event source for it yet).
- Mail on mobile beyond the snippet: threads, snooze, drafts and undo send are desktop-only for now.

## Owner verification

- Live Hostinger/Dovecot: `APPEND` to Drafts, folder discovery, search operators, list threading, snooze wake (marks unread), Undo send, unsubscribe `mailto:`.
- Windows WebView2: pane resizing, density, keyboard shortcuts, the Undo bar, the snooze dialog and the model picker star.
- Times in the mail list use the offset reported by the webview; check them against your clock.

## Verification

- CI on the release branch: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo check` and tests on Ubuntu and Windows, the desktop UI check, `flutter analyze` and `flutter test`.
- New tests: `mail_ux` (operators, threading, unsubscribe, snippets, retry maths), `store_mail` (snooze, contacts, saved searches, retries, stale rows), a fake-IMAP API test over threads, snooze, contacts, saved searches, drafts and undo hold, desktop client round trips, `mail_view` and `picker_search` (std-only).
