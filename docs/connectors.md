# Connectors

Optional integrations under `[connectors.*]` in `serabot.toml`. Every connector
defaults to **`enabled = false`**. Config stores **secret-store key names
only**. Values belong in the age store (`serabot secrets set <name>`). The
repository has no client secrets.

## Per-user connectors (OneDrive)

OneDrive is the first connector that is **per user** rather than per server.
The `[onedrive]` section now describes availability and defaults (the server's
app registration, tenant, folder, conflict behavior); each user connects their
own Microsoft account and their token is sealed under their own data key in the
envelope-encrypted credential store, never in the shared age store.

- The operator's existing global refresh token is imported into the admin's
  credential rows once, losslessly and idempotently; a later disconnect is not
  undone by a restart.
- `onedrive_upload` and `onedrive_list` resolve the acting user's client. A
  user with no connected drive gets an explicit "connect OneDrive first" error
  and never the admin's drive.
- Token rotation writes back to the credential's owner: the acting user, or the
  grantor when the provider was granted by the admin.
- The consent flow is authorization code + PKCE against the single server app
  registration, with `Files.ReadWrite` and `offline_access` added incrementally
  at connect time.

Members connect from their console; the API surface is documented in
[Owner API](api.md#user-credentials-grants-and-telegram).

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

## Personal token connectors (GitHub, Todoist, Slack, Exa)

These connect with a token each user pastes in the console, like Notion. The
server verifies it with one cheap read, seals it in the per-user credential
store, and never shows it again. Tools always run as the acting user with that
user's own token; a user who has not connected gets a "connect it first" answer
and never anyone else's access. The API bases live under `[token_connectors]`
(`github_api_base`, `todoist_api_base`, `slack_api_base`, `exa_api_base`,
`timeout_secs`, `max_response_bytes`). Bases must be HTTPS. Every tool goes
through the gateway, results are untrusted data, responses are size capped, and
the token is redacted from all output. A `401` shows "<name> rejected your
token" without echoing it. Writes ask for approval and the approval is bound
to where the effect lands (the repository, the channel, the task).

### GitHub

Tools: `github_search_repos`, `github_list_issues`, `github_get_issue`,
`github_get_file` (read) and `github_create_issue`, `github_comment_issue`
(write, approval).

1. Open <https://github.com/settings/personal-access-tokens/new> and create a
   **fine-grained personal access token**.
2. Under **Repository access**, choose only the repositories your bots need.
3. Under **Repository permissions**, set **Issues: Read and write** and
   **Contents: Read-only** (Metadata read is added automatically). Leave the
   rest at No access. Drop Issues to read-only if bots should never write.
4. Generate it, copy it, and paste it into the GitHub card in Connectors.

Search results are paged (30 per page at most, 10 pages), files over 200 KB are
refused, and file text is cut at 20,000 characters. The REST API version header
is pinned to `2022-11-28`.
### Todoist

Tools: `todoist_list_tasks` (read), `todoist_add_task` and
`todoist_complete_task` (write, approval).

1. In Todoist open **Settings, Integrations, Developer** (direct link:
   <https://app.todoist.com/app/settings/integrations/developer>).
2. Copy the **API token** and paste it into the Todoist card in Connectors.

The connector uses the current unified REST API (`/api/v1`, cursor paging),
not the deprecated v2 endpoints. Todoist API tokens are full account access and
have no scopes, so keep the write tools behind approval. Task ids are
validated before any request, listings are capped at 50 per page, and a `403`
from Todoist is treated as a rejected token (that is how Todoist reports one).
### Slack

Tools: `slack_list_channels`, `slack_read_channel` (read) and
`slack_post_message` (write, approval; the approval names the channel and
thread).

1. Go to <https://api.slack.com/apps> and choose **Create New App**, from
   scratch, in your workspace.
2. Under **OAuth & Permissions**, add these **Bot Token Scopes**:
   `channels:read`, `groups:read`, `channels:history`, `groups:history`,
   `chat:write`. (Add `chat:write.public` only if the bot should post in
   channels it has not joined.)
3. **Install to Workspace**, then copy the **Bot User OAuth Token** (it starts
   with `xoxb-`) into the Slack card in Connectors. Other token types are
   refused.
4. In Slack, invite the bot to each channel it should read or post in
   (`/invite @your-app`).

Slack reports most errors inside an HTTP 200; the connector maps them to plain
messages (revoked token, bot not in the channel, missing scope with the scope
name). Channel ids (`C…` or `G…`) are validated before any request, history is
capped at 50 messages per call, and posting refuses `@channel`, `@here` and
`@everyone` mentions and turns link previews off.