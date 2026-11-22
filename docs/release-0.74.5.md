# Release 0.74.5

Patch batch on 0.74: the mail connector grows from triage into real mailbox
management — folders, search, flags, move, delete, attachments, signatures —
plus a desktop mail window that exists only while a mailbox is connected.

## Mail management tools (`crates/serabot/src/mail.rs`, 0.74.5)

- `mail_folders` — `LIST "" "*"`, parses the untagged LIST replies.
- `mail_search` — `UID SEARCH OR OR SUBJECT q FROM q TEXT q` per folder, newest
  matches first, then FETCHes them like `mail_list`.
- `mail_flag` — `UID STORE ±FLAGS.SILENT (...)`; flags validated as `\` +
  letters only. Approval-gated (`DESTRUCTIVE_TOOLS`).
- `mail_move` — `UID MOVE` to a destination folder. Approval-gated.
- `mail_delete` — `\Deleted` + `UID EXPUNGE`. Approval-gated, irreversible.
- All take the optional `account` (named mailbox, 0.74.4). Registered in
  `tools.rs::known_tool_names`; `docs/approvals.md` updated.
- `mail_read` now parses multipart bodies: text comes from the first
  `text/plain` part, and attachments are listed (name, type, size; parts up to
  512 KiB, 2 MiB total, also returned base64).

## Signatures (0.74.5)

- `MailCredentials.signature` (optional, ≤ 4096 chars, no control chars), set
  via `POST /v1/mail/connect` and the desktop/mobile cards. `mail_send`
  appends `\n\n-- \n<signature>` to every outgoing body (`body_with_signature`);
  bots are told not to add it to drafts.

## Desktop mail window (0.74.5)

- Read-only API for the console: `GET /v1/mail/folders?account=`,
  `GET /v1/mail/messages?account=&folder=&limit=`,
  `GET /v1/mail/messages/{uid}?account=&folder=` (UID-scoped, never any password).
- Integrations → Mail card gains an "Open inbox" button **only while a mailbox
  is connected**; the `MailWindow` sheet (`ui_connectors.rs`) has an account
  selector (default + named accounts), folder selector, message list, and a
  reading pane with flags and attachments. Styles in `pages-connect.css`.

## Mobile (0.74.5)

- Mail screen: account-name field (connect + disconnect pass it), connected
  mailboxes list in the status card; status is re-fetched after
  connect/disconnect so the accounts list stays current
  (`mail_screen.dart`, `mail_models.dart`, `api_client.dart`).

## Bots & skill (0.74.5)

- `hostinger` and `mailbox` get `mail_folders`, `mail_search`, `mail_flag`,
  `mail_move`, `mail_delete` (the three mailbox-changing ones in
  `require_approval`); `inbox` gets `mail_folders` + `mail_search`.
  Prompts and `skills/mail/SKILL.md` document the new tools, the `account`
  rule, and the automatic signature.

## Housekeeping

- Workspace version 0.74.5 (`Cargo.toml`), mobile `0.74.5+79`
  (`apps/serabot_mobile/pubspec.yaml`), `tasks/todo.md`, `README.md`,
  `AGENTS.md`, `docs/approvals.md`.

## Verification

- `cargo fmt --all -- --check`, `cargo clippy -p serabot -p serabot-desktop
  --all-targets -- -D warnings`, `cargo check --all-targets` — clean.
- All 15 `mail::` tests executed green via a scratch harness compiling the real
  `mail.rs` (fake in-process IMAP/SMTP servers): new coverage for
  LIST/SEARCH/STORE/MOVE/EXPUNGE transcripts, multipart attachment parsing,
  flag validation, and signature appending. Full `cargo test --workspace` runs
  in CI (sandbox memory cap).
- Not verified live: real Hostinger mailbox operations — owner to re-test on
  Windows (Integrations → Mail → Open inbox).
