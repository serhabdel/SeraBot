# Release 0.44.0

Wave 7 visual preview dock and chat UX improvements across `serabot` and `serabot-desktop`.

## Highlights

- **Server visual preview geometry & rich blocks** (`crates/serabot/src/api/preview.rs`, `crates/serabot/src/api/preview_pdf.rs`, `crates/serabot/src/api/tests/preview_api.rs`):
  - Added PDF visual page geometry extraction (MediaBox dimensions, background/colored rectangles, positioned text blocks).
  - Added PPTX structured slide geometry and shapes (text, rect, table, image) alongside `slide_count`.
  - Added XLSX multi-sheet `sheets` array with sheet name, range, rows, and truncation status.
  - Added DOCX `rich_blocks` extraction (headings, paragraphs, ordered/unordered list items, tables, embedded images).
  - Maintained full image preview metadata (`data_url`, dimensions, `size_bytes`).

- **Desktop Right Sidebar Visual Preview Dock** (`crates/serabot-desktop/src/preview.rs`, `crates/serabot-desktop/src/ui_files.rs`, `crates/serabot-desktop/src/ui/shell.rs`, `crates/serabot-desktop/src/session.rs`, `crates/serabot-desktop/src/state.rs`, `crates/serabot-desktop/assets/files.css`):
  - Added `PreviewVisual` model and visual renderers in the Right Sidebar Preview Dock for Image, PDF, PPTX, XLSX, DOCX, and Markdown.
  - Added active media card highlight and inline image thumbnails on media cards.

- **Chat UX & Activity Box fixes** (`crates/serabot-desktop/src/chat_feed.rs`, `crates/serabot-desktop/src/ui/chat.rs`, `crates/serabot-desktop/src/ui.rs`, `crates/serabot-desktop/assets/foundation.css`):
  - Fixed expanded `.activity` / `details.activity.done` box layout, `.activity-steps` scroll behavior, and `details.step-more` toggle styling.
  - Styled `.stop-btn` / `.composer-stop` button in the composer.
  - Ensured `TrailingMediaCards` renders in chat whenever pending `media_jobs` are active even before trailing library entries land.
