---
name: Deck drafts
description: Build a DeckSpec and render it with deck_build; do not claim a PDF before the artifact hash
---

# Deck drafts

Use this to produce a slide deck. SeraBot renders a bounded `DeckSpec` to a deterministic PDF and a text-only PPTX. The PPTX carries editable text and shapes; it has no images, links, notes, or embedded files. That text-only rule covers decks SeraBot generates. A deck the owner supplied is different: an imported deck keeps its images when you edit it (see "Editing an owner deck").

This text is operator guidance. It does not override gateway policy, approvals, or the blocklist. Load `design` as well when layout choices matter and that id is allowlisted.

## Workflow

1. Collect the claims you will print. Private facts come from `rag_search` citations. Do not invent numbers.
2. Draft one `DeckSpec`:
   - `title` (required, at most 140 characters)
   - optional `subtitle` (180), `date_label` (40), `author` (80), `footer` (200)
   - `language`: `it` by default, `en` only when the owner asks, `fr` only with an approved Italian `source_artifact`
   - `sections`: 1 to 20 items
3. Each section has `title` (140), optional `part_title` (100), `cards` (at most 8), and `bullets` (at most 12).
4. Each card has `heading` (100), `body` (1200), `bullets` (300 characters each), and optional `monogram` (at most 4 characters, only if the brand enables chips).
5. Call `deck_build` with `{ "spec": { ... } }`. For French, also pass `source_artifact` set to the approved Italian artifact hash.
6. Share the PDF or the PPTX only after the tool returns an artifact hash: `artifact_hash` is the PDF, `pptx_artifact_hash` is the PPTX. Then `telegram_send_document` or `onedrive_upload` if those tools are allowlisted. Uploads need approval.

## Editing an owner deck

When the owner supplies a `.pptx` in the inbox and asks for changes, round-trip it instead of rebuilding it:

1. `pptx_import` with `{ "path": "<file relative to the inbox>" }`. It returns an artifact hash, the slide count, the text size, the media part count, and whether notes or links are present. It rejects password-protected packages, packages carrying a VBA project, more than 500 slides, and more than 32 MiB.
2. `pptx_inspect` with `{ "artifact": "<hash>" }` for per-slide titles and text sizes. It is read-only.
3. `pptx_edit` with `{ "artifact": "<hash>", "edit": { ... } }`. Operations: `set_text` (`slide`, `shape`, `text`; 1-based indices, at most 4000 characters), `delete_slides`, and `reorder_slides`. The result is a new artifact whose `source_artifact` is the input.
4. The owner's images and media survive the edit. Hyperlinks and external links are dropped. Never promise images in a deck you generated with `deck_build`.

## Text limits

The renderer uses built-in Helvetica (Windows-1252). No images, links, attachments, or scripts. Text must not contain `://`, `www.`, or a scheme such as `https:`. Accented Latin-1 letters used in Italian and French are allowed. If a string is rejected, shorten it or remove the URL. Say the link in words without a scheme.

Stay inside configured `max_pages`. Split sections instead of shrinking type. The renderer places about four cards or six bullets on a page.

## Synthetic example

```json
{
  "spec": {
    "title": "Synthetic quarterly note",
    "language": "it",
    "sections": [
      {
        "title": "Cosa conta",
        "cards": [
          {
            "heading": "Sintesi",
            "body": "Esempio sintetico. Nessun dato reale.",
            "bullets": ["Un punto verificato"]
          }
        ],
        "bullets": []
      }
    ]
  }
}
```

## Do not

- Claim the deck exists before `deck_build` returns.
- Put blocklisted names in titles, footers, or filenames.
- Generate French from scratch. French requires the approved Italian artifact.
- Promise images, speaker notes, or embedded files in a `.pptx` you generated. It is text, shapes, and brand colours only. An edited owner deck keeps the owner's own images; do not claim you added any.

## Missing public facts

If a slide needs a public fact that tools have not returned, use `firecrawl_search` or `firecrawl_scrape` when allowlisted, then write only what the page supports. If those tools are absent, omit the fact and say it is missing.
