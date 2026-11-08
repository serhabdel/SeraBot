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
/// What an ability needs before it can do anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Needs {
    /// Works as soon as it is on.
    Nothing,
    /// At least one of these connectors (ids from `GET /v1/connectors`) must
    /// be connected. `name` is what the owner reads ("Email").
    Connector {
        name: &'static str,
        ids: &'static [&'static str],
    },
    /// The owner's Telegram chat must be linked.
    Telegram,
}

pub struct Capability {
    /// Short heading the row is listed under.
    pub group: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub needs: Needs,
    pub tools: &'static [&'static str],
}

pub const CAPABILITIES: &[Capability] = &[
    // ---- Web
    Capability {
        group: "Web",
        needs: Needs::Nothing,
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
        group: "Web",
        needs: Needs::Nothing,
        label: "Use your passwords & keys",
        hint: "Sign in with logins you saved. It never sees the values.",
        tools: &["vault_list"],
    },
    Capability {
        group: "Web",
        needs: Needs::Nothing,
        label: "Search and read websites",
        hint: "Fetch pages and search the web.",
        tools: &[
            "fetch_url",
            "firecrawl_scrape",
            "firecrawl_map",
            "firecrawl_search",
            "firecrawl_crawl",
            "firecrawl_extract",
            "exa_search",
            "exa_get_contents",
        ],
    },
    Capability {
        group: "Web",
        needs: Needs::Nothing,
        label: "Watch sources for news",
        hint: "Check the sources you follow.",
        tools: &["watch_now"],
    },
    // ---- Files
    Capability {
        group: "Files",
        needs: Needs::Nothing,
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
        group: "Files",
        needs: Needs::Nothing,
        label: "Build slide decks",
        hint: "Turn an outline into a presentation.",
        tools: &["deck_build"],
    },
    Capability {
        group: "Files",
        needs: Needs::Nothing,
        label: "Inspect, resize and convert images",
        hint: "PNG, JPEG and WebP: sizes and camera info, smaller copies, format changes.",
        tools: &["image_inspect", "image_resize", "image_convert"],
    },
    Capability {
        group: "Files",
        needs: Needs::Connector {
            name: "a cloud account",
            ids: &["onedrive", "google_drive", "google_sheets", "notion"],
        },
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
        group: "Files",
        needs: Needs::Nothing,
        label: "Work with folders on this PC",
        hint: "Browse and search big folders you allow in Settings, read files, bring Word, Excel, PowerPoint, PDF and image files in to work on, and with your approval save results back. Works only while the SeraBot app is open on this PC.",
        tools: &[
            "local_list",
            "local_search",
            "local_stat",
            "local_read",
            "local_write",
            "local_import",
            "local_export",
        ],
    },
    // ---- Messages
    Capability {
        group: "Messages",
        needs: Needs::Connector {
            name: "an email account",
            ids: &["gmail", "outlook", "agentmail"],
        },
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
        group: "Messages",
        needs: Needs::Connector {
            name: "a calendar",
            ids: &["google_calendar", "outlook_calendar"],
        },
        label: "Check your calendar",
        hint: "Google and Outlook calendars.",
        tools: &[
            "google_calendar_list_events",
            "outlook_calendar_list_events",
        ],
    },
    Capability {
        group: "Messages",
        needs: Needs::Telegram,
        label: "Send files to Telegram",
        hint: "Share finished files in your Telegram chat.",
        tools: &["telegram_send_document"],
    },
    Capability {
        group: "Messages",
        needs: Needs::Connector {
            name: "Slack",
            ids: &["slack"],
        },
        label: "Read and post in Slack",
        hint: "Read channels the bot is in. Posting a message asks you first.",
        tools: &[
            "slack_list_channels",
            "slack_read_channel",
            "slack_post_message",
        ],
    },
    // ---- Code and tasks
    Capability {
        group: "Code and tasks",
        needs: Needs::Connector {
            name: "GitHub",
            ids: &["github"],
        },
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
        group: "Code and tasks",
        needs: Needs::Connector {
            name: "Todoist",
            ids: &["todoist"],
        },
        label: "Manage your to-do list",
        hint: "Read your Todoist tasks. Adding and completing tasks ask you first.",
        tools: &[
            "todoist_list_tasks",
            "todoist_add_task",
            "todoist_complete_task",
        ],
    },
    // ---- Media
    Capability {
        group: "Media",
        needs: Needs::Nothing,
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
        group: "Media",
        needs: Needs::Connector {
            name: "ElevenLabs",
            ids: &["elevenlabs"],
        },
        label: "Use your ElevenLabs tools",
        hint: "Voice, music, sound, images and video your ElevenLabs plan includes. Anything that spends credits asks you first.",
        tools: &["elevenlabs_mcp_list_tools", "elevenlabs_mcp_call"],
    },
    // ---- Memory
    Capability {
        group: "Memory",
        needs: Needs::Nothing,
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
        group: "Memory",
        needs: Needs::Nothing,
        label: "Use skills",
        hint: "Look up and follow the skills you added.",
        tools: &["skill_list", "skill_search", "skill_load"],
    },
];

/// Whether a connector from the list counts as connected for the signed-in
/// account: per-user products report their own state, the rest are connected
/// once a secret is configured (or the server says so).
pub fn connector_connected(connector: &crate::ConnectorStatus) -> bool {
    match &connector.personal {
        Some(personal) => personal.state == "connected",
        None => connector
            .connected
            .unwrap_or_else(|| connector.secrets.values().any(|s| s.configured)),
    }
}

/// What is missing for an ability, if anything. `connectors` is `None` while
/// the list has not loaded and `telegram_linked` is `None` while unknown: an
/// unknown state never blocks the toggle.
pub fn missing(
    needs: Needs,
    connectors: Option<&[crate::ConnectorStatus]>,
    telegram_linked: Option<bool>,
) -> Option<Missing> {
    match needs {
        Needs::Nothing => None,
        Needs::Telegram => (telegram_linked == Some(false)).then_some(Missing {
            name: "Telegram",
            page: MissingPage::Settings,
        }),
        Needs::Connector { name, ids } => {
            let list = connectors?;
            let any = list
                .iter()
                .any(|c| ids.contains(&c.id.as_str()) && connector_connected(c));
            (!any).then_some(Missing {
                name,
                page: MissingPage::Connectors,
            })
        }
    }
}

/// Where the Connect button goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissingPage {
    Connectors,
    Settings,
}

/// An unmet requirement: shown as "Needs <name>".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Missing {
    pub name: &'static str,
    pub page: MissingPage,
}
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

    fn connector(id: &str, state: Option<&str>) -> crate::ConnectorStatus {
        crate::ConnectorStatus {
            id: id.into(),
            kind: "x".into(),
            personal: state.map(|state| crate::PersonalStatus {
                state: state.into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn every_capability_has_a_known_group_in_order() {
        let groups = [
            "Web",
            "Files",
            "Messages",
            "Code and tasks",
            "Media",
            "Memory",
        ];
        let mut last = 0;
        for capability in CAPABILITIES {
            let at = groups
                .iter()
                .position(|g| *g == capability.group)
                .expect("known group");
            assert!(at >= last, "{} is out of group order", capability.label);
            last = at;
        }
    }

    #[test]
    fn needs_connector_until_one_is_connected() {
        let calendar = CAPABILITIES
            .iter()
            .find(|c| c.label == "Check your calendar")
            .unwrap();
        // Unknown list: never blocks.
        assert!(missing(calendar.needs, None, None).is_none());
        let none = [connector("google_calendar", Some("ready"))];
        let m = missing(calendar.needs, Some(&none), None).expect("missing");
        assert_eq!(m.page, MissingPage::Connectors);
        let some = [
            connector("google_calendar", Some("ready")),
            connector("outlook_calendar", Some("connected")),
        ];
        assert!(missing(calendar.needs, Some(&some), None).is_none());
    }

    #[test]
    fn telegram_and_plain_abilities() {
        assert!(missing(Needs::Telegram, None, None).is_none());
        assert!(missing(Needs::Telegram, None, Some(true)).is_none());
        let m = missing(Needs::Telegram, None, Some(false)).unwrap();
        assert_eq!(m.page, MissingPage::Settings);
        assert!(missing(Needs::Nothing, Some(&[]), Some(false)).is_none());
    }

    #[test]
    fn labels_and_uncovered_split_the_list() {
        let tools = vec!["vault_list".to_owned(), "mystery_tool".to_owned()];
        assert_eq!(labels(&tools), vec!["Use your passwords & keys"]);
        assert_eq!(uncovered(&tools), vec!["mystery_tool".to_owned()]);
    }
}
