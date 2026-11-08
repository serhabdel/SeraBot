# SeraBot 0.24.0

## Long turns and media

### Bot work limits
- A bot turn may use up to **150 tool calls** and **60 tool rounds** by default (was 12 and 10). The server ceiling is **500 calls** and **200 rounds**; no setting goes above it.
- You can change both in Settings, under Chat, in the "Bot work limits" card. Each value must be a whole number from 1 to the ceiling. "Reset to default" removes your override.
- The API is `GET`, `PUT` and `DELETE /v1/settings/turn-limits`. Every change is audited (`settings.turn_limits_updated`, `settings.turn_limits_reset`) with the value it replaced.
- Resolution order: your override, otherwise the config default (`[runtime] max_tool_calls_per_turn`, and the higher of `[runtime] max_tool_iterations` and `[llm] max_tool_rounds`). The result is clamped to the ceiling.

### When a turn reaches its limit
- The bot no longer ends with a bare "tool limit" line. It makes one final model call **without tools**, with a note telling it to answer from what it has gathered and to say what it did not get to. That answer is the reply.
- If that call fails or returns no text, the reply lists the tool steps that ran (names and a short target) and the calls that were not run.
- The turn is stored as `limit_reached`. The reply shows a **Continue** button, which sends "Continue from where you stopped." as your next message. The bot sees the earlier turn in history, and the new turn has a fresh budget.
- The "N steps" activity list stays visible on the limited turn.
- The events `turn.limit_reached` and `turn.completed` (with `limit_reached: true`) are emitted, and the audit ledger records the limit kind (`tool_calls` or `tool_rounds`).

### Long tasks keep their lease
- A running task renews its lease every third of `[runtime] task_lease_secs` (300 s, so every 100 s). Before, a turn that ran past the lease could be requeued while still running, which could repeat external work. The renewal uses the task's fencing token, so a reclaimed task cannot be kept alive by the worker that lost it.

### Several files from one tool call
- A tool can return more than one artifact. Put the extra files under `data.extra_artifacts` as `{hash, filename, title}` (see `EXTRA_ARTIFACTS_KEY` in `gateway.rs`). Each is checked like the primary file (a valid SHA-256 hash, the blocklist on its name) and recorded once in the conversation Library.

### Background media jobs (infrastructure)
- A provider render that takes minutes can run as a **media job** (`crates/serabot/src/media_jobs.rs`, migration `0029_media_jobs.sql`). A job is created only after the provider accepted the work. The server then polls it every 10 seconds, through the poller registered for its provider name, until it finishes or 20 minutes pass.
- A finished job saves its file to your artifact store, records it in the conversation Library, and posts "Your <kind> is ready." A failed job posts "Your <kind> could not be made."; one that times out posts a note saying so.
- A job is **never submitted again**. A submission whose outcome is unknown creates no job, so it is reported rather than retried.
- The chat shows a "Generating a video…" (or image, or audio) placeholder while a job is pending. It refreshes on `media_job.created` and `media_job.updated` events. The owner's jobs come from `GET /v1/conversations/{id}/media-jobs`.
- Video and audio files play inline in the Library cards. A file is read back only when you press Play, and the player refuses files above 40 MB; those still open and save as before.
- No ElevenLabs job uses this yet. The ElevenLabs integration can plug in later with `register_media_poller` and `Store::create_media_job`.

## Not verified
- No live provider run. Tests use fake pollers, synthetic artifacts and mocked model responses.
- Inline playback was type-checked only; it has not been run in the WebView on Windows.
- Per-bot work limits are not implemented. The limit is per owner.
