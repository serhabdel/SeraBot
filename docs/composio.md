# Composio

Each member pastes their own Composio API key in Connectors. The server
checks it with Composio (`GET /api/v3/toolkits`), seals it in the per-user
envelope-encrypted store, and never returns it. Nothing is configured
server-wide except the optional `[composio]` block (`api_base`,
`timeout_secs`, `max_response_bytes`).

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

Execution requests send no `version` field, so Composio applies its default
toolkit version.

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
| 502 | `composio_rate_limited` | Composio is rate limiting requests (429). |
| 502 | `composio_unavailable` | Composio returned 5xx, or could not be reached (no response). |

SeraBot never answers an upstream failure with 401, because the console
treats 401 as an expired session. A `403` from SeraBot itself means no key is
saved yet, not a policy block.

## Troubleshooting

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

## Bot tools (through the gateway)

- `composio_search_tools`: find actions for a task.
- `composio_connect`: get a sign-in link for an app.
- `composio_execute`: run an action with JSON arguments. Asks for approval
  only when the action slug looks destructive or sending (delete, remove,
  send, post, publish, transfer, and similar).

Results are untrusted data, bounded in size, and pass through vault
redaction and the blocklist like every other tool output.

## Not yet: Composio MCP tool router

Composio also offers an MCP tool router. SeraBot does not use it yet. The
current integration calls the REST API above, one tool at a time, through
the gateway. A tool router would need its own approval and audit design
before it could be added.
