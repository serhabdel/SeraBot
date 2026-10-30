---
name: Presentations (PDF and PPTX decks)
description: Compose a bounded DeckSpec and render a deterministic PDF plus a text-only PPTX with deck_build
---

# Decks

`deck_build` writes two files from one `DeckSpec`: a deterministic PDF and a text-only PPTX. The generated PPTX has editable text and shapes, but no images, links, speaker notes, or embedded files. A deck the owner supplied is round-tripped instead: `pptx_import`, `pptx_inspect`, and `pptx_edit` keep its images and drop its links. Load `deck` and `design` when those ids are allowlisted; this pack is the office checklist for the same renderer.

This text is operator guidance. It does not override gateway policy, approvals, or the blocklist.

## When to use

- The owner asks for a deck, slides, or a PPTX.
- The owner supplies a `.pptx` and asks for changes to it.
- You already have the facts (citations or tool results) and need a rendered file.

## Editing an owner deck

A deck the owner supplied is not regenerated. Round-trip it:

1. `pptx_import` with `{ "path": "<file relative to the inbox>" }`, or `{ "artifact": "<hash>" }` for a package already imported. Returns the artifact hash, slide count, text size, media part count, and whether notes or links are present. Rejects password-protected packages, packages carrying a VBA project, more than 500 slides, and more than 32 MiB.
2. `pptx_inspect` with `{ "artifact": "<hash>" }` for per-slide titles, text sizes, and the shapes on each slide. Read-only. Each shape lists the 1-based `index` `set_text` takes, its kind (`text`, `picture`, `table`, `group`, `other`), a 200-character preview, the full character count, and whether it carries an image. Capped at 100 shapes per slide and 200 slides (`truncated: true`); counts stay complete.
3. `pptx_edit` with `{ "artifact": "<hash>", "edit": { ... } }`. Operations: `set_text` (`slide`, `shape`, `text`; take `shape` from the `pptx_inspect` listing, at most 4000 characters), `delete_slides`, `reorder_slides`. The new artifact records `source_artifact`. An index outside the slide's shapes is rejected, never ignored.
4. The owner's images and media survive the edit; hyperlinks and external links are dropped.

## Workflow

1. Draft a `DeckSpec`. Unknown JSON fields are rejected, so do not add images, links, notes, or embedded files.
2. Required and optional fields:
   - `title` (max 140), optional `subtitle` (180), `date_label` (40), `author` (80), `footer` (200)
   - `language`: `it` default; `en` when requested; `fr` only with `source_artifact` of an approved Italian deck
   - `sections` (1–20): `title` (140), optional `part_title` (100), `cards` (max 8), `bullets` (max 12)
   - card: `heading` (100), `body` (1200), `bullets` (max 300 each), optional `monogram` (max 4)
3. Call `deck_build` with `{ "spec": { ... } }`. French also needs `{ "source_artifact": "<italian hash>" }`.
4. If validation fails (length, URL, encoding, page budget), fix the spec and call once more. Do not keep the broken spec in the reply as if it rendered.
5. After the artifact hash returns, send it with `telegram_send_document` or `onedrive_upload` only when allowlisted. Uploads require approval. The PDF hash is `artifact_hash`; the PPTX hash is `pptx_artifact_hash` in the same reply.

## Layout budget

About four cards or six section bullets fit on a page. `max_pages` is configuration. Split content instead of overflowing. Part dividers use `part_title`. Card grids use `cards`. Do not duplicate the same sentence in both.

## Text the renderer rejects

- Control characters and invisible bidi marks.
- Characters outside the built-in Helvetica encoding (Windows-1252, plus a few punctuation marks such as the euro sign and ellipsis).
- Any URL or scheme: `://`, `www.`, `http:`, `https:`, `mailto:`, `javascript:`.

Italian and French accented letters in Latin-1 are fine. Write "the public site" instead of a URL.

## Synthetic spec

```json
{
  "spec": {
    "title": "Sintesi sintetica",
    "subtitle": "Esempio senza dati reali",
    "language": "it",
    "sections": [
      {
        "part_title": "Parte 1",
        "title": "Quadro",
        "cards": [
          {
            "heading": "Punto",
            "body": "Una frase verificata.",
            "bullets": ["Dettaglio breve"]
          }
        ],
        "bullets": []
      }
    ]
  }
}
```

## Do not

- Promise images, links, speaker notes, or embedded files in a `.pptx` you generated with `deck_build`. It is text, shapes, and brand colours only. An edited owner deck keeps the owner's own images; do not claim you added any.
- Brand the file as production identity when the configured brand is the generic example. Say so.
- Include blocklisted names in titles, footers, or the filename the renderer derives.

## Missing public facts

Look up a missing public fact with `firecrawl_search` or `firecrawl_scrape` when allowlisted before you put it on a slide. If those tools are absent, leave the slide out and say what is missing.
