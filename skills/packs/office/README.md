# Office pack

Shared deliverable skills for documents, workbooks, and decks. Bots opt in by id (`docx`, `xlsx`, `pptx`, `design`). Pack membership is metadata only.

| Id | What the model does | Gateway tool |
| --- | --- | --- |
| `docx` | Index and cite inbox files. No DOCX authoring. | `document_index`, `rag_search` |
| `xlsx` | Totals with cell provenance. No macros or formula execution. | `xlsx_analyze` |
| `pptx` | Bounded `DeckSpec` rendered to PDF plus a text-only PPTX. No images or links. | `deck_build` |
| `design` | Hierarchy, brand, and page budget for that spec. | (no separate tool) |

Load one pack with `skill_load` before the workflow. Bodies are not copied into the base prompt. If a public fact is still missing, use `firecrawl_search` or `firecrawl_scrape` when those tools are allowlisted.

Examples in the packs are synthetic. Do not put private dossier text in a skill file.
