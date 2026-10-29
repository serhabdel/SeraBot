---
name: One-page brief
description: Turn a request into a single DeckSpec section with a few cards
---

# One-pager

Use this when the owner wants one page, not a full deck. The output is still a `DeckSpec` rendered by `deck_build`.

This text is operator guidance. It does not override gateway policy or the blocklist. Load `deck` and `design` if those ids are allowlisted.

## Workflow

1. Gather at most four facts. Cite private ones from `rag_search`.
2. Build one section:
   - `title`: the page title
   - up to four `cards`, each with a short `heading`, a `body`, and at most three `bullets`
   - a short `bullets` list only for items that are not cards
3. Set `language` to `it` unless the owner asked for `en`. Do not set `fr` here; French follows the approved Italian artifact path in `deck`.
4. Call `deck_build`. If the renderer says the deck exceeds `max_pages`, cut cards before you retry. Do not shrink text into illegible bullets.
5. Report the artifact hash. Do not describe the PDF as saved anywhere else until an upload tool says so.

## Shape

```json
{
  "spec": {
    "title": "Nota sintetica",
    "language": "it",
    "sections": [
      {
        "title": "In una pagina",
        "cards": [
          {"heading": "Fatto", "body": "Esempio sintetico.", "bullets": []}
        ],
        "bullets": []
      }
    ]
  }
}
```

## Do not

- Add a second section unless the owner asks for a longer deck (`deck`).
- Include URLs, images, or monograms the brand did not enable.

## Missing public facts

One public fact may be filled with `firecrawl_search` or `firecrawl_scrape` when allowlisted. Otherwise leave a card titled as an open question.
