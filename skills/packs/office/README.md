# Office pack

Shared deliverable skills for documents, workbooks, and decks. Bots opt in by id (`docx`, `xlsx`, `pptx`, `design`). Pack membership is metadata only.

| Id | What the model does | Gateway tools |
| --- | --- | --- |
| `docx` | Create, import, inspect, read, and edit Word documents. | `docx_build`, `docx_import`, `docx_inspect`, `docx_read`, `docx_edit` |
| `xlsx` | Create, import, inspect, read, and edit workbooks; totals with cell provenance. Formulas are stored, never evaluated. | `xlsx_build`, `xlsx_import`, `xlsx_inspect`, `xlsx_read`, `xlsx_edit`, `xlsx_analyze` |
| `pptx` | Bounded `DeckSpec` rendered to PDF plus a text-only PPTX; owner decks round-trip through import/inspect/edit and keep their images; links are dropped. | `deck_build`, `pptx_import`, `pptx_inspect`, `pptx_edit` |
| `design` | Hierarchy, brand, and page budget for that spec. | (no separate tool) |

Load one pack with `skill_load` before the workflow. Bodies are not copied into the base prompt. If a public fact is still missing, use `firecrawl_search` or `firecrawl_scrape` when those tools are allowlisted.

Examples in the packs are synthetic. Do not put private dossier text in a skill file.

## Delivery

Every build, import, and edit returns an artifact hash. Pass that exact hash to `telegram_send_document` or `onedrive_upload` when those tools are allowlisted. Both require approval, and the approval binds the hash.

## Formats

| Format | Create | Import an owner file | Read | Edit |
| --- | --- | --- | --- | --- |
| `.docx` | `docx_build` | `docx_import` | `docx_inspect`, `docx_read` | `docx_edit` |
| `.xlsx` | `xlsx_build` | `xlsx_import` | `xlsx_inspect`, `xlsx_read`, `xlsx_analyze` | `xlsx_edit` |
| `.pptx` | `deck_build` | `pptx_import` | `pptx_inspect` | `pptx_edit` |
| `.pdf` | `deck_build` | (inbox path accepted directly) | `pdf_inspect` | `pdf_edit`, `pdf_merge` |
