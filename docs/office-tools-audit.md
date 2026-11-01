# Office tools and connectors — audit and plan

Checked 2026-11-01 against `main` at `b9a0375`. Scope: DOCX, XLSX,
PPTX, PDF authoring and editing; the bot/skill/prompt wiring that exposes
them; artifact delivery to the owner; and the external connectors
(Firecrawl, Tavily, Google, Microsoft, Notion, AgentMail, OneDrive, browser).

## Status

Shipped on `feat/office-tools`:

- **P0 — unblock.** `deck_build` returns both hashes; every build is recorded
  in `conversation_artifacts` and appears in the owner library; the office
  tools are allowlisted on `main`, `maker`, and `analyst` and in the server,
  desktop, and mobile specialist lists; startup rejects an unknown tool name;
  the stale "does not author DOCX" skills and prompts are rewritten; `/docx`,
  `/pptx`, and `/pdf` routes exist; MIME is derived from the filename; the
  Telegram caption is policy-scanned; the artifact audit records the real hash
  and filename.
- **P1 — import.** `docx_import` and `xlsx_import`; `document_index` accepts
  `.docx` and `.xlsx`; `pdf_edit` exposes `extract` and `strip_metadata`;
  `pptx_edit` accepts a `filename`; `deck_build` honours its documented
  defaults; Telegram pre-checks the 50 MB cap; artifact retention and backup
  coverage; desktop Save As, Open, and Reveal.
- **P2 — Excel depth.** Formula cells in `xlsx_build`; `xlsx_edit` gains
  `set_cell_style`, `set_row_height`, `freeze_panes`, `add_sheet`,
  `rename_sheet`, `delete_sheet`, `insert_rows`, `delete_rows`,
  `insert_columns`, `delete_columns`, and `add_chart`.
- **P2 — conversion.** `docx_pdf` and `pptx_pdf` render an owner document or
  deck to PDF with selectable text. The craft PDF exporters are written against
  the krilla fork the craft apps pin, so the workspace carries a `[patch.crates-io]`
  entry for that revision; the published krilla 0.8.2 does not compile with
  `craft-pdf`.
- **P4 — upload.** A document sent to the Telegram bot is saved to the inbox
  under a sanitized, non-overwriting name, so the import tools can use it.
- **Firecrawl.** `firecrawl_extract` adds structured extraction over 1-5 URLs;
  a crawl that times out returns its partial pages instead of discarding them;
  `tavily_search` joins `EXTERNAL_EFFECT_TOOLS`.

Still open: DOCX images/headers/page breaks, PPTX slide/table/image insertion,
PDF text extraction and watermarking, the desktop attach button, and the
connector work in P3.

The short version: **the authoring engines were built and tested in 0.9.0,
then never connected to the fleet.** The agent cannot create a document
because no bot is allowed to call the tool, the skills tell it the tool does
not exist, and the one build tool the fleet does have (`deck_build`) does not
return the hash needed to deliver its own output.

---

## 1. What actually exists

All of the following are registered in the global tool registry and covered
by tests. `docx` and `pptx` are default cargo features
(`crates/serabot/Cargo.toml:13`), so release builds include them.

| Tool | Purpose | Key limits |
| --- | --- | --- |
| `docx_build` | Build a .docx from a text spec: heading (level 1-4), paragraph, bullets, table | 2000 spec entries, 5000 blocks, table 32x500, 20k chars/block |
| `docx_inspect` | List addressable blocks (zero-based index, kind, style, preview) | 400 listed, 160-char preview |
| `docx_read` | Full text of one block or a range | 50 blocks/call, 20k chars |
| `docx_edit` | `replace_text`, `insert_paragraph`, `delete_block`, `apply_style` | 256 ops, paragraph-only |
| `xlsx_build` | Build a workbook from literal values (text/number/date) | 20 sheets, 64 cols, 5000 rows/sheet, 50k cells |
| `xlsx_inspect` | Sheet summary, used ranges, formulas, charts, images | read-only |
| `xlsx_read` | Read one worksheet range | 2000 cells/call |
| `xlsx_edit` | `set_value`, `set_formula`, `set_number_format`, `set_column_width`, `merge_cells` | 500 edits, `.xlsx` only |
| `xlsx_analyze` | Financial analysis of an inbox workbook (incl. xls/xlsm/ods/csv) | read-only, no formula evaluation |
| `pptx_import` | Ingest an owner .pptx from the inbox into the artifact store | 32 MiB, 500 slides |
| `pptx_inspect` | Per-slide shapes with the 1-based index `set_text` takes | 200 slides, 100 shapes/slide |
| `pptx_edit` | `set_text`, `delete_slides`, `reorder_slides` | 200 ops, 4000 chars |
| `deck_build` | DeckSpec to deterministic PDF **and** text-only PPTX | 20 sections, 8 cards, page budget |
| `pdf_inspect` | Page geometry and active-content report | 2000 pages |
| `pdf_edit` | `rotate`, `delete`, `reorder` (one operation per call) | 64 MiB, 2000 pages |
| `pdf_merge` | Concatenate up to 32 PDFs | 64 MiB combined |

Artifacts are SHA-256 content-addressed and stored under
`data/artifacts/<hash>.<ext>` with a JSON sidecar (`maker.rs:48-65`).
Every build/edit tool returns `artifact_hash`, `filename`, and
`artifact_path`.

### What the tools cannot do

These are deliberate design limits, not bugs, but they are the limits the
owner will hit first:

- **DOCX**: no images/logo, no hyperlinks, no page breaks, no
  headers/footers, no page numbers, no sections, no TOC, no footnotes, no
  page-size/margin control, no page-count reporting. `docx_edit` cannot
  touch table cells, insert bullet items, or add formatting runs.
- **XLSX**: no chart creation (charts are only preserved), no sheet
  add/rename/delete, no row/column insert/delete, no cell styling
  (bold/fill/border), no freeze panes, no row heights, no formulas in
  `xlsx_build` (they must be added by a second `xlsx_edit` call).
- **PPTX**: no add/duplicate slide, no add text box, no add image, no add
  table, no table-cell edit, no notes, no layout/geometry changes.
- **PDF**: no text extraction, no page extraction (the function exists but
  is not exposed), no metadata stripping (same), no split, no watermark, no
  form filling. `pdf_merge` drops annotations, form fields, bookmarks, and
  metadata by construction (`pdf_ops.rs:697-703`).

### The import asymmetry

`pptx_import` is the only tool that can bring an owner-supplied file into
the artifact store. There is **no `docx_import` and no `xlsx_import`**, and
`document_index` accepts only `.pdf`, `.txt`, `.md`, `.markdown`
(`documents.rs:60-64`).

Consequences:

- An owner's `.docx` cannot be read, inspected, edited, indexed, or
  converted. It is invisible to the agent.
- An owner's `.xlsx` can only be *analyzed* (`xlsx_analyze` reads the inbox
  directly). It cannot be inspected cell-by-cell, read by range, or edited.
- A `.pptx` and a `.pdf` can be round-tripped; Word and Excel cannot.

---

## 2. Why the agent cannot create a document

Ordered by impact. Each item is independently sufficient to break the
request.

### 2.1 No bot is allowed to call the document tools

The gateway rejects any tool not in the bot's `tools` list
(`gateway.rs:203-217`). The shipped fleet:

| Bot | Document tools allowed |
| --- | --- |
| `main` (orchestrator) | none |
| `maker` (deliverables) | `deck_build` only |
| `analyst` | `xlsx_analyze`, `document_index` |
| `watcher` | none |
| `legal` | none |

`docx_build`, `docx_edit`, `docx_inspect`, `docx_read`, `xlsx_build`,
`xlsx_edit`, `xlsx_inspect`, `xlsx_read`, `pdf_inspect`, `pdf_edit`,
`pdf_merge`, `pptx_import`, `pptx_inspect`, and `pptx_edit` appear in **no**
bot file. `bots/` was last touched in release 0.6.3; the 0.9.0 commit that
added the tools changed only `Cargo.toml` and `Cargo.lock`.

### 2.2 The owner console cannot grant them either

`SPECIALIST_TOOL_ALLOWLIST` (`config.rs:1318-1360`) gates what the admin API
may write into a bot file, and the desktop mirrors it in
`SPECIALIST_TOOLS` (`serabot-desktop/src/api.rs:195-237`). Neither list
contains a single `docx_*`, `xlsx_build/edit/inspect/read`, `pdf_*`, or
`pptx_*` tool. Editing a bot from the console is rejected with "Choose tools
from the specialist list."

### 2.3 The skills tell the model the capability does not exist

The `maker` bot loads the `docx` skill (`bots/maker.toml:4`), whose body
says:

> `skills/packs/office/docx/SKILL.md:3` — "Index inbox files and answer with
> page citations; SeraBot does not author DOCX"
>
> `:8` — "SeraBot does not run Word, LibreOffice, or a DOCX writer."
>
> `:25` — "Do not claim a `.docx` was written."

`skills/write/SKILL.md:8` repeats it, and
`skills/packs/office/README.md:7` maps the `docx` skill to
`document_index`/`rag_search` with "No DOCX authoring." The `xlsx` skill
never mentions `xlsx_build`, `xlsx_edit`, `xlsx_inspect`, or `xlsx_read`.

So the model is instructed not to attempt the thing the tool would do.

### 2.4 The prompts do not mention documents

`prompts/main.txt` lists `rag_search`, `fetch_url`, Firecrawl, Tavily,
audio, and skills — no document creation, and its routing line says only
"`/deck` (maker), `/xlsx` and `/timesheets` (analyst)". `prompts/maker.txt`
talks only about decks and media. `prompts/analyst.txt` talks only about
analysis.

### 2.5 There is no route for a Word or PDF request

`runtime.rs:513-581` maps `/brief`, `/watch`, `/ask`, `/deck`, `/xlsx`,
`/timesheets`. There is no `/docx` or `/pdf`. An owner asking the front door
for a Word file gets ordinary chat, and the orchestrator has no document
tool to delegate to.

### 2.6 `deck_build` never returns its artifact hash

`deck_build`'s `ToolOutput.content` contains the title, filename, and
rendered text — but not `artifact_hash` or `pptx_artifact_hash`
(`maker.rs:404-418`). The runtime forwards only `content` to the model
(`runtime.rs:1649-1675`); the hashes live in `data`, which the model never
sees.

Every other build tool includes the hash in `content` (`docx_ops.rs:1867`,
`workbooks.rs:1701`, `pdf_ops.rs:1294`, `pptx_ops.rs:1171`). `deck_build` is
the sole outlier, and it is the only build tool the maker bot can call.

Effect: the model cannot call `telegram_send_document` or `onedrive_upload`
for a deck it just built, because it has no hash to pass.

### 2.7 The library only lists approval-path artifacts

`list_conversation_artifacts` joins `actions` (`store.rs:730-738`), and an
`actions` row is created only when a tool asks for approval
(`gateway.rs:226-233`). Non-external tools such as `docx_build`,
`xlsx_build`, `deck_build`, `pdf_edit`, and `pptx_edit` execute directly and
create no row. Their artifacts never appear in the desktop Library tab.

### 2.8 The tool-count cap is already tight

`MAX_BOT_CONFIG_TOOLS = 32` (`api.rs:2376`). `maker` has 17 tools. Adding
the 15 documented tools (docx 4, xlsx 4, pdf 3, pptx 3, `document_index`)
lands exactly on 32, leaving no room for future work.

---

## 3. Delivery breakpoints

| # | Break | Evidence |
| --- | --- | --- |
| D1 | `deck_build` omits its hash from `content` | `maker.rs:404-418` |
| D2 | Library only sees approval-path actions | `store.rs:730-738`, `gateway.rs:235-238` |
| D3 | Download URL is Bearer-only and loopback by default; no signed or one-time link the model could emit | `api.rs:189, 2584-2627` |
| D4 | `GET /v1/artifacts/{hash}` hardcodes `application/pdf` for every artifact | `api.rs:904` |
| D5 | `onedrive_upload` hardcodes `application/pdf` | `maker.rs:585` |
| D6 | `telegram_send_document` hardcodes `application/pdf`, has no 50 MB pre-check, and uses a 45 s client timeout | `telegram.rs:368`, `maker.rs:483-509` |
| D7 | Telegram caption is model-authored and sent inside `execute`; the gateway scans only `output.content` afterwards, so the blocklist does not gate it | `maker.rs:488-500`, `gateway.rs:479-492` |
| D8 | Desktop has no Files screen, no preview, no Save As, no "open with Word", no "reveal in Explorer"; downloads always overwrite `~/Downloads/<name>` | `session.rs:2630-2649`, `ui.rs:3446-3488` |
| D9 | No retention, no delete route, no artifact coverage in `serabot backup` | `runtime.rs:849-861`, `main.rs:272-279` |
| D10 | `artifact.created` audit records a fabricated `artifacts/<filename>` path and `bytes = content.len()` (the text length, not the file size) | `gateway.rs:493-509` |
| D11 | No owner-to-bot file upload: Telegram parses only text/voice/audio, desktop attach is disabled | `telegram.rs:812-820`, `ui.rs:2612` |

---

## 4. Connectors

Full matrix in the connector audit; the headline is that almost everything
is read-only, unpaginated, and cannot be authorized from inside the product.

| Connector | Read | Write | Notes |
| --- | --- | --- | --- |
| Firecrawl | scrape, map, search, crawl | — | no `/v2/extract`; crawl discards partial results on timeout |
| Tavily | search | — | snippets only; missing from `EXTERNAL_EFFECT_TOOLS` |
| Google Drive | list first 20 files | — | no `q`, no pagination, no download, no upload |
| Google Sheets | read one range | — | no write, no metadata |
| Google Calendar | list next 20 events | — | primary calendar only, `timeMin=now`, no create/update |
| Gmail | message ids only | — | no `messages.get`, no `q`; effectively unusable |
| Outlook mail | list 20 messages | — | id/subject/date only |
| Outlook Calendar | list 20 events | — | no date window, no create/update |
| Notion | query one database | — | no page read/create, no scope requested |
| AgentMail | list inboxes | send email | no HTML, cc/bcc, attachments, or threading |
| OneDrive | list folder | upload artifact | no download; MIME hardcoded PDF; overwrite forbidden |
| Browser | list tabs | open tab | no DOM read, no click, no screenshot; `GET /json/new` likely rejected by modern Chromium |
| Telegram | — | send document | text messages only via the notifier |

Two structural problems:

1. **OAuth cannot be completed in-product.** The authorization-URL and
   token-exchange code exists but has no production caller; the admin API
   rejects every connector id except `elevenlabs`
   (`api.rs:2181-2185, 2210-2214`). The owner must mint refresh tokens out
   of band.
2. **No connector paginates.** `page_size` is capped at 20 and no tool
   accepts a page token, so item 21 is unreachable in every provider.

---

## 5. Gap analysis by realistic request

| Owner request | Today | Missing |
| --- | --- | --- |
| "Create a 5-page Word report with headings, tables and our logo" | Partial | Tool unreachable; no image/logo; no page breaks or page count |
| "Open my existing .docx and change section 3" | No | No `docx_import`; tools take artifact hashes only |
| "Read this .docx and summarise it" | No | `document_index` rejects `.docx` |
| "Edit row 12 of my budget and add a chart" | No | No `xlsx_import`; no chart creation |
| "Make a 10-slide pitch deck from this outline" | Partial | `deck_build` works but cannot deliver (no hash); slide count is derived, not controlled; no images/notes |
| "Merge these 3 PDFs and rotate page 2" | Yes | Two calls; merge drops annotations/bookmarks/metadata |
| "Extract pages 3-5 into a new PDF" | No | `pdf_ops::extract` exists but is not exposed |
| "Add a table to slide 3" | No | `pptx_edit` has no shape/table insertion; existing tables are not editable |
| "Send me the file" | No | Model has no hash for decks; no delivery tool outside `maker`; Telegram MIME/size issues |
| "What's on my calendar tomorrow?" | Partial | List-only, next 20 events, no date window |
| "Create a calendar event" | No | No write path |
| "Add this to Notion" | No | Query only |

---

## 6. Plan

Phased so that each phase is independently shippable and testable. P0 is
the "it works at all" phase and is small; P1 closes the import asymmetry;
P2 makes the tools genuinely capable; P3 fixes connectors; P4 makes the
desktop feel native.

### P0 — Unblock the existing tools (small, highest impact)

1. **Return hashes from `deck_build`.** Append `artifact:` and
   `pptx_artifact:` to `content` (`maker.rs:405-418`). One line; restores
   the entire deck delivery path.
2. **Record every artifact.** Add migration `0008_artifacts.sql` with an
   `artifacts` table (hash, conversation_id, turn_id, tool, filename, title,
   created_at) and write a row from `finish_tool_call` whenever a tool
   returns `artifact_hash`. Union it into `list_conversation_artifacts` so
   the Library shows builds, not only approvals.
3. **Grant the tools.** Add all document tools to
   `SPECIALIST_TOOL_ALLOWLIST` (`config.rs`) and `SPECIALIST_TOOLS`
   (`serabot-desktop/src/api.rs`). Raise `MAX_BOT_CONFIG_TOOLS` to 48.
4. **Allowlist them.** `maker` gets docx/xlsx/pdf/pptx authoring plus
   `document_index`; `analyst` gets the read/import path
   (`docx_import`, `docx_inspect`, `docx_read`, `xlsx_import`,
   `xlsx_inspect`, `xlsx_read`, `pdf_inspect`, `document_index`).
5. **Rewrite the stale guidance.** `skills/packs/office/docx/SKILL.md`,
   `skills/packs/office/xlsx/SKILL.md`, `skills/packs/office/README.md`,
   `skills/write/SKILL.md`, and `prompts/{main,maker,analyst}.txt` must
   describe what the tools actually do, including the limits.
6. **Add routes.** `/docx` and `/pdf` in `runtime.rs`, both to `maker`.
   Update the main prompt's routing line.
7. **Correct MIME.** One `mime_for(filename)` helper used by the API
   download, `onedrive_upload`, and `telegram_send_document`.
8. **Gate outgoing text.** Scan the Telegram caption (and the OneDrive
   subfolder) with the policy before sending. Pass `Arc<Policy>` into the
   tools, as ElevenLabs already does.
9. **Fix audit accuracy.** Record the real artifact path and byte length in
   `artifact.created`.
10. **Tests.** A test that every registered document tool is allowlisted by
    at least one enabled bot; a test that no skill body contains "does not
    author DOCX"; a test that `deck_build` content contains both hashes; a
    MIME table test; a caption-blocklist test.

### P1 — Import owner files, close the asymmetry

1. `docx_import` and `xlsx_import` using `ArtifactStore::ingest_inbox_file`,
   mirroring `pptx_import` (same bounds, same VBA/macro rejection).
2. `document_index` accepts `.docx` and `.xlsx` (text extraction; no OCR).
3. Expose `pdf_extract_pages` and `pdf_strip_metadata` (implemented in
   `pdf_ops.rs`, not reachable).
4. `pptx_edit` accepts an output `filename`; `docx_inspect`/`docx_read`
   return `artifact_hash`.
5. `deck_build`: `#[serde(default)]` on `language`, `cards`, `bullets` so
   the documented defaults are true.
6. Telegram: local 50 MB pre-check with a clear error; per-extension MIME.
7. Artifact retention (TTL + size cap), a delete route, and artifact
   coverage in `serabot backup`.
8. Desktop: Save As, "Open with default app", "Reveal in Explorer", and
   overwrite protection.

### P2 — Make the tools genuinely capable

- **DOCX**: image/logo block, page break, header/footer, page numbers,
  hyperlink runs, table cell edit, bullet insertion, bold/italic runs,
  page-count reporting.
- **XLSX**: chart creation (bar/line/pie), sheet add/rename/delete,
  row/column insert/delete, cell styles (bold/fill/border), freeze panes,
  row heights, formulas in `xlsx_build`.
- **PPTX**: add/duplicate slide, add text box/image/table, table-cell edit,
  speaker notes.
- **PDF**: text extraction, split, watermark, page numbering.

### P3 — Connectors

- In-product OAuth for Google, Microsoft, and Notion (the code exists; wire
  it to the admin API and the desktop Connectors screen).
- Pagination and filters on every list tool.
- Write paths: Calendar create/update, Notion page create/append, Sheets
  write, Drive upload/download, Gmail message get.
- Firecrawl `/v2/extract`; add `tavily_search` to `EXTERNAL_EFFECT_TOOLS`.
- Fix browser navigation (`PUT /json/new`) or document it as unsupported.

### P4 — Native desktop experience

- A real Files section: list, preview, Save As, open, reveal, delete.
- Approvals screen shows the artifact and offers a download.
- Owner-to-bot file upload (Telegram `document`, desktop attach).

---

## 7. Recommendation

Ship **P0 and P1 together** as one release. They are bounded, they touch
configuration, prompts, skills, delivery, and two new import tools, and
together they turn "the agent cannot make me a document" into "the agent
can create, import, edit, and deliver Word, Excel, PowerPoint, and PDF
files". P2 is the next release and is where the real authoring depth lives.

Do not start P3 before P0: connector work is wasted while the agent cannot
deliver the file it just produced.

## 8. Open questions for the owner

1. Should document authoring live on `maker` (current plan) or on a new
   `office` bot with its own command (`/doc`)?
2. Is a logo/image block in DOCX and PPTX a hard requirement for the first
   release, or can it wait for P2?
3. Should the agent auto-deliver a built file to Telegram, or always ask
   for approval first?
4. Which connectors matter first: Calendar (read+write), Notion
   (read+write), or Google Drive/Sheets (read+write)?
