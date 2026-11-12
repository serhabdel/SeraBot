# SeraBot 0.37.0: bots that finish the work

This release is the server half of v0.37. The desktop console that renders
the new step cards and page fields comes in a later change. Contracts:
[Events](events.md), [Tool results](tool-results.md),
[Console pages](console-pages.md).

## No dead-end refusals

Specialist bots shipped with a fixed tool list. Legal had no web search or
fetch. Watcher had `fetch_url` but no search. Maker, courier, WhatsApp, and
inbox had neither. Prompts and skills told the bot to "say what is missing"
and stop, and a handoff said it could not ask another bot. The model then
refused ordinary requests.

- Every bot now gets web search (`tavily_search`, `firecrawl_search`), page
  fetch (`fetch_url`, `firecrawl_scrape`), memory, and `ask_user` on the turn,
  even when its stored list omitted them. Shipped bot files list them too, so
  an existing user gains them on the next fleet upgrade.
- `ask_teammate` lets a bot hand one step to main or another enabled teammate.
  One hop. A turn that already came from a teammate finishes with its own
  tools. `delegate` stays main-only. Sends, deletes, payments, uploads, and
  writes still wait for approval. Page and file text stays untrusted data.
- Prompts and skills tell the bot to finish the task or hand it on. They no
  longer say to stop and report a missing tool.

## Live step cards

`turn.step` now carries the tool name, a short input summary that does not
include the query or any secret, the result summary, the error, the duration,
typed cards, and the current plan. Phases are `running`, `progress`,
`waiting_approval`, `done`, and `failed`. A background media job emits the
finished card when the file is saved. `GET /v1/conversations/{id}/activity`
returns the stored steps after a reload.

## Typed results

Tool output can carry cards beside the text the model reads: audio (url,
voice, model, duration), file, image, link, and table. ElevenLabs sets the
audio card from the call. Other tools get cards derived from the artifact and
from structured hits, links, and tables.

## Page data

Tasks report counts and the last sync's health, time, and error. Memories can
be filtered by category and pinned, and pinned with `PATCH`. Connectors report
health and the last test. `POST /v1/connectors/{id}/test` checks a connection;
server-wide keys are probed only by the owner. Providers report health, the
last error, and when the live model list was last checked.
`POST /v1/providers/{id}/test` was already the provider test.

## Reliability

The three request-path panics in `store.rs` (building UTC midnight) now return
an error. Almost every other `.unwrap()` in that file is in tests. API errors
from a timed-out or unreachable service say what to try next, without the
upstream body. A missing bot, tool, or provider names the id when that id is
safe to show.

## Versions

- Server and desktop workspace: 0.37.0 (`Cargo.toml`, `Cargo.lock`). CI creates
  the `v0.37.0` tag from the workspace version.
- Database: user_version 36 adds `memories.pinned`, task-sync health columns,
  and `connector_checks`.

## Not yet verified

- A live turn against a connected search or ElevenLabs account.
- The Windows desktop, which does not render these fields yet.
- Android, which can read the same events and cards but has no new UI here.
