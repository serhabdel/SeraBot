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

## Sign-in, users, and invitations

SeraBot is invitation-only. The operator (the bootstrap admin) mints
single-use invite codes; a new subject needs one to create an account, and an
existing subject signing in again does not. Sign-in is a thin OIDC
passthrough: the identity provider authenticates, and the returned `id_token`
is verified against the provider's JWKS before anything is stored. Sessions
are opaque random tokens, stored only as SHA-256 hashes.

| Method | Path | Notes |
| --- | --- | --- |
| `GET` | `/v1/auth/providers` | Public. `{providers: [{id, enabled}]}` in a fixed order. Microsoft is enabled in v1; Google and GitHub are reported `enabled: false` until configured, and clients render them visibly disabled. |
| `POST` | `/v1/auth/login/start` | Public. `{provider, invite_code?, return_to?, handoff_challenge?, client_state?}` → `{url}`. The last three are all-or-none (native clients); `return_to` must be exactly `http://127.0.0.1:<1024-65535>/auth-done` or `serabot://auth`, `handoff_challenge` is the 43-char base64url SHA-256 of the client's verifier, `client_state` is 8-128 URL-safe chars; anything else is `400`. Validates the provider and, when present, the invite (a stale code never reaches the provider). The invite is consumed at the callback, inside the transaction that creates the user, so an abandoned login never burns it. |
| `GET` | `/v1/auth/callback` | Public. Provider redirect target. Verifies state, nonce, and signature, upserts the namespaced subject (`microsoft:<sub>`), enforces `auth.max_users`. With a `return_to` it answers `302` to `return_to?handoff=<code>&state=<client_state>` (or `?error=<denied|invite_required|invite_invalid|disabled|max_users|server>&state=...`); without one it shows a "signed in, return to the app" page. No token is ever rendered. |
| `POST` | `/v1/auth/handoff` | Public. `{handoff, verifier}` → `200 {token, expires_at, user:{id,email,display_name,role}}`. The code is single use, valid 120 s, stored only as a SHA-256 hash, and bound to the S256 challenge; a wrong verifier, reuse, or expiry is `400 {"error":"invalid handoff"}` and burns the code. The session is minted at redemption. Audited as `auth.handoff.issued` / `.redeemed` / `.failed`. |
| `GET` | `/v1/me` | Authenticated. `{id, email, display_name, role}` for the caller. |
| `POST` | `/v1/auth/logout` | Authenticated. Revokes the calling session. A static config token has no session row and returns `409`. |
| `POST` | `/v1/auth/invites` | Admin only. `{uses}` → `{code}`; the raw code is returned once and only its hash is stored. |
| `GET` | `/v1/users` | Admin only. Lists accounts with role and status. |
| `POST` | `/v1/users/{id}/disable`, `/enable` | Admin only. Disabled accounts cannot authenticate; the bootstrap admin cannot be disabled. |

`[auth] enabled = false` is the default and every auth route fails closed while
it is off, so existing single-operator deployments are unaffected.

## User credentials, grants, and Telegram

Per-user credentials live in an envelope-encrypted store: a server master key
(`secrets.master_key_file`) wraps a per-user data key, which seals that user's
provider tokens with ChaCha20-Poly1305. Associated data binds every ciphertext
to its owner and provider, so a blob cannot be replayed across users. No
endpoint ever returns a secret.

| Method | Path | Notes |
| --- | --- | --- |
| `GET` | `/v1/providers` | The acting user's usable providers with a `credential` status (own, granted, or none). |
| `PUT` | `/v1/providers/{id}/key` | Stores the acting user's API key for that provider. |
| `DELETE` | `/v1/providers/{id}` | Removes the acting user's credential. |
| `POST` | `/v1/admin/grants` | Admin only. `{grantee, provider}` grants a member use of an admin-owned provider. Audited and revocable; the secret is never exposed. |
| `GET` | `/v1/admin/grants` | Admin only. Active grants. |
| `DELETE` | `/v1/admin/grants/{grantee}/{provider}` | Admin only. Revokes a grant; resolution fails closed immediately. |
| `GET` | `/v1/connectors` | The acting user's connector status (connected or not; never a token). |
| `POST` | `/v1/connectors/onedrive/start` | Returns the OneDrive consent URL (auth code + PKCE) for the acting user. |
| `GET` | `/v1/connectors/onedrive/callback` | Public. Exchanges the code and seals the refresh token under the user's data key. Plain HTML, no token echo. |
| `DELETE` | `/v1/connectors/onedrive` | Disconnects the acting user's drive. |
| `GET` | `/v1/telegram` | The acting user's Telegram binding status. |
| `POST` | `/v1/telegram/link` | Returns a single-use linking code (10 minutes). The user sends `/start <code>` to the bot. |
| `DELETE` | `/v1/telegram/link` | Unbinds the acting user's Telegram chat. |

Telegram keeps one server bot token (admin-managed). Inbound messages from a
bound `(telegram_user_id, telegram_chat_id)` act as that user; unbound chats
are ignored exactly as before. The configured owner pair is bootstrapped as the
admin's binding, so an existing single-operator install keeps working.

## Health and status

| Method | Path | Notes |
| --- | --- | --- |
| `GET` | `/v1/health` | `{ok, version, build}`; `503` if the database is unavailable. The database path is not returned. |
| `GET` | `/v1/status` | Pending tasks, pending approvals, loopback API status. |
| `GET` | `/v1/usage?days=7&bot=&conversation_id=` | Token rollup over `audit_log` rows with event `model.used`: `{totals: {calls, prompt_tokens, completion_tokens, total_tokens}, per_day: [...], per_bot: [...]}`. `days` is clamped to 1–90 (default 7); `bot` and `conversation_id` are optional filters. Read-only; egress-scanned. Use it to measure the re-read share of prompt tokens (see `docs/architecture-grok-team-review.md`). |

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

In `room:` conversations, `@bot-id` mentions in the owner text are parsed and
validated against the configured bot ids: in `all` mode the fan-out is
restricted to the mentioned members; in `selected` mode the orchestrator prompt
gets an `OWNER ADDRESSED: @…` clause. Mentions naming unknown bots are ignored
and never become instructions.

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

Catalog note: when a provider has `catalog_fetch` on, data egress approved, and
a fresh live `/models` snapshot, its advertised list in `/v1/models` is the
**full live catalog** (not just the configured allowlist). Selecting a model is
still gated: only ids present in the verified snapshot are accepted, so the
wider list cannot smuggle in an unverified model.

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

`GET /v1/bots/{id}` returns the owner-facing detail for one bot: `id`,
`display_name`, `role`, `description`, `avatar`, `enabled`, `provider`, `model`,
`fallback_provider`, `fallback_model`, `routing_classes`, `instructions`
(prompt body, capped at 32 KB), `instructions_bytes`, `skills`, `tools`,
`require_approval`, `browser_profile` (profile id, `default` when unassigned),
`memory_count`, `artifact_count`. No secrets or secret values are returned.
Unknown bots get `404`.

`PUT /v1/bots/{id}` accepts `enabled`, `prompt`, `tools`, `skills`,
`require_approval` and, since 0.16, `display_name` (1-48 characters),
`description` (0-280; empty clears), `model` (`{"provider","model"}` — the
provider must list the model — or `null` for the default model) and
`browser_profile` (an id from `GET /v1/browser/profiles`, or `null` for
`default`). At least one field is required; invalid values are `422` and change
nothing. The response's `bot` carries `display_name`, `description`, `model`
(model id or `null`) and `browser_profile`.

`PUT /v1/admin/bots/{id}/config` (owner token) updates `tools`, `skills`, and/or
`require_approval` for an existing bot:

```json
{"skills": ["ask", "triage"], "tools": ["skill_list", "rag_search"]}
```

At least one field is required. Validation mirrors scaffold rules (≤32 tools,
≤16 skills, `delegate` only on the orchestrator, tools must be registered,
skills must exist, `require_approval ⊆ tools`). The change is stored as an
admin overlay, audited as `config.bot_config_saved`, and the response is
`{"saved": true, "restart_required": true}`.

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
`GET /v1/artifacts/{hash}`, which only serves artifacts produced in the caller's
own conversations (404 otherwise). Entries also carry `mime`.

## Stop a reply (0.19)

- `POST /v1/conversations/{id}/cancel` asks the caller's running turns in that
  conversation to stop. Returns `{"conversation_id", "stopping"}`, where
  `stopping` counts the turns marked; 0 means nothing was running. Another
  user's conversation reads as not found.
- Stop is cooperative. The turn loop checks the mark before each model call,
  after each model reply, and before each tool call. A tool call already in
  flight finishes, the calls not yet started are dropped, and the turn ends
  with a single "Stopped." reply. Nothing is replayed, and a stop never
  retries an external effect.
- A turn that is waiting for an approval is not marked: decline the approval
  to end it. A turn still queued for a worker is not marked either; it runs.
- The mark is cleared when the task exits, whether it completed, parked for an
  approval, failed, or was superseded, so a late stop cannot end a later turn.

## Chat files (0.18)

- `POST /v1/conversations/{id}/attachments`: raw file body with an
  `X-Filename` header. Up to 25 MB; images (png, jpg, gif, webp), PDF, DOCX,
  XLSX, PPTX, TXT, MD, CSV; the content must match the extension; the blocklist
  is applied to the filename. Stored under the user's own inbox area
  (`<watch_dir>/<user_id>/chat/<conversation>/`). Returns
  `{"attachment": {"id", "filename", "mime", "size_bytes", "is_image"}}`.
- `POST /v1/messages` accepts `"attachments": ["<id>", ...]` (at most 6, the
  caller's own, unattached, uploaded to that conversation). Text may be empty
  when attachments are present. The bot receives names, types, sizes, the inbox
  path (for the document tools), and a bounded extracted text excerpt for TXT,
  MD, CSV and digital PDFs. Images are stored but not passed to a model yet.
- `GET /v1/conversations/{id}/messages` adds `attachments` to user messages.
- `GET /v1/attachments/{id}`: download an own upload.

## Local folders (0.18)

Desktop relay for the `local_list`, `local_read` and `local_write` bot tools
(`docs/specs/local-folders.md`). `POST /v1/local/poll` (body
`{"roots": [{"id","label","mode"}], "wait_secs": 0-25}`) registers the desktop's
allowlisted roots and long-polls for one request; `POST /v1/local/result/{id}`
answers it; `GET /v1/local/status` reports `online` and the registered roots.
All three act for the authenticated user only.

## Memory

Owner-scoped memory with mandatory provenance. Mutations are audited and
content is scanned by the content policy. Each turn injects the facts recalled
for the owner's message (ranked, bounded, framed as data) and the conversation
summary; `delegate` handoffs can attach up to five explicit ids. See
`docs/memory.md`.

| Method | Path | Notes |
| --- | --- | --- |
| `GET` | `/v1/memories?limit=100&offset=0&q=<text>&origin=<origin>&status=<status>&bot=<bot>` | Newest first. `origin` is `explicit`, `extracted`, or `all`; `status` is `active` (default), `superseded`, `pending`, or `all`. `q` is a lexical search over every memory that passes the filters (`query` is an older alias); a search ignores `offset`. At most 500 entries. |
| `POST` | `/v1/memories` | `{"kind": "preference", "content": "...", "source": "optional"}`. An exact repeat is reported as the existing memory. |
| `PATCH` | `/v1/memories/{id}` | `{"content": "...", "importance": 0.7, "status": "active"}`; every field is optional, unknown fields are rejected. |
| `GET` | `/v1/memories/stats` | Active count, counts by origin and status, and the last extraction time. |
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
name and presence. The `elevenlabs` entry additionally reports `auth`,
`auth_modes`, the `access_token`/`refresh_token` secret presence, and an
`oauth` metadata block (see *Connector OAuth sign-in* below). Use
`serabot secrets set <name>` on the server to supply any missing value.

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
| `elevenlabs` | `GET {api_base}/v2/voices?page_size=1` with `xi-api-key`, or with `Authorization: Bearer` when `auth = "oauth"` (requires auth either way, so it validates the credential) |

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

## Provider OAuth sign-in

Owner token required. Both routes live under `/v1/admin/providers/{id}/oauth/`.
Tokens are stored in the server's encrypted store and are never returned.

- `POST .../oauth/start` — `google-antigravity` (PKCE): body
  `{"challenge","state","redirect_uri"}` where `redirect_uri` is the console's
  loopback listener (`http://127.0.0.1:<port>/oauth-callback`). Returns
  `{"flow":"pkce","url":"https://accounts.google.com/..."}`. `muse-code` (device
  flow): no body. The server polls in the background (bounded to 15 minutes, one
  per provider) and returns `{"flow":"device","url","code"}`; the owner enters
  the code on the page and the key is stored when approved.
- `POST .../oauth/complete` — PKCE only: body `{"code","verifier","redirect_uri"}`.
  The server exchanges the code with the OAuth client secret already in its
  store (`antigravity_oauth_client_secret` by default), stores the access and
  refresh tokens under the provider's secret names, records the discovered Cloud
  Code project on an owner-managed provider that has none, and returns
  `{"signed_in":true,"project_saved":bool,"restart_required":true}`.

A provider of another kind answers `422`; an unknown id `404`. Audit entries
(`config.provider_oauth_signed_in` / `config.provider_oauth_failed`) carry the
provider id and kind only. `GET /v1/providers` reports sign-in state through
`secrets.refresh_token.configured` (PKCE) or `secrets.api_key.configured` (device).

## Connector OAuth sign-in (ElevenLabs)

Owner token required. Routes under `/v1/admin/connectors/{id}/oauth/`; only
`id = "elevenlabs"` is supported (others answer `404`).

- `POST .../oauth/start` — PKCE: body `{"challenge","state","redirect_uri"}`.
  The redirect URI must be the fixed loopback callback declared in the server's
  public CIMD document (`http://127.0.0.1:51122/oauth-callback`). Returns
  `{"flow":"pkce","url":"https://elevenlabs.io/app/oauth/authorize?..."}` with
  the CIMD `client_id`, the configured scope, and the S256 challenge.
- `POST .../oauth/complete` — body `{"code","verifier","redirect_uri"}`. The
  server exchanges the code at the pinned token endpoint
  (`https://api.us.elevenlabs.io/v1/oauth/token`, public client, no secret),
  stores the tokens under `elevenlabs_access_token` / `elevenlabs_refresh_token`
  in the age store (never returned), and answers
  `{"signed_in":true,"restart_required":true}`.

`PUT /v1/admin/connectors/elevenlabs` accepts `"auth": "api_key" | "oauth"`
(alongside `enabled`/`values`); the mode is kept in `admin-overrides.json` and
applies after restarting `serabot.service`. `GET /v1/connectors` reports the
elevenlabs entry with `auth`, `auth_modes`, the OAuth secret names, and an
`oauth` block (`client_id`, `authorize_url`, `cimd_path`, `callback_uri`,
`scope`). The public CIMD document itself is served unauthenticated at
`GET /.well-known/serabot-oauth-client.json` (`application/json`, `no-store`);
it contains no secrets. Audit: `config.connector_oauth_started` /
`config.connector_oauth_signed_in` carry the connector id only.
