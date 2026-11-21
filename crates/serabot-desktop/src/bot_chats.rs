//! Several chats per bot (v0.35). The server lists a bot's chats (the primary
//! chat `dir:<bot>` first, then extra chats), creates a new one that starts
//! from the bot's memory and a short summary, and keeps one "memory refresh"
//! setting per bot. This module holds the response shapes and the small rules
//! the sidebar needs, as plain data so they can be tested without a window.

use serde::{Deserialize, Deserializer};

/// A value the server may send as JSON `null` (for example the timestamps of a
/// main chat that has no message yet). `#[serde(default)]` alone only covers a
/// missing field, so a `null` would fail the whole list.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

fn null_as_default_refresh<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<u32>::deserialize(deserializer)?.unwrap_or(DEFAULT_REFRESH_TURNS))
}

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
    #[serde(default, deserialize_with = "null_as_default")]
    pub bot: String,
    #[serde(default)]
    pub title: Option<String>,
    /// The bot's original chat (`dir:<bot>`).
    #[serde(default, deserialize_with = "null_as_default")]
    pub primary: bool,
    #[serde(default, deserialize_with = "null_as_default")]
    pub archived: bool,
    /// The chat whose summary this one started from.
    #[serde(default)]
    pub carried_from: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub created_at: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub updated_at: String,
    #[serde(default)]
    pub last_message_at: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub turns: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    pub has_summary: bool,
}

/// `GET /v1/bots/{bot}/chats`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct BotChatList {
    #[serde(default, deserialize_with = "null_as_default")]
    pub bot: String,
    #[serde(
        default = "default_refresh",
        deserialize_with = "null_as_default_refresh"
    )]
    pub refresh_every_turns: u32,
    #[serde(default, deserialize_with = "null_as_default")]
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

/// Maximum character length for a derived short chat title.
const DERIVED_TITLE_MAX_CHARS: usize = 44;

/// Whether `text` looks like a raw generated chat id (`chat-01a1dfd...`,
/// `Chat 01a1dfd96...`, or a bare 8+ hex string) that must never appear in UI.
pub fn is_raw_chat_id(text: &str) -> bool {
    let trimmed = text.trim();
    if let Some(rest) = trimmed
        .strip_prefix("dir:chat-")
        .or_else(|| trimmed.strip_prefix("chat-"))
        .or_else(|| trimmed.strip_prefix("Chat "))
    {
        let core = rest.trim_end_matches(['.', '\u{2026}']);
        return !core.is_empty() && core.len() >= 6 && core.chars().all(|c| c.is_ascii_hexdigit());
    }
    trimmed.len() >= 8 && trimmed.chars().all(|c| c.is_ascii_hexdigit())
}

/// Derive a clean short title from the first user message of a thread.
pub fn derive_title_from_message(message: &str) -> Option<String> {
    let first_line = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let collapsed: String = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    if collapsed.chars().count() <= DERIVED_TITLE_MAX_CHARS {
        return Some(collapsed);
    }
    let mut clipped = String::new();
    let mut last_space = None;
    for (idx, ch) in collapsed.chars().enumerate() {
        if idx >= DERIVED_TITLE_MAX_CHARS {
            break;
        }
        if ch == ' ' {
            last_space = Some(clipped.len());
        }
        clipped.push(ch);
    }
    if let Some(pos) = last_space
        && pos > 12
    {
        clipped.truncate(pos);
    }
    let trimmed = clipped.trim_end();
    Some(format!("{trimmed}\u{2026}"))
}

/// What a chat row shows as its title, with optional fallback from the first
/// user message or bot display name so raw IDs never appear.
pub fn chat_title_with_fallback(
    chat: &BotChat,
    first_user_message: Option<&str>,
    bot_name: Option<&str>,
) -> String {
    match chat.title.as_deref().map(str::trim) {
        Some(title) if !title.is_empty() && !is_raw_chat_id(title) => title.to_owned(),
        _ if chat.primary => "Main chat".to_owned(),
        _ => {
            if let Some(derived) = first_user_message.and_then(derive_title_from_message) {
                return derived;
            }
            match bot_name.map(str::trim).filter(|name| !name.is_empty()) {
                Some(name) => format!("New conversation \u{00b7} {name}"),
                None => "New conversation".to_owned(),
            }
        }
    }
}

/// What a chat row shows as its title.
pub fn chat_title(chat: &BotChat) -> String {
    chat_title_with_fallback(chat, None, None)
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

/// The chats of a bot whose title contains `needle` (case-insensitive), for the
/// sidebar search. An empty needle matches nothing here: the caller shows the
/// bot's normal rows instead.
pub fn chats_matching<'a>(list: Option<&'a BotChatList>, needle: &str) -> Vec<&'a BotChat> {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    list.map(|list| {
        live_chats(list)
            .into_iter()
            .filter(|chat| chat_title(chat).to_lowercase().contains(&needle))
            .collect()
    })
    .unwrap_or_default()
}

/// One entry of the chat switcher: a bot's chat, as the header menu lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitcherEntry {
    pub id: String,
    pub title: String,
    pub primary: bool,
    pub active: bool,
    pub turns: u64,
    pub last_activity: Option<String>,
}

/// Date bucket used by the chat history popover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HistoryDateGroup {
    Today,
    Yesterday,
    ThisWeek,
    Older,
}

impl HistoryDateGroup {
    pub const ALL: [Self; 4] = [Self::Today, Self::Yesterday, Self::ThisWeek, Self::Older];

    pub fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Yesterday => "Yesterday",
            Self::ThisWeek => "This week",
            Self::Older => "Older",
        }
    }
}

/// Enriched entry ready to render inside the chat history popover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntryView {
    pub id: String,
    pub title: String,
    pub primary: bool,
    pub active: bool,
    pub turns: u64,
    pub last_activity: Option<String>,
    pub relative_time: String,
    pub group: HistoryDateGroup,
}

/// Date-grouped slice of [`HistoryEntryView`] items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryGroup {
    pub group: HistoryDateGroup,
    pub label: &'static str,
    pub entries: Vec<HistoryEntryView>,
}

/// Format a turn count concisely (`0 turns`, `1 turn`, `12 turns`).
pub fn format_turn_count(turns: u64) -> String {
    if turns == 1 {
        "1 turn".to_string()
    } else {
        format!("{turns} turns")
    }
}

fn parse_ymd(stamp: &str) -> Option<(i32, u32, u32)> {
    let s = stamp.trim();
    let bytes = s.as_bytes();
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let y: i32 = s[0..4].parse().ok()?;
    let m: u32 = s[5..7].parse().ok()?;
    let d: u32 = s[8..10].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y, m, d))
}

fn civil_days(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(m);
    let d = i64::from(d);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse an ISO-8601 / RFC-3339 timestamp or `YYYY-MM-DD` date into UTC Unix seconds.
pub fn parse_timestamp_secs(stamp: &str) -> Option<i64> {
    let s = stamp.trim();
    let (y, m, d) = parse_ymd(s)?;
    let days = civil_days(y, m, d);
    let rest = s.get(10..).unwrap_or("");
    if rest.is_empty() {
        return Some(days * 86_400);
    }
    let rest = rest
        .strip_prefix('T')
        .or_else(|| rest.strip_prefix('t'))
        .or_else(|| rest.strip_prefix(' '))?;
    if rest.len() < 8 {
        return None;
    }
    let rb = rest.as_bytes();
    if rb[2] != b':' || rb[5] != b':' {
        return None;
    }
    let hh: i64 = rest[0..2].parse().ok()?;
    let mm: i64 = rest[3..5].parse().ok()?;
    let ss: i64 = rest[6..8].parse().ok()?;
    if !(0..=23).contains(&hh) || !(0..=59).contains(&mm) || !(0..=60).contains(&ss) {
        return None;
    }
    let mut after = &rest[8..];
    if let Some(frac) = after.strip_prefix('.') {
        let digits = frac.bytes().take_while(|b| b.is_ascii_digit()).count();
        after = &frac[digits..];
    }
    let offset_secs: i64 = if after.is_empty() || after.starts_with('Z') || after.starts_with('z') {
        0
    } else if after.starts_with('+') || after.starts_with('-') {
        let sign = if after.starts_with('+') { 1 } else { -1 };
        let tz = &after[1..];
        if tz.len() >= 5 && tz.as_bytes()[2] == b':' {
            let th: i64 = tz[0..2].parse().ok()?;
            let tm: i64 = tz[3..5].parse().ok()?;
            sign * (th * 3600 + tm * 60)
        } else if tz.len() >= 4 && tz.as_bytes()[0..4].iter().all(|b| b.is_ascii_digit()) {
            let th: i64 = tz[0..2].parse().ok()?;
            let tm: i64 = tz[2..4].parse().ok()?;
            sign * (th * 3600 + tm * 60)
        } else {
            return None;
        }
    } else {
        return None;
    };
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss - offset_secs)
}

/// Bucket an activity timestamp into `Today`, `Yesterday`, `This week`, or `Older`.
pub fn history_date_group(ts: &str, today: &str) -> HistoryDateGroup {
    let Some((ty, tm, td)) = parse_ymd(today) else {
        return HistoryDateGroup::Older;
    };
    let today_days = civil_days(ty, tm, td);
    let ts_days = if let Some(secs) = parse_timestamp_secs(ts) {
        secs.div_euclid(86_400)
    } else if let Some((sy, sm, sd)) = parse_ymd(ts) {
        civil_days(sy, sm, sd)
    } else {
        return HistoryDateGroup::Older;
    };
    let diff = today_days - ts_days;
    if diff <= 0 {
        HistoryDateGroup::Today
    } else if diff == 1 {
        HistoryDateGroup::Yesterday
    } else if diff <= 6 {
        HistoryDateGroup::ThisWeek
    } else {
        HistoryDateGroup::Older
    }
}

/// Format an activity timestamp relative to `now_secs` (`Just now`, `5m ago`, `3h ago`, `2d ago`, `2w ago`, or `YYYY-MM-DD`).
pub fn format_relative_time(ts: &str, now_secs: u64) -> String {
    let trimmed = ts.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let Some(ts_secs) = parse_timestamp_secs(trimmed) else {
        return trimmed.to_string();
    };
    let now = i64::try_from(now_secs).unwrap_or(i64::MAX);
    let delta = now.saturating_sub(ts_secs);
    if delta < 60 {
        "Just now".to_string()
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86_400 {
        format!("{}h ago", delta / 3600)
    } else if delta < 86_400 * 7 {
        format!("{}d ago", delta / 86_400)
    } else if delta < 86_400 * 28 {
        format!("{}w ago", delta / (86_400 * 7))
    } else if let Some(date) = trimmed.get(0..10).filter(|d| parse_ymd(d).is_some()) {
        date.to_string()
    } else {
        format!("{}d ago", delta / 86_400)
    }
}

/// Filter `entries` case-insensitively by title, sort newest activity first, and group by date.
pub fn filter_and_group_switcher_entries(
    entries: Vec<SwitcherEntry>,
    query: &str,
    today: &str,
    now_secs: u64,
) -> Vec<HistoryGroup> {
    let needle = query.trim().to_lowercase();
    let mut ranked: Vec<(i64, usize, SwitcherEntry)> = entries
        .into_iter()
        .enumerate()
        .filter(|(_, entry)| needle.is_empty() || entry.title.to_lowercase().contains(&needle))
        .map(|(idx, entry)| {
            let sort_ts = entry
                .last_activity
                .as_deref()
                .and_then(parse_timestamp_secs)
                .unwrap_or(i64::MIN);
            (sort_ts, idx, entry)
        })
        .collect();

    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.2.active.cmp(&a.2.active))
            .then_with(|| b.2.primary.cmp(&a.2.primary))
            .then_with(|| a.1.cmp(&b.1))
    });

    let mut buckets: [Vec<HistoryEntryView>; 4] = Default::default();
    for (_, _, entry) in ranked {
        let (group, relative_time) = match entry.last_activity.as_deref() {
            Some(ts) if !ts.trim().is_empty() => (
                history_date_group(ts, today),
                format_relative_time(ts, now_secs),
            ),
            _ => (HistoryDateGroup::Older, String::new()),
        };
        let slot = match group {
            HistoryDateGroup::Today => 0,
            HistoryDateGroup::Yesterday => 1,
            HistoryDateGroup::ThisWeek => 2,
            HistoryDateGroup::Older => 3,
        };
        buckets[slot].push(HistoryEntryView {
            id: entry.id,
            title: entry.title,
            primary: entry.primary,
            active: entry.active,
            turns: entry.turns,
            last_activity: entry.last_activity,
            relative_time,
            group,
        });
    }

    HistoryDateGroup::ALL
        .into_iter()
        .enumerate()
        .filter_map(|(idx, group)| {
            if buckets[idx].is_empty() {
                None
            } else {
                Some(HistoryGroup {
                    group,
                    label: group.label(),
                    entries: std::mem::take(&mut buckets[idx]),
                })
            }
        })
        .collect()
}

#[cfg(test)]
mod history_tests {
    use super::*;

    #[test]
    fn date_grouping_handles_boundaries_and_year_wraps() {
        let today = "2026-03-01";
        assert_eq!(
            history_date_group("2026-03-01T14:20:00Z", today),
            HistoryDateGroup::Today
        );
        assert_eq!(
            history_date_group("2026-02-28T23:59:59Z", today),
            HistoryDateGroup::Yesterday
        );
        assert_eq!(
            history_date_group("2026-02-23T08:00:00Z", today),
            HistoryDateGroup::ThisWeek
        );
        assert_eq!(
            history_date_group("2026-02-22T23:59:59Z", today),
            HistoryDateGroup::Older
        );
        assert_eq!(
            history_date_group("2025-12-31T23:00:00Z", "2026-01-01"),
            HistoryDateGroup::Yesterday
        );
        // Timezone offset pushing local late evening into next UTC day
        assert_eq!(
            history_date_group("2026-03-01T00:30:00+02:00", today),
            HistoryDateGroup::Yesterday
        );
        assert_eq!(history_date_group("", today), HistoryDateGroup::Older);
    }

    #[test]
    fn relative_time_formats_ranges_cleanly() {
        let now = parse_timestamp_secs("2026-03-10T12:00:00Z").unwrap() as u64;
        assert_eq!(
            format_relative_time("2026-03-10T11:59:30Z", now),
            "Just now"
        );
        assert_eq!(format_relative_time("2026-03-10T11:45:00Z", now), "15m ago");
        assert_eq!(format_relative_time("2026-03-10T09:00:00Z", now), "3h ago");
        assert_eq!(format_relative_time("2026-03-07T12:00:00Z", now), "3d ago");
        assert_eq!(format_relative_time("2026-02-24T12:00:00Z", now), "2w ago");
        assert_eq!(
            format_relative_time("2026-01-15T12:00:00Z", now),
            "2026-01-15"
        );
        assert_eq!(format_relative_time("", now), "");
    }

    #[test]
    fn filter_and_group_sorts_newest_first_and_filters_by_title() {
        let now = parse_timestamp_secs("2026-03-10T12:00:00Z").unwrap() as u64;
        let today = "2026-03-10";
        let entries = vec![
            SwitcherEntry {
                id: "older".into(),
                title: "Older plugin discussion".into(),
                primary: false,
                active: false,
                turns: 3,
                last_activity: Some("2026-02-01T10:00:00Z".into()),
            },
            SwitcherEntry {
                id: "undated".into(),
                title: "Main chat".into(),
                primary: true,
                active: false,
                turns: 0,
                last_activity: None,
            },
            SwitcherEntry {
                id: "today-old".into(),
                title: "Morning plugins check".into(),
                primary: false,
                active: false,
                turns: 2,
                last_activity: Some("2026-03-10T08:00:00Z".into()),
            },
            SwitcherEntry {
                id: "today-new".into(),
                title: "Oki, can we test some plugins".into(),
                primary: false,
                active: true,
                turns: 8,
                last_activity: Some("2026-03-10T11:50:00Z".into()),
            },
            SwitcherEntry {
                id: "yesterday".into(),
                title: "Release notes".into(),
                primary: false,
                active: false,
                turns: 1,
                last_activity: Some("2026-03-09T18:00:00Z".into()),
            },
        ];

        let all = filter_and_group_switcher_entries(entries.clone(), "", today, now);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].group, HistoryDateGroup::Today);
        assert_eq!(all[0].entries[0].id, "today-new");
        assert_eq!(all[0].entries[0].relative_time, "10m ago");
        assert_eq!(all[0].entries[1].id, "today-old");
        assert_eq!(all[1].group, HistoryDateGroup::Yesterday);
        assert_eq!(all[2].group, HistoryDateGroup::Older);
        assert_eq!(all[2].entries[0].id, "older");
        assert_eq!(all[2].entries[1].id, "undated");

        let filtered = filter_and_group_switcher_entries(entries, "  PLUGINS ", today, now);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].group, HistoryDateGroup::Today);
        assert_eq!(filtered[0].entries.len(), 2);
        assert_eq!(format_turn_count(1), "1 turn");
        assert_eq!(format_turn_count(8), "8 turns");
    }
}

/// The chats to offer in the header switcher: the main chat first, then the
/// others by recent activity. The main chat is always listed, even before the
/// list has loaded, so the switcher is never empty and a bot with a single chat
/// still shows where New chat lives.
pub fn switcher_entries(list: Option<&BotChatList>, bot: &str, active: &str) -> Vec<SwitcherEntry> {
    let main_id = format!("dir:{bot}");
    let mut entries: Vec<SwitcherEntry> = list
        .map(|list| {
            live_chats(list)
                .into_iter()
                .map(|chat| {
                    let last_activity = chat
                        .last_message_at
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .or_else(|| {
                            (!chat.updated_at.trim().is_empty()).then_some(chat.updated_at.trim())
                        })
                        .or_else(|| {
                            (!chat.created_at.trim().is_empty()).then_some(chat.created_at.trim())
                        })
                        .map(str::to_owned);
                    SwitcherEntry {
                        id: chat.id.clone(),
                        title: chat_title(chat),
                        primary: chat.primary,
                        active: chat.id == active,
                        turns: chat.turns,
                        last_activity,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    if !entries.iter().any(|entry| entry.primary) {
        entries.insert(
            0,
            SwitcherEntry {
                id: main_id.clone(),
                title: "Main chat".to_owned(),
                primary: true,
                active: main_id == active,
                turns: 0,
                last_activity: None,
            },
        );
    }
    entries
}

/// "2 chats" for a bot row, or nothing for a bot with only its main chat.
pub fn chat_count_label(extra_chats: usize) -> Option<String> {
    match extra_chats {
        0 => None,
        1 => Some("2 chats".to_owned()),
        n => Some(format!("{} chats", n + 1)),
    }
}

/// Whether a bot's chats show in the sidebar when the reader has not chosen.
/// They open for the bot being talked to (main or any of its chats), so the
/// way to switch and to start a new chat is always in sight.
pub fn default_chats_open(bot_is_active: bool) -> bool {
    bot_is_active
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
    fn parses_the_server_list_for_a_bot_that_has_not_started_yet() {
        // Verbatim shape of the server's main chat before its first message:
        // the timestamps are JSON null, not missing. A parser that only
        // tolerates missing fields rejects the whole list, and the sidebar then
        // shows no chats for that bot.
        let parsed: BotChatList = serde_json::from_str(
            r#"{"bot":"legal","refresh_every_turns":40,"chats":[
                {"id":"dir:legal","bot":"legal","title":null,"primary":true,"archived":false,
                 "carried_from":null,"created_at":null,"updated_at":null,
                 "last_message_at":null,"turns":0,"has_summary":false}]}"#,
        )
        .expect("a list with null timestamps must parse");
        assert_eq!(parsed.chats.len(), 1);
        assert!(parsed.chats[0].primary);
        assert_eq!(parsed.chats[0].created_at, "");
        // Every other field may be null too without losing the list.
        let nulls: BotChatList = serde_json::from_str(
            r#"{"bot":null,"refresh_every_turns":null,"chats":[{"id":"dir:x","bot":null,"title":null,
                "primary":null,"archived":null,"turns":null,"has_summary":null}]}"#,
        )
        .expect("nulls fall back to defaults");
        assert_eq!(nulls.refresh_every_turns, DEFAULT_REFRESH_TURNS);
        assert!(!nulls.chats[0].archived);
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
        let mut c = chat("dir:chat-01a1dfd96", false, "x");
        assert_eq!(chat_title(&c), "New conversation");
        c.title = Some("Chat 01a1dfd96...".into());
        assert_eq!(chat_title(&c), "New conversation");
        c.title = Some("  Budget review ".into());
        assert_eq!(chat_title(&c), "Budget review");
        let primary = chat("dir:analyst", true, "x");
        assert_eq!(chat_title(&primary), "Main chat");
    }

    #[test]
    fn smart_title_derives_from_first_user_message_or_bot_name() {
        let c = chat("dir:chat-01a1dfd96", false, "x");
        assert_eq!(
            chat_title_with_fallback(&c, None, Some("Analyst")),
            "New conversation \u{00b7} Analyst"
        );
        assert_eq!(
            chat_title_with_fallback(
                &c,
                Some("  Please compare   our Q3 revenue against last year\nwith details"),
                Some("Analyst")
            ),
            "Please compare our Q3 revenue against last\u{2026}"
        );
        assert!(is_raw_chat_id("Chat 01a1dfd96..."));
        assert!(is_raw_chat_id("chat-01a1dfd96"));
        assert!(!is_raw_chat_id("Budget review"));
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
    #[test]
    fn the_switcher_always_lists_the_main_chat_and_marks_the_open_one() {
        // Before the list loads: just the main chat.
        let before = switcher_entries(None, "analyst", "dir:analyst");
        assert_eq!(before.len(), 1);
        assert!(before[0].primary && before[0].active);
        assert_eq!(before[0].title, "Main chat");

        let l = list(vec![
            chat("dir:analyst", true, "2026-11-01T00:00:00Z"),
            chat("dir:chat-aa", false, "2026-11-05T00:00:00Z"),
        ]);
        let entries = switcher_entries(Some(&l), "analyst", "dir:chat-aa");
        assert_eq!(entries.len(), 2);
        assert!(entries[0].primary && !entries[0].active);
        assert!(entries[1].active);
        // A list that lacks the main chat still gets one at the top.
        let only_extra = list(vec![chat("dir:chat-aa", false, "x")]);
        let e = switcher_entries(Some(&only_extra), "analyst", "dir:chat-aa");
        assert_eq!(e[0].id, "dir:analyst");
        assert_eq!(e.len(), 2);
    }

    #[test]
    fn search_finds_chats_by_title() {
        let mut budget = chat("dir:chat-aa", false, "2026-11-05T00:00:00Z");
        budget.title = Some("Budget review".into());
        let l = list(vec![chat("dir:analyst", true, "x"), budget]);
        assert_eq!(chats_matching(Some(&l), "budget").len(), 1);
        assert_eq!(chats_matching(Some(&l), "BUDGET")[0].id, "dir:chat-aa");
        // The main chat is findable by its plain name too.
        assert_eq!(chats_matching(Some(&l), "main")[0].id, "dir:analyst");
        assert!(chats_matching(Some(&l), "").is_empty());
        assert!(chats_matching(Some(&l), "zzz").is_empty());
        assert!(chats_matching(None, "budget").is_empty());
    }

    #[test]
    fn chat_counts_read_naturally() {
        assert_eq!(chat_count_label(0), None);
        assert_eq!(chat_count_label(1).as_deref(), Some("2 chats"));
        assert_eq!(chat_count_label(4).as_deref(), Some("5 chats"));
        assert!(default_chats_open(true));
        assert!(!default_chats_open(false));
    }
}
