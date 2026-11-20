# UX report: Chat window (desktop console)

**Intent:** Review / audit · **Stack:** Dioxus 0.7 Desktop (WebView2); content is web (CSS in `crates/serabot-desktop/assets/`), shell is Windows · **Form factor:** desktop · **Register:** operate · **Depth:** `better-interface` full, with `laws-of-ux` for the Why, `design-review` contrast script for numbers · **Skills run:** definitive-ux → stack-map (Dioxus/web map), better-interface, laws-of-ux, design-review (contrast only)
**Scope:** `src/ui/chat.rs`, `src/ui/shell.rs` (`ActivityRow`), `src/activity.rs`, `src/ui/wait_cards.rs`, `assets/app.css`, `foundation.css`, `settings.css`, `system.css`; backend `runtime/recall.rs`, `extract.rs`, `compaction.rs`, `memory.rs`.
**Assumed primary task and user:** an invited member asks a bot for something, watches it work (tools, approvals), then reads and acts on the reply.
**Not verified:** no render was taken (Windows WebView2 is not available in this Linux container). Findings come from the source and the computed cascade, not from a screenshot.

**Verdict:** Ship with polish list. Nothing is broken, but two structural gaps matter more than any pixel: tool steps are not tied to the reply they produced, and memory is invisible during a turn.

## Important

### 1. Tool steps float at the bottom of the feed and are lost after the next send
- Evidence: `src/activity.rs:188` ("Steps of the most recent turn. Cleared when the owner sends again"); `src/ui/chat.rs` renders `activity` / `details.activity.done` after every message, before `#messages-end`, never inside a `MessageRow`.
- Why: Common Region / Proximity. The summary "N steps" is not visually owned by the reply it explains. Scrolling back, older replies have no trace of what tools they ran, so "why did it say that?" can't be answered from the transcript.
- Fix: persist the step summary per task (the server already has `task_id` on every `turn.step`), attach it to the bot message with the same `task_id`, and render a collapsed `details.activity.done` *inside* that `MessageRow`, above `.msg-body`. Keep the live block at the bottom only while the turn runs.

### 2. Memory is invisible during a turn
- Evidence: `runtime/recall.rs` injects recalled facts and `runtime/extract.rs` stores new ones, but neither emits an event (the only turn events are `turn.step|completed|compacted|continued_after_limit|limit_reached|refused|stopped`). The chat shows memory only as the new-chat divider (`src/ui/chat.rs:714`).
- Why: Visibility of system status; trust. The person can't see that a fact shaped the reply, or that something they said was just remembered, so they can't correct a wrong memory at the moment it matters.
- Fix: emit `turn.memory` with `{recalled: [{id, excerpt}], saved: [{id, excerpt, origin}]}` (bounded, blocklist-filtered, UID-scoped like the rest), then render a quiet chip row under the reply: "Used 3 memories · Remembered 1" → opens the facts with Forget / Edit, reusing `ui/memory.rs`.

### 3. The text-size preference doesn't reach most chat chrome
- Evidence: `settings.css:50-69` scales `--fs-*` tokens, but `foundation.css` hard-codes the speaker line and chips: `.msg-name { font-size: 13px }`, `.msg-time { 11.5px }`, `.msg-head .chip { 11px }`, `.msg-avatar.avatar { 12px }`; `.msg-action` is `12px` (`app.css` ~6380). 76 hard-coded `font-size: Npx` rules remain across `app.css`, `foundation.css`, `system.css`.
- Why: an accessibility setting that half-works. In Large mode the body grows to 17px while names and times stay at 11–13px, so the hierarchy inverts unevenly.
- Fix: replace them with tokens (`.msg-name → var(--fs-caption)` with weight 650, `.msg-time / chips → var(--fs-micro)`, `.msg-action → var(--fs-micro)`). Also raise Small's `--fs-micro` from 10px to 11px: 10px is below a readable floor on Windows ClearType.

### 4. Three files fight over the same chat selectors
- Evidence: `.msg` at `app.css:1238`, `app.css:3299`, `foundation.css:148`, `foundation.css:1856`; `.msg-name/.msg-time` in both `app.css:1288` and `foundation.css:1913`; `.msg-main` gap is 4px in app.css and 2px in foundation.css; `.msg-body p` margin is 10px and then 8px.
- Why: load order (`src/ui.rs:85-98`), not intent, decides the result, and every polish pass adds another layer.
- Fix: run `analyze-components` / `spacing-audit` on the chat block and fold the winning values into one place (`foundation.css`), deleting the losers in `app.css`.

### 5. Reply measure: the 66ch cap is overridden
- Evidence: `app.css:1300` sets `.msg-body { max-width: 66ch }` and explains why; `foundation.css` `.msg.bot .msg-body { max-width: 100% }` wins on specificity. The transcript column is `max-width: 760px` (`app.css:3294`), which is about 95–100 characters at 15px Segoe UI.
- Why: long answers lose the return sweep. Your own comment states the rule.
- Fix: `.msg.bot .msg-body { max-width: 72ch; }` (tables and code blocks can opt out with `max-width: none`).

## Polish

### 6. Message actions are hidden until hover, except on the last reply
- Evidence: `.msg-actions { opacity: 0 }` (`app.css:6355`); `.msg.last-reply .msg-actions { opacity: 1 }`.
- Why: Discoverability vs. calm. This is acceptable on desktop, and the keyboard toolbar (`role="toolbar"`, arrow keys) is well done. But Copy is the most common action on older replies too. Consider `opacity: .45` at rest for Copy only.

### 7. Two status words per running step
- Evidence: `ActivityRow` shows a spinner plus `step-time` "Running" (`src/ui/shell.rs` ~1155) next to the title; Progress becomes "Still working" only in the aria label.
- Fix: show elapsed time while running ("4s") instead of the word, so a long tool call visibly ages (Doherty threshold: after about 1s, show progress).

### 8. Transparency: keep it on the topbar only
- Evidence: the composer is correctly opaque (`app.css:1376` comment), and the fade above it is a gradient to `--bg`. `backdrop-filter: blur(24px)` still appears 36× in `app.css` and 19× in `system.css`; system.css undoes some of it ("crisp non-blurred chrome").
- Why: blur over moving text costs GPU on WebView2 and lowers legibility. You've already chosen crisp. Finish the job: delete the dead `backdrop-filter` rules that system.css overrides, instead of stacking `none` on top.

### 9. Composer fade can hide the last line during streaming
- Evidence: `.composer-wrap::before` overlays 36px of gradient at `top: -32px`.
- Fix: add matching `padding-bottom` to `.messages` (≥ 36px) so the final streamed line never sits under the fade.

## Keep (these are good)
- Typeface: `Segoe UI Variable Text` first, `Cascadia Code` for mono. That's the right native pair on Windows 11.
- Contrast (computed with the bundled script): `--label-3` on `--bg` is 6.97:1 in dark and 5.98:1 in light; `--label-2` is 10.86:1. All pass AA.
- Body 15px / line-height 1.6–1.65; user bubbles at `min(580px, 82%)` with a sender-side corner.
- Approval inside the step (Approve / Deny, Once only, never "always"), with failed steps auto-opening the summary so they can't be hidden.
- Bounded step data (`MAX_STEPS 64`, text caps, control-char stripping) and polite live regions with one announcement per reply.
- Virtualized feed with a fingerprinted row cache; Jump-to-latest pill with an unseen count and a live dot.
- Reduced-motion rules are present in app, foundation, system and runs CSS.

## Agentic system notes (backend, as seen from the chat)
- The loop (`runtime/turn_loop.rs`) has bounded tool rounds and calls per turn, compaction at about 75% of the window with at most N compactions, and "Continue after limit". The chat surfaces compaction (`CompactionNote`) and the limit (Continue button). Good.
- Missing: a per-turn context meter. You already know the token estimate when deciding to compact, so a thin bar in the model bar ("context 62%") would explain compaction before it happens.
- Plan (`chat_plan.rs`) and steps are separate surfaces. If finding 1 is done, nest plan items above the step list in the same reply-owned block.

## Rig and AutoAgents: do we use them, do we need them?
- **Used today:** no. `Cargo.lock` has neither `rig-core` nor `autoagents*`. SeraBot has its own transports (`wire/anthropic.rs`, `wire/responses.rs`, SSE), route chain with failover, compaction, recall and the gateway.
- **Rig** (rig.rs; the site and docs.rs were unreachable from this session, so this is from general knowledge, not a fresh read): a Rust LLM-application library with provider clients, agents with tools, embeddings and vector-store integrations. It's pre-1.0 with frequent breaking changes.
- **AutoAgents** (README read): a multi-agent framework with ReAct executors, derive-macro tools, a WASM tool sandbox, sliding-window memory, Qdrant only, typed pub/sub between agents, and optional Python bindings and local LLM backends. It's MIT/Apache-2.0, about 790 stars, and shows no release version on the page.
- **Recommendation: do not adopt either.** Both own the tool-call loop, and AGENTS.md requires every tool execution to go through `gateway` (approvals, policy scan, audit, approval binding). Wrapping a framework's executor to keep those guarantees costs more than the code it replaces. AutoAgents' agent-to-agent pub/sub and local LLM backends are explicitly out of the product boundary. What's worth borrowing as ideas only: Rig's provider-trait shape if more providers are added, and AutoAgents' guardrail policy names (Block / Sanitize / Audit) for the existing policy scanner.

## Not done
- The X article (`x.com/manthanguptaa/article/2117992940610138380`) couldn't be fetched: x.com doesn't resolve from this session. Paste its text and I'll map its points onto this report.

## Next step
Highest leverage: finding 1 plus 2 together. Make each bot reply own its "what I did" block (steps + memories used/saved). That's one server event, one per-message store, and one component.

---

## 0.61.0 status

Findings 1–5 are implemented (see `docs/release-0.61.0.md`). Merging the
chat styles turned up one more problem, now fixed: in the light theme, user
bubbles had white text on a pale accent tint, at 1.41:1 contrast (it is now
12.25:1).

## The Grok Bot memory article, compared with SeraBot

| The article's point | SeraBot today |
| --- | --- |
| One memory pool shared by all of a user's bots, with a private scope per bot | Same shape. Memories are per user, every bot of that user recalls them, and a fact from the same bot gets a small boost. There is no private per-bot scope yet. |
| Every note tagged with its date and the bot that saved it | **Added in 0.61** (`saved YYYY-MM-DD`, `via <bot>`). |
| Limits applied when notes are read (500 characters per note, a cap on how many fit) | Same kind of limit: 300 characters per fact and a 1,200-character section. Memory refresh runs consolidation, but there is no write-time budget like Hermes has. |
| Keyword-only recall misses paraphrases | **Same gap.** Recall is FTS5 BM25 plus importance and recency, and only the optional Mem0 adds semantic recall. Next candidate: embedding recall from `embeddings.rs` combined with BM25. |
| Writes only ever append, so contradicting notes pile up | Partly handled. A near-duplicate (token Jaccard ≥ 0.8) supersedes the older note, but a rewritten decision ("hard rule" becoming "optional") is not caught. The new dates let the model pick the newer note. A contradiction check at write time is still open. |
| Team writes go through a review queue | Similar. Facts extracted from a turn that read untrusted content are held as `pending` until the owner approves them. |
| No way to list everything | Already covered: the Memory page and `/v1/memory` list everything for the acting user. |

## 0.71.0 audit (chat + Usage + chat switcher)

Source-level, no render (WebView2 unavailable in the container); every change
preserves the computed cascade of the rules it replaces.

- **Duplicate chat selectors consolidated.** `.messages` (2 definitions),
  `.composer-wrap` (2) and `.composer-wrap::before` (3) in `app.css` were
  decided by load order; each now has one definition in the chat transcript
  block at the end of `system.css`. `.msg-actions` had competing rules in two
  `system.css` blocks (`opacity: 0` vs `0.72`); one rule now owns it
  (in-flow, 0.72 at rest, full on hover/focus/last reply, visible on touch).
- **Palette hard-coded px → `--fs-*` tokens** (the last fully hard-coded
  component); dead `backdrop-filter` blur removed for `.menu`,
  `.connect-card`, `.composer` (already `none` in `system.css`).
- **Chat switcher card:** focus goes to the open chat on open (not row 0);
  menu capped at `min(420px, 60vh)` with scroll; the overflow-flip JS resets
  inline offsets before flipping (idempotent, no visible jump); rename input
  is uncontrolled so keystrokes never move the caret.
- **Usage page:** `usage-tile*` leftovers replaced by the shared `stat-tile`;
  section anchor chips + "Updated Ns ago" in the page head; day labels under
  the tokens-per-day bars.

Not done: a visual pass on a Windows device (owner), and the sidebar rename
input has the same controlled-`value` caret pattern as the switcher had.
