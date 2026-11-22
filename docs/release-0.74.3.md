# Release 0.74.3

Big patch batch on 0.74: LiveCard working indicator, chat rendering stability,
handwriting-mixed replies (desktop + mobile, server strips fences on
Telegram/WhatsApp), and mobile catch-up (history search, Mail card,
Tasks/Kanban polish).

## Chat rendering stability (desktop)

- Row-id scroll anchor instead of 96px spacer math (`ui/chat_feed_view.rs`,
  `chat_scroll.rs`): the feed reports the first visible `.msg` row id.
- `transcript_fingerprint()` now includes `TurnLedger.revision` and
  `Activity.steps` (`chat_feed.rs`, `ui/chat.rs`), so trace/memory updates
  invalidate the row cache.
- Generation-counted composer (`chat_compose.rs`, `session.rs`,
  `session/orchestration.rs`, `ui_files.rs`): `composer_gen` bumps only on
  intentional replace; selectionStart/End saved/restored around repaints.
- `latest_context()` returns the newest turn's context or None
  (`turn_trace.rs`); server re-emits `turn.context` after compaction and
  after failover (`runtime/turn_loop.rs`, `runtime/compaction.rs`).
- One working indicator with live elapsed time (`activity.rs`,
  `ui/chat.rs`, `ui/wait_cards.rs`, `ui/shell.rs`).

## LiveCard (desktop)

- New `LiveCard` in `ui/wait_cards.rs`, replacing the working line +
  `wait_line` in `ui/chat.rs`: Thinking / Using tool + connection chip /
  Writing / Delegating / Waiting for model / Approval needed / Failover
  (transient, ~6s) / Failed (reuses FailedTurnCard + elapsed time).
- Client-side `started_at: Instant`, reusing the 1s repaint. No handoff
  objectives, tool arguments, queries, file contents, or provider error
  text are added to events. Runs sidebar remains the drill-down.

## Handwriting-mixed replies

- Desktop: `:::hand` block in `ui/markdown.rs` (`MdBlock::HandNote`),
  Caveat (OFL) bundled as base64 woff2 (`fonts.rs`, `assets/fonts/`),
  `--font-hand` token, `.md-hand` class, Appearance toggle
  (default On, `UiPrefs.handwritten_notes`). Model rule in
  `prompts/main.txt`: max 1 block/reply, max 3 lines, notes/sign-offs
  only, Latin-script only, never facts/numbers/dates/amounts/code/links.
- Mobile: `MdBlockKind.hand` in `lib/markdown.dart`, `fontFamily:
  'Caveat'` with system fallback. Caveat TTF is NOT bundled (binary
  cannot round-trip through the GitHub MCP text tools); add
  `assets/fonts/Caveat-Regular.ttf` via a binary-capable route and
  register it in `pubspec.yaml` to get the real face.
- Server: `:::hand` fences stripped in `telegram.rs` and
  `whatsapp_link/client.rs` before sending (plain text, `✎ ` prefix
  on handwritten passages); fences can never leak literally.
- Not in this batch: inline hand spans, whole-reply handwriting,
  tilt/paper effects, PDF/DOCX handwriting.

## Mobile catch-up

- Full-text conversation search: `GET /v1/conversations/search` client
  (`api_client.dart`, `chat_models.dart`) + Message matches section in
  Chats with snippets and jump-to-chat.
- Mail (SMTP/IMAP) card (`mail_models.dart`, `mail_screen.dart`,
  Settings tile): Hostinger preset, editable hosts/ports/security,
  masked write-only password, live SMTP/IMAP test badges, Disconnect.
- Tasks/Kanban/Calendar polish: stat tiles, Board (To do / In progress /
  Done) segment, shared empty states.
- Already at parity, unchanged: Costs, Jobs, Alerts.
- Deferred: per-bot filter on message search, Kanban drag-and-drop,
  mail_list/mail_read inbox UI.

## CI fixes

- IMAP greeting: `Wire::expect()` in `mail.rs` compared only the first 3
  bytes, so `* OK` / `a1 OK` replies were rejected; now a full-prefix
  match, with tests using a real Hostinger-style greeting.
- Pinned MCP clients (`mcp_client.rs` `new_pinned`) never use an
  environment proxy: a proxy would receive the credential and bypass the
  DNS pin, and made the pinned-connection test time out wherever a proxy
  was set (reproduced locally: `TimedOut` after 5s with a proxy,
  0.01s without).
- Windows-only clippy error in the new handwriting block
  (`ui/markdown.rs:956`): a `key` on `InlineMarkdown` is the second node
  in its `for` body, which Dioxus rejects as a deprecated `Warning`
  under `-D warnings` on the Windows target (Linux clippy stays silent);
  the key is dropped — the `br` above it is the first node and keeps
  its key.
- `gen` is a reserved keyword in Rust 2024 (`ui/chat.rs`); `Cargo.lock`
  0.74.3 sync; `cargo fmt`; clippy `nonminimal_bool` /
  `collapsible_match` / `too_many_arguments`; always-true test in
  `auth.rs`; LiveCard `PartialEq` / `ondismiss` fixes; mobile mail
  model/screen fixes.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace --locked`
- `cd apps/serabot_mobile && flutter analyze && flutter test`
- Not verified live: Windows WebView2 render pass, real Hostinger
  SMTP/IMAP login via desktop/mobile cards, real OneDrive, WhatsApp
  pairing, mobile Message-matches jump against a real 0.74.2 server.
