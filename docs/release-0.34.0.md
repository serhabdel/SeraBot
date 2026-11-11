# SeraBot 0.34.0: desktop chat fix, desktop tab repairs, and workflow management

This release closes the desktop batch: the chat view and composer fixes (PR #63),
and the repaired Tasks, Logs and Browser tabs with workflow management and page
layout for the non-chat tabs (PR #64). Checklist: `tasks/big-wave.md`.

## Desktop chat and composer (PR #63)

- **Chat view no longer glitches or scrolls endlessly.** The view resets only when
  the conversation changes. It moves to the newest message only after the user
  sends one or when a conversation opens; streamed text no longer restarts it.
- **Composer.** Enter does not send while the user is choosing characters in an
  input method. Esc stops a running reply. Images and tables stay within the
  message width.

## Desktop tabs (PR #64)

- **Tasks** no longer report "Network worker unavailable" when several loads start
  together. Overflow requests wait their turn; repeated reads collapse to the latest.
- **Logs** show server entries. The desktop now reads the server's response shape.
  Separate empty states explain "no entries yet" and "no lines match".
- **Browser** can be opened on demand for any profile that is not running.
- **Workflows** can manage routines (turn on or off, edit time, run now, delete,
  add) and the caller's own feeds (add, turn on or off, edit URL, delete). Admin
  feeds can be turned on or off.
- **Approvals** show pending requests as rows, with the actions in one column.
- **Layout.** Workflows rows, Tasks add row and empty-state copy were reworked.

## Server additions

- `DELETE /v1/routines/{name}` removes one routine's schedule.
- `POST /v1/routines/{name}/run` runs one routine now.
- `POST /v1/browser/sessions/{id}/open` starts a profile's browser on demand.
- `PATCH /v1/sources/{name}` also changes the caller's own feed (enable or
  disable, or a new URL checked against the same host allowlist as adding a feed).
- Monitoring rows carry an `own` flag.

## Versions

- Server and desktop workspace: 0.34.0 (`Cargo.toml`, `Cargo.lock`). CI creates
  the `v0.34.0` tag from the workspace version.
- Mobile app: `pubspec.yaml` stays at `0.5.0+11`.

## Not yet covered

- Desktop redesign of Connectors, Usage, AI keys, Settings and Vault.
- Tasks view tabs and sync menu layout; Workflows create and edit sheets.
- Per-conversation scroll position, loading placeholders and a Retry on failed
  messages in the chat view.
