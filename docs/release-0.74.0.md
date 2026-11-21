# Release 0.74.0

The code workspace: a real editor for project folders on the desktop, wired to
the 0.73 check harness — click a problem, the file opens at that line. Plus the
proposed LSP spec for 0.75.

## Workspace editor (desktop, Projects page)

- Per project: a **file tree** (text files open in the editor, others are
  listed but not editable) and an **editor** with line-number gutter, mono
  font, synced scroll, dirty/saved state and a Save button.
- The editor textarea is uncontrolled (initial content set once on mount, the
  file path is the key), so typing never resets the caret — the same fix as
  the 0.71/0.72 rename inputs.
- **Problems click-through:** clicking a diagnostic opens its file in the
  editor and scrolls to the exact line, focusing the caret there.
- Save goes through a new contained write path; every save is audit-logged.

## Server: project file write

- `PUT /v1/projects/{slug}/files/{*path}` with `{"content": "..."}` (0.74).
  `projects::write_project_file` rejects `..`/absolute paths, never follows
  symlinks, creates parent folders, and enforces canonicalized containment
  inside the user's project folder; 1 MiB cap; path policy-scanned; audit
  event `project.file_write`. Tests cover containment, overwrite and
  cross-user refusal with tempdirs.

## LSP spec (proposed, not built)

- `docs/specs/lsp-workspace.md`: rust-analyzer over stdio on the desktop only,
  allowlisted binary, one allowed folder per project, capability-restricted
  client (no executeCommand/codeAction/workspace writes), cleared child env,
  bounded I/O, edits still through the gated project-file write path. Queued as
  0.75 **pending owner approval** since it widens the local-process exception.

## Queued next

0.75 LSP workspace (spec above) · 0.76 memory v2 (per-bot private scope +
console-fact contradiction check) · 0.77 phone parity wave. Tracked in
`tasks/todo.md`.

## Verification

- `cargo fmt --all -- --check`; clippy `-D warnings` (server lib, desktop
  all-targets); `cargo check -p serabot --locked`;
  `cargo check -p serabot-desktop --features ui-check --locked`;
  `cargo test -p serabot-desktop --locked` (772 + 54) locally; full workspace
  suite in CI (sandbox OOMs on the server lib-test target locally).
- Not verified live: no editor session on Windows, no real check run. Owner:
  open a project, edit a file, save, run checks, click a problem.
