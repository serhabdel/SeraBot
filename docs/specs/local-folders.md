# Local folders (desktop harness tools)

Status: 0.18 design; per-bot grants, bulk reading and the inbox ownership rule
land in 0.24. Minimal by intent.

## Goal

A user can let their bots read (and, if allowed, write) files in folders on
their own Windows PC, without any server-side filesystem access to that PC. The
default is off: an empty allowlist exposes nothing. Each folder is shared with
the bots the owner picks.

## Shape

```
bot tool local_list/read/read_many/extract/brief/write
   -> gateway (allowlist, approval, audit, blocklist)
   -> LocalRelay (server, in memory, per user; checks the calling bot's grant)
   -> desktop long-poll  POST /v1/local/poll  (authenticated as that user)
   -> desktop enforces its OWN allowlist again, runs the operation
   -> desktop POST /v1/local/result/{request_id}
   -> tool output (untrusted data)
```

The server never connects to the desktop; the desktop pulls. If no desktop
polled within the last 45 s the tools fail fast with "Your PC app is closed, so
I can't reach your folders" and a pointer to the owner.

## Allowlist and grants

- Stored only on the desktop (`local_folders.json` in the app data dir). Each
  entry: `{id, path, mode: "read" | "read_write", bots}`. `id` is a short slug
  the model sees (`docs`, `reports`); the model never sees or chooses absolute
  paths.
- `bots` says which of the owner's bots may use the folder:
  `{"mode":"all"}` or `{"mode":"only","bots":["analyst","main"]}`. The owner
  sets it per folder in Settings › Local folders, from a list of the enabled
  bots (their titles are shown, the bot names are sent). A folder with no
  `bots` key (saved before 0.24) reads as `only` with `main`, `analyst` and
  `maker`: the bots that had local folder tools before 0.24, never as `all`.
  A new folder starts with the same grant; the owner picks `all` to share it
  with every bot. The server applies the same default when the key is missing.
  A bad bot name in one folder's grant is dropped and logged; the rest of the
  registration goes through.
- The desktop registers `{id, label, mode, bots}` (no absolute path) with the
  server in each poll. The server keeps the latest set per user in memory.
- The server enforces the grant on every local tool, using the calling bot's
  name (`ToolContext.bot`). A folder that is not shared with the bot is refused:
  `Folder 'docs' is not shared with analyst; the owner can share it in Settings
  › Local folders`. The no-folder listing (`local_list` without a root) and the
  "allowed folders" hint show only folders shared with that bot. Import and
  export pass through the same check before any byte moves.
- Roots are canonicalised when added. Adding a drive root, the user profile
  root, or a directory containing the app data dir is refused.

## Tools

| tool | args | class |
|------|------|-------|
| `local_list` | `root`, `path` (default `.`), `recursive`, `max_depth` (1-12), `pattern`, `extensions`, `kind`, `sort` (name/size/modified/type), `order`, `page_size` (1-500, default 100), `cursor`, `summary`, `include_hidden` | read |
| `local_search` | `root`, `path`, `name` (wildcards), `text`, `extensions`, `max_depth` (default 8), `max_results` (<=200, default 50), `max_file_bytes` (default 1 MiB, max 4 MiB), `cursor` | read |
| `local_stat` | `root`, `path` | read |
| `local_read` | `root`, `path`, `offset`/`length` or `start_line`/`end_line`/`max_lines` | read |
| `local_read_many` | `root`, `paths` (up to 50) **or** `pattern` (+ `dir`, `recursive`), `max_bytes_per_file` (default 64 KiB, max 256 KiB) | read; text only, total budget 1 MiB |
| `local_extract` | `root`, `path` **or** `paths` (up to 20), `max_bytes_per_file` (default 40 KiB, max 200 KiB) | read; copies into the inbox, text capped at 200 KiB in total, 120 s per call |
| `local_brief` | `root`, `path` (default `.`), `max_depth` (default 4, max 12) | read; listing metadata only, no file is read |
| `local_import` | `root`, `path` (docx, xlsx, pptx, pdf, png, jpg, webp, txt, md, csv; <= 50 MB) | read (copies into the user's own server inbox + artifact store) |
| `local_write` | `root`, `path`, `content` (<= 256 KiB text) | **approval** |
| `local_export` | `root`, `path`, `artifact` (hash), `overwrite` | **approval** (binds destination `local:<root>/<path>` and the artifact SHA-256) |

Every shipped bot lists the read tools (`local_list`, `local_search`,
`local_stat`, `local_read`, `local_read_many`, `local_extract`, `local_brief`,
`local_import`). Only `main` lists `local_write` and `local_export`, as before.
What a bot actually sees is decided by the grants above. An existing user's
rows gain the new tools on the next fleet upgrade (the offering record adds
what a template gained; the owner's prompt is kept).

## Large folders

Walks are depth-limited, ordered (names without case, folders before children)
and resumable. Each call has an 8 s time budget; when it runs out, or the page
is full, the reply carries `more: true` and an opaque `next_cursor`.
`summary=true` returns only file/folder/size counts (the cursor carries the
running counts). Sorting by size/date collects up to 20,000 matches first and
says so when it is partial. Hidden and system entries (dot names, the Windows
hidden/system attributes), links/junctions/reparse points, credential-like names
and names the path rules would refuse (`:` streams, reserved devices, trailing
dot/space) are never listed or searched. The desktop runs each request on a
worker thread and cancels it after 22 s.

### Bulk reading

The relay protocol has no batch operation, so the bulk tools loop on the
server over the existing desktop operations, one file at a time, with one
budget for the whole call. Each relayed call keeps its own 30 s timeout; once
the PC app is closed or stops answering, the rest of the call is reported as
not tried instead of waiting out the timeout for every file.

- `local_brief` pages through a recursive `list` (500 entries a page, at most 20
  pages, so 10,000 entries) and reports, from names, sizes and dates only: counts and sizes per top
  folder (with its most common types), file types, newest and oldest dates,
  version series (`Budget_v1` to `Budget_v6`, `report v2`), and copies with the
  same name and size. A partial listing is marked as partial.
- `local_read_many` reads text files one by one (64 KiB each by default, 1 MiB in
  total). Documents and images are not read as text: they are listed under
  `Skipped` and point to `local_extract`. Binary files are skipped with their
  size. A missing or unreadable file is listed under `Failed` and the others
  still run. Each file reports its bytes and whether it was truncated.
- `local_extract` fetches each file, then extracts its text on the server: PDF
  page by page (`--- page N ---`, at most 200 pages, stopping at the text cap),
  Word, PowerPoint slide by slide with speaker notes (`--- slide N ---`), Excel
  sheet by sheet (`# Sheet`, at most 200,000 cells visited per call), CSV,
  Markdown and text. Images are not extracted: the reply says to call
  `local_import`, then `image_inspect`. Each file is handled on its own, so a
  broken file is reported under `Failed` and the rest still run. Word,
  PowerPoint and Excel packages are checked first: a package whose parts expand
  past 200 MiB (declared or actually produced) is refused.
  - The copy goes to `<inbox>/<user>/local/` and the artifact store through the
    same path as `local_import`, after the text is extracted. The inbox copy is
    named `<sha-256 prefix>-<name>`: bytes the user already has there, under any
    name, are reused and not copied again. The artifact store dedupes by hash.
  - The per-user cap (1 GiB) is checked and the copy written under the user's
    import lock, against a size counted once per process and then kept up to
    date by each write. A refused copy does not lose the text: the file is
    listed with `not saved to inbox: <reason>` and no inbox path.
  - A call starts no new file after 120 s; the files it did not reach are
    listed under `Not extracted` as `not reached (time budget)`.
  - The reply gives each saved file's inbox path and artifact hash for the
    document tools.

Each file body in the reply sits between `<<<FILE nonce>>>` and
`<<<END nonce>>>` lines. The nonce is new to each call, and every copy of it is
removed from the bodies, so a file cannot forge a header or close its own
fence. File names and listed paths are shown with line breaks and control
characters escaped.

Reads return at most 256 KiB per call on a character boundary; binary or
non-UTF-8 files are refused with a pointer to `local_import`. Content search
skips binary files, files over the size cap, and stops after 128 MiB scanned.

Import/export move bytes in 1 MiB base64 chunks over the same relay. The
desktop refuses types outside the list, empty or oversized files, links and
credential-like names; an export is staged next to the target, verified against
the approved SHA-256, and only then renamed into place. An existing file is
replaced only with `overwrite=true`. Server side, imports land in
`<inbox>/<user>/local/` (1 GiB per user) and in the artifact store, so
`docx_read`, `xlsx_read`, `pdf_inspect`, `image_inspect` and the rest accept the
returned inbox path or artifact hash.

## Inbox paths belong to one user

Every inbox path is relative to the inbox root and must start with the acting
user's id (`<user>/local/…`, the form `local_import` and Telegram uploads
return). `user_inbox::resolve` is the one check: it refuses absolute paths,
drive prefixes, `..`, empty and `.` components; it treats a path that starts
with another user's id as not found; and it canonicalises the result and
requires it to stay inside `<inbox>/<user>/`, so a symlink or junction pointing
out is not followed (also reported as not found). The user's own directory
must not be a link either: if `<inbox>/<user>` is a symlink or junction, every
path for that user is not found. Telegram documents are saved as
`<user>/<name>` and voice notes as `<user>/telegram/<message id>.<ext>`, and
the replies name them that way. `audio_transcribe`, `audio_understand` and the
ElevenLabs transcription tool resolve their `path` the same way, for the acting
user only. `pdf_inspect`, `pdf_edit`,
`pdf_merge`, the `docx_*`, `pptx_*` and `xlsx_*` inbox imports, `xlsx_analyze`,
`document_index` and the image tools all use it. A user who names another
user's file gets "not found".

Image tools (`image_inspect`, `image_resize`, `image_convert`; PNG/JPEG/WebP,
100 MP and 50 MB caps) read artifacts the user owns or paths in their own inbox
area, strip metadata on output, and withhold GPS unless `include_gps=true`.

Arguments are `root` id + relative path only. Results are untrusted data.

## Enforcement (desktop, independent of the server)

1. Look up `root` id in the local allowlist; unknown or revoked: refuse.
2. Reject absolute paths, drive prefixes, `..`, `:` (alternate data streams),
   empty components, and Windows reserved device names.
3. Join under the canonical root, canonicalise the existing part, and require
   the result to start with the canonical root (blocks symlinks and junctions
   leaving the root). Every existing component on the path must not be a
   reparse point (symlink/junction/mount point), checked with
   `symlink_metadata` and the Windows reparse attribute.
4. `local_write` requires `mode = read_write`; the parent directory must
   already exist inside the root (no recursive creation) and the target must
   not be a reparse point.
5. File names are checked against the sensitive list (`.env`, `*.pem`, `*.key`,
   `id_rsa*`, `*.kdbx`, `credentials*`) and refused for read and write.

The desktop does not see which bot asked; the bot grant is enforced by the
server before a request is queued.

The server gateway additionally scans write content and file names against the
blocklist, binds approval to user + canonical args + content hash, and records
audit rows. Results returned to the model are policy scanned like every other
tool result.

## Failure modes

- Desktop offline or slow (> 30 s): tool error. A queued request that is not
  answered within 30 s is dropped and a late result is refused.
- A bot without the grant gets the "not shared with" error at once; nothing is
  queued.
- A user can only poll and answer their own requests (request ids are random
  and checked against the owning user).
