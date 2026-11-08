# SeraBot 0.24.0

## Local folders
- Each folder you allow on your PC now has its own bot access. In Settings › Local folders, pick "All bots" or "Only these bots" and tick the bots that may use it. Titles are shown; the server only receives bot names. A folder you allowed before this release stays open to every bot, as it was.
- The server refuses a folder to any bot it is not shared with, before anything reaches your PC: "Folder 'docs' is not shared with analyst; the owner can share it in Settings › Local folders". Listings and hints show only the folders a bot may use.
- Every shipped bot can now read local folders (`local_list`, `local_search`, `local_stat`, `local_read`, `local_import`). Which folders each bot sees is decided by the grants above. Writing and exporting stay with Main, behind your approval, as before. Existing bots gain the new tools on the next fleet upgrade; your prompts are kept.
- Bulk reading that works on big folders:
  - `local_brief` gives an overview of a folder tree from names, sizes and dates only: counts and sizes per top folder, file types, newest and oldest dates, version series (for example Budget v1 to v6) and copies with the same name and size. No file is read.
  - `local_read_many` reads up to 50 text files in one call, or every text file matching a pattern such as `*.md`. Each file is capped at 64 KiB by default and the call at 1 MiB in total. Documents and binaries are listed as skipped, with a pointer to the right tool.
  - `local_extract` reads PDF, Word, PowerPoint (with speaker notes), Excel, CSV, Markdown and text files, up to 20 per call, with page, slide and sheet markers. Each file is copied into your server inbox first, the same copy `local_import` makes, so the document tools can use it afterwards. A broken file is reported and the rest still run. Images are not read as text: import them and use `image_inspect`.
- Bots are told that folders you shared are theirs to read, that they should work through large sets in batches and report progress, and that they must not copy credentials, ID numbers or bank details into memory.

## Security fix: inbox paths stay in your own area
- Every inbox path is resolved inside your own inbox folder (`<inbox>/<your id>/`). A path that names another user's file is now "not found", and a symlink or junction that points out of your folder is not followed. This covers the PDF tools, the Word, PowerPoint and Excel imports, `xlsx_analyze`, `document_index` and the image tools. One shared check now does this for all of them.
- Paths returned by `local_import`, `local_extract` and Telegram uploads already start with your id, so they keep working.

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
