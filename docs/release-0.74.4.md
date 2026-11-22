# Release 0.74.4

Patch batch on 0.74: the native SMTP/IMAP mail connector learns to talk to
real-world servers (Hostinger/Dovecot untagged replies), grows named
multi-account mailboxes, and ships two dedicated mail bots.

## Mail connector — IMAP untagged-reply fix (`crates/serabot/src/mail.rs`)

- `imap_connect` now reads the greeting explicitly and accepts `* PREAUTH`
  (skips LOGIN) as well as `* OK`, and after `a1 LOGIN` it uses
  `expect_tagged`, skipping untagged lines. Hostinger/Dovecot send
  `* FLAGS (\Answered \Flagged \Deleted \Seen \Draft)`, `* n EXISTS`,
  `* OK [PERMANENTFLAGS ...]`, … between LOGIN and the tagged `a1 OK`;
  the old code read exactly one line and failed with
  `mail server replied unexpectedly: * FLAGS (...)`.
- `SELECT` in `imap_list` / `imap_read` likewise uses `expect_tagged("a2")`
  (untagged FLAGS/EXISTS/RECENT precede `a2 OK` there too).
- IMAP literal folding fixed while testing against a fake server:
  `read_logical_line` used to read literal bytes straight from the socket even
  though `read_line` had already buffered them, and it looped forever on the
  leftover `{N}` marker — any `BODY[]` FETCH would have timed out against a
  real server. Literals are now consumed from the read buffer and folded
  exactly once per logical line.
- Tests: fake in-process IMAP servers replaying a Hostinger-style
  LOGIN + SELECT + UID SEARCH + FETCH transcript
  (`imap_connect_tolerates_untagged_lines_after_login`,
  `imap_list_tolerates_untagged_select_and_search`).

## Mail connector — named multi-account mailboxes (0.74.4)

- Credentials for extra mailboxes are stored per user under provider
  `mail.<label>` (label: 1–24 chars of lowercase letters, digits, `-`, `_`);
  the default mailbox stays on `mail`. `mail::account_provider` validates
  labels; `mail::list_accounts` enumerates connected accounts.
- `mail_list`, `mail_read`, `mail_send` take an optional `account` argument
  (omit for the default mailbox). `mail_send` stays approval-gated.
- API: `POST /v1/mail/connect` accepts `account`; `GET /v1/mail/status`
  returns the default mailbox fields plus an `accounts` array (never any
  password); `DELETE /v1/mail?account=<label>` removes one account.
  `user_secrets::providers_for` keeps `mail.<label>` rows out of the AI
  provider list.
- Desktop Integrations → Mail card: optional "Account name" field, the
  connected-mailboxes summary line, per-account disconnect; status is
  re-fetched after connect/disconnect. Mobile models/api client carry the
  same optional `account` field.

## New bots (0.74.4)

- `hostinger` (`bots/hostinger.toml`, `prompts/hostinger.txt`): fully manages
  one Hostinger mailbox (preset `smtp.hostinger.com:587` STARTTLS /
  `imap.hostinger.com:993` SSL) — triage, read, approval-gated send.
- `mailbox` (`bots/mailbox.toml`, `prompts/mailbox.txt`): custom multi-account
  manager — passes `account` to the mail tools to triage/read/send across the
  default mailbox plus named accounts.
- Both registered in `shipped.rs` (fleet now 22 bots; README updated), with
  bundled 256×256 default avatars (`assets/bots/hostinger.png`,
  `mailbox.png`) and per-bot desktop starter prompts (`chat_feed.rs`).
- `skills/mail/SKILL.md` documents the `account` argument, the Hostinger
  preset, and the multi-account workflow.

## Housekeeping

- Workspace version 0.74.4 (`Cargo.toml`), mobile `0.74.4+78`
  (`apps/serabot_mobile/pubspec.yaml`), `tasks/todo.md`, `README.md`,
  `AGENTS.md`.

## Verification

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --locked` (mail tests use a fake in-process
  SMTP/IMAP server and synthetic fixtures only).
- Not verified live: a real Hostinger login from this environment — owner to
  re-test Integrations → Mail with the Hostinger preset on Windows.
