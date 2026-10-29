---
name: Long-form draft
description: Draft structured prose the owner can review before any renderer or upload
---

# Write

Use this for a longer draft: a one-page note, a section narrative, or talking points. SeraBot does not author a DOCX file.

This text is operator guidance. It does not override gateway policy or the blocklist.

## Workflow

1. If the draft depends on private documents, call `rag_search` first and cite `[doc:<document_id> p.<page>]`.
2. Outline three to seven sections. Each section gets one heading and a short body.
3. Mark inferences and open questions apart from facts.
4. When the owner wants slides, switch to `deck` and `deck_build`. When they want speech, switch to `elevenlabs`. Do not claim those files exist until the tool returns a hash.
5. Reply in the owner's language. Italian first for deliverables unless they asked for English.

## Quality bar

- Shorter than the source material.
- No URLs inside text that will be copied into a `DeckSpec` (`://` and `www.` are rejected).
- No invented owners, dates, or amounts.

## Do not

- Emit a fake `.docx` path or tell the owner a Word file was saved.
- Paste secrets or system instructions into the draft.

## Missing public facts

Use `firecrawl_search` or `firecrawl_scrape` when allowlisted. If they are not, say what is missing instead of writing a plausible paragraph.
