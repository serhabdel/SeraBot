# Release 0.74.10

Patch batch on 0.74: the **chat, composer and core** release — the chat view
stops losing your place and stops lagging while a reply streams, the
composer catches mobile up with desktop and stops silently losing work, and
the core stops forgetting long conversations, stops replaying outbound
effects, and tells "slow" from "dead". No minor bump, no full token
streaming, no Memory v2, no LSP, no tool-packs — those stay 0.75.

## Chat view (desktop)

- **Scroll restore that holds.** First load no longer force-pins to the
  bottom when the bridge has a saved non-following position; the feed window
  with no anchor keeps its last known start instead of jumping to the tail;
  the bridge restore wait is uncapped while history is still loading (it
  used to give up after 180 frames and land at the top).
- **Streaming no longer costs O(transcript) per token.** The transcript
  fingerprint now hashes only count + head id + tail (id, len) + ledger
  revision + an activity digest; search haystacks are cached per row id;
  library assignment runs behind the row-cache hit; while streaming, the
  markdown parser bypasses the parse cache entirely and `tokenize` is
  memoised on (language, code-hash); the soft-pin eval is skipped while
  following.
- **Delegating card works.** It is derived from the handoff ledger
  (`runs::active_handoff_bots` — started, not yet completed) instead of
  `WorkingBots`, so "Handed off to Analyst and 2 more" shows parallel
  handoffs with a count; handoff queued/started/resumed events also reach
  the RunBoard again.
- **Needs review is a real state.** New desktop `TaskStatus::NeedsReview`
  (non-terminal, never auto-retried) with its own LiveCard state — "Something
  may have happened — check the result before trying again" — instead of a
  spinner that never stops when the server parks a task in `needs_review`.
  Server test: an uncertain final failure goes to `needs_review`, a plain
  one fails.
- **Streaming markdown stops morphing.** `MarkdownBody` takes a
  `streaming` flag: a trailing unclosed fence / unclosed `:::hand` / table
  header without its separator renders as plain text with the fence hidden
  while streaming, and the code Copy button is hidden until the reply is
  complete.
- **Second wave:** search next/prev (Enter / Shift+Enter, prev/next buttons)
  jumps to the match and sets the feed anchor; a thin context meter now sits
  above the composer in chat, driven by the turn ledger (child turns are
  already ignored there).

## Composer (desktop + mobile)

- **Mobile catches up.** Sending while a reply runs queues the message
  instead of refusing it (queued chips with delete, pump on finish); a
  compact `@` bot picker and `/` command picker; the composer restores its
  text from `chat.draft` on build; `textInputAction.send` + `onSubmitted`.
- **Files survive switching chats.** Composer files, pending uploads and the
  held draft are stashed per conversation and restored on return, like text
  drafts and the send queue already were. A quit while still focused with an
  unsaved draft still loses it (no Drop-time sync persist).
- **Enter stops dying.** With the `@`/`/` picker open and zero matches,
  Enter falls through to a normal send instead of doing nothing; the mention
  query now requires `@` at the start or after whitespace, accepts
  uppercase, and works mid-sentence (`a@b` never opens the picker).
- **Send failures explain themselves.** `submit()` toasts a reason on every
  guard — "Still uploading…", "Waiting for connection", "Message is over
  the limit", "Type a message first." — instead of a silent no-op.
- **Drafts persist.** Drafts and the queue are saved to
  `composer_drafts.json` in the settings dir (text only, secret-bearing
  drafts dropped, 32 KiB / 64-conversation caps, atomic tmp+rename) and
  restored on launch; mobile persists per-chat drafts the same way
  (`serabot_drafts.json`, debounced).
- **Voice works.** Cancel button + elapsed timer, the transcribing state is
  rendered, and a failed transcription keeps the audio bytes for one retry.
- **More:** Up/Down cycles through sent-message history; queue accepts
  already-uploaded files (still refuses while an upload is in flight);
  queued items show a file chip; over-limit gets an "Attach as file" action
  (mobile: counter from 7,200 chars + one-time toast at 8,000); pickers carry
  `listbox` / `aria-activedescendant` roles; paste/drop are gated on
  `accepts` with a JS size pre-check; the composer auto-grows via a
  `scrollHeight` fallback when `field-sizing` is unsupported.
- **Server cap confirmed, not added:** the 8,000-char cap already exists in
  `runtime/routing.rs` (`submit_text_attached` → `Validation("message is too
  long")`, surfaced via `SAFE_VALIDATION_TEXTS`), so no server change was
  needed for it.

## Core (server)

- **Compaction stops destroying history.** Summaries are stored regardless
  of folded Tool messages (already redacted + policy-checked); the folded
  range is summarised in chunks (≤24k-char greedy chunks, max 4, tail-merge
  on overflow) instead of tail-truncating to the last 24k chars; the folded
  boundary is resolved from the messages table instead of copying the
  previous summary's `through` id; a compaction that cannot shrink the
  prompt fails the turn at once with a user-facing "too long" message
  instead of burning retries. Stored-summary cap 800 → 2,000 chars;
  `FACTS_MAX_CHARS` 1,200 → 2,500.
- **Heartbeat, not streaming.** While a model call is in flight, a
  `turn.progress` event (`phase: "waiting_model"`, elapsed, beat #) is
  emitted every 15 s (max 40 beats) on the existing SSE feed, so the client
  can tell alive-but-slow from dead. Full token streaming stays 0.75.
- **Duplicate-send guard.** The task is marked uncertain before *every*
  outbound-effect tool call (direct and standing-grant paths, not just the
  approved path) and cleared on a known outcome, so `fail_task` routes an
  ambiguous outcome to `needs_review` instead of silently requeueing;
  completed effectful calls are recorded in persisted turn state and skipped
  on retry; state is persisted after each effecting call; a whole call batch
  is validated before any of it executes.
- **Tool digest between turns.** Each turn persists a bounded per-turn tool
  digest as one Tool-role message (invisible in chat) that
  `history_for_llm` carries, so the next turn can use what the previous one
  found; history is now also capped by estimated chars
  (`history_max_chars`, default 60,000) as well as by the 50-message count.
- **Honest context meter + failover window.** `turn.context` is emitted
  once after `state.messages` is fully built and re-emitted each round, with
  the 100% clamp removed; the estimator now counts tool-call names +
  arguments, reasoning signatures, and 1,000 tokens per inline image;
  `near_window` runs against the `context_selection` window; on
  `ContextTooLong` the remaining candidates with a strictly larger known
  window are tried before failing.
- **Also:** expired approvals fail/resume the parked turn with a
  plain-language notice ("That approval expired before it was answered, so
  nothing was sent. Ask again if you still want it."); `ask_teammate` writes
  a handoff ledger row (deduped, counted against the child quota) and child
  failures are appended as a one-line summary to the parent conversation;
  up to 2 pinned memories are always recalled outside `recall_k`.

## Deliberately not in 0.74.10

Full token streaming, Memory v2, LSP, tool-packs, true parent/child
join/synthesis, ink handwriting input. The unified approval UI was also
left out (it needs `ui_cards.rs` plumbing); the failed card already uses
the fixed-vocabulary reason chips.

## Verification

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --
  -D warnings`, `cargo check --workspace --all-targets --locked`,
  `cargo check -p serabot-desktop --features ui-check --locked`,
  `cargo test --workspace --locked` (`SERABOT_TEST_MODE=1`) — clean (CI).
- Windows-target clippy for `serabot-desktop` — clean (CI).
- `flutter analyze` / `flutter test` — clean (CI).
- New tests: chat slice (scroll restore, fingerprint/haystack, feed window,
  handoff bots, `needs_review` round-trip + `fail_task_final`, streaming
  markdown), composer slice (queue-with-files, history cycling,
  files-per-conversation, persisted round-trip incl. secret dropping,
  submit-reason toasts, transcription retry, mention boundary/case,
  paste/drop gating, mobile composer queue/pickers/drafts), core slice
  (chunked summarisation, folded boundary, pinned recall, history char cap,
  estimator counts, direct-dispatch uncertain flag, ask_teammate dedup).
- Not verified live: Windows WebView2 render pass (scroll restore, LiveCard
  states, composer), real long conversations against a running 0.74.10
  server (compaction/heartbeat/digest), mobile APK build (no Android SDK
  here — CI covers it), server release build (CI ubuntu step).
