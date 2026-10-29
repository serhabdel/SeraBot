# Connectors

Optional integrations under `[connectors.*]` in `serabot.toml`. Every connector
defaults to **`enabled = false`**. Config stores **secret-store key names
only**. Values belong in the age store (`serabot secrets set <name>`). The
repository has no client secrets.

Tools register only when that connector is enabled, and only if the named
client id and client secret (or AgentMail API key) are present. A missing
value fails startup closed and names the key, not the value. OAuth read tools
also refuse to leave the host until the refresh-token key is set. Once it is
set, those reads are external effects: the gateway requires approval and
records the API origin as the destination.

There is no loopback OAuth callback server. Authorization URL construction and
token exchange (authorization-code and refresh-token grants) are implemented
so an owner can finish login later. A refresh token from either grant is
written to that connector's configured age-store key. A failed write fails
the call. Token values are not logged.

`redirect_uri`, when set, must be `http://127.0.0.1:<port>/<path>` or
`http://localhost:<port>/<path>` with no query, fragment, or credentials.

| Section | Tool | Scope | Default secret names |
| --- | --- | --- | --- |
| `google_drive` | `google_drive_list` | `drive.readonly` | `google_oauth_client_id`, `google_oauth_client_secret`, `google_drive_refresh_token` |
| `google_sheets` | `google_sheets_read` | `spreadsheets.readonly` | same Google client, `google_sheets_refresh_token` |
| `google_calendar` | `google_calendar_list_events` | `calendar.readonly` | same Google client, `google_calendar_refresh_token` |
| `gmail` | `gmail_list_messages` | `gmail.readonly` | same Google client, `gmail_refresh_token` |
| `outlook` | `outlook_list_messages` | `Mail.Read` + `offline_access` | `outlook_client_id`, `outlook_client_secret`, `outlook_refresh_token` |
| `outlook_calendar` | `outlook_calendar_list_events` | `Calendars.Read` + `offline_access` | same Outlook client, `outlook_calendar_refresh_token` |
| `notion` | `notion_query_database` | set on the Notion integration | `notion_oauth_client_id`, `notion_oauth_client_secret`, `notion_refresh_token` |
| `agentmail` | `agentmail_inboxes_list`, `agentmail_send` | API key | `agentmail_api_key` |

Google authorize/token hosts are `accounts.google.com` and
`oauth2.googleapis.com`. Microsoft uses
`login.microsoftonline.com/common/oauth2/v2.0`. Notion uses
`api.notion.com/v1/oauth`. Read calls go to the public Drive, Sheets,
Calendar, Gmail, Microsoft Graph, and Notion API origins.

## What the owner configures later

1. Create an OAuth client (Google Cloud, Microsoft Entra, or a Notion public integration) and a loopback redirect if you will build an authorization URL.
2. `serabot secrets set` the client id, client secret, and refresh token under the key names above. Do not commit the values.
3. Set `enabled = true` for that section only.
4. Add the tool to a bot allowlist. Reads and AgentMail sends ask for approval.

AgentMail is native HTTPS against `api_base` (default `https://api.agentmail.to/v0`).
Docs: [agentmail.to/docs](https://www.agentmail.to/docs/api-reference). Send needs
`default_inbox_id` or an `inbox_id` argument of letters, digits, `_`, or `-`.

Specialist scaffolds may list these tools. OAuth reads and both AgentMail tools
are in the external-effect list, so a scaffold puts them in `require_approval`.
