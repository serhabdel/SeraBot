Brings `feat/office-tools` onto the multi-user main line.

## What lands

- **DOCX/PPTX to PDF export** (`docx_pdf`, `pptx_pdf`) via `deckcraft-pdf` / `wordcraft-pdf` and the `krilla` patch the craft apps pin.
- **Telegram file intake**: send a DOCX/XLSX/PPTX/PDF/CSV/TXT/MD file to the bot and it lands in the inbox ready for the document tools.
- **Firecrawl structured extraction** (`firecrawl_extract`), approval-gated like the other Firecrawl tools.
- **XLSX workbook work** and the **conversation artifact library**, so a document built by a non-approval tool is visible before delivery.

## Conflict resolutions (both sides kept)

- The office branch's `0008_conversation_artifacts` collided with `0008_users_and_sessions`. It is renumbered to **0013** and gains `user_id`, so an artifact belongs to the user who owns the conversation.
- `record_conversation_artifact` takes the acting user; `list_conversation_artifacts` and `count_bot_artifacts` filter by user.
- Telegram document intake moved into `handle_bound_message` (the multi-user refactor), so uploads act as the linked user.
- Uploads land in a **per-user inbox subdirectory** named by the validated account id, so a member's file is never indexed into the admin's documents.
- `maker.rs` keeps both the 50 MB Telegram guard and the per-user chat resolution.

## New command

`serabot fleet reseed [--user <id>] [--dry-run]` — the database is authoritative after seeding, so a binary upgrade never rewrites a fleet. This is the explicit opt-in that replaces a user's bot rows with the shipped templates. **Without it the new office tools never reach an existing deployment's bots.**

## Verification

- Windows: `fmt` clean, `clippy -D warnings` clean, **719 tests passing**.
- Linux (WSL gate script): `fmt` clean, `clippy` clean, **705 tests passing**, release build green — `gate status: 0`.
- New test covers the reseed path (edited prompt, narrowed tool list, and scaffolded bot all replaced by the templates).
