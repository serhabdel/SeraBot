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

## Not verified
- Not run against a live PC folder or a live model. Tests use a fake desktop, synthetic files and fixture bytes.
- Per-bot grants are enforced on the server. The desktop does not see which bot asked, so it cannot re-check the grant on its own.
- `local_export` asks for approval before it checks the bot grant (the approval step has no bot to check). A bot without the grant is refused when the approved export runs, and nothing is written.
- The new Local folders controls were compiled but not looked at on screen.
- PowerPoint extraction is tested on a synthetic three-slide deck with notes; no real customer deck was read.
