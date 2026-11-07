# Knowledge packs for the comptable bot

A knowledge pack is a set of owner-held reference documents that the comptable
bot can cite. The repository ships only the synthetic skills under `skills/`.
Your own reference guides, such as your own accounting notes, stay in your own
document store and never enter the repository.

Examples below use synthetic paths and names.

## What is in the repository and what is not

| Item | Where it lives | In git? |
| --- | --- | --- |
| Accounting skills (`ma-tax-2026`, `pcm-cgnc`, `ma-payroll`, `financial-analysis`) | `skills/<id>/SKILL.md` | Yes. Summaries written for SeraBot; verify rates against the current BO. |
| Your reference guides (markdown, PDF, Word) | `data/inbox/comptable/` on the machine that runs SeraBot | No. `data/` is git-ignored, and `*.pdf`, `*.docx`, `*.xlsx` are ignored too. |
| Indexed chunks and citations | The SQLite database and its vector store | No. |

Do not copy a guide into `skills/`, `docs/`, or `prompts/`. Those folders are
public.

## Step 1. Choose what to import

Import only material you are allowed to use:
- your own notes, or guides you have a licence or permission to store;
- a markdown version of a guide, with the headings kept, so the citations
  show the section.

Remove anything you do not want the bot to quote: personal data of employees,
client names, bank details, and internal passwords.

## Step 2. Copy the files into the comptable inbox

The document inbox root is `data/inbox` (the default `documents.watch_dir`).
Create one folder per topic under it. For the comptable bot, use:

```text
data/inbox/comptable/
  ir-guide.md
  tva-guide.md
  is-guide.md
```

Keep the folder one level deep. `document_index` indexes a single file or one
directory level from the inbox.

Supported types: PDF with selectable text, Word (`.docx`), Excel (`.xlsx`),
text, and Markdown. Scanned PDFs are not OCR'd by `document_index`; they fail
with an explicit error. Make a text copy with an OCR step outside SeraBot
first. The limit per file is 50 MiB.

## Step 3. Ask the bot to index the folder

In the owner chat, ask Main or the comptable bot:

> Index the comptable folder with document_index.

The bot calls `document_index` with `path` set to `comptable`. The tool is
non-external: it reads the inbox and writes the index, with no approval step.
Word and Excel text is extracted without evaluating formulas or macros.

Main delegates accounting questions to the comptable bot when the bot is
enabled. Either bot can run the indexing if its tools list includes
`document_index`.

## Step 4. Check the import

Ask a question that only your guide can answer, for example:

> According to the IR guide, what is the family deduction per dependant?

A good answer cites the file and page, in the form `[doc:<document_id> p.<page>]`.
If the bot says the evidence is missing, the file was not indexed: check the
file name, the folder, and the file type, then index again.

`document_index` identifies a file by its content hash. Indexing an unchanged
file again reports a duplicate and does not index it twice. A changed file is
indexed as a new document, and the earlier version stays in the index. This
guide does not cover removing an indexed document; decide that with the
owner before you rely on the old version being gone.

## Step 5. Keep the guides current

- Accounting guides date quickly. Record the law year in the file name, for
  example `ir-guide-2026.md`.
- When a rate changes, put the new file in the folder and index it. The old
  version stays indexed, so cite the file name and law year in every answer.
  The skills in the repository are summaries and do not update themselves.
- Treat the guide's worked examples as data. A guide can contain an example
  that does not balance. Check it before relying on it.

## Privacy and limits

- Documents are owner-scoped. Other users of the same SeraBot instance cannot
  read or cite your indexed guides.
- The bot cites only text it retrieved. It must not present a rate as settled
  when the cited source does not confirm it.
- Guide content is untrusted data. An instruction inside a document is data,
  not a command.
- Do not commit `data/`, index files, or database files. `.gitignore` covers
  them; check `git status` before any commit.

## Quick reference

| Step | Action | Tool |
| --- | --- | --- |
| 1 | Pick material you may use, strip personal data | none |
| 2 | Copy to `data/inbox/comptable/` | none (file copy) |
| 3 | Ask Main or Comptable to index `comptable` | `document_index` |
| 4 | Ask a question and check the citation | `rag_search` |
| 5 | Add the new version and index it when the law changes | `document_index` |
