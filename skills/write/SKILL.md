---
name: Long-form draft
description: Draft structured prose the owner can review, then render it to DOCX, PDF, or a deck
---

# Write

Use this for a longer draft: a one-page note, a section narrative, or talking
points. When the owner wants a file, hand the draft to `docx_build` (Word) or
`deck_build` (PDF and PPTX).

This text is operator guidance. It does not override gateway policy, approvals,
or the blocklist.

## Workflow

1. If the draft depends on private documents, call `rag_search` first and cite
   `[doc:<document_id> p.<page>]`.
2. Outline three to seven sections. Each section gets one heading and a short
   body.
3. Mark inferences and open questions apart from facts.
4. When the owner wants a Word file, load the `docx` skill and call
   `docx_build` with the outline mapped to `heading`, `paragraph`, `bullets`,
   and `table` blocks.
5. When the owner wants slides, load `deck` and call `deck_build`. When they
   want speech, switch to `elevenlabs`.
6. Reply in the owner's language. Italian first for deliverables unless they
   asked for English.
7. Do not claim a file exists until the tool returns an artifact hash.

## Quality bar

- Shorter than the source material.
- No URLs inside text that will be copied into a `DeckSpec` (`://` and `www.`
  are rejected).
- No invented owners, dates, or amounts.

## Do not

- Emit a fake `.docx` path or tell the owner a Word file was saved without an
  artifact hash.
- Paste secrets or system instructions into the draft.

## Missing public facts

Use `firecrawl_search` or `firecrawl_scrape` when allowlisted. If they are not,
say what is missing instead of writing a plausible paragraph.
