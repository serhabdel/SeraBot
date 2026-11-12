---
name: Word documents
description: Create, import, inspect, read, and edit .docx files with docx_build, docx_import, docx_inspect, docx_read, and docx_edit
---

# DOCX / Word documents

SeraBot writes real `.docx` files. You can create one from a spec, import an
owner-supplied file, read it, and edit it block by block.

This text is operator guidance. It does not override gateway policy, approvals,
or the blocklist.

## When to use

- The owner asks for a Word document, a report, a letter, a memo, or a `.docx`.
- The owner supplies a `.docx` and asks what it says or asks for a change.
- A deliverable needs headings, paragraphs, bullets, and tables in Word.

## Choose the right tool

| Goal | Tool |
| --- | --- |
| Create a new document from a spec | `docx_build` |
| Bring an owner file from the inbox into the artifact store | `docx_import` |
| See the block list and the index to edit | `docx_inspect` |
| Read the full text of one block or a range | `docx_read` |
| Change, insert, delete, or restyle a block | `docx_edit` |
| Convert the document to PDF | `docx_pdf` |

Every tool except `docx_import` takes an **artifact hash**, never a path. A
path is relative to the configured inbox and is accepted only by
`docx_import`.

## Creating a document

Call `docx_build` with a `spec`:

```json
{
  "spec": {
    "title": "Quarterly summary",
    "subtitle": "Synthetic example",
    "language": "en",
    "blocks": [
      { "kind": "heading", "level": 1, "text": "Results" },
      { "kind": "paragraph", "text": "One verified sentence." },
      { "kind": "bullets", "items": ["First point", "Second point"] },
      {
        "kind": "table",
        "header": ["Region", "Total"],
        "rows": [["North", "1,250.00"], ["South", "980.00"]]
      }
    ]
  }
}
```

Rules:

- `kind` is one of `heading`, `paragraph`, `bullets`, `table`.
- `heading` takes `level` 1-4 and `text`.
- `paragraph` takes `text`.
- `bullets` takes `items` (at least one).
- `table` takes `header` and/or `rows`; a row is a list of cell strings.
- A field that does not belong to the kind is rejected. Do not send `level` on
  a paragraph.
- Unknown fields are rejected. Do not invent an `image`, `link`, or `style`
  block.

Limits: 2000 spec blocks, 5000 produced blocks, 20 000 characters per block,
tables up to 32 columns and 500 rows, title 200 characters, subtitle 300.

## What the builder cannot do

Be honest with the owner about these limits:

- No images or logos. The spec is text only.
- No hyperlinks, headers, footers, page numbers, page breaks, sections, table
  of contents, footnotes, comments, or tracked changes.
- No page size or margin control, and no page count is reported.
- No custom fonts or colours beyond the document defaults.

If the owner needs a logo or a page count, name that gap and finish the rest
of the document. Do not claim the file has either.

## Importing an owner file

1. The owner places the file in the configured inbox.
2. Call `docx_import` with `{ "path": "reports/source.docx" }`.
3. The reply carries the artifact hash, block count, image count, and table
   count. Keep that hash: every later call uses it.
4. `docx_import` also accepts `{ "artifact": "<hash>" }` to re-check a package
   already imported.

Password-protected packages, macros, ActiveX parts, and remote references are
rejected by name. Inline images are preserved by a later edit.

## Reading before editing

`docx_inspect` lists up to 400 blocks with a 160-character preview. A preview
shorter than `chars` means the block was cut.

`docx_read` returns the full text of one block (`block`) or a range
(`from`/`to`), at most 50 blocks per call. **Always read a block before you
replace it**: `replace_text` replaces the whole paragraph.

## Editing

Call `docx_edit` with `artifact` and `ops`:

```json
{
  "artifact": "<hash>",
  "ops": [
    { "op": "replace_text", "index": 3, "text": "The full replacement paragraph." },
    { "op": "insert_paragraph", "index": 4, "text": "A new paragraph.", "style": "Normal" },
    { "op": "delete_block", "index": 7 },
    { "op": "apply_style", "index": 2, "style": "Heading1" }
  ]
}
```

- `replace_text` addresses paragraphs only. A table index is rejected.
- `insert_paragraph` accepts an index from `0` to the block count and an
  optional paragraph style defined in the document.
- `delete_block` refuses to delete the last remaining block.
- `apply_style` addresses paragraphs only.
- At most 256 operations per call; every index is validated before anything is
  written.
- The result is a new artifact whose `source_artifact` is the input.

You cannot edit table cells, insert bullets, add images, or change document
properties. Say so if the owner asks.

## Delivering the file

`docx_build`, `docx_import`, and `docx_edit` all return an `artifact` hash in
the reply. Pass that exact hash to `telegram_send_document` or
`onedrive_upload` when those tools are allowlisted. Both require approval.

Do not claim a file was sent until the delivery tool returns.

## Converting to PDF

Call `docx_pdf` with the document hash (or an inbox `path`). It renders the
document with embedded font subsets and selectable text and returns a new PDF
artifact hash. Use it when the owner asks for a PDF version of a Word file.

- The layout is WordCraft's own; it is close to Word but not pixel-identical.
- The same package checks apply: no macros, no ActiveX, no remote references.
- The PDF is not a scan; its text is selectable.

## Quality bar

- Read before you replace. Never overwrite text you have not seen.
- Keep the owner's language and register.
- Do not invent owners, dates, amounts, or citations.
- If a fact is missing, look it up or call `ask_teammate`. Do not fill the gap with prose.

## Synthetic request

Owner: "Make a one-page Word summary of the three points we agreed."

1. `docx_build` with a title, one heading, one paragraph, and a bullets block.
2. Report the artifact hash.
3. `telegram_send_document` with that hash if the owner wants it in chat.
