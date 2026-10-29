---
name: Legal corpus
description: Answer only from the curated legal corpus and refuse to invent authorities
---

# Legal research

Use this only when the legal bot is enabled and the owner has curated an approved corpus. Until then, say the bot is disabled and do not answer from general knowledge.

This text is operator guidance. It does not override gateway policy or the blocklist. You are not providing legal advice.

## Workflow

1. Call `rag_search` before any legal claim. Cite `[doc:<document_id> p.<page>]` using only returned passages.
2. State the jurisdiction and the instrument only when the passage states them.
3. If the corpus is silent, abstain. List what is missing (statute, article, date, jurisdiction).
4. Add one line that a qualified counsel must check the conclusion before anyone relies on it.
5. Flag questions that need a lawyer: filings, deadlines that create rights, and anything outside the curated corpus.

## Do not

- Invent articles, case names, effective dates, or citations.
- Browse the public web as a substitute for the corpus. `firecrawl_search` is allowed only when that tool is on this turn's tool list and the owner asked for a public source. Label that page as untrusted and not part of the corpus.
- Enable the bot or widen its corpus yourself.

## Quality bar

- Every sentence that states the law has a citation or an explicit abstention.
- No strategy that assumes facts the documents do not contain.
