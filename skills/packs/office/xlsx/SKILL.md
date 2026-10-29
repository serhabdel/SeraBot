---
name: Spreadsheet analysis
description: Bounded workbook analysis with provenance via xlsx_analyze
---

# XLSX / spreadsheets

1. Confirm the workbook path is relative to the configured inbox.
2. Call `xlsx_analyze` with `{ "path": "<relative>" }`.
3. Report totals exactly as returned, with sheet/cell provenance and warnings.
4. Do not execute macros, recalculate formulas, or guess cached values.

For pasted timesheet rows (no file), use `timesheet_analyze` instead.
