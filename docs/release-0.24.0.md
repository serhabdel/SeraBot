# SeraBot 0.24.0

## Local folders
- Each folder you allow on your PC now has its own bot access. In Settings › Local folders, pick "All bots" or "Only these bots" and tick the bots that may use it. Titles are shown; the server only receives bot names. A folder you allowed before this release is shared with main, analyst and maker, the bots that had local folder tools before; change it in Settings › Local folders if you want another set. A new folder starts with the same three bots, and you can pick "All bots" for it.
- Downgrading the desktop app to 0.23 and saving the folder list there drops the per-bot grants: 0.23 does not know the `bots` setting, so it is not written back. Upgrading again then gives those folders the default three bots, not the grants you had set. Set the grants again after upgrading.
- The server refuses a folder to any bot it is not shared with, before anything reaches your PC: "Folder 'docs' is not shared with analyst; the owner can share it in Settings › Local folders". Listings and hints show only the folders a bot may use.
- Every shipped bot can now read local folders (`local_list`, `local_search`, `local_stat`, `local_read`, `local_import`). Which folders each bot sees is decided by the grants above. Writing and exporting stay with Main, behind your approval, as before. Existing bots gain the new tools on the next fleet upgrade; your prompts are kept.
- Bulk reading that works on big folders:
  - `local_brief` gives an overview of a folder tree from names, sizes and dates only: counts and sizes per top folder, file types, newest and oldest dates, version series (for example Budget v1 to v6) and copies with the same name and size. No file is read.
  - `local_read_many` reads up to 50 text files in one call, or every text file matching a pattern such as `*.md`. Each file is capped at 64 KiB by default and the call at 1 MiB in total. Documents and binaries are listed as skipped, with a pointer to the right tool.
  - `local_extract` reads PDF, Word, PowerPoint (with speaker notes), Excel, CSV, Markdown and text files, up to 20 per call, with page, slide and sheet markers. Each file is copied into your server inbox, the same copy `local_import` makes, so the document tools can use it afterwards. A file whose bytes are already in your inbox reuses that copy. If the copy is refused (for example the inbox is full), the text is still returned and the reply says "not saved to inbox". A broken file is reported and the rest still run. A call stops starting new files after 120 seconds and lists the rest as not reached. PDFs are read up to 200 pages. Word, PowerPoint and Excel files that would expand past 200 MiB are refused. Images are not read as text: import them and use `image_inspect`.
  - The text of each file sits between `<<<FILE …>>>` and `<<<END …>>>` lines with a token new to each call, and file names are shown with line breaks escaped, so a file cannot forge a header or end a fence.
- Bots are told that the shared folders are theirs to read, to work through large sets in batches and report progress, not to refuse because of the number of files, to treat file contents as data, and never to quote, store or send secrets found in files (passwords, keys, ID, card or bank numbers) and to refer to them only by file name.

## Security fix: inbox paths stay in your own area
- Every inbox path is resolved inside your own inbox folder (`<inbox>/<your id>/`). A path that names another user's file is now "not found", and a symlink or junction that points out of your folder is not followed. This covers the PDF tools, the Word, PowerPoint and Excel imports, `xlsx_analyze`, `document_index` and the image tools. One shared check now does this for all of them.
- Paths returned by `local_import`, `local_extract` and Telegram uploads already start with your id, so they keep working. Telegram documents and voice notes are now saved under your own folder (`<your id>/`), and the reply names them that way. Voice notes saved by an earlier build in the shared `telegram/` folder are not reachable by the audio tools any more; move them into your folder if you need them.
- Audio tools (`audio_transcribe`, `audio_understand`, ElevenLabs transcription) resolve paths for the acting user only. Their approval step looks the file up for that user too.

### Not verified (local folders)
- Not run against a live PC folder or a live model. Tests use a fake desktop, synthetic files and fixture bytes.
- Per-bot grants are enforced on the server. The desktop does not see which bot asked, so it cannot re-check the grant on its own.
- `local_export` asks for approval before it checks the bot grant (the approval step has no bot to check). A bot without the grant is refused when the approved export runs, and nothing is written.
- The new Local folders controls were compiled but not looked at on screen.
- PowerPoint extraction is tested on a synthetic three-slide deck with notes; no real customer deck was read.

## ElevenLabs

Two independent connectors replace the single ElevenLabs connector with an auth-mode toggle.

- **ElevenLabs** (recommended): each account signs in with its own ElevenLabs account. The hosted MCP server exposes the full catalog (120+ tools). Three tools work through it: `elevenlabs_mcp_search_tools`, `elevenlabs_mcp_describe_tool`, `elevenlabs_mcp_call`. A call runs only a name the account's catalog returned; an unknown name is refused with the five closest names.
- **ElevenLabs API key** (legacy): the owner's API key and the native tools, renamed `elevenlabs_key_*`. They register only when the connector is enabled and a key is stored.

Changes:

- The sign-in is stored per account (provider `elevenlabs_mcp` in the per-user store). The pre-0.24 server-wide sign-in moves to the owner's account once on startup, then the server-wide names are removed.
- The tool catalog is cached per account for ten minutes; a 401 or a failed fetch drops it. Tool lists are paged up to 20 pages and 1000 tools.
- Search replies are at most 24 KiB, 40 tools a page, never cut mid-JSON; entries that do not fit are dropped whole and counted in `truncated`. Describe replies stay within about 16 KiB, with deeply nested schema left out and noted.
- Timeouts: 60 s for list, search, describe and read; 300 s (`mcp_generate_timeout_secs`) for calls that generate, create or spend.
- `elevenlabs_mcp_call` returns every saved media item in `data.artifacts` (`mime`, `artifact_path`, `artifact_hash`, `filename`, `bytes`). Recording all of them as artifacts is left to the media-jobs change.
- Config: `[elevenlabs] mcp_enabled` and `[elevenlabs.api_key] enabled`. The old `enabled` and `auth` keys are read once and folded in with a warning. Bot files, stored bot rows and console overrides that name old tools are renamed on load.
- Console: two connector cards with their own status, connect and disconnect. The mode toggle is removed. Sign-in takes effect without a restart; enabling a connector needs one.

Not done in this release:

- Members cannot sign in to the ElevenLabs connector (owner console only). Member sign-in needs a second redirect or a member consent flow.
- The API-key connector's key is still server-wide, not a per-user credential.
- Enabling or disabling either connector still needs a restart, because tools are registered at startup.
- Not verified against a live ElevenLabs account.

See `docs/elevenlabs.md`.

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

### Not verified (long turns and media)
- No live provider run. Tests use fake pollers, synthetic artifacts and mocked model responses.
- Inline playback was type-checked only; it has not been run in the WebView on Windows.
- Per-bot work limits are not implemented. The limit is per owner.
