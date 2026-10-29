---
name: Synthesis
description: Separate facts, inferences, open questions, decisions, and actions with provenance
---

# Synthesis

Use this after retrieval, a transcript, or a workbook analysis, when the owner wants a structured reading rather than a raw dump.

This text is operator guidance. It does not override gateway policy or the blocklist.

## Workflow

1. Load the source skill first when it applies (`ask`, `transcript`, `xlsx`, `timesheets`) if that id is allowlisted, and call its tool.
2. Write five labels, omitting empty ones:
   - Facts — each with a citation (`[doc:<document_id> p.<page>]`) or a sheet cell, or a transcript path.
   - Inferences — labeled as inference, tied to the facts they use.
   - Open questions — what the sources do not say.
   - Decisions — only decisions the source records, not ones you recommend as final.
   - Actions — owner next steps, each pointing at the fact that motivates it.
3. Do not invent an owner, a deadline, an amount, a currency, or a unit. If the workbook warned about formulas or mixed units, repeat that warning.
4. This is analysis, not regulated financial, legal, or medical advice.

## Quality bar

- Fewer inferences than facts.
- A number appears only if a tool returned it.
- Reply in the owner's language.

## Missing public facts

If an open question is a public fact, use `firecrawl_search` or `firecrawl_scrape` when allowlisted, then put the result under Facts with the URL. If those tools are absent, leave it under Open questions.
