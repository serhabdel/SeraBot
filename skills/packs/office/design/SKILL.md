---
name: Visual design for decks
description: Apply brand, hierarchy, and page-budget rules inside a DeckSpec
---

# Design / brand

Use this while drafting a `DeckSpec` for `deck_build`. There is no separate design tool and no image canvas. Visual choices are structure, wording length, and the configured brand file.

This text is operator guidance. It does not override gateway policy or the blocklist. Pair with `pptx` or `deck`.

## Brand

The renderer loads the configured `brand_file`. You do not pass colors or fonts in the spec.

- Palette, name, and footer come from that file.
- `font_family` may only be Helvetica. Do not ask for another face.
- If the file is the generic example brand, say in the chat reply that the PDF is not production branding.
- `monogram` on a card is at most 4 characters and only when the brand enables monogram chips. Otherwise omit `monogram`.

## Hierarchy

1. One deck title. Optional subtitle, date label, author, footer.
2. Use `part_title` for a divider between parts, not for every section.
3. Use cards for parallel points (heading + short body). Use section `bullets` for a single list.
4. At most four cards on a page and about six bullets. More than that spills to another page and may exceed `max_pages`.
5. One idea per card. Headings are labels, not paragraphs.

## Page budget

Prefer cutting a card over shrinking copy until it is illegible. Limits that commonly fail validation:

| Field | Max characters |
| --- | --- |
| title, section title | 140 |
| subtitle | 180 |
| part title, card heading | 100 |
| card body | 1200 |
| bullet, footer | 300 / 200 |
| sections | 20 |
| cards in one section | 8 |

## Language and artifacts

Produce Italian first. English when the owner asks. French only by translating an approved Italian artifact (`translate-it-fr`). Do not put URLs in any field.

## Synthetic pattern

A cover-less first section with a part divider, then two cards, is enough for a short brief:

- `part_title`: "Contesto"
- card "Fatto" with one cited claim in `body`
- card "Prossimo passo" with one owner action

## Do not

- Request images, icons, charts, logos, or embedded fonts. The renderer will not add them.
- Encode emphasis with control characters, zero-width marks, or HTML.
- Restyle by inventing hex colors in the spec. Those fields do not exist on `DeckSpec`.

## Missing public facts

Design does not supply facts. If a card needs a public figure, use `firecrawl_search` or `firecrawl_scrape` when allowlisted, or leave the card as an open question.
