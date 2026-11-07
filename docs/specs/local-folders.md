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

| tool | args | approval |
|------|------|----------|
| `local_list` | `root`, `path` (relative, default `.`) | no |
| `local_read` | `root`, `path` (relative file) | no |
| `local_write` | `root`, `path` (relative file), `content` | **yes** (create/overwrite) |

Arguments are `root` id + relative path only. Limits: read <= 256 KiB text per
call (UTF-8 only, binary refused), list <= 200 entries, write <= 256 KiB, no
overwrite of a directory. Results are untrusted data.

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
