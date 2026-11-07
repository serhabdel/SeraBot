//! Plain-language capabilities.
//!
//! The server stores a bot's tools as raw ids (`browser_navigate`,
//! `vault_list`, ...). Nobody should have to read those to decide what a bot
//! may do, so the console groups them into a few things a person understands
//! and shows a toggle for each. The raw ids stay available under "Advanced".
//!
//! Toggling a capability on adds all of its tools; toggling it off removes
//! them. Tools no capability covers are never touched.

/// One switch in the Bots screen.
pub struct Capability {
    pub label: &'static str,
    pub hint: &'static str,
    pub tools: &'static [&'static str],
}

pub const CAPABILITIES: &[Capability] = &[
    Capability {
        label: "Use the web browser",
        hint: "Open pages, click and type for you.",
        tools: &[
            "browser_navigate",
            "browser_snapshot",
            "browser_click",
            "browser_type",
            "browser_press",
            "browser_screenshot",
            "browser_close",
            "browser_tabs",
            "browser_new_tab",
        ],
    },
    Capability {
        label: "Use your passwords & keys",
        hint: "Sign in with logins you saved. It never sees the values.",
        tools: &["vault_list"],
    },
    Capability {
        label: "Read and write documents",
        hint: "Word, Excel, PowerPoint and PDF files, and your indexed documents.",
        tools: &[
            "document_index",
            "rag_search",
            "docx_build",
            "docx_import",
            "docx_pdf",
            "docx_inspect",
            "docx_read",
            "docx_edit",
            "xlsx_analyze",
            "timesheet_analyze",
            "xlsx_build",
            "xlsx_import",
            "xlsx_inspect",
            "xlsx_read",
            "xlsx_edit",
            "pptx_import",
            "pptx_pdf",
            "pptx_inspect",
            "pptx_edit",
            "pdf_inspect",
            "pdf_edit",
            "pdf_merge",
        ],
    },
    Capability {
        label: "Build slide decks",
        hint: "Turn an outline into a presentation.",
        tools: &["deck_build"],
    },
    Capability {
        label: "Search and read websites",
        hint: "Fetch pages and search the web.",
        tools: &[
            "fetch_url",
            "firecrawl_scrape",
            "firecrawl_map",
            "firecrawl_search",
            "firecrawl_crawl",
            "firecrawl_extract",
        ],
    },
    Capability {
        label: "Watch sources for news",
        hint: "Check the sources you follow.",
        tools: &["watch_now"],
    },
    Capability {
        label: "Work with voice and audio",
        hint: "Transcribe recordings and speak replies.",
        tools: &[
            "audio_transcribe",
            "audio_understand",
            "elevenlabs_list_voices",
            "elevenlabs_text_to_speech",
            "elevenlabs_sound_effect",
            "elevenlabs_transcribe",
            "elevenlabs_generate_image",
            "elevenlabs_generate_video",
        ],
    },
    Capability {
        label: "Read and send email",
        hint: "Gmail, Outlook and AgentMail.",
        tools: &[
            "gmail_list_messages",
            "outlook_list_messages",
            "agentmail_inboxes_list",
            "agentmail_send",
        ],
    },
    Capability {
        label: "Check your calendar",
        hint: "Google and Outlook calendars.",
        tools: &[
            "google_calendar_list_events",
            "outlook_calendar_list_events",
        ],
    },
    Capability {
        label: "Use your cloud files and notes",
        hint: "OneDrive, Google Drive, Sheets and Notion.",
        tools: &[
            "onedrive_upload",
            "onedrive_list",
            "google_drive_list",
            "google_sheets_read",
            "notion_query_database",
        ],
    },
    Capability {
        label: "Use GitHub",
        hint: "Search repositories and read issues and files. Opening issues and comments ask you first.",
        tools: &[
            "github_search_repos",
            "github_list_issues",
            "github_get_issue",
            "github_create_issue",
            "github_comment_issue",
            "github_get_file",
        ],
    },
    Capability {
        label: "Manage your to-do list",
        hint: "Read your Todoist tasks. Adding and completing tasks ask you first.",
        tools: &[
            "todoist_list_tasks",
            "todoist_add_task",
            "todoist_complete_task",
        ],
    },
    Capability {
        label: "Work with folders on this PC",
        hint: "Read, and with your approval write, folders you allow in Settings.",
        tools: &["local_list", "local_read", "local_write"],
    },
    Capability {
        label: "Send files to Telegram",
        hint: "Share finished files in your Telegram chat.",
        tools: &["telegram_send_document"],
    },
    Capability {
        label: "Remember things for you",
        hint: "Save and recall notes between chats.",
        tools: &[
            "memory_save",
            "memory_list",
            "memory_search",
            "memory_update",
            "memory_delete",
        ],
    },
    Capability {
        label: "Use skills",
        hint: "Look up and follow the skills you added.",
        tools: &["skill_list", "skill_search", "skill_load"],
    },
];

/// A capability counts as on when the bot has any of its tools.
pub fn is_on(capability: &Capability, tools: &[String]) -> bool {
    capability
        .tools
        .iter()
        .any(|tool| tools.iter().any(|have| have == tool))
}

/// The tool list after switching `capability` on or off.
pub fn toggled(capability: &Capability, tools: &[String], on: bool) -> Vec<String> {
    let mut next: Vec<String> = tools
        .iter()
        .filter(|tool| !capability.tools.contains(&tool.as_str()))
        .cloned()
        .collect();
    if on {
        // Keep the order of the allowlist so the saved list is stable.
        for tool in capability.tools {
            if crate::SPECIALIST_TOOLS.contains(tool) {
                next.push((*tool).to_owned());
            }
        }
    }
    next
}

/// Labels of the capabilities a bot has, for compact chips.
pub fn labels(tools: &[String]) -> Vec<&'static str> {
    CAPABILITIES
        .iter()
        .filter(|capability| is_on(capability, tools))
        .map(|capability| capability.label)
        .collect()
}

/// Tools no capability covers, shown only under "Advanced".
pub fn uncovered(tools: &[String]) -> Vec<String> {
    tools
        .iter()
        .filter(|tool| {
            !CAPABILITIES
                .iter()
                .any(|capability| capability.tools.contains(&tool.as_str()))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SPECIALIST_TOOLS;

    #[test]
    fn every_specialist_tool_belongs_to_exactly_one_capability() {
        for tool in SPECIALIST_TOOLS {
            let owners = CAPABILITIES
                .iter()
                .filter(|capability| capability.tools.contains(tool))
                .count();
            assert_eq!(
                owners, 1,
                "{tool} must have exactly one plain-language home"
            );
        }
        for capability in CAPABILITIES {
            for tool in capability.tools {
                assert!(SPECIALIST_TOOLS.contains(tool), "{tool} is not allowlisted");
            }
        }
    }

    #[test]
    fn toggling_adds_or_removes_only_that_capability() {
        let browser = &CAPABILITIES[0];
        let start = vec!["vault_list".to_owned(), "mystery_tool".to_owned()];
        let on = toggled(browser, &start, true);
        assert!(is_on(browser, &on));
        assert!(on.contains(&"vault_list".to_owned()));
        assert!(on.contains(&"mystery_tool".to_owned()));
        let off = toggled(browser, &on, false);
        assert_eq!(off, start);
        let clean = toggled(browser, &["vault_list".to_owned()], true);
        assert!(crate::validate_tool_list(&clean).is_ok());
    }

    #[test]
    fn labels_and_uncovered_split_the_list() {
        let tools = vec!["vault_list".to_owned(), "mystery_tool".to_owned()];
        assert_eq!(labels(&tools), vec!["Use your passwords & keys"]);
        assert_eq!(uncovered(&tools), vec!["mystery_tool".to_owned()]);
    }
}
