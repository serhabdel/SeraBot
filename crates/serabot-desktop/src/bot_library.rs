//! A bot's library (0.63): every file in the bot's chats, the ones its bots
//! made and the ones the member uploaded, with filters, search, date groups
//! and sort. Pure, so the view rules are tested without a window.

use serde::Deserialize;

/// Most items the desktop accepts in one library response.
pub const MAX_ITEMS: usize = 3_500;

/// One file in a bot's library, as `GET /v1/bots/{bot}/library` sends it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct LibraryItem {
    /// `generated` (a file the bots made) or `uploaded` (the member's file).
    pub kind: String,
    /// Artifact hash for `generated`, attachment id for `uploaded`.
    pub id: String,
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub mime: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    pub conversation_id: String,
    #[serde(default)]
    pub conversation_title: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BotLibrary {
    pub bot: String,
    pub items: Vec<LibraryItem>,
}

impl LibraryItem {
    pub fn generated(&self) -> bool {
        self.kind == "generated"
    }

    /// The name to show: title, then file name, then a short id.
    pub fn label(&self) -> String {
        self.name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .or_else(|| self.filename.clone())
            .unwrap_or_else(|| self.id.chars().take(12).collect())
    }

    fn mime(&self) -> &str {
        self.mime.as_deref().unwrap_or_default()
    }

    pub fn is_media(&self) -> bool {
        let mime = self.mime();
        mime.starts_with("image/") || mime.starts_with("audio/") || mime.starts_with("video/")
    }

    pub fn is_document(&self) -> bool {
        !self.is_media()
    }

    /// "pptx", "pdf" … for the type chip.
    pub fn extension(&self) -> String {
        self.filename
            .as_deref()
            .and_then(|name| {
                name.rsplit_once('.')
                    .map(|(_, ext)| ext.to_ascii_lowercase())
            })
            .filter(|ext| ext.len() <= 5)
            .unwrap_or_default()
    }

    /// `YYYY-MM-DD` of the item.
    pub fn day(&self) -> &str {
        self.created_at.get(0..10).unwrap_or_default()
    }
}

/// Which items the library shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KindFilter {
    #[default]
    All,
    Generated,
    Uploaded,
    Media,
    Documents,
}

impl KindFilter {
    pub const ALL: [Self; 5] = [
        Self::All,
        Self::Generated,
        Self::Uploaded,
        Self::Media,
        Self::Documents,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Generated => "Made by bots",
            Self::Uploaded => "Uploaded",
            Self::Media => "Media",
            Self::Documents => "Documents",
        }
    }

    fn admits(self, item: &LibraryItem) -> bool {
        match self {
            Self::All => true,
            Self::Generated => item.generated(),
            Self::Uploaded => !item.generated(),
            Self::Media => item.is_media(),
            Self::Documents => item.is_document(),
        }
    }
}

/// How far back the library looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DateFilter {
    #[default]
    Any,
    Today,
    Week,
    Month,
}

impl DateFilter {
    pub const ALL: [Self; 4] = [Self::Any, Self::Today, Self::Week, Self::Month];

    pub fn label(self) -> &'static str {
        match self {
            Self::Any => "Any time",
            Self::Today => "Today",
            Self::Week => "7 days",
            Self::Month => "30 days",
        }
    }

    fn days(self) -> Option<i64> {
        match self {
            Self::Any => None,
            Self::Today => Some(0),
            Self::Week => Some(7),
            Self::Month => Some(30),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortOrder {
    #[default]
    Newest,
    Oldest,
    Name,
}

impl SortOrder {
    pub const ALL: [Self; 3] = [Self::Newest, Self::Oldest, Self::Name];

    pub fn label(self) -> &'static str {
        match self {
            Self::Newest => "Newest",
            Self::Oldest => "Oldest",
            Self::Name => "Name",
        }
    }
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` day.
fn day_number(day: &str) -> Option<i64> {
    let year: i64 = day.get(0..4)?.parse().ok()?;
    let month: i64 = day.get(5..7)?.parse().ok()?;
    let d: i64 = day.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// The date group heading of one item: Today, Yesterday, This week, This
/// month, or the month ("2026-08").
pub fn date_group(item_day: &str, today: &str) -> String {
    match (day_number(item_day), day_number(today)) {
        (Some(day), Some(now)) => match now - day {
            0 => "Today".into(),
            1 => "Yesterday".into(),
            2..=6 => "This week".into(),
            7..=30 => "This month".into(),
            _ => item_day.get(0..7).unwrap_or(item_day).to_owned(),
        },
        _ => "Earlier".into(),
    }
}

/// The items to show, filtered and sorted.
pub fn view<'a>(
    items: &'a [LibraryItem],
    kind: KindFilter,
    date: DateFilter,
    query: &str,
    sort: SortOrder,
    today: &str,
) -> Vec<&'a LibraryItem> {
    let needle = query.trim().to_lowercase();
    let now = day_number(today);
    let mut out: Vec<&LibraryItem> = items
        .iter()
        .filter(|item| kind.admits(item))
        .filter(|item| match (date.days(), now, day_number(item.day())) {
            (None, _, _) => true,
            (Some(window), Some(now), Some(day)) => now - day <= window && day <= now,
            _ => false,
        })
        .filter(|item| {
            needle.is_empty()
                || item.label().to_lowercase().contains(&needle)
                || item
                    .filename
                    .as_deref()
                    .is_some_and(|name| name.to_lowercase().contains(&needle))
                || item.tool.to_lowercase().contains(&needle)
                || item
                    .conversation_title
                    .as_deref()
                    .is_some_and(|title| title.to_lowercase().contains(&needle))
        })
        .collect();
    match sort {
        SortOrder::Newest => out.sort_by(|a, b| b.created_at.cmp(&a.created_at)),
        SortOrder::Oldest => out.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        SortOrder::Name => out.sort_by_key(|item| item.label().to_lowercase()),
    }
    out
}

/// "2.4 MB" for a size, when known.
pub fn human_size(bytes: Option<u64>) -> Option<String> {
    let bytes = bytes?;
    Some(if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: &str, name: &str, mime: &str, at: &str) -> LibraryItem {
        LibraryItem {
            kind: kind.into(),
            id: format!("id-{name}"),
            tool: if kind == "generated" {
                "deck_build"
            } else {
                "upload"
            }
            .into(),
            name: Some(name.into()),
            filename: Some(name.into()),
            mime: Some(mime.into()),
            size_bytes: None,
            conversation_id: "dir:analyst".into(),
            conversation_title: None,
            created_at: at.into(),
        }
    }

    fn sample() -> Vec<LibraryItem> {
        vec![
            item(
                "generated",
                "plan.pptx",
                "application/vnd.ms-powerpoint",
                "2026-11-19T09:00:00Z",
            ),
            item(
                "uploaded",
                "photo.jpg",
                "image/jpeg",
                "2026-11-18T10:00:00Z",
            ),
            item(
                "generated",
                "voice.mp3",
                "audio/mpeg",
                "2026-10-01T10:00:00Z",
            ),
            item(
                "uploaded",
                "contract.pdf",
                "application/pdf",
                "2026-11-12T10:00:00Z",
            ),
        ]
    }

    fn names(items: Vec<&LibraryItem>) -> Vec<String> {
        items.into_iter().map(LibraryItem::label).collect()
    }

    #[test]
    fn kinds_dates_search_and_sort() {
        let items = sample();
        let today = "2026-11-19";
        assert_eq!(
            names(view(
                &items,
                KindFilter::Generated,
                DateFilter::Any,
                "",
                SortOrder::Newest,
                today
            )),
            vec!["plan.pptx", "voice.mp3"]
        );
        assert_eq!(
            names(view(
                &items,
                KindFilter::Media,
                DateFilter::Any,
                "",
                SortOrder::Name,
                today
            )),
            vec!["photo.jpg", "voice.mp3"]
        );
        assert_eq!(
            names(view(
                &items,
                KindFilter::All,
                DateFilter::Week,
                "",
                SortOrder::Oldest,
                today
            )),
            vec!["contract.pdf", "photo.jpg", "plan.pptx"]
        );
        assert_eq!(
            names(view(
                &items,
                KindFilter::All,
                DateFilter::Today,
                "",
                SortOrder::Newest,
                today
            )),
            vec!["plan.pptx"]
        );
        assert_eq!(
            names(view(
                &items,
                KindFilter::All,
                DateFilter::Any,
                "CONTRACT",
                SortOrder::Newest,
                today
            )),
            vec!["contract.pdf"]
        );
    }

    #[test]
    fn date_groups_read_naturally() {
        let today = "2026-11-19";
        assert_eq!(date_group("2026-11-19", today), "Today");
        assert_eq!(date_group("2026-11-18", today), "Yesterday");
        assert_eq!(date_group("2026-11-14", today), "This week");
        assert_eq!(date_group("2026-11-01", today), "This month");
        assert_eq!(date_group("2026-08-03", today), "2026-08");
        assert_eq!(date_group("bad", today), "Earlier");
    }

    #[test]
    fn labels_sizes_and_extensions() {
        let mut nameless = item(
            "generated",
            "x.pdf",
            "application/pdf",
            "2026-11-19T00:00:00Z",
        );
        nameless.name = None;
        nameless.filename = None;
        assert_eq!(nameless.label(), "id-x.pdf");
        assert_eq!(item("uploaded", "A.PDF", "", "").extension(), "pdf");
        assert_eq!(human_size(Some(2_621_440)).as_deref(), Some("2.5 MB"));
        assert_eq!(human_size(Some(2048)).as_deref(), Some("2 KB"));
        assert_eq!(human_size(None), None);
    }
}
