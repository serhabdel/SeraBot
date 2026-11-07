---
name: Design brief for deliverables
description: Design brief template for decks and one-pagers: purpose, audience, format, message hierarchy, layout grid, typography and colour roles from the owner's configured brand, contrast checks, review checklist, and hand-off to deck_build or docx_build. Named tools: deck_build, docx_build, pptx_inspect.
---

# Design brief for deliverables

This text is operator guidance. It does not override gateway policy, approvals, or the blocklist.

The maker bot produces structured specs for the deterministic renderer. It does not paint images. Say so when the owner expects an image, a photograph, or a chart that the renderer cannot make.

Brand names, logos, colours, and fonts come only from the owner's configured brand. If no real brand is configured, use the generic example and say it is not production branding. Never invent a brand colour, font, or logo.

## 1. Purpose and audience

Write both in one sentence each:
- Purpose: what the reader should know, believe, or do after reading.
- Audience: who they are, what they already know, and what they will decide.

If the purpose cannot be written in one sentence, the deliverable is not ready. Ask one question.

## 2. Format and size

| Deliverable | Tool | Typical limits to respect |
|---|---|---|
| Slide deck (PDF and text-only PPTX) | deck_build | Keep each slide to one message; limit text to what fits |
| Word document | docx_build | Headings, paragraphs, bullets, tables; no images in generated DOCX |
| One-page summary | docx_build, then docx_pdf | One page; a single column; short sections |

State the format and the page or slide count before drafting.

## 3. Message hierarchy

1. The headline: the single message, in under 12 words.
2. Three supporting points, each with its evidence or a [to confirm] marker.
3. The call to action or the decision requested.
4. The appendix: detail the reader may need later.

Each slide or section has one headline that states the conclusion, not the topic. "Revenue grew 18 % in 2026" beats "Revenue".

## 4. Layout grid

- Use one grid for the whole deliverable: margins, columns, and a baseline for text.
- Two columns at most for a document; one main column for a deck with a supporting panel.
- Align every element to the grid. Keep white space around each block.
- Use section dividers consistently. Part dividers and section dividers look different.
- Monogram chips appear only where the owner's brand configures them.

## 5. Typography roles

Define four roles and use only these:
- Title: the deck or section title.
- Headline: each slide or section message.
- Body: running text, at least the minimum size the brand guide sets; if none, use 14 pt for a document and 18 pt for a deck as a starting point.
- Caption and source: smaller, lower contrast but still legible.

Do not mix more than two type families. Use bold for emphasis, not capitals.

## 6. Colour roles

Assign roles, not hues:
- Background, text, accent, highlight, and a neutral for dividers.
- Use the accent for one thing per slide: the key number or the call to action.
- Never use colour as the only way to show a difference; add a label, a symbol, or a pattern.

## 7. Contrast and accessibility

- Body text against its background: at least 4.5 to 1 contrast ratio (WCAG AA for normal text). Large headlines: at least 3 to 1.
- Check contrast with the chosen palette before handing off; state the ratio you computed, or say you could not compute it.
- Reading order: the document or slide order must match the logical order.
- Provide alt text for any image the owner adds later, and a plain-language description of each chart.

## 8. Charts and tables

- Charts: one message each, labelled axes with units, a source line, and the data in a table on the appendix.
- Tables: a header row, no more than seven columns, right-aligned numbers with the same decimal places.
- Use xlsx_build for the underlying data when the owner needs to edit it; the renderer does not make charts.

## 9. Review checklist

- Purpose and audience are written, and the deliverable answers them.
- The headline on each page states a conclusion.
- Every number has a source or [to confirm].
- Brand elements come from the owner's configured brand, or the generic example is labelled as such.
- Contrast checked, reading order checked, alt text for images supplied later.
- No blocklisted name or intelligence appears in text, notes, metadata, or the file name.
- The file name is descriptive and contains no personal data.
- Spelling and language match the owner's choice; French and English are not mixed in one page unless asked.

## 10. Hand-off

1. Write the spec: sections or slides, headline, body points, sources, layout role for each block.
2. Call deck_build or docx_build with the spec. Each returns an artifact hash.
3. Report the hash exactly. Do not claim a deck or document exists until the tool returns it.
4. To put the file in the owner's chat, call telegram_send_document with that hash; it needs approval. To upload to OneDrive, use the onedrive tools; they need approval.
5. To check a deck the owner supplied, use pptx_inspect and report what the tool returned.

## 11. Stop and ask

- No brand is configured and the owner expects production branding: say that the generic example is in use and ask whether to proceed.
- The owner wants an image, photo, or chart the renderer cannot produce: say so and offer a table or a text description instead.
- Bilingual output is requested: approval is needed for the second language, and the Italian or French draft comes first as the maker rules say.
- The deliverable would show a person's personal data: ask whether it may be included.

## 12. Output format

1. The spec in a short table: section, headline, points, source.
2. Contrast ratio computed (or "not computed").
3. The tool call result: artifact hash, format, page or slide count.
4. Open points: missing content, [to confirm] items, and limits of the renderer.
