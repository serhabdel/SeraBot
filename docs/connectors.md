# Connectors framework

Optional integrations under `[connectors.*]` in `serabot.toml`. Each connector defaults to
**`enabled = false`** and names **secrets only** (values in the age store).

OAuth connectors register scaffold tools until flows are implemented. **AgentMail** uses a native HTTPS client.

| Section | Tools | Default secret names |
| --- | --- | --- |
| `google_drive` | `google_drive_list` | `google_oauth_client_id`, `google_oauth_client_secret`, `google_drive_refresh_token` |
| `google_sheets` | `google_sheets_read` | … `google_sheets_refresh_token` |
| `google_calendar` | `google_calendar_list_events` | … `google_calendar_refresh_token` |
| `gmail` | `gmail_list_messages` | … `gmail_refresh_token` |
| `outlook` | `outlook_list_messages` | `outlook_client_id`, `outlook_client_secret`, `outlook_refresh_token` |
| `outlook_calendar` | `outlook_calendar_list_events` | … `outlook_calendar_refresh_token` |
| `notion` | `notion_query_database` | `notion_oauth_*`, `notion_refresh_token` |
| `agentmail` | `agentmail_inboxes_list`, `agentmail_send` | `agentmail_api_key` |

AgentMail API: [agentmail.to/docs](https://www.agentmail.to/docs/api-reference).
