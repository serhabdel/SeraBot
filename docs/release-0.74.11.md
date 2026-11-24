# Release 0.74.11

Patch batch on 0.74 wiring `0.74.10`'s server heartbeat and `needs_review`
safeties across Desktop and Mobile, connecting multi-account native IMAP
mailboxes to Morning Brief and Daily Email Triage, unifying the desktop
Approval card, and making Library actionable on both Desktop and Android.

## Live heartbeat (`turn.progress`) & `needs_review` recovery (Desktop + Mobile)

- **Desktop (`working.rs`, `activity.rs`, `session.rs`, `ui/wait_cards.rs`, `ui/chat.rs`):**
  - Routed `turn.progress` SSE events into `WorkingBots` (refreshing `entry.seen`
    so long reasoning turns never hit `STALE_AFTER` falsely and updating
    `"Thinking… (<N>s)"`) and `TurnActivity` (`heartbeat_label()`: `"Still thinking · <N>s"`).
  - Added explicit recovery controls on `LiveCardData::NeedsReview`:
    **Mark reviewed** (acknowledges and clears the parked card without
    re-sending) and **Send again** (requires an explicit two-click `"Confirm resend?"`
    confirmation).
- **Mobile (`models.dart`, `chat_controller.dart`, `screens/chat_screen.dart`):**
  - Added `TaskStatus.needsReview` (`'needs_review'`, non-terminal) and wired
    `turn.progress` into the active working indicator (`"Thinking · <N>s"`).
  - Replaced infinite spinners on `needs_review` with an amber warning banner
    and **Dismiss / Reviewed** action.

## Morning Brief & Daily Email Triage for native multi-account IMAP (`brief/morning.rs`, `ops.rs`)

- **Morning Brief (`brief/morning.rs`):**
  - `fetch_feeds` now checks up to 5 configured native IMAP accounts per user
    (each bounded by a 10 s timeout, read-only `INBOX` headers, degrading
    silently on error/timeout) and merges last-24h subjects with Outlook into
    `MailPeek` (prefixing `[account]` when multiple sources are present).
- **Daily Email Triage (`ops.rs`):**
  - `submit_due_triage` now triggers when a member has **either** Outlook/Gmail
    connected **or** at least one native IMAP account (`mail::list_accounts`),
    and `TRIAGE_PROMPT` directs `inbox` to check native IMAP (`mail_list`) too.

## Unified Approval Card (Desktop — `ui_cards.rs`, `ui/approvals.rs`, `assets/system.css`)

- Introduced one shared `ApprovalCard` component (full + compact variants) used
  across both inline chat approvals and the Approvals page: bot avatar, tool
  category badge (`Mail`, `OneDrive`, `Browser`, `Project Check`, `Tool`),
  expiry countdown pill, destination chip, clickable artifact link, and
  **Always allow** strictly gated on `tool_allows_standing_grant` (never shown
  for destructive/vault/upload/`mail_send` tools).

## Library actionability (Desktop "Attach to chat" + Mobile Global Library)

- **Desktop (`ui_library.rs`):** Added **Attach to chat** on Library cards and
  item dialogs to attach existing artifacts straight to the active composer
  without re-uploading, plus clearer OneDrive mirror status feedback.
- **Mobile (`screens/library_screen.dart`, `api_client.dart`, `device.dart`, `home_screen.dart`):**
  - Added `getGlobalLibrary()` (`GET /v1/library`) and `uploadToLibrary()`
    (`POST /v1/library/upload`, 25 MiB cap via `file_picker`).
  - Added top-level **Library** tab in `HomeScreen` with **All** / **Made by bots**
    / **Uploaded** tabs, search, Grid/List toggle, authenticated thumbnails +
    `InteractiveViewer`, PPTX/XLSX/PDF previews, Share, and Upload FAB.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo clippy -p serabot-desktop --features ui-check --all-targets -- -D warnings`
- `cargo test -p serabot-desktop --locked`
- `cargo test -p serabot --lib --locked brief::morning ops::tests mail::tests`
- `cd apps/serabot_mobile && flutter analyze && flutter test` (239 passed)
- Not verified live: real Hostinger IMAP morning brief/triage run, Windows
  WebView2 visual pass on Unified ApprovalCard / Library Attach-to-chat /
  NeedsReview recovery, and Android APK on device.
