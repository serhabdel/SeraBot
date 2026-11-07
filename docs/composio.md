# Composio

Each member pastes their own Composio key in Connectors. The server detects the
kind of key by its prefix, checks it with Composio, seals it (with the kind) in
the per-user envelope-encrypted store, and never returns it. Nothing is
configured server-wide except the optional `[composio]` block (`api_base`,
`mcp_base`, `timeout_secs`, `max_response_bytes`, `tool_version`).

## Key kinds

| Prefix | Kind | Sent as | To | Checked with |
| --- | --- | --- | --- | --- |
| `ak_` | project key | `x-api-key` | REST API (`api_base`) | `GET /api/v3/toolkits` |
| `uak_` | user key | `x-user-api-key` | REST API | `GET /api/v3/toolkits` |
| `ck_` | Composio Connect (For You) consumer key | `x-consumer-api-key` | Connect MCP (`mcp_base`, default `https://connect.composio.dev/mcp`) | MCP `initialize` + `tools/list` |
| other | unknown | `x-api-key`, then once `x-user-api-key` after a 401 | REST API | as before |

A prefixed key goes only to its own header; a `ck_` key is never sent to the
REST API. Composio keeps Connect (consumer) connections apart from developer
project connections, so a Connect key sees the apps linked in Composio Connect,
not apps linked through a project key. For a `ck_` key the console shows "Your
apps are managed in Composio Connect" with a button that opens
`https://dashboard.composio.dev/`; linking and unlinking happen there. The apps
list is read from the MCP `tools/list` (apps are the slug prefix of each tool).

## Routes (all act on the signed-in user)

- `PUT /v1/connectors/composio/key` `{"key": "..."}`: validate and store.
- `DELETE /v1/connectors/composio`: forget the key.
- `GET /v1/connectors/composio/toolkits?q=`: search apps.
- `GET /v1/connectors/composio/accounts`: this user's linked apps.
- `POST /v1/connectors/composio/connect` `{"toolkit": "gmail"}`: hosted
  sign-in link (`url`, https only). Open it in the system browser.
- `DELETE /v1/connectors/composio/accounts/{id}`: unlink an app the user owns.

`GET /v1/connectors` lists `composio` with `connected` for the caller.

## Composio endpoints used

`GET /api/v3/toolkits`, `GET /api/v3/tools`, `GET/POST /api/v3/auth_configs`,
`POST /api/v3/connected_accounts/link`, `GET /api/v3/connected_accounts`,
`DELETE /api/v3/connected_accounts/{id}`, `POST /api/v3/tools/execute/{slug}`.
Requests carry the SeraBot user id as Composio `user_id`, and the key in one
of two headers:

- `x-api-key`: a project API key (the default, and what the console asks for).
- `x-user-api-key`: a user-level key. When Composio refuses a key sent as
  `x-api-key` with a 401, the server tries it once as `x-user-api-key`. If
  that works, the key is stored with that header kind and every later call
  uses it. The header kind is stored inside the same sealed value as the key;
  rows saved before this rule are read as project keys.

Tool execution sends `"version": "latest"` by default. Composio uses an
OLD pinned toolkit version (`00000000_00`) when `version` is omitted on /api/v3,
so the field is always sent. To pin a version, set `tool_version` in the
`[composio]` block (for example `00000000_00`). The value must be 1-32 letters,
digits, `.`, `-`, or `_`; a malformed value stops startup.

## Errors the console shows

Composio's own failures are mapped to a stable `code` and a message that is
safe to show. The message keeps Composio's `message` and its
`suggested_fix` when it sends one. The key is removed from every text first.

| HTTP | `code` | Meaning |
| --- | --- | --- |
| 422 | `composio_rejected` | Composio refused the key (401 or 403). |
| 422 | `composio_plan_required` | Composio needs a paid plan or credits (402). |
| 422 | `composio_not_found` | The app or tool does not exist, or is not linked (404). |
| 422 | `composio_bad_request` | Composio refused the request (other 4xx). |
| 503 | `composio_rate_limited` | Composio is rate limiting requests (429). The call was refused before it ran. |
| 502 | `composio_unavailable` | Composio returned 5xx, or could not be reached (no response). |

Only `composio_unavailable` (502) and transport failures are unknown outcomes:
the action may or may not have run, so do not retry it blindly. A rate limit
(503) is known not to have run.

SeraBot never answers an upstream failure with 401, because the console
treats 401 as an expired session. A `403` from SeraBot itself means no key is
saved yet, not a policy block.

## Troubleshooting

- **Which key do I paste?** SeraBot accepts project keys (`ak_`), user keys
  (`uak_`) and Connect keys (`ck_`). A `ck_` key is not a project key: it only
  works on the Connect MCP endpoint. Copy it from Composio Connect → Settings →
  Sessions & API Key. The rejection message names the kind detected from the
  prefix and where to get the right one; it never repeats the key.
- **"Composio rejected this API key (…)"**: the key is wrong, revoked, or
  from another Composio project. Copy the project API key from
  platform.composio.dev → Settings → API keys, and paste it again. Do not
  paste a key from another product or a sign-in token.
- **"Composio needs a plan or credits for this request"**: the account has
  no credits left for this action, or the plan does not include it. Check
  the billing page on platform.composio.dev. Nothing is wrong with the key.
- **"Composio could not find that"**: the app slug or tool slug is wrong,
  or the app is not linked for this user. Search again with
  `composio_search_tools`, or link the app with `composio_connect`.
- **"Composio is rate limiting requests"**: wait a minute and try again.
  Nothing is lost; the request was not run.
- **"Composio could not be reached"**: Composio is down, or this server
  cannot reach `backend.composio.dev` (proxy, firewall, or DNS). Try again
  later. If the message gives an HTTP status, Composio itself answered with
  an error.
- **"Composio is not connected yet"**: no key is saved for this account.
  Add the key under Connectors.

## Connect over MCP

`composio_mcp.rs` is a small MCP Streamable HTTP client: JSON-RPC 2.0 over
HTTPS `POST` to `mcp_base` only (no redirects). Each operation runs its own
short session: `initialize`, the `notifications/initialized` notification, then
`tools/list` (up to 5 pages, 200 tools) or `tools/call`, then a best-effort
`DELETE` of the session. The `Mcp-Session-Id` and `MCP-Protocol-Version`
headers are echoed after `initialize`. A reply may be plain JSON or
`text/event-stream`; the event carrying the request id is used and the stream is
dropped after it. Bodies are capped at `max_response_bytes`, and requests use
`timeout_secs`.

The bot tools are the same for every key kind, so approvals, policy scanning,
the blocklist and audit apply unchanged:

- `composio_search_tools` filters `tools/list` by the query words. If nothing
  matches, it returns the whole list with a note that `COMPOSIO_SEARCH_TOOLS`
  (run through `composio_execute`) finds app actions.
- `composio_execute` runs `tools/call` with the slug and arguments. The result
  is reshaped to `successful` / `error` / `data`; an MCP `isError` becomes
  `successful: false`.
- `composio_connect` returns the Composio Connect link instead of a sign-in link.
- Approval: a destructive slug (as above) needs approval, and so do Connect meta
  tools that can run anything (`COMPOSIO_MULTI_EXECUTE_TOOL`,
  `COMPOSIO_REMOTE_WORKBENCH`, `COMPOSIO_REMOTE_BASH_TOOL`, other `COMPOSIO_*`
  except search, get and manage-connections). Writes never run without it.

## Bot tools (through the gateway)

- `composio_search_tools`: find actions for a task.
- `composio_connect`: get a sign-in link for an app.
- `composio_execute`: run an action with JSON arguments. Asks for approval
  only when the action slug looks destructive or sending (delete, remove,
  send, post, publish, transfer, and similar).

Results are untrusted data, bounded in size, and pass through vault
redaction and the blocklist like every other tool output.

## Not used: the project tool router

Composio's per-project tool router is not used. REST keys call one tool at a
time; Connect keys use only the consumer MCP endpoint above, always through the
gateway.

## Rolling back to 0.18

Keys saved by 0.19 are sealed as a small JSON value that records the header
kind. 0.18 reads the whole value as the key, so after rolling back, every
member who connected Composio in 0.19 must paste the key again in Connectors.
Keys saved by 0.18 keep working on 0.19 without any change.
