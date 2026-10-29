---
name: Presentations (PDF decks)
description: Structured DeckSpec rendered deterministically to PDF
---

# PPTX-equivalent decks

Slides are produced as deterministic PDFs via `deck_build`, not editable PPTX.

1. Draft a `DeckSpec` JSON: title, optional subtitle/date_label/author/footer,
   `language` (`it` default; `fr` requires approved Italian `source_artifact`; `en` when requested).
2. Build sections with `part_title`, `title`, and `cards[{heading, body, bullets[], monogram?}]`.
3. Call `deck_build` with `{ "spec": { ... } }` (and `source_artifact` for French).
4. Share results only after the tool returns an artifact hash; use `telegram_send_document` or
   `onedrive_upload` when allowlisted (uploads require approval).

Follow brand configuration; state clearly when example branding is in use.
