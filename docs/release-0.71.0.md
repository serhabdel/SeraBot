# Release 0.71.0

A UI/UX audit and consolidation pass on the desktop console, focused on the
chat window and the Usage page, plus the chat switcher in the chat header.
Source-level audit only (same method as `docs/reviews/chat-window-ux.md`):
no render was taken — Windows WebView2 is not available in the build
container — so every change preserves the computed cascade of the old rules.

## Chat stylesheet: one owner per rule

- `.messages`, `.composer-wrap` and `.composer-wrap::before` each had two or
  three definitions in `app.css` where load order silently decided the
  winner. They are now defined once, in the chat transcript block at the end
  of `assets/system.css`, with the values the old cascade produced
  (`.messages`: `gap: 6px; padding: 16px 18px 108px`; the composer fade stays
  off, so the 108px bottom padding is the only clearance for the last
  streamed line).
- `.msg-actions` had competing rules in the 0.60 polish block and the 0.61
  chat block (`opacity: 0` vs `0.72`, both in `system.css`). The single rule
  now lives in the chat block: in-flow, quiet at 0.72 at rest, full on
  hover/focus/last reply, always visible on touch (`hover: none`).
- The command palette (Ctrl+K) was the last fully hard-coded component: its
  15/13.5/12/11/13 px sizes are now on the `--fs-*` tokens, so the text-size
  preference reaches it.
- Dead `backdrop-filter` rules removed from `app.css` for the selectors
  `system.css` already keeps crisp (`.menu`, `.connect-card`, `.composer`).
  The remaining blur (glass surfaces, toasts, scrims, pops) is intentional
  and untouched.

## Chat switcher (chat header card)

- Opening the card now focuses the chat that is already open instead of the
  first row, so the list no longer scrolls away from where you are.
- The menu is capped (`max-height: min(420px, 60vh)`, scrollable) — with many
  chats it previously grew past the viewport and the overflow-flip script
  moved it after mount (a visible jump). That script now resets its inline
  offsets first, so re-runs are idempotent.
- Renaming a chat no longer resets the input's caret on every keystroke: the
  input is uncontrolled, its initial text is set once on mount.

## Usage page

- One tile system: the leftover `usage-tile*` classes (quota card, health
  items) now use the shared `stat-tile` classes; the old rules are deleted
  from `usage.css`.
- Anchor chips under the page head jump to each section (Quota, Reliability,
  Alerts & costs, Job search, Activity, Insights, Bot health, Breakdown — the
  ones that render), and the head shows "Updated Ns ago".
- The tokens-per-day chart gained a day-label row under the bars (every
  ~6th day plus the last), so the axis endpoints are no longer the only
  visible dates; bar tooltips are unchanged.

## Verification

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --locked`, and the desktop
  `cargo check -p serabot-desktop --features ui-check` gate (see CI).
- Not verified on a device: no screenshot was taken. Owner: look at the chat
  (light/dark, Small/Large text), open the chat switcher with several chats,
  and scroll the Usage page on Windows.
