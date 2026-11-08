# Local folders (desktop harness tools)

Status: 0.18 design. Minimal by intent.

## Goal

A user can let the bot read (and, if allowed, write) files in folders on their
own Windows PC, without any server-side filesystem access to that PC. The
default is off: an empty allowlist exposes nothing.

## Shape

```
bot tool local_list/read/write
   -> gateway (allowlist, approval, audit, blocklist)
   -> LocalRelay (server, in memory, per user)
   -> desktop long-poll  GET /v1/local/poll  (authenticated as that user)
   -> desktop enforces its OWN allowlist again, runs the operation
   -> desktop POST /v1/local/result/{request_id}
   -> tool output (untrusted data)
```

The server never connects to the desktop; the desktop pulls. If no desktop
polled within the last 45 s the tools fail fast with
`local folders unavailable: the desktop app is offline`.

## Allowlist

- Stored only on the desktop (`local_folders.json` in the app data dir). Each
  entry: `{id, path, mode: "read" | "read_write"}`. `id` is a short slug the
  model sees (`docs`, `reports`); the model never sees or chooses absolute
  paths.
- The desktop registers `{id, label, mode}` (no absolute path) with the server
  in each poll; the server keeps the latest set per user in memory so the bot
  can be told which roots exist and the gateway can refuse writes to a
  read-only root before relaying.
- Roots are canonicalised when added. Adding a drive root, the user profile
  root, or a directory containing the app data dir is refused.

## Tools

| tool | args | class |
|------|------|-------|
| `local_list` | `root`, `path` (default `.`), `recursive`, `max_depth` (1-12), `pattern`, `extensions`, `kind`, `sort` (name/size/modified/type), `order`, `page_size` (1-500, default 100), `cursor`, `summary`, `include_hidden` | read |
| `local_search` | `root`, `path`, `name` (wildcards), `text`, `extensions`, `max_depth` (default 8), `max_results` (<=200, default 50), `max_file_bytes` (default 1 MiB, max 4 MiB), `cursor` | read |
| `local_stat` | `root`, `path` | read |
| `local_read` | `root`, `path`, `offset`/`length` or `start_line`/`end_line`/`max_lines` | read |
| `local_import` | `root`, `path` (docx, xlsx, pptx, pdf, png, jpg, webp, txt, md, csv; <= 50 MB) | read (copies into the user's own server inbox + artifact store) |
| `local_write` | `root`, `path`, `content` (<= 256 KiB text) | **approval** |
| `local_export` | `root`, `path`, `artifact` (hash), `overwrite` | **approval** (binds destination `local:<root>/<path>` and the artifact SHA-256) |

Large folders: walks are depth-limited, ordered (names without case, folders
before children) and resumable. Each call has an 8 s time budget; when it runs
out, or the page is full, the reply carries `more: true` and an opaque
`next_cursor`. `summary=true` returns only file/folder/size counts (the cursor
carries the running counts). Sorting by size/date collects up to 20,000 matches
first and says so when it is partial. Hidden and system entries (dot names, the
Windows hidden/system attributes), links/junctions/reparse points, credential-like
names and names the path rules would refuse (`:` streams, reserved devices,
trailing dot/space) are never listed or searched. The desktop runs each request
on a worker thread and cancels it after 22 s.

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

The server gateway additionally scans write content and file names against the
blocklist, binds approval to user + canonical args + content hash, and records
audit rows. Results returned to the model are policy scanned like every other
tool result.

## Failure modes

- Desktop offline or slow (> 30 s): tool error, nothing queued beyond expiry.
- Requests expire server-side after 60 s; late results are dropped.
- A user can only poll and answer their own requests (request ids are random
  and checked against the owning user).
