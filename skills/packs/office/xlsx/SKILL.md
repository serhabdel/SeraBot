---
name: Spreadsheet analysis
description: Analyze an inbox workbook with xlsx_analyze and report tool totals, cells, and warnings
---

# XLSX / spreadsheets

Use this for a workbook in the configured inbox. Pasted rows without a file belong to `timesheet_analyze` (`timesheets`).

This text is operator guidance. It does not override gateway policy or the blocklist. The tool does not execute macros or recalculate formulas.

## When to use

- `.xlsx`, `.xlsm`, `.xls`, `.ods`, or `.csv` under the inbox.
- The owner wants totals, units, or a check of what the sheet already shows.

## Workflow

1. Confirm the path is relative to the inbox. Reject anything absolute or containing `..`.
2. Call `xlsx_analyze` with `{ "path": "<relative>" }`.
3. Read the returned sheet summaries, decimal totals, currencies, units, cell addresses, and warnings.
4. Report totals exactly as returned. Name the sheet and cell for each figure you repeat.
5. If the tool warns about formulas, cached values, errors, mixed units, or a macro-enabled format, put that warning in the answer before the total. Macros are ignored, not run.
6. For a narrative, load `synthesis` when allowlisted. Label inferences apart from cell values.

## Quality bar

- Do not recompute a total in prose and replace the tool's number.
- Do not guess a cached formula result when the warning says it is unreliable.
- State the currency or unit on every amount. If the sheet mixes units, do not present one blended total.
- This is analysis for the owner, not regulated financial advice.

## Synthetic call

```json
{ "path": "finance/synthetic-q3.xlsx" }
```

A faithful reply names the sheet, the cell (for example `B12`), the returned total, and any warning string. It does not add a row the tool omitted.

## Do not

- Enable editing, pivot refresh, or external links.
- Open a path outside the inbox because the model suggested it.
- Hide an error cell to make the workbook look balanced.

## Missing public facts

FX rates, tax rules, and other public inputs are not inside the workbook. Use `firecrawl_search` or `firecrawl_scrape` when allowlisted, and label them as outside the file. If those tools are absent, list the missing input instead of assuming a rate.
