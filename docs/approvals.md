# Tool approvals

Only destructive tools need owner approval. Every other tool runs on the
model's judgement.

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
| `github_create_issue` | Opens an issue in a repository the user's own GitHub token can write to. |
| `github_comment_issue` | Posts a comment on a GitHub issue or pull request. |
| `todoist_add_task` | Adds a task to the user's Todoist. |
| `todoist_complete_task` | Completes a Todoist task (a recurring task moves on). |

## Not destructive (no approval)

- Browser: `browser_navigate`, `browser_snapshot`, `browser_click`, `browser_type`, `browser_press`, `browser_screenshot`, `browser_close`. Typing a vault secret into a page whose host is not in the entry's allowed domains still fails closed.
- Vault: `vault_list`.
- Memory: `memory_save`, `memory_list`.
- Research and media: `firecrawl_scrape`, `firecrawl_map`, `firecrawl_search`, `firecrawl_crawl`, `firecrawl_extract`, `tavily_search`, `fetch_url`, `watch_now`, `audio_transcribe`, `audio_understand`, `elevenlabs_list_voices`, `elevenlabs_text_to_speech`, `elevenlabs_sound_effect`, `elevenlabs_transcribe`, `elevenlabs_generate_image`, `elevenlabs_generate_video`.
- Documents (produce new artifacts): `docx_build`, `docx_import`, `docx_inspect`, `docx_read`, `docx_edit`, `docx_pdf`, `xlsx_build`, `xlsx_import`, `xlsx_inspect`, `xlsx_read`, `xlsx_edit`, `xlsx_analyze`, `timesheet_analyze`, `pptx_import`, `pptx_inspect`, `pptx_edit`, `pptx_pdf`, `pdf_inspect`, `pdf_edit`, `pdf_merge`, `deck_build`, `document_index`, `rag_search`.
- Local folders (read only): `local_list`, `local_read`. They only work while the desktop is online and only inside folders the user allowed.
- Read-only connectors: `google_drive_list`, `google_sheets_read`, `google_calendar_list_events`, `gmail_list_messages`, `outlook_list_messages`, `outlook_calendar_list_events`, `notion_query_database`, `agentmail_inboxes_list`, `onedrive_list`, `github_search_repos`, `github_list_issues`, `github_get_issue`, `github_get_file`, `todoist_list_tasks`.
- Orchestration: `delegate`, `team_ask`, `team_reply`, `skill_list`, `skill_search`, `skill_load`.

## Bot `require_approval` lists

By default a bot's stored `require_approval` list is ignored for
non-destructive tools, so existing stored lists need no migration. Set
`[approvals] respect_bot_lists = true` to let those lists add approval again.
The seed templates list only destructive tools.

Approvals keep the same binding: user, canonical arguments, destination,
artifact hash, policy version and expiry. Changing any of them needs a new
approval.
