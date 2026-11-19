# Tool approvals

Only destructive tools need owner approval (0.28). Every other tool —
including ElevenLabs MCP generation and OpenRouter image generation — runs on
the model's judgement without an approval prompt.

A tool is destructive when it deletes or irreversibly overwrites user data,
or sends something to a third party that cannot be recalled. The list lives in
`DESTRUCTIVE_TOOLS` (`crates/serabot/src/gateway.rs`); keep this page in sync.

## Destructive (approval required)

| Tool | Why |
| --- | --- |
| `memory_delete` | Deletes a stored memory. |
| `agentmail_send` | Sends email to a third party. |
| `telegram_send_document` | Sends a file into a chat. |
| `onedrive_upload` | Writes (and may overwrite) a file in the owner's OneDrive. |
| `local_write` | Creates or overwrites a text file in a read-write local folder on the owner's PC (through the desktop). |
| `local_export` | Saves an artifact into a read-write local folder on the owner's PC. Approval binds the destination and the artifact hash. |
| `github_create_issue` | Opens an issue in a repository the user's own GitHub token can write to. |
| `github_comment_issue` | Posts a comment on a GitHub issue or pull request. |
| `todoist_add_task` | Adds a task to the user's Todoist. |
| `todoist_complete_task` | Completes a Todoist task (a recurring task moves on). |
| `slack_post_message` | Posts a message that everyone in a Slack channel can read. The approval names the channel. |
| `whatsapp_send_message` | Sends a WhatsApp message to an external contact via WhatsApp Business Cloud API. The approval names the recipient and exact text. |
| `whatsapp_send_template` | Sends an approved WhatsApp template message to an external contact. The approval names the recipient and template. |
| `whatsapp_link_send` | Sends a message from the member's linked WhatsApp number to one of their allowed chats. The approval names the chat and binds the text. |

Not approval-gated (0.63): `whatsapp_link_reply_file` and `telegram_reply_file`
send a file the member asked for back into the same WhatsApp or Telegram chat
the conversation came from. They take no recipient, check that the member owns
the file, pass the blocklist, and count toward the WhatsApp per-minute and
daily caps. See `docs/specs/whatsapp-linked-device.md`.

## Not destructive (no approval)

- Browser: `browser_navigate`, `browser_snapshot`, `browser_click`, `browser_type`, `browser_press`, `browser_screenshot`, `browser_close`. Typing a vault secret into a page whose host is not in the entry's allowed domains still fails closed. Typing a vault secret always asks for approval bound to the entry name and the page host, on an https page only (see `docs/vault.md`). "Always" is never a standing grant for a vault secret: it acts as "once".
- Vault: `vault_list` and `vault_request_secret` are not destructive; the second one asks the owner for the value in a secure card instead.
- Memory: `memory_save`, `memory_list`.
- Research and media: `firecrawl_scrape`, `firecrawl_map`, `firecrawl_search`, `firecrawl_crawl`, `firecrawl_extract`, `tavily_search`, `fetch_url`, `watch_now`, `audio_transcribe`, `audio_understand`, `elevenlabs_mcp_search_tools`, `elevenlabs_mcp_describe_tool`, `elevenlabs_mcp_call` (0.28: MCP calls, including generation, run without approval; a name the server marks as changing data is still refused), `openrouter_generate_image`.
- Documents (produce new artifacts): `docx_build`, `docx_import`, `docx_inspect`, `docx_read`, `docx_edit`, `docx_pdf`, `xlsx_build`, `xlsx_import`, `xlsx_inspect`, `xlsx_read`, `xlsx_edit`, `xlsx_analyze`, `timesheet_analyze`, `pptx_import`, `pptx_inspect`, `pptx_edit`, `pptx_pdf`, `pdf_inspect`, `pdf_edit`, `pdf_merge`, `deck_build`, `document_index`, `rag_search`.
- Local folders (read only): `local_list`, `local_search`, `local_stat`, `local_read`, `local_import`. They only work while the desktop is online and only inside folders the user allowed.
- Image tools (image_inspect, image_resize, image_convert): run without approval; results are new artifacts with metadata removed.
- Read-only connectors: `google_drive_list`, `google_sheets_read`, `google_calendar_list_events`, `gmail_list_messages`, `outlook_list_messages`, `outlook_calendar_list_events`, `notion_query_database`, `agentmail_inboxes_list`, `onedrive_list`, `github_search_repos`, `github_list_issues`, `github_get_issue`, `github_get_file`, `todoist_list_tasks`, `slack_list_channels`, `slack_read_channel`, `whatsapp_phone_info`, `whatsapp_list_templates`, `exa_search`, `exa_get_contents`.
- Orchestration: `delegate`, `team_ask`, `team_reply`, `skill_list`, `skill_search`, `skill_load`.
- Fleet reads (v0.50, `main` only, audited): `bot_list`, `bot_read`, `model_list`, `handoff_status`. `model_list` shows accounts as id, label and state only.
- Chat cards: `ask_user` (asks the owner a question in the chat; it sends nothing outside SeraBot and returns at once). See [API](api.md#chat-cards-023).

## Fleet management (approval required) — v0.50

These tools belong to `main` and run only in a turn the owner started. A
handoff, team, help or routine turn never gets them, and a specialist cannot be
granted them. Each call is bound to the user, the canonical arguments, the
policy version and the expiry; changing any argument needs a new approval.
None of them accepts a standing grant, and YOLO never covers them: "always"
acts as "once".

| Tool | Why |
| --- | --- |
| `bot_set_route` | Changes a bot's route, fallbacks, failover policy or thinking level. The target must be `main` or one of its delegates, and idle (no running task, no approval pending). It can never grant cross-provider consent. |
| `bot_set_enabled` | Pauses one of `main`'s delegates (it also leaves `main`'s delegate list). It cannot switch a bot on or touch `main`. |
| `bot_set_delegates` | Sets `main`'s delegate list to enabled specialists. |
| `subagent_retry` | Runs a failed subagent again, at most twice per handoff, never after an effect with an unknown outcome. Approval is needed only when `route_override` names a full route (a possibly new provider); `{chain_index}` stays inside the bot's own chain and needs none. |

Every change goes through the same path as the owner's own edit (`PUT
/v1/bots/{id}`), including the policy scan and the `bot.profile_changed` audit
row with the before and after values. A bot never widens `tools`, `skills`,
`require_approval`, its instructions or egress consent.

## Bot `require_approval` lists

By default a bot's stored `require_approval` list is ignored for
non-destructive tools, so existing stored lists need no migration. Set
`[approvals] respect_bot_lists = true` to let those lists add approval again.
The seed templates list only destructive tools.

Approvals keep the same binding: user, canonical arguments, destination,
artifact hash, policy version and expiry. Changing any of them needs a new
approval.

## YOLO (auto-approve) — 0.28

The per-conversation YOLO switch auto-approves non-destructive calls through
the full approval chain (action and approval rows are still created and bound,
and the audit ledger records `approval.auto_approved`). The owner sets it in
the console, or a bot may set it itself with `yolo_toggle` when its judgement
says the task should run without prompts; every self-enable is audited as
`yolo.self_enabled`. **YOLO never covers a destructive tool** — those always
wait for the owner's own decision, even with YOLO on.
