---
name: Deck drafts
description: Build a DeckSpec and render it with deck_build; do not claim a PDF before the artifact hash
---

# Deck drafts

Use this to produce a slide deck. SeraBot renders a bounded `DeckSpec` to a deterministic PDF. It does not write an editable PPTX.

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
6. Share the PDF only after the tool returns an artifact hash. Then `telegram_send_document` or `onedrive_upload` if those tools are allowlisted. Uploads need approval.

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

## Missing public facts

If a slide needs a public fact that tools have not returned, use `firecrawl_search` or `firecrawl_scrape` when allowlisted, then write only what the page supports. If those tools are absent, omit the fact and say it is missing.
