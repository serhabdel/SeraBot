# SeraBot 0.42.0: desktop core resilience and server feature hardening

This release hardens shared desktop session/event/updater plumbing and fixes edge-case bugs across the server's brief, digest, analyst, watcher, spreadsheet, workbook, and document pipelines.

## Desktop core, session, keyboard routing & approvals

- **Session refresh & state resilience (`session.rs`, `session/flows.rs`, `session/bot_settings.rs`, `state.rs`, `worker.rs`)**:
  - Fixed pending-gate silent drops in `refresh_marketplace()`, `refresh_logs()`, `refresh_workflows()`, `fetch_models()`, and `refresh_approvals()` via `queue_backlog`, and expanded read deduplication across catalog, marketplace, approvals, grants, providers, connectors, bots, skills, conversations, archived chats, memories, memory stats, jobs, chat plan, team, and voice reads.
  - Prevented `scheduled` clobbering on `UnarchiveConversation` and `MediaJobs` follow-ups; added `rollback_aborted_request` to clear pre-dispatch guards/flags on abort or dispatch failure; added dedicated `OperationKind::VoiceSettings` so `voice.loading` resets cleanly without re-running `Health` connect hooks.
  - Prevented generic `History` events from overwriting pending `Approvals`/`RoomTeam` refreshes, reset event/loading state on `clear_session`, gated `ensure_section_data` on `!poll_paused`, cleared `confirm_delete` when switching bots in `open_bot_settings`, extended `State::fail` `unknown_outcome` coverage, and propagated non-404/405 `default_route()` errors in `worker.rs`.
- **Keyboard routing, standing grants, SSE & updater (`ui.rs`, `ui/approvals.rs`, `events.rs`, `updater.rs`)**:
  - Upgraded `GLOBAL_KEYS_JS` `Cmd/Ctrl+K` to focus the active screen's visible search input before falling back to `#global-search`.
  - Wired `standing_grants` helpers (`active_grants`, `grant_matches`, `grant_destination_label`, `short_hash`) into `ui/approvals.rs` with `#approvals-search`, a `!grants_loaded` skeleton, and two-step `Revoke` confirmation.
  - Capped `SseParser.data` at `MAX_SSE_BUFFER` with clean frame recovery and preserved reconnect backoff until bytes are read; added `replace_from_part` `.bak` backup/restore in `updater.rs`.

## Server feature quality & edge-case hardening

- **Brief & Digest (`brief.rs`, `digest.rs`)**:
  - Split deadline queries into near-term and older overdue (`ORDER BY due_date DESC, due_time DESC LIMIT 500`) so large overdue backlogs cannot starve `Due soon`, surfaced `[withdrawn] ` updates, redacted URL queries via `redact_url_queries`, excluded active in-flight runs from `source_warnings`, and truncated `MAX_BRIEF_CHARS` on newline boundaries.
  - Updated `digest::telegram_text` to sanitize whitespace, cap title/source/URL lengths, redact URL queries, note item capping at `MAX_ITEMS`, and enforce `MAX_TELEGRAM_CHARS = 3900` on newline boundaries.
- **Analyst & Watcher (`analyst.rs`, `watcher.rs`)**:
  - Reset section/action state on unrecognized `#` headings and empty labels, supported Markdown `- [ ] ` checkboxes, fixed `todo_action_text` boundaries, eliminated UTF-8 byte-offset panic risk in `clean_tag_piece`, validated calendar dates in `looks_like_date`, and respected `max_chars` in `synthesis_outline`.
  - Redacted URL queries in `watch_source` errors, added `user_id` defense-in-depth on `active_deadline`/`supersede_deadlines`/opportunity updates, added `content:encoded` and Dublin Core date fallbacks in `parse_rss`, skipped `<script>`/`<style>`/`<noscript>`/`<svg>`/`<template>` in HTML text extraction, accent-folded `looks_withdrawn`, and improved `parse_textual_date`.
- **Spreadsheets, Workbooks & Documents (`spreadsheets.rs`, `workbooks.rs`, `documents.rs`)**:
  - Split `detect_header_signals` vs `detect_signals` to avoid false unit triggers on headers, stripped UTF-8 BOM in `parse_timesheet_rows`, distinguished Decimal overflow, and negated/warned on parenthesized accounting amounts.
  - Wired `RawEdit.row` into `set_row_height`, preserved styles on formula `SetValue`, shifted rows/cols/merges on row/column inserts & deletes, handled `_xlfn.` functions, validated `AddChart`, and enforced `max_chars` strictly in `plain_text`.
  - Rejected 0-byte files early, gated duplicate shortcut strictly on `status == "ready"`, preserved per-page OCR provenance, enforced `MAX_PDF_PAGES` and `MAX_INDEX_TEXT_CHARS`, cleaned `\r\n`/control chars, snapped `chunk_ranges` to whitespace boundaries, and added `user_id` scoping to `purge_document`'s final delete.

## Verification

- `scripts\cargo-win.bat fmt --all -- --check`
- `scripts\cargo-win.bat clippy --workspace --all-targets -- -D warnings`
- `scripts\cargo-win.bat test --workspace --locked`
