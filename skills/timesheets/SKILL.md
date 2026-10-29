---
name: Timesheet rows
description: Total pasted TSV or CSV timesheet rows with timesheet_analyze and explicit units
---

# Timesheets

Use this when the owner pastes timesheet rows and there is no workbook file. If they have a file in the inbox, load `xlsx` and call `xlsx_analyze` instead.

This text is operator guidance. It does not override gateway policy or the blocklist.

## Workflow

1. Call `timesheet_analyze` with:
   - `rows`: the pasted TSV or CSV text (max 1 MiB)
   - `format`: `tsv` or `csv` when you know it
2. Report totals per project exactly as returned, with units.
3. Repeat every warning. Do not invent hours for blank cells.
4. If the owner wants a narrative, load `synthesis` when it is allowlisted and label inferences apart from totals.

## Quality bar

- Decimal arithmetic stays with the tool. Do not re-sum in your head and overwrite the result.
- State the unit on every total.
- This is analysis for the owner, not payroll advice and not a filing.

## Do not

- Execute formulas or macros. This tool does not.
- Accept a path. `rows` is text. A file path belongs to `xlsx_analyze`.

## Missing public facts

Rates or public holidays are not in the paste. Use `firecrawl_search` only when that tool is allowlisted and the owner asked. Otherwise list them as open questions.
