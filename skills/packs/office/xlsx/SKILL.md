---
name: Excel workbooks
description: Create, import, inspect, read, edit, and analyze .xlsx workbooks with xlsx_build, xlsx_import, xlsx_inspect, xlsx_read, xlsx_edit, and xlsx_analyze
---

# XLSX / spreadsheets

SeraBot reads and writes real `.xlsx` workbooks. Formulas are stored as text
and **never evaluated** by SeraBot; Excel recalculates them when the owner
opens the file.

This text is operator guidance. It does not override gateway policy, approvals,
or the blocklist.

## When to use

- The owner asks for a spreadsheet, a budget, a tracker, or a `.xlsx`.
- The owner supplies a workbook and asks what it contains or asks for a change.
- The owner pastes rows without a file: use `timesheet_analyze` instead.

## Choose the right tool

| Goal | Tool |
| --- | --- |
| Create a new workbook from a spec | `xlsx_build` |
| Bring an owner file from the inbox into the artifact store | `xlsx_import` |
| See sheets, used ranges, and counts | `xlsx_inspect` |
| Read the cells of one range | `xlsx_read` |
| Change values, formulas, formats, styles, sheets, rows, columns, or charts | `xlsx_edit` |
| Compute totals from an inbox file with provenance | `xlsx_analyze` |

`xlsx_analyze` reads the inbox directly and accepts `.xlsx`, `.xlsm`, `.xls`,
`.ods`, and `.csv`. The other tools work on an artifact hash and accept
`.xlsx` only.

## Creating a workbook

Call `xlsx_build` with a `spec`:

```json
{
  "spec": {
    "title": "Quarterly budget",
    "language": "en",
    "sheets": [
      {
        "name": "Budget",
        "header": ["Region", "Plan", "Actual", "Variance"],
        "rows": [
          [
            { "type": "text", "value": "North" },
            { "type": "number", "value": 12500 },
            { "type": "number", "value": 11800 },
            { "type": "formula", "value": "=C2-B2" }
          ],
          [
            { "type": "text", "value": "South" },
            { "type": "number", "value": 9800 },
            { "type": "number", "value": 10150 },
            { "type": "formula", "value": "=C3-B3" }
          ]
        ]
      }
    ]
  }
}
```

Rules:

- A cell is `{"type":"text","value":"..."}`, `{"type":"number","value":1.5}`,
  `{"type":"date","value":"2026-01-31"}`, or
  `{"type":"formula","value":"=C2-B2"}`.
- Dates are strict `YYYY-MM-DD`, years 1900-9999.
- Formula text is stored verbatim and never evaluated. Do not invent a cached
  result.

Limits: 20 sheets, 64 columns, 5000 rows per sheet, 50 000 cells, 500
characters per text cell, sheet names 31 characters.

## Importing an owner file

1. The owner places the file in the configured inbox.
2. Call `xlsx_import` with `{ "path": "finance/budget.xlsx" }`.
3. The reply carries the artifact hash, sheet names, and counts of formulas,
   charts, and images. Keep that hash.
4. `xlsx_import` also accepts `{ "artifact": "<hash>" }` to re-check a
   workbook already imported.

Macros, external workbook links, and embedded objects are rejected by name.
Styles, number formats, column widths, row heights, merged cells, charts, and
images survive a later edit.

## Reading before editing

1. `xlsx_inspect` with `{ "source_artifact": "<hash>" }` returns each sheet's
   name, used range, cell count, formula count, image and chart counts, and
   merged ranges.
2. `xlsx_read` with `{ "source_artifact": "<hash>", "range": "Budget!A1:D20" }`
   returns each stored cell's address, kind, and value. A formula cell returns
   its text and the result Excel last cached, labelled separately.
3. A read covers at most 2000 cells. A larger range is refused, not truncated:
   read a narrower range.
4. The range must sit inside the sheet's used range.

**Always read a range before you overwrite it.** `set_value` replaces the cell.

## Editing

Call `xlsx_edit` with `source_artifact` and `edits`. Every operation names a
`sheet` except `add_sheet`.

### Values and formulas

```json
{ "op": "set_value", "sheet": "Budget", "address": "C2",
  "value": { "type": "number", "value": 11950 } }
{ "op": "set_formula", "sheet": "Budget", "address": "D2",
  "formula": "=C2-B2" }
```

- `address` is a single A1 cell. Ranges are not accepted.
- `formula` is stored verbatim and never evaluated. It must not contain `[` or
  `]`, and remote functions such as `WEBSERVICE`, `FILTERXML`, `RTD`, `DDE`,
  `CALL`, `EXEC`, `REGISTER.ID`, `IMPORTDATA`, `IMPORTHTML`, `IMPORTXML`,
  `IMPORTFEED`, and `IMPORTRANGE` are rejected.

### Format and layout

```json
{ "op": "set_number_format", "sheet": "Budget", "address": "C2",
  "format": "#,##0.00" }
{ "op": "set_column_width", "sheet": "Budget", "column": "C", "width": 14 }
{ "op": "set_row_height", "sheet": "Budget", "row": 1, "height": 24 }
{ "op": "merge_cells", "sheet": "Budget", "range": "A5:C5" }
{ "op": "freeze_panes", "sheet": "Budget", "rows": 1, "columns": 0 }
```

- `set_number_format` keeps the cell's other formatting.
- `set_column_width` is in characters, 0.5-255.
- `set_row_height` is in points, 0-409.
- `merge_cells` needs at least two cells and must not overlap an existing
  merge.
- `freeze_panes` freezes the top `rows` rows and the left `columns` columns;
  at least one must be non-zero.

### Cell style

```json
{ "op": "set_cell_style", "sheet": "Budget", "range": "A1:D1",
  "bold": true, "italic": false, "font_size": 12,
  "font_color": "#FFFFFF", "fill": "#1F4E79",
  "border": "thin", "align": "center", "wrap": true }
```

- Every style field is optional, but at least one is required.
- `font_color` and `fill` are hex colours such as `#1F4E79`.
- `border` is one of `none`, `thin`, `medium`, `thick`, `dashed`, `dotted`,
  `double`; it applies to all four sides.
- `align` is `left`, `center`, or `right`.
- A style edit covers at most 10 000 cells.

### Sheets, rows, and columns

```json
{ "op": "add_sheet", "name": "Notes" }
{ "op": "rename_sheet", "sheet": "Notes", "name": "Appendix" }
{ "op": "delete_sheet", "sheet": "Appendix" }
{ "op": "insert_rows", "sheet": "Budget", "at": 2, "count": 1 }
{ "op": "delete_rows", "sheet": "Budget", "at": 5, "count": 2 }
{ "op": "insert_columns", "sheet": "Budget", "at": 2, "count": 1 }
{ "op": "delete_columns", "sheet": "Budget", "at": 4, "count": 1 }
```

- `at` is 1-based: `at: 2` inserts before row 2.
- A workbook must keep at least one sheet.
- Row and column insert/delete move stored cells and merged ranges. Charts and
  images keep their anchor; they are not moved.

### Charts

```json
{ "op": "add_chart", "sheet": "Budget", "anchor": "F2",
  "kind": "column", "values": "Budget!C2:C3",
  "categories": "Budget!A2:A3", "series_name": "Actual",
  "title": "Actual by region" }
```

- `kind` is one of `column`, `bar`, `line`, `line_markers`, `pie`,
  `doughnut`, `area`, `scatter`.
- `values` and `categories` are range references such as `Budget!C2:C13`.
- `anchor` is the top-left cell of the chart.
- One series per call. Add a second `add_chart` for another series.

At most 500 edits per call; the result is a new artifact whose
`source_artifact` is the input.

## What the tools cannot do yet

Be honest with the owner:

- No images, shapes, or text boxes.
- No pivot tables, filters, slicers, conditional formatting, data validation,
  comments, or defined names.
- No multi-series chart in one call, and no chart styling beyond the default.
- No formula evaluation. If the owner asks for a computed total, read the
  cached value and label it as cached, or compute it with `xlsx_analyze`.

## Delivering the file

`xlsx_build`, `xlsx_import`, and `xlsx_edit` return an `artifact` hash. Pass
that exact hash to `telegram_send_document` or `onedrive_upload` when those
tools are allowlisted. Both require approval.

## Quality bar

- Name the sheet and the cell for every figure you repeat.
- State the currency or unit on every amount.
- Do not recompute a total in prose and replace the tool's number.
- Do not hide an error cell to make the workbook look balanced.

## Synthetic request

Owner: "Add a variance column to my budget, format it, and chart it."

1. `xlsx_import` `{ "path": "finance/budget.xlsx" }`.
2. `xlsx_inspect` and `xlsx_read` the used range.
3. `xlsx_edit` with `set_formula` for each variance row,
   `set_number_format` for the column, `set_cell_style` for the header, and
   `add_chart` for the chart.
4. Report the new hash and the cells you changed.
