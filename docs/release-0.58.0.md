# Release 0.58.0

Every bot gets the basic tools, attached PDFs are actually read (OCR for
scans, also after a handoff), per-user project folders, and WhatsApp through a
linked spare number. Synthetic fixtures only; nothing here was run against a
live WhatsApp account.

## Basic tools for every bot

- `collaboration::SHARED_TOOLS` now gives every bot document reading, editing
  and building (PDF, Word, Excel, slides), `document_index` (with the OCR
  fallback), `rag_search`, audio, image, local-file and the member's connected
  connector tools, whatever its stored list says. Unconnected connectors are
  skipped; everything on `DESTRUCTIVE_TOOLS` still needs approval.

## Attachments that get read

- A PDF attachment with no text layer tells the bot to call `document_index`,
  which reads it with OCR when `[ocr] enabled = true`. The `document_index`
  description no longer claims that no OCR is performed.
- Handoff turns (`ask_teammate`, `delegate`) inherit the attachments of the
  owner turn their chain started from, so a teammate reads the file from the
  inbox instead of searching OneDrive or the member's PC.
- The gateway logs `gateway tool failed` with the error.

## Project folders

- `projects.rs`: `project_list`, `project_create` and `project_file` organise
  `<inbox>/<user>/projects/<slug>/`. Moves stay inside the member's own inbox,
  never overwrite, and attachment notes tell the bot to file documents into a
  matching project.

## WhatsApp as a linked device

- Unofficial `whatsapp-rust` client, off unless `[whatsapp_link] enabled = true`
  (`docs/specs/whatsapp-linked-device.md`). A member links a spare number from
  the desktop card (QR or pairing code). Messages from chats on the member's
  allowlist reach the `whatsapp` bot: text, voice notes (transcribed), documents
  and images (chat attachments). Replies go back to the same chat; messages the
  bot starts itself go through the approval-gated `whatsapp_link_send`.
- Migration 0041 (`whatsapp_link_accounts`, `whatsapp_link_chats`).
- `rusqlite` 0.39 / `r2d2_sqlite` 0.34 (`libsqlite3-sys` 0.37), needed by the
  client's session store.

## Operations

- `deploy/serabot.production.toml.example` sets `[logging] dir` and
  `[whatsapp_link] dir` under `/var/lib/serabot`, the only writable path under
  `ProtectSystem=strict`.

## Fixes

- Two clock-dependent tests made deterministic (`stop_during_model_call…`,
  `a_server_error_or_dead_network_keeps_everything`).
