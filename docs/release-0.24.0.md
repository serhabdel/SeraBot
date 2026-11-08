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

## Not verified
- Not run against a live PC folder or a live model. Tests use a fake desktop, synthetic files and fixture bytes.
- Per-bot grants are enforced on the server. The desktop does not see which bot asked, so it cannot re-check the grant on its own.
- `local_export` asks for approval before it checks the bot grant (the approval step has no bot to check). A bot without the grant is refused when the approved export runs, and nothing is written.
- The new Local folders controls were compiled but not looked at on screen.
- PowerPoint extraction is tested on a synthetic three-slide deck with notes; no real customer deck was read.
