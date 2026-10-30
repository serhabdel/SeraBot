# Owner API

The API is loopback-only by default (`127.0.0.1:8787`). Terminate TLS in an
approved reverse proxy or tunnel. Every route requires:

```
Authorization: Bearer <owner token>
```

Tokens must be 32–512 bytes. Two kinds are accepted:

- **Device tokens** minted with `serabot token mint --label <label> [--days N]`.
  The plaintext token is printed once; only its SHA-256 hash is stored in the
  `device_tokens` table. `serabot token list` shows id, label and timestamps but
  never hashes. `serabot token revoke <id>` disables a token immediately;
  revoked and expired tokens return `401`. `last_used_at` is refreshed at most
  once per hour. Omitting `--days` means no expiry; revocation is the control.
- The static `[api].token_hash` fallback, kept for existing deployments.

Run token commands as the service user so the SQLite WAL stays consistent:

```bash
sudo runuser -u serabot -- /opt/serabot/serabot --config /etc/serabot/serabot.toml \
  token mint --label desktop-win
```

Treat the printed token as a credential: deliver it through a secure channel,
never in a prompt, ticket or commit.

Requests are rate limited (`server.rate_limit_per_minute`, default 60/minute)
and the body limit is `server.body_limit_bytes` (default 2 MiB). Error bodies
are generic and never include internal details. Token values and hashes are
never logged.

## Public routes

These routes are unauthenticated and intended for client distribution and
health probing. They expose no data and read no database.

| Method | Path | Notes |
| --- | --- | --- |
| `GET` | `/` or `/app` | Embedded landing page. Strict CSP (`default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`), inline CSS only, no scripts. Download links appear only when the server was built with `SERABOT_RELEASES_REPO`. |
| `GET` | `/app/latest.apk` | `302 Found` to `https://github.com/<repo>/releases/latest/download/serabot-mobile-latest.apk`; `Cache-Control: no-store`. `503` when no release repository is compiled in. |
| `GET` | `/app/SHA256SUMS` | `302 Found` to the release `SHA256SUMS` asset; same cache and `503` behavior. |
| `GET` | `/healthz` | `{ok, version, build}`; `build` is the compiled `SERABOT_BUILD_SHA` or `dev`. No database details. |

Redirect targets come from a validated build-time `SERABOT_RELEASES_REPO`
(`owner/repo`); request input is never used in a path. Authentication, rate
limits, and the body limit apply to every `/v1/*` route exactly as before.

## Health and status

| Method | Path | Notes |
| --- | --- | --- |
| `GET` | `/v1/health` | `{ok, version, build}`; `503` if the database is unavailable. The database path is not returned. |
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

`GET /v1/bots` lists bot files under the configured bots directory. Each entry
is `id`, `role` (`orchestrator` or `specialist`), `enabled`, `live`, `tools`,
and `skills`. Prompt text, model endpoints, secret names, and approval lists
are omitted. `live` is true only when that id is already loaded. A bot created
with `POST /v1/bots` shows up here with `enabled: false` and `live: false`
until you restart.

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

`GET /v1/conversations?limit=50&cursor=<opaque>&include_archived=false`

Lists conversations ordered by activity (`updated_at` descending). Archived
conversations are hidden unless `include_archived=true`. Each entry is `id`,
`kind`, `parent_id`, `title`, `created_at`, `updated_at`, `last_message_at`,
`archived_at`, `unread_count`, and `accepts_turns`. `unread_count` counts
assistant messages newer than the conversation's `last_read_at`; reading that
conversation's messages updates `last_read_at`. `next_cursor` is `null` on the
final page; pass it back verbatim (it is an opaque `timestamp|id` pair, not a
secret).

`PATCH /v1/conversations/{id}` sets a title:

```json
{"title": "Finance reviews"}
```

Titles are trimmed, whitespace-collapsed, 1–120 characters, and scanned by the
content policy.

`DELETE /v1/conversations/{id}` soft-archives: the row and every message stay,
`archived_at` is set, and the conversation leaves default lists.

`POST /v1/conversations/{id}/unarchive` clears `archived_at` and returns
`{"id": "...", "archived": false}`. Unknown ids are 404. Both archive moves are
audited.

## Group members and library

`GET /v1/conversations/{id}/team` returns `{"members": [...], "mode":
"selected|all", "rules": "..."}` for a `room:` conversation, or `{"members": [],
"mode": "selected", "rules": ""}` when none is set. Other kinds are rejected.

`PUT /v1/conversations/{id}/team` accepts
`{"members": ["analyst", "maker"], "mode": "selected", "rules": "..."}`. Members
are 1–5 enabled bots; `rules` is optional owner-written group instruction text,
at most 4,000 characters, scanned by the content policy. Non-empty rules are
appended to the system prompt of every turn in that room, for the orchestrator
and for members alike, as bounded owner data — operating rules, approvals, and
the content policy still win. The write is audited with the member list, mode,
and rules.

`GET /v1/conversations/{id}/library` lists artifacts produced inside one
conversation, newest first (at most 100). Each entry is `action_id`, `tool`,
`hash`, `created_at`, and `filename`/`title` when the artifact sidecar has them.
Rows come from gateway actions joined to the conversation's turns, so nothing is
produced outside the approval path. Download the bytes through
`GET /v1/artifacts/{hash}`.

## Memory

Owner-scoped memory with mandatory provenance. Mutations are audited and
content is scanned by the content policy. The orchestrator system prompt
includes the 20 most recently updated memories as owner-approved facts;
`delegate` handoffs can attach up to five explicit ids.

| Method | Path | Notes |
| --- | --- | --- |
| `GET` | `/v1/memories?limit=100&query=<text>` | Newest first. `query` filters kind, content, and source case-insensitively. At most 500 entries. |
| `POST` | `/v1/memories` | `{"kind": "preference", "content": "...", "source": "optional"}` |
| `DELETE` | `/v1/memories/{id}` | Deletes exactly one memory. |

Kinds: `preference`, `fact`, `decision`, `context`, `note`. Content is 1–4,000
characters. `source` defaults to `operator:desktop` and records provenance.

## Providers and connectors

Read-only status. Configuration, enablement, OAuth login, and secret values
stay file/CLI-side; these routes never accept writes.

`GET /v1/providers` lists each configured provider with `id`, `kind`, `models`,
`routing_classes`, `data_egress_approved`, `catalog_fetch`, `catalog_verified`,
and a `secrets` object. Every secret entry is
`{"name": "...", "configured": true|false}` — names and presence only, never
values. Catalogs come from local state; this route does not call providers.

`GET /v1/connectors` lists Google Drive, Google Sheets, Google Calendar, Gmail,
Outlook, Outlook Calendar, Notion, AgentMail, Tavily, Firecrawl, and ElevenLabs.
Every entry carries a console `group` (`Productivity`, `Email`, `Web Research`,
or `Voice & Media`). OAuth entries report
`enabled`, `redirect_uri`, and `client_id`/`client_secret`/`refresh_token`
secret names plus presence booleans. API-key entries (AgentMail, Tavily,
Firecrawl, ElevenLabs) report `enabled`, `api_base`, and their `api_key` secret
name and presence. Use `serabot secrets set <name>` on the server to supply any
missing value.

### Test a connector

`POST /v1/admin/connectors/{id}/test`  (owner token; no body)

Checks that a connector's stored credentials are usable. It sends at most one
read-only request per API-key connector, bounded to 10 seconds, and never
changes configuration:

| Connector | Request |
| --- | --- |
| `agentmail` | `GET {api_base}/inboxes?limit=1` |
| `tavily` | `POST {api_base}/search` with `max_results: 1` (this consumes one search credit) |
| `firecrawl` | `GET {api_base}/v2/team/credit-usage` |
| `elevenlabs` | `GET {api_base}/v1/user` |

OAuth connectors (`google_drive`, `google_sheets`, `google_calendar`, `gmail`,
`outlook`, `outlook_calendar`, `notion`) do not contact the provider. They
report only whether all required secrets are stored.

Response (`200`):

```json
{
  "id": "elevenlabs",
  "ok": true,
  "message": "Key accepted",
  "checked_at": "2026-10-30T10:00:00Z"
}
```

`ok` is `false` with one of these fixed messages: `Missing secret: <name>`,
`Key rejected (401)` or `Key rejected (403)`, `Rate limited (429)`,
`Provider returned HTTP <status>`, `Timed out`, `Could not connect`,
`Request failed`, or `Connector api_base must be HTTPS`. Messages never include
secret values or provider response bodies. An unknown connector id returns
`404`. Each test is recorded in the audit log as `config.connector_tested`
with the connector id and result only.

## Conversation history

`GET /v1/conversations/{id}/messages?limit=100`

Returns visible `user` and `assistant` messages in chronological order
(`limit` 1–200, default 100). System, tool, and internal messages are excluded.
Message content that violates the current content policy is replaced with
`[Content withheld by policy]`. Messages are fetched only for the requested
conversation id.

An assistant message may include `name` when it is a bot id: the orchestrator
on a room turn, or the specialist after `delegate`. Names that are not bot ids
are omitted. See [Orchestration](orchestration.md).

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
| `GET` | `/v1/events?after=<event id>&limit=100` | Server-Sent Events; replays after a known id (`after` wins over the standard `Last-Event-ID` reconnect header); 15-second keepalive. |
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
