//! Several chats per bot (v0.35). The server lists a bot's chats (the primary
//! chat `dir:<bot>` first, then extra chats), creates a new one that starts
//! from the bot's memory and a short summary, and keeps one "memory refresh"
//! setting per bot. This module holds the response shapes and the small rules
//! the sidebar needs, as plain data so they can be tested without a window.

use serde::Deserialize;

/// Chats shown under a bot before "Show N more".
pub const VISIBLE_CHATS: usize = 5;
/// Server default for the memory refresh, in user turns.
pub const DEFAULT_REFRESH_TURNS: u32 = 40;
pub const MIN_REFRESH_TURNS: u32 = 5;
pub const MAX_REFRESH_TURNS: u32 = 500;
/// Upper bound on one list, so a bad response cannot balloon the sidebar.
pub const MAX_CHATS_PER_BOT: usize = 500;

/// One chat of a bot.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct BotChat {
    pub id: String,
    #[serde(default)]
    pub bot: String,
    #[serde(default)]
    pub title: Option<String>,
    /// The bot's original chat (`dir:<bot>`).
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub archived: bool,
    /// The chat whose summary this one started from.
    #[serde(default)]
    pub carried_from: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub last_message_at: Option<String>,
    #[serde(default)]
    pub turns: u64,
    #[serde(default)]
    pub has_summary: bool,
}

/// `GET /v1/bots/{bot}/chats`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct BotChatList {
    #[serde(default)]
    pub bot: String,
    #[serde(default = "default_refresh")]
    pub refresh_every_turns: u32,
    #[serde(default)]
    pub chats: Vec<BotChat>,
}

fn default_refresh() -> u32 {
    DEFAULT_REFRESH_TURNS
}

/// `POST /v1/bots/{bot}/chats`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct CreatedBotChat {
    pub chat: BotChat,
    #[serde(default)]
    pub carried_from: Option<String>,
}

/// `PUT /v1/bots/{bot}/chat-settings`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct BotChatSettings {
    #[serde(default)]
    pub bot: String,
    pub refresh_every_turns: u32,
}

/// Whether a refresh interval is one the server accepts.
pub fn valid_refresh_turns(turns: u32) -> bool {
    (MIN_REFRESH_TURNS..=MAX_REFRESH_TURNS).contains(&turns)
}

/// Parses what the user typed in the refresh field. `None` for anything the
/// server would refuse, so the field can show its own message.
pub fn parse_refresh_turns(text: &str) -> Option<u32> {
    let turns: u32 = text.trim().parse().ok()?;
    valid_refresh_turns(turns).then_some(turns)
}

/// What a chat row shows as its title.
pub fn chat_title(chat: &BotChat) -> String {
    match chat.title.as_deref().map(str::trim) {
        Some(title) if !title.is_empty() => title.to_owned(),
        _ if chat.primary => "Main chat".to_owned(),
        _ => "New chat".to_owned(),
    }
}

/// The chats of a list that are not archived, newest activity first. The
/// primary chat always stays first: it is the bot's home.
pub fn live_chats(list: &BotChatList) -> Vec<&BotChat> {
    let mut chats: Vec<&BotChat> = list.chats.iter().filter(|chat| !chat.archived).collect();
    chats.sort_by(|a, b| {
        b.primary.cmp(&a.primary).then_with(|| {
            let left = a.last_message_at.as_deref().unwrap_or(&a.created_at);
            let right = b.last_message_at.as_deref().unwrap_or(&b.created_at);
            right.cmp(left)
        })
    });
    chats
}

/// The rows to show under a bot and how many are folded behind "Show N more".
/// The open chat is always visible, so switching never hides where you are.
pub fn visible_chats<'a>(
    chats: &[&'a BotChat],
    show_all: bool,
    active: &str,
) -> (Vec<&'a BotChat>, usize) {
    if show_all || chats.len() <= VISIBLE_CHATS {
        return (chats.to_vec(), 0);
    }
    let mut shown: Vec<&BotChat> = chats.iter().take(VISIBLE_CHATS).copied().collect();
    if !shown.iter().any(|chat| chat.id == active)
        && let Some(open) = chats.iter().find(|chat| chat.id == active)
    {
        shown.pop();
        shown.push(open);
    }
    let hidden = chats.len() - shown.len();
    (shown, hidden)
}

/// The bot that owns a chat id, looking through the loaded lists. The primary
/// chat `dir:<bot>` needs no list.
pub fn bot_of_chat<'a>(
    lists: impl IntoIterator<Item = &'a BotChatList>,
    conversation: &str,
) -> Option<String> {
    for list in lists {
        if list.chats.iter().any(|chat| chat.id == conversation) {
            return Some(list.bot.clone());
        }
    }
    // An extra chat that is not in a loaded list yet has no known bot; never
    // read its id as a bot slug.
    if is_extra_chat_id(conversation) {
        return None;
    }
    conversation.strip_prefix("dir:").map(str::to_owned)
}

/// Extra chats look like `dir:chat-<hex>`. They belong under their bot, not in
/// the general chat list.
pub fn is_extra_chat_id(id: &str) -> bool {
    id.strip_prefix("dir:chat-")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The quiet line at the top of a new chat that says where it started from.
pub fn memory_divider(
    chat: &BotChat,
    previous_title: Option<&str>,
    bot_name: &str,
) -> Option<String> {
    if chat.primary {
        return None;
    }
    let from = previous_title
        .map(str::trim)
        .filter(|title| !title.is_empty());
    Some(
        match (chat.has_summary || chat.carried_from.is_some(), from) {
            (true, Some(title)) => {
                format!(
                    "Starts with {bot_name}\u{2019}s memory and a summary of \u{201c}{title}\u{201d}."
                )
            }
            (true, None) => {
                format!("Starts with {bot_name}\u{2019}s memory and a summary of an earlier chat.")
            }
            (false, _) => format!("Starts with {bot_name}\u{2019}s memory."),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: &str, primary: bool, last: &str) -> BotChat {
        BotChat {
            id: id.to_owned(),
            bot: "analyst".into(),
            title: None,
            primary,
            archived: false,
            carried_from: None,
            created_at: "2026-11-01T00:00:00Z".into(),
            updated_at: last.to_owned(),
            last_message_at: Some(last.to_owned()),
            turns: 0,
            has_summary: false,
        }
    }

    fn list(chats: Vec<BotChat>) -> BotChatList {
        BotChatList {
            bot: "analyst".into(),
            refresh_every_turns: 40,
            chats,
        }
    }

    #[test]
    fn parses_the_documented_list_shape() {
        let parsed: BotChatList = serde_json::from_str(
            r#"{"bot":"analyst","refresh_every_turns":40,"chats":[
                {"id":"dir:analyst","bot":"analyst","title":null,"primary":true,"archived":false,
                 "carried_from":null,"created_at":"2026-11-01T00:00:00Z","updated_at":"2026-11-02T00:00:00Z",
                 "last_message_at":null,"turns":12,"has_summary":true},
                {"id":"dir:chat-0a1b2c3d4e5f","bot":"analyst","title":"Q3","primary":false,"archived":false,
                 "carried_from":"dir:analyst","created_at":"2026-11-03T00:00:00Z","updated_at":"2026-11-03T00:00:00Z",
                 "last_message_at":"2026-11-03T01:00:00Z","turns":2,"has_summary":true}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.chats.len(), 2);
        assert!(parsed.chats[0].primary);
        assert_eq!(parsed.chats[1].carried_from.as_deref(), Some("dir:analyst"));
        // Missing optional fields fall back to safe defaults.
        let sparse: BotChatList = serde_json::from_str(r#"{"chats":[{"id":"dir:x"}]}"#).unwrap();
        assert_eq!(sparse.refresh_every_turns, DEFAULT_REFRESH_TURNS);
        assert!(!sparse.chats[0].archived);
    }

    #[test]
    fn parses_the_created_chat_reply() {
        let created: CreatedBotChat = serde_json::from_str(
            r#"{"chat":{"id":"dir:chat-aabbccddeeff"},"carried_from":"dir:analyst"}"#,
        )
        .unwrap();
        assert_eq!(created.chat.id, "dir:chat-aabbccddeeff");
        assert_eq!(created.carried_from.as_deref(), Some("dir:analyst"));
    }

    #[test]
    fn refresh_interval_accepts_only_five_to_five_hundred() {
        assert_eq!(parse_refresh_turns("40"), Some(40));
        assert_eq!(parse_refresh_turns(" 5 "), Some(5));
        assert_eq!(parse_refresh_turns("500"), Some(500));
        assert_eq!(parse_refresh_turns("4"), None);
        assert_eq!(parse_refresh_turns("501"), None);
        assert_eq!(parse_refresh_turns("forty"), None);
        assert_eq!(parse_refresh_turns(""), None);
        assert_eq!(parse_refresh_turns("-3"), None);
    }

    #[test]
    fn the_primary_chat_comes_first_then_newest_activity() {
        let l = list(vec![
            chat("dir:chat-aa", false, "2026-11-05T00:00:00Z"),
            chat("dir:analyst", true, "2026-11-01T00:00:00Z"),
            chat("dir:chat-bb", false, "2026-11-09T00:00:00Z"),
        ]);
        let ids: Vec<&str> = live_chats(&l).iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["dir:analyst", "dir:chat-bb", "dir:chat-aa"]);
    }

    #[test]
    fn archived_chats_are_left_out() {
        let mut old = chat("dir:chat-aa", false, "2026-11-05T00:00:00Z");
        old.archived = true;
        let l = list(vec![chat("dir:analyst", true, "2026-11-01T00:00:00Z"), old]);
        assert_eq!(live_chats(&l).len(), 1);
    }

    #[test]
    fn five_rows_show_and_the_rest_fold_behind_show_more() {
        let chats: Vec<BotChat> = (0..8)
            .map(|i| {
                chat(
                    &format!("dir:chat-{i:02}"),
                    false,
                    &format!("2026-11-0{}T00:00:00Z", 9 - i),
                )
            })
            .collect();
        let refs: Vec<&BotChat> = chats.iter().collect();
        let (shown, hidden) = visible_chats(&refs, false, "dir:chat-00");
        assert_eq!(shown.len(), VISIBLE_CHATS);
        assert_eq!(hidden, 3);
        let (all, none) = visible_chats(&refs, true, "dir:chat-00");
        assert_eq!(all.len(), 8);
        assert_eq!(none, 0);
        let few: Vec<&BotChat> = chats.iter().take(5).collect();
        assert_eq!(visible_chats(&few, false, "x").1, 0);
    }

    #[test]
    fn the_open_chat_is_never_hidden_behind_show_more() {
        let chats: Vec<BotChat> = (0..8)
            .map(|i| {
                chat(
                    &format!("dir:chat-{i:02}"),
                    false,
                    &format!("2026-11-0{}T00:00:00Z", 9 - i),
                )
            })
            .collect();
        let refs: Vec<&BotChat> = chats.iter().collect();
        let (shown, hidden) = visible_chats(&refs, false, "dir:chat-07");
        assert_eq!(shown.len(), VISIBLE_CHATS);
        assert!(shown.iter().any(|c| c.id == "dir:chat-07"));
        assert_eq!(hidden, 3);
    }

    #[test]
    fn titles_fall_back_to_plain_words() {
        let mut c = chat("dir:chat-aa", false, "x");
        assert_eq!(chat_title(&c), "New chat");
        c.title = Some("  Budget review ".into());
        assert_eq!(chat_title(&c), "Budget review");
        let primary = chat("dir:analyst", true, "x");
        assert_eq!(chat_title(&primary), "Main chat");
    }

    #[test]
    fn extra_chat_ids_are_recognised() {
        assert!(is_extra_chat_id("dir:chat-0a1b2c3d4e5f"));
        assert!(!is_extra_chat_id("dir:analyst"));
        assert!(!is_extra_chat_id("dir:chat-"));
        assert!(!is_extra_chat_id("dir:chat-zz"));
        assert!(!is_extra_chat_id("group:chat-aa"));
    }

    #[test]
    fn a_chat_resolves_to_its_bot() {
        let l = list(vec![
            chat("dir:analyst", true, "x"),
            chat("dir:chat-0a1b2c3d4e5f", false, "x"),
        ]);
        assert_eq!(
            bot_of_chat([&l], "dir:chat-0a1b2c3d4e5f").as_deref(),
            Some("analyst")
        );
        assert_eq!(bot_of_chat([&l], "dir:legal").as_deref(), Some("legal"));
        assert_eq!(bot_of_chat([&l], "windows-owner"), None);
        // An extra chat from a list that is not loaded yet is not a bot slug.
        assert_eq!(bot_of_chat([&l], "dir:chat-ffeeddccbbaa"), None);
    }

    #[test]
    fn the_new_chat_divider_says_what_it_carries() {
        let mut c = chat("dir:chat-aa", false, "x");
        c.has_summary = true;
        c.carried_from = Some("dir:analyst".into());
        assert_eq!(
            memory_divider(&c, Some("Q3 review"), "Analyst").unwrap(),
            "Starts with Analyst\u{2019}s memory and a summary of \u{201c}Q3 review\u{201d}."
        );
        assert!(
            memory_divider(&c, None, "Analyst")
                .unwrap()
                .contains("earlier chat")
        );
        c.has_summary = false;
        c.carried_from = None;
        assert_eq!(
            memory_divider(&c, None, "Analyst").unwrap(),
            "Starts with Analyst\u{2019}s memory."
        );
        assert_eq!(
            memory_divider(&chat("dir:analyst", true, "x"), None, "Analyst"),
            None
        );
    }
}
