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
Requests carry `x-api-key` and the SeraBot user id as Composio `user_id`.

## Bot tools (through the gateway)

- `composio_search_tools`: find actions for a task.
- `composio_connect`: get a sign-in link for an app.
- `composio_execute`: run an action with JSON arguments. Asks for approval
  only when the action slug looks destructive or sending (delete, remove,
  send, post, publish, transfer, and similar).

Results are untrusted data, bounded in size, and pass through vault
redaction and the blocklist like every other tool output.
