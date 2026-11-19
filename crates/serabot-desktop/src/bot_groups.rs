//! Order of the bots in the sidebar (0.63): pinned bots first, then the bots
//! used recently (newest first, or with unread messages), then everyone else
//! from A to Z. Pure, so the order is tested without a window.

/// Days a bot stays in "Recent" after its last message.
pub const RECENT_DAYS: i64 = 7;

/// The sidebar sub-group a bot falls in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BotGroup {
    Pinned,
    Recent,
    All,
}

impl BotGroup {
    pub fn title(self) -> &'static str {
        match self {
            Self::Pinned => "Pinned",
            Self::Recent => "Recent",
            Self::All => "All bots",
        }
    }
}

/// What the order needs to know about one bot row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotKey {
    pub name: String,
    pub pinned: bool,
    pub unread: i64,
    /// RFC 3339 time of the latest message in the bot's chats, if any.
    pub last_at: Option<String>,
}

/// Days since 1970-01-01 of a `YYYY-MM-DD…` timestamp.
fn day_number(stamp: &str) -> Option<i64> {
    let year: i64 = stamp.get(0..4)?.parse().ok()?;
    let month: i64 = stamp.get(5..7)?.parse().ok()?;
    let day: i64 = stamp.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// The group of one bot, given today's `YYYY-MM-DD`.
pub fn group_of(key: &BotKey, today: &str) -> BotGroup {
    if key.pinned {
        return BotGroup::Pinned;
    }
    let recent = key.unread > 0
        || matches!(
            (key.last_at.as_deref().and_then(day_number), day_number(today)),
            (Some(last), Some(now)) if now - last <= RECENT_DAYS
        );
    if recent {
        BotGroup::Recent
    } else {
        BotGroup::All
    }
}

/// Indices of `keys` in sidebar order, each with its group.
pub fn order(keys: &[BotKey], today: &str) -> Vec<(usize, BotGroup)> {
    let mut rows: Vec<(usize, BotGroup)> = keys
        .iter()
        .enumerate()
        .map(|(index, key)| (index, group_of(key, today)))
        .collect();
    rows.sort_by(|(a, ga), (b, gb)| {
        ga.cmp(gb).then_with(|| {
            let (ka, kb) = (&keys[*a], &keys[*b]);
            match ga {
                // Pinned keep the order they were pinned in (the input order).
                BotGroup::Pinned => a.cmp(b),
                BotGroup::Recent => kb
                    .last_at
                    .cmp(&ka.last_at)
                    .then_with(|| ka.name.to_lowercase().cmp(&kb.name.to_lowercase())),
                BotGroup::All => ka.name.to_lowercase().cmp(&kb.name.to_lowercase()),
            }
        })
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str, pinned: bool, unread: i64, last: Option<&str>) -> BotKey {
        BotKey {
            name: name.into(),
            pinned,
            unread,
            last_at: last.map(str::to_owned),
        }
    }

    #[test]
    fn pinned_then_recent_then_a_to_z() {
        let keys = vec![
            key("Writer", false, 0, Some("2026-10-01T10:00:00Z")),
            key("Analyst", false, 0, Some("2026-11-18T09:00:00Z")),
            key("main", true, 0, None),
            key("Legal", false, 2, Some("2026-09-01T10:00:00Z")),
            key("Auditor", false, 0, None),
            key("Maker", false, 0, Some("2026-11-19T08:00:00Z")),
        ];
        let got: Vec<(&str, BotGroup)> = order(&keys, "2026-11-19")
            .into_iter()
            .map(|(i, g)| (keys[i].name.as_str(), g))
            .collect();
        assert_eq!(
            got,
            vec![
                ("main", BotGroup::Pinned),
                ("Maker", BotGroup::Recent),
                ("Analyst", BotGroup::Recent),
                ("Legal", BotGroup::Recent),
                ("Auditor", BotGroup::All),
                ("Writer", BotGroup::All),
            ]
        );
    }

    #[test]
    fn the_recent_window_and_bad_dates() {
        assert_eq!(
            group_of(
                &key("a", false, 0, Some("2026-11-12T00:00:00Z")),
                "2026-11-19"
            ),
            BotGroup::Recent
        );
        assert_eq!(
            group_of(
                &key("a", false, 0, Some("2026-11-11T23:59:00Z")),
                "2026-11-19"
            ),
            BotGroup::All
        );
        assert_eq!(
            group_of(&key("a", false, 0, Some("garbage")), "2026-11-19"),
            BotGroup::All
        );
        assert_eq!(day_number("1970-01-01"), Some(0));
        assert_eq!(day_number("2000-03-01"), Some(11_017));
    }
}
