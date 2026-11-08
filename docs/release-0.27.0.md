# SeraBot 0.27.0: a harness that finishes the job

## Autonomy and reliability
- Every bot, on every turn (owner, handoff and team), now gets shared
  autonomy rules: finish the task end to end, retry transient tool failures
  (up to 3 attempts per step, never a denied approval), state assumptions
  instead of asking, and find files and hashes itself. See
  `crates/serabot/src/runtime/prompts.rs` (`AUTONOMY_RULES`).

## Find files yourself — `conversation_library`
- New read-only tool on every bot (0.27): lists the conversation's artifacts
  with hash and filename, background media jobs with status and result hash,
  and files written to the owner's PC (`local:<root>/<path>`). Bots use it
  instead of asking the owner for a hash, a path, or a job id.

## Media generation: see every variant
- A background ElevenLabs job that finishes now posts one ready line naming
  **every variant with its artifact hash** (`Your image is ready: 2 variants —
  a.png (<hash>), b.png (<hash>).`), instead of a bare "is ready".
- The MCP poller delivers all files a generation produced, not just the first.
- Bots send each variant to the owner's chat with `telegram_send_document`
  (still approval-gated).

## Every bot can deliver to Telegram
- `telegram_send_document` is granted to every bot (shipped fleets and stored
  configs alike), and is listed in every `bots/*.toml`. It remains in the
  global destructive list, so every send still requires the owner's approval.

## Parallel subagents
- `main` may now queue up to 8 handoffs per turn; several `delegate` calls in
  one round fan out to parallel specialist subagents, and the turn reply
  names them all (`Queued handoffs to analyst, maker.`).

## Tests

Synthetic fixtures only: `scripts\cargo-win.bat test --workspace --locked`.
