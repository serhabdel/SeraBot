---
name: Word-style documents
description: Ingest, index, and cite owner documents; no direct DOCX authoring
---

# DOCX / long-form documents

SeraBot does not run a Word runtime. Use the document pipeline:

1. Place files in the configured inbox (`documents.watch_dir`).
2. Call `document_index` for a relative inbox path when the bot allowlists it.
3. Answer with `rag_search` and cite `[doc:<document_id> p.<page>]`.
4. For outbound deliverables, prefer `deck_build` (PDF deck) or owner-approved uploads.

Never invent page numbers or document ids. If indexing fails, report the error.
