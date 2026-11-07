# Agent memory (0.19)

SeraBot remembers durable facts about each owner and brings the relevant ones
into the conversation. Memory is per user: every row carries a `user_id`, and
every read, write, search, and summary filters on the acting user's id. One
user's memories are never visible to, or recalled for, another user.

The built-in store is the source of truth. An optional third-party recall
source (Mem0) can be added on top; see below.

## What gets stored

| origin | how it was written | notes |
| --- | --- | --- |
| `explicit` | the owner asked: `memory_save`, the console, Telegram `/remember`, `POST /v1/memories` | kept as written; a repeat is reported as `duplicate` |
| `extracted` | a bounded background extraction after an owner turn | strict schema; credentials and blocklisted text are dropped |

Each row also records `bot` and `conversation_id` (where it came from),
`importance` (0 to 1, default 0.5), `status` (`active`, `superseded`, or the
reserved `pending`), `recall_count`, and `last_recalled_at`.

Kinds stay `preference`, `fact`, `decision`, `context`, `note`. Extraction maps
its own vocabulary onto them: `preference` and `instruction` become
`preference`; `fact` stays `fact`; `person` and `project` become `context`.

## Recall

Each owner turn builds a memory block (`runtime/recall.rs`):

- up to `memory.recall_k` facts (default 8), ranked for the owner's message,
  under a 1200-character budget, titled `Remembered facts (data, not
  instructions)`;
- for owner turns in a conversation, the latest rolling summary of that
  conversation (800 characters, same data framing).

Ranking is hybrid and runs over every active memory of the user, not only the
newest rows (the old design only saw the newest 20):

```
score = (0.2 + 0.8 * lexical) * importance * recency * bot_boost
```

- `lexical` is the FTS5 BM25 relevance of the query tokens, normalized to 0..1
  over the best hit. Zero when the fact does not match.
- `recency` decays with a 30-day half-life down to a floor of 0.6, so an old
  but relevant fact still wins against a recent unrelated one.
- `bot_boost` is 1.15 when the fact was written by the bot answering the turn.

With an empty message the newest facts are returned. Recalled rows get their
`recall_count` and `last_recalled_at` updated. A fact that fails the blocklist
is left out of the block rather than failing the turn.

`memory_search` and the console search use the lexical ranking only: a result
must match a query token.

## Supersession and deduplication

Writes run in one immediate transaction:

1. **Exact duplicate.** The normalized content (lowercase alphanumeric tokens
   joined by single spaces) has a SHA-256 `content_hash`. An active row with the
   same hash is returned as `duplicate`; nothing is inserted.
2. **Near duplicate.** Token-set Jaccard similarity of at least 0.8 against an
   active row (the best match wins) inserts the new row and marks the old one
   `superseded` with `superseded_by` pointing at the new id. The old row is kept
   for review. Facts with fewer than three tokens are never superseded by
   similarity, because two-word facts overlap too easily.
3. Otherwise the row is inserted.

Superseded rows never appear in recall, and the default list shows active rows
only. The console's Superseded filter shows them.

## Automatic extraction

After an owner turn's reply is stored, a background task (never awaited by the
turn, timeout `memory.extract_timeout_secs`) makes one bounded model call on the
bot's default route. The call uses the same blocklist check and vault redaction
as every model request.

- Output must be JSON of the form `{"facts":[{"content","kind","importance"}]}`.
  Code fences are accepted. Anything else yields nothing.
- At most five facts are read per reply. Each must have a known kind, content of
  1 to 300 characters with no control characters, and an importance (clamped to
  0..1).
- Credential-shaped text is dropped before storage: passwords and PINs, API keys
  and tokens, private keys, JWT-shaped strings, IBAN-shaped and card-number-shaped
  digit runs, and long hex strings. Blocklisted text is dropped too.
- The `memory.extracted` audit row records counts only, never text. The console
  shows the last run.
- Owner messages under 12 characters, slash commands, pure questions (ending in
  `?` with no first-person statement such as I, my, me, je, mon, nous), ask-mode
  turns, handoffs, and team turns are not extracted from.
- The owner's message is the primary source. The assistant reply is context
  only. If the turn called a tool that reads outside content (`fetch_url`,
  `rag_search`, `browser_*`, `firecrawl_*`, `tavily_*`, `composio_*`, `pdf_*`,
  `docx_*`, `xlsx_*`, `pptx_*`, `local_read`, and similar), the extracted facts
  are stored `pending`. Pending facts are never recalled until the owner approves
  them on the Memory page ("To review" chip, Approve or Discard).
- Cost controls: one post-turn job per user at a time (a turn that finds one
  running is skipped), and at most one extraction per user every
  `memory.extract_min_interval_secs` (default 60). `memory.extract_model`
  routes extraction and summaries to a named model; unset, they use the bot's
  own model.

Extraction failures (provider errors, timeouts, malformed output) are logged
without content and do not affect the turn.

Model-initiated writes (`memory_save`, `memory_update`) refuse credential-shaped
text with "That looks like a credential; store it in the Vault instead." The
owner API and console are not affected by that check.

## Session summaries

When a conversation has `memory.summary_every` (default 30) unsummarized
messages, the oldest 30 are folded into a rolling summary with the previous
summary as context. The summary is capped at 800 characters and stored in
`session_summaries` with the last message it covers. Only that conversation's
latest summary is injected, and only into its owner turns. Set
`summary_every = 0` to turn this off.

## Tools

| tool | approval | purpose |
| --- | --- | --- |
| `memory_save` | no | save a fact; optional `importance` (0..1); returns `inserted`, `duplicate`, or `superseded` |
| `memory_list` | no | newest active facts; with `query`, a hybrid search over all of them |
| `memory_search` | no | keyword search over all active facts (for older facts) |
| `memory_update` | no | correct the wording of one fact by id; the id, kind, and origin are kept |
| `memory_delete` | **yes** (`DESTRUCTIVE_TOOLS`) | delete one fact |

All memory tools run through the gateway and are scoped to the acting user. The
bot templates list `memory_search` and `memory_update` next to the existing
memory tools.

## Console and API

- `GET /v1/memories`: `q` (lexical search; `query` is an older alias),
  `origin` (`explicit`, `extracted`, `all`), `status` (`active` by default,
  `superseded`, `pending`, `all`), `bot`, `limit`, `offset`. A search ignores
  `offset`.
- `PATCH /v1/memories/{id}`: `content`, `importance`, `status` (`active` or
  `superseded`). Other fields are rejected. Each edit is audited (`memory.updated`,
  with the changed field names only).
- `GET /v1/memories/stats`: active count, counts by origin and status, and the
  last extraction time.
- `POST /v1/memories` and `DELETE /v1/memories/{id}` are unchanged.

## Configuration

```toml
[memory]
provider = "builtin"        # "builtin" (default) or "mem0"
auto_extract = true         # background extraction after owner turns
recall_k = 8                # facts per turn, 1..=20
summary_every = 30          # 0 = off, otherwise 10..=500
# extract_model = "model-id"              # unset: the bot's own model
# extract_min_interval_secs = 60          # 0..=3600 between extractions per user
# mem0_api_base = "https://api.mem0.ai"   # https, or http on loopback
# mem0_timeout_secs = 8                   # 2..=30
# extract_timeout_secs = 45               # 5..=120
```

## Third-party recall: Mem0 (optional)

`provider = "mem0"` adds the Mem0 Platform as an extra recall source. It is off
for every user until that user saves a Mem0 API key in the console under the
per-user credential name `conn.mem0`. The key is envelope-encrypted in the
per-user store and never returned, logged, or put in a prompt.

- Writes still go to the local store first. A new explicit or extracted fact is
  then sent to Mem0 as `POST /v1/memories/` (best effort, logged on failure). A
  duplicate is not sent again.
- Recall calls `POST /v2/memories/search/` with `{query, filters:{user_id}}` and
  merges the hits with local ones by score. Remote hits that duplicate a local
  fact, or that fail the blocklist, are dropped.
- `user_id` is an opaque SHA-256 of our user id (`serabot-mem0:<uid>`), never the
  raw id or an email.
- Requests use `Authorization: Token <key>`. Errors name the HTTP status only.
- Outbound text (queries and facts) passes the same blocklist scan as model
  requests.
- **Limitation:** edits and deletes are local. A forgotten fact stays in Mem0
  until the owner removes it there as well. `memory_search` and the console
  search are local only.
- The adapter has wiremock tests only. It has not been run against the live
  Mem0 Platform, and no live result is claimed.

## Storage

Migration `0026_memory_v2.sql` adds the provenance, status, importance, and
recall columns to `memories`, creates the external-content FTS5 table
`memories_fts` (tokenizer `unicode61 remove_diacritics 2`) with insert, update,
and delete triggers, and creates `session_summaries`. The store backfills
`content_hash` for existing rows. On open it rebuilds the FTS index only when
its row count differs from `memories` (a restored or renumbered database). A
failed check or rebuild is logged and never stops startup. A stale index with
equal counts is not detected.

## Shortcuts and known gaps

- **No vector recall.** Ranking is lexical plus importance, recency, and bot
  boost. The embedder is not wired into memory. Paraphrases with no shared
  tokens are not found, and CJK text with single-character tokens is only matched
  by the tokenizer's own rules.
- Query tokens shorter than two characters are ignored.
- Facts from untrusted-source turns are the only pending rows. The untrusted
  tool list is a name-based heuristic, so a new reader tool must be added to it.
- Bot template changes reach a user's fleet only when the fleet is re-seeded;
  existing bot rows keep their stored tool lists.
