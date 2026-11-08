// Generated from crates/serabot-desktop/src/capabilities.rs so the phone
// groups a bot's tool ids into the same plain-language abilities.

class Ability {
  const Ability(this.group, this.label, this.hint, this.tools, {this.needs});
  final String group;
  final String label;
  final String hint;
  final List<String> tools;
  final String? needs;

  bool isOn(List<String> have) => tools.every(have.contains);

  /// Tools after switching this ability on or off; other tools are untouched.
  List<String> toggled(List<String> have, bool on) {
    final out = have.where((t) => !tools.contains(t)).toList();
    if (on) {
      out.addAll(tools);
    }
    return out;
  }
}

const abilities = <Ability>[
  Ability('Web', 'Use the web browser', 'Open pages, click and type for you.', ['browser_navigate', 'browser_snapshot', 'browser_click', 'browser_type', 'browser_press', 'browser_screenshot', 'browser_close', 'browser_tabs', 'browser_new_tab']),
  Ability('Web', 'Use your passwords & keys', 'Sign in with logins you saved. It never sees the values.', ['vault_list']),
  Ability('Web', 'Search and read websites', 'Fetch pages and search the web.', ['fetch_url', 'firecrawl_scrape', 'firecrawl_map', 'firecrawl_search', 'firecrawl_crawl', 'firecrawl_extract', 'exa_search', 'exa_get_contents']),
  Ability('Web', 'Watch sources for news', 'Check the sources you follow.', ['watch_now']),
  Ability('Files', 'Read and write documents', 'Word, Excel, PowerPoint and PDF files, and your indexed documents.', ['document_index', 'rag_search', 'docx_build', 'docx_import', 'docx_pdf', 'docx_inspect', 'docx_read', 'docx_edit', 'xlsx_analyze', 'timesheet_analyze', 'xlsx_build', 'xlsx_import', 'xlsx_inspect', 'xlsx_read', 'xlsx_edit', 'pptx_import', 'pptx_pdf', 'pptx_inspect', 'pptx_edit', 'pdf_inspect', 'pdf_edit', 'pdf_merge']),
  Ability('Files', 'Build slide decks', 'Turn an outline into a presentation.', ['deck_build']),
  Ability('Files', 'Inspect, resize and convert images', 'PNG, JPEG and WebP: sizes and camera info, smaller copies, format changes.', ['image_inspect', 'image_resize', 'image_convert']),
  Ability('Files', 'Use your cloud files and notes', 'OneDrive, Google Drive, Sheets and Notion.', ['onedrive_upload', 'onedrive_list', 'google_drive_list', 'google_sheets_read', 'notion_query_database'], needs: 'a cloud account'),
  Ability('Files', 'Work with folders on this PC', 'Browse and search big folders you allow in Settings, read files, bring Word, Excel, PowerPoint, PDF and image files in to work on, and with your approval save results back. Works only while the SeraBot app is open on this PC.', ['local_list', 'local_search', 'local_stat', 'local_read', 'local_write', 'local_import', 'local_export']),
  Ability('Messages', 'Read and send email', 'Gmail, Outlook and AgentMail.', ['gmail_list_messages', 'outlook_list_messages', 'agentmail_inboxes_list', 'agentmail_send'], needs: 'an email account'),
  Ability('Messages', 'Check your calendar', 'Google and Outlook calendars.', ['google_calendar_list_events', 'outlook_calendar_list_events'], needs: 'a calendar'),
  Ability('Messages', 'Send files to Telegram', 'Share finished files in your Telegram chat.', ['telegram_send_document'], needs: 'Telegram linked'),
  Ability('Messages', 'Read and post in Slack', 'Read channels the bot is in. Posting a message asks you first.', ['slack_list_channels', 'slack_read_channel', 'slack_post_message'], needs: 'Slack'),
  Ability('Code and tasks', 'Use GitHub', 'Search repositories and read issues and files. Opening issues and comments ask you first.', ['github_search_repos', 'github_list_issues', 'github_get_issue', 'github_create_issue', 'github_comment_issue', 'github_get_file'], needs: 'GitHub'),
  Ability('Code and tasks', 'Manage your to-do list', 'Read your Todoist tasks. Adding and completing tasks ask you first.', ['todoist_list_tasks', 'todoist_add_task', 'todoist_complete_task'], needs: 'Todoist'),
  Ability('Media', 'Work with voice and audio', 'Transcribe recordings and speak replies.', ['audio_transcribe', 'audio_understand', 'elevenlabs_list_voices', 'elevenlabs_text_to_speech', 'elevenlabs_sound_effect', 'elevenlabs_transcribe', 'elevenlabs_generate_image', 'elevenlabs_generate_video']),
  Ability('Memory', 'Remember things for you', 'Save and recall notes between chats.', ['memory_save', 'memory_list', 'memory_search', 'memory_update', 'memory_delete']),
  Ability('Memory', 'Use skills', 'Look up and follow the skills you added.', ['skill_list', 'skill_search', 'skill_load']),
];
