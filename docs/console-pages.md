# Console page data (0.37)

The Tasks, Memory, Connectors, and Providers pages read these fields. Older
clients ignore the new ones. Nothing here returns a secret, a key, or a
provider response body.

## Tasks

`GET /v1/tasks` still returns `tasks` and `limit`. It also returns:

| Field | Meaning |
| --- | --- |
| `counts` | `{ "todo", "doing", "done" }` for this user. |
| `sync.provider` | Chosen sync provider, or `none`. |
| `sync.health` | `ok`, `error`, `not_configured`, or `never_run`. |
| `sync.last_run` | When the last attempt finished, or null. |
| `sync.last_ok` | Whether that attempt succeeded, or null. |
| `sync.last_error` | A short, secret-free reason, or null. |

`GET /v1/tasks/sync/status` returns the same health fields beside `prefs` and
`signed_in`. `POST /v1/tasks/sync/run` records the attempt, including a
failure, before it returns.

Each task still has `status` (`todo`, `doing`, `done`), `due`, and `updated_at`.

## Memory

Each memory already has `source` (where the fact came from) and `kind`.
`category` is the same value as `kind`: `preference`, `fact`, `decision`,
`context`, or `note`.

0.37 adds `pinned` (boolean). Pinned rows sort first.

| Call | Use |
| --- | --- |
| `GET /v1/memories?category=fact&pinned=true` | Filter. `pinned` is `true` or `false`. |
| `PATCH /v1/memories/{id}` | `{ "pinned": true }` pins one fact. Other fields are unchanged. |
| `GET /v1/memories/stats` | `stats.pinned` counts active pinned facts. |

## Connectors

Each object from `GET /v1/connectors` gains:

| Field | Meaning |
| --- | --- |
| `health` | `ok`, `error`, `disabled`, `not_configured`, or `unknown`. |
| `last_error` | The last failed test's fixed message, or null. |
| `last_checked` | When that test ran, or null. |

`POST /v1/connectors/{id}/test` runs a connection check for the signed-in user
and stores that result.

- Per-user connectors (ElevenLabs, Composio, OneDrive, token connectors, Google
  and Microsoft sign-in) report whether **this** account is connected. They do
  not call the provider again and they never return a token.
- Server-wide keys (Tavily, Firecrawl, AgentMail) are probed only by the
  owner. A member receives `422` with code `owner_only`.
- The reply is `{ "id", "ok", "health", "message", "checked_at" }`. `message`
  is one of a fixed set of sentences.

## Providers

Each object from `GET /v1/providers` gains:

| Field | Meaning |
| --- | --- |
| `health` | `ok`, `error`, or `unknown`. |
| `last_error` | What to do when the last live model fetch failed, or null. |
| `last_checked` | When this account last fetched or failed, or null. Lost on restart; the catalog is in memory. |

`status` is unchanged: `ok`, `key_rejected`, `unreachable`, or `unchecked`.

`POST /v1/providers/{id}/test` is the test-connection call. The reply now also
includes `health`, `last_error`, and `last_checked`. It still never returns a
key or a response body.
