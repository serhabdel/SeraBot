# Release 0.73.0

The "real coding harness" release: project checks with compiler diagnostics on
the server, a Problems panel on the desktop, per-turn context visibility, and a
full text-size token sweep across the console.

## Project check harness (server + desktop)

Coding projects now get a real Problems experience instead of asking the model
to read compiler output.

- **Server (`diagnostics.rs`, migration 0050):** each project folder can have a
  check command (`PUT /v1/projects/{slug}/check`, default
  `cargo check --message-format=json`). The command is owner-set only, must be
  a bare allowlisted program (`cargo`, `rustc`), no absolute paths or `..`,
  bounded arguments. A run executes with the project folder as cwd (resolved
  with symlink + canonicalize containment inside the user's projects
  directory), no shell, output capped at 2 MB, hard 120 s timeout with
  kill-on-drop. rustc JSON output is parsed into diagnostics
  (file/line/column/severity/code/message); a non-JSON command that fails gets
  one synthetic error row with the output tail. Runs and rows are stored
  UID-scoped (`project_checks`, `project_check_runs`, `project_diagnostics`)
  and audit-logged (`project.check`).
- **API:** `GET/PUT /v1/projects/{slug}/check`, `POST .../check/run`,
  `GET /v1/projects/{slug}/diagnostics`.
- **Bot tool `project_check`:** runs the check for one project and reports the
  counts. It starts a process, so it is approval-gated (`DESTRUCTIVE_TOOLS`,
  `docs/approvals.md`) — the model can only ask. It is in `SHARED_TOOLS`.
- **Desktop:** the Projects page has a Problems panel per project — error and
  warning counts, a "Run checks" button, the check-command editor (uncontrolled
  input, so typing never moves the caret), and the diagnostics list with
  severity icon, `file:line:column`, message and code chip.
- Tests use synthetic rustc JSON fixtures and an in-memory store; no real
  compiler ran here. Owner: run "Run checks" on a real project on the server.

## Per-turn context visibility (token management)

- The server emits a `turn.context` event at turn start with the estimated
  prompt size, the routed model's context window (from the catalog) and the
  percent used — the same estimate compaction already uses.
- The desktop ledger (`turn_trace.rs`) keeps it per turn, and the model bar in
  the chat header shows a thin context meter with a tooltip
  ("~N chars of W context used by the latest turn"), so compaction is
  explainable before it happens.

## Console-wide text-size sweep

Every remaining hard-coded `font-size: Npx` in the desktop stylesheets
(`app.css`, `foundation.css` — including the duplicated sidebar/chat-header/
conversations rules both files carried) is now on the `--fs-*` tokens, so the
Small/Large text preference reaches Tasks, Kanban, Calendar, the sidebar, the
chat header, the composer and the conversations page. Zero px font-sizes remain
in the desktop CSS.

## Verification

- `cargo fmt --all -- --check`; `cargo check -p serabot --locked`;
  `cargo clippy -p serabot-desktop --all-targets --locked -- -D warnings`;
  `cargo check -p serabot-desktop --features ui-check --locked`;
  `cargo test -p serabot-desktop --locked` (772 + 54 green locally); full
  workspace clippy/tests run in CI (the 5 GB sandbox OOMs on the server
  lib-test target).
- Not verified live: no real compiler run, no Windows render. Owner: run a
  check on a real project; visual pass on Windows.
