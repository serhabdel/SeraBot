# Release 0.61.0

Chat window release from the chat UX review (`docs/reviews/chat-window-ux.md`,
findings 1–5). Each reply now owns what its turn did, memory use is visible
in the chat, and the chat text is styled from one place with the text-size
setting applied throughout. Tests use synthetic fixtures only; nothing here
was verified with a render on Windows or against a live model.

## Each reply shows what its turn did (finding 1)

- Desktop `turn_trace.rs`: a bounded per-conversation ledger (64 turns) that
  groups `turn.step` and `turn.memory` events by `turn_id`. It is fed by the
  live stream and rebuilt after a load from
  `GET /v1/conversations/{id}/activity`, a route that already existed but that
  the desktop never read.
- `Message.turn_id` (already sent by the server) is now read. The last reply
  of each turn draws a collapsed "4 steps, 12s · Used 2 memories ·
  Remembered 1" line above its text. It opens by itself when a step failed or
  waits for approval.
- The live block at the foot of the feed stays while a turn runs. The old
  "N steps" summary at the bottom is only drawn when the reply has no turn id,
  for example on an older server.

## Memory is visible per turn (finding 2)

- New `turn.memory` event (`runtime/recall.rs`), sent once when the turn
  starts (`recalled`) and once after the post-turn extraction (`saved`, with
  `pending` set when the fact is held for review). It holds at most 8 items
  per side, and excerpts are capped at 120 characters and scanned like any
  outgoing text. A fact the scan rejects is listed with no text. It is
  user-scoped like every event, and nothing is sent when a turn used or saved
  no memory.
- `run_extraction` now returns the memories it stored.
- `conversation_steps` (behind `/activity`) returns `turn.memory` as well as
  `turn.step`.
- Injected fact lines now carry the date the fact was last saved and the bot
  that saved it (`[id:… | fact | extracted | saved 2026-11-04 | via research]`),
  so the model can prefer the newer of two facts that disagree. This was
  prompted by the multi-agent memory write-up the owner shared.
- The chat lists the facts with "you saved" and "held for review" markers,
  plus a "Review in Memory" button that opens the Memory page to edit or
  forget them.

## Text size reaches the chat (finding 3)

- Names, times, role chips, avatar letters and message actions now use the
  `--fs-*` tokens instead of fixed pixel sizes, so Small, Default and Large
  scale them with the message text.
- Small's smallest size is now 11px (it was 10px).

## One home for chat styles (finding 4)

- Chat message rules were defined in `app.css`, `foundation.css` and
  `system.css`, and load order decided the result. They are now one block at
  the end of `system.css`. The values are the ones that cascade produced,
  except for the fixes listed here. A test checks that each selector is
  defined once.
- **Bug fixed while merging:** user bubbles had white text (`--on-accent`) on
  the 14% accent tint added in 0.56. In the light theme that measured 1.41:1.
  Bubble text, links, inline code and file chips now use the normal label
  colours (12.25:1 light, 12.95:1 dark).
- The touch-screen rule that keeps message actions visible is kept after the
  merged block.

## Reply line length (finding 5)

- Replies are capped at 72ch. Before, a later rule set `max-width: 100%`,
  which gave lines of about 95–100 characters in the 760px column.

## Not adopted: Rig and AutoAgents

Both were evaluated. Neither is adopted, because both run their own tool loop
and every tool call here must go through the gateway (approvals, policy scan,
audit, approval binding). See the review for details.
