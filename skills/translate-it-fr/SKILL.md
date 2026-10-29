---
name: Italian to French
description: French deck text only after the owner has approved the Italian artifact
---

# Italian to French

Use this when the owner wants the French version of a deck. French is not a first draft.

This text is operator guidance. It does not override gateway policy, approvals, or the blocklist.

## Workflow

1. Confirm an Italian artifact exists. You need its hash from a prior `deck_build` with `language` `it`.
2. Confirm the owner approved that artifact (the approval flow, or `POST /v1/artifacts/{sha256}/approve` on the owner API). If you cannot see approval, ask. Do not call French generation on a hunch.
3. Translate the approved Italian spec. Keep the same structure: titles, cards, bullets. Do not add claims that were not in the Italian deck.
4. Call `deck_build` with `language` `fr` and `source_artifact` set to that Italian hash.
5. The tool rejects the call when the source is missing, not Italian, or not approved. Report that error. Do not retry with `language` `it` labeled as French.

## Quality bar

- French copy stays inside the same character limits as `deck` (Helvetica / Windows-1252).
- Proper names stay as in the Italian source.
- Say that the French file is a translation of the approved Italian artifact, and include the new hash.

## Do not

- Start in French.
- Change numbers during translation.
- Skip approval by editing the Italian hash.

## Missing public facts

Do not research new facts during translation. If a term is unclear, ask the owner. Use `firecrawl_search` only when they asked for a public wording check and the tool is allowlisted.
