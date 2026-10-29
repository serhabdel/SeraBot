# Owner API

The API is loopback-only by default (`127.0.0.1:8787`). Terminate TLS in an
approved reverse proxy or tunnel. Every route requires:

```
Authorization: Bearer <owner token>
```

Tokens must be 32–512 bytes; the server stores only a SHA-256 hash in
`[api].token_hash`. Requests are rate limited (`server.rate_limit_per_minute`,
default 60/minute) and the body limit is `server.body_limit_bytes` (default
2 MiB). Error bodies are generic and never include internal details.

## Health and status

| Method | Path | Notes |
| --- | --- | --- |
| `GET` | `/v1/health` | `{ok, version, database}`; `503` if the database is unavailable. |
| `GET` | `/v1/status` | Pending tasks, pending approvals, loopback API status. |

## Chat

`POST /v1/messages`

```json
{"conversation_id": "windows-owner", "text": "hello", "bot": "main"}
```

- `conversation_id`: 1–128 characters, ASCII letters/digits and `_ - :` only.
- `text`: non-empty, at most 8,000 characters.
- `bot`: optional; defaults to `main`. Explicit `/ask`, `/deck`, `/xlsx`,
  `/timesheets`, `/brief`, and `/watch` commands still select their bot.

Response: `{"task": {"id", "kind", "bot", "status", "attempts", "created_at", "run_after"}}`.

Submitting a message queues work; it does not wait for the model. A dropped
response has an unknown outcome—read task state and history instead of blindly
resubmitting.

`GET /v1/tasks/{id}` returns the task view.

## Provider and model selection

`GET /v1/models?conversation_id=<id>&bot=<optional>`

```json
{
  "providers": [
    {"id": "default", "models": ["mistral-large-latest"], "data_egress_approved": false, "kind": "mistral", "catalog_fetch": false, "catalog_verified": false}
  ],
  "entries": [
    {"provider": "default", "model": "mistral-large-latest", "data_egress_approved": false, "catalog_verified": false}
  ],
  "selection": {"provider": "default", "model": "mistral-large-latest"},
  "effective_selection": {"provider": "default", "model": "mistral-large-latest"}
}
```

`PUT /v1/conversations/{id}/model`

```json
{"provider": "default", "model": "mistral-large-latest"}
```

Automatic routing:

```json
{"provider": "auto"}
```

Returns `{"selection": {...}}`. Unknown providers/models and unapproved
profiles are rejected; client-supplied endpoints are not accepted. The
selection applies to future turns only. See [model routing](model-routing.md).

## Fleet scaffold

`POST /v1/bots`

```json
{
  "id": "notes",
  "description": "Synthetic notes specialist.",
  "tools": ["skill_list", "rag_search"],
  "skills": ["ask"]
}
```

Creates a disabled specialist from a fixed template under the configured `bots`
and `prompts` directories. `id` is a slug. There is no path field. `tools` and
`skills` are optional; omitted tools default to `skill_list`, `skill_search`,
and `skill_load`. Unknown tools, `delegate`, and unknown skills are rejected.
The description is checked against the content policy. The response reports
`enabled: false` and `live: false` — restart before the bot can run, and add it
to `main.delegates` yourself if `main` should hand work to it.

See [Orchestration](orchestration.md).

## Skill packs

`GET /v1/skills` returns on-disk summaries (`id`, `name`, `description`,
optional `pack`). Bodies are omitted.

`POST /v1/skills`

```json
{
  "id": "notes-pack",
  "description": "Synthetic notes workflow.",
  "body": "# Notes\n\nUse allowlisted tools.\n"
}
```

Creates `<skills.dir>/<id>/SKILL.md`. `id` is a slug. There is no path field.
`description` and `body` are optional; the body is scanned with the content
policy before the directory is created. The response reports `live: false` —
restart and add the id to a bot allowlist before a bot can load it. Duplicate
ids conflict. Audit records the id and relative file, not the body. See
[skills](skills.md).

## Conversation tree

`POST /v1/conversations`

```json
{"kind": "group", "slug": "ops", "parent_id": "sec:finance"}
```

`kind` is `section`, `group`, `direct`, or `room`. The server assigns the id.
Sections and groups do not accept turns; `POST /v1/messages` to those ids is
rejected. Direct and room conversations do. Duplicate ids conflict.

## Conversation history

`GET /v1/conversations/{id}/messages?limit=100`

Returns visible `user` and `assistant` messages in chronological order
(`limit` 1–200, default 100). System, tool, and internal messages are excluded.
Message content that violates the current content policy is replaced with
`[Content withheld by policy]`. Messages are fetched only for the requested
conversation id.

## Approvals

| Method | Path | Notes |
| --- | --- | --- |
| `GET` | `/v1/approvals` | Pending approvals (max 100). |
| `POST` | `/v1/approvals/{id}/decide` | `{"decision": "once" \| "always" \| "deny", "note": "optional"}` |

Approval requests bind canonical arguments, destination, artifact hash, policy
version, and expiry. Changing any of these requires a new approval. Expired
approvals cancel suspended tasks.

## Events, audit, and artifacts

| Method | Path | Notes |
| --- | --- | --- |
| `GET` | `/v1/events?after=<event id>&limit=100` | Server-Sent Events; replays after a known id; 15-second keepalive. |
| `GET` | `/v1/audit?limit=50` | Newest audit entries; no content or secrets. |
| `GET` | `/v1/artifacts/{sha256}` | Downloads a stored PDF with sanitized filename. |
| `POST` | `/v1/artifacts/{sha256}/approve` | Approves Italian deck content for French generation. |

Event kinds include `message`, `approval`, `task.queued`, `turn.completed`,
`memory.added`, `memory.deleted`, `approval.grants_revoked`,
`model.selected`, and `model.used`. Client code should treat unknown event
kinds as informational.

## Polling guidance

The client should stay well inside the rate limit. The bundled desktop client
uses at least 3 seconds between requests and alternates task/history polling
about every 5 seconds (roughly 12–20 reads per minute). On `429`, wait for the
`Retry-After` window.
