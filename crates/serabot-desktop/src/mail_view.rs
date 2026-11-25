//! Pure logic behind the 0.74.14 mail window (std only, no UI types).
//!
//! Dates, sender names and avatars, conversation grouping, multi-select, keyboard
//! shortcuts, recipient autocomplete tokens, density and pane sizing. The Dioxus
//! components in `ui_mail.rs` call these so the behaviour can be tested without a
//! window.

use std::collections::BTreeSet;

// ------------------------------------------------------------- dates ----

pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];

fn month_number(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    MONTHS
        .iter()
        .position(|m| lower.starts_with(&m.to_ascii_lowercase()))
        .map(|i| i as u32 + 1)
}

fn parse_clock(text: &str) -> Option<(i64, i64, i64)> {
    let mut parts = text.split(':');
    let h: i64 = parts.next()?.trim().parse().ok()?;
    let m: i64 = parts.next()?.trim().parse().ok()?;
    let s: i64 = parts.next().map_or(Some(0), |p| p.trim().parse().ok())?;
    ((0..24).contains(&h) && (0..60).contains(&m) && (0..61).contains(&s)).then_some((h, m, s))
}

fn parse_offset_minutes(text: &str) -> Option<i64> {
    let text = text.trim();
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    let digits: String = digits.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 4 {
        return None;
    }
    let h: i64 = digits[..2].parse().ok()?;
    let m: i64 = digits[2..].parse().ok()?;
    Some(sign * (h * 60 + m))
}

/// Unix seconds (UTC) from an RFC 2822 `Date:` header (`Mon, 23 Nov 2026 09:00:00
/// +0100 (CET)`) or an ISO date (`2026-11-23`, `2026-11-23T09:00:00Z`).
pub fn parse_mail_date(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    let raw = raw.split(" (").next().unwrap_or(raw).trim();
    if raw.len() >= 10 && raw.as_bytes()[4] == b'-' && raw.as_bytes()[7] == b'-' {
        let y: i64 = raw[..4].parse().ok()?;
        let m: u32 = raw[5..7].parse().ok()?;
        let d: u32 = raw[8..10].parse().ok()?;
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        let mut secs = days_from_civil(y, m, d) * 86_400;
        let rest = raw[10..].trim_start_matches(['T', ' ']);
        if !rest.is_empty() {
            let clock: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == ':')
                .collect();
            let (h, mi, s) = parse_clock(&clock)?;
            secs += h * 3600 + mi * 60 + s;
            let tail = &rest[clock.len()..];
            if let Some(off) = parse_offset_minutes(tail) {
                secs -= off * 60;
            }
        }
        return Some(secs);
    }
    let after_comma = raw.split_once(',').map_or(raw, |(_, rest)| rest).trim();
    let mut parts = after_comma.split_whitespace();
    let d: u32 = parts.next()?.parse().ok()?;
    let m = month_number(parts.next()?)?;
    let mut y: i64 = parts.next()?.parse().ok()?;
    if y < 100 {
        y += if y < 70 { 2000 } else { 1900 };
    }
    let (h, mi, s) = parse_clock(parts.next()?)?;
    let offset = parts.next().and_then(parse_offset_minutes).unwrap_or(0);
    if !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60 + s - offset * 60)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DayBucket {
    Today,
    Yesterday,
    ThisWeek,
    ThisMonth,
    Earlier,
}

impl DayBucket {
    pub fn label(self) -> &'static str {
        match self {
            DayBucket::Today => "Today",
            DayBucket::Yesterday => "Yesterday",
            DayBucket::ThisWeek => "This week",
            DayBucket::ThisMonth => "This month",
            DayBucket::Earlier => "Earlier",
        }
    }
}

fn local_days(ts: i64, offset_min: i64) -> i64 {
    (ts + offset_min * 60).div_euclid(86_400)
}

/// Which list section a message belongs to (`offset_min` is the viewer's UTC offset).
pub fn day_bucket(ts: i64, now: i64, offset_min: i64) -> DayBucket {
    let age = local_days(now, offset_min) - local_days(ts, offset_min);
    match age {
        i64::MIN..=0 => DayBucket::Today,
        1 => DayBucket::Yesterday,
        2..=6 => DayBucket::ThisWeek,
        7..=30 => DayBucket::ThisMonth,
        _ => DayBucket::Earlier,
    }
}

/// `09:41` today, `Mon` within the week, `23 Nov` this year, `23 Nov 2025` before.
pub fn short_time_label(ts: i64, now: i64, offset_min: i64) -> String {
    let local = ts + offset_min * 60;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let age = local_days(now, offset_min) - days;
    let (y, m, d) = civil_from_days(days);
    let (now_y, _, _) = civil_from_days(local_days(now, offset_min));
    if age <= 0 {
        format!("{:02}:{:02}", secs / 3600, (secs % 3600) / 60)
    } else if age < 7 {
        WEEKDAYS[days.rem_euclid(7) as usize].to_owned()
    } else if y == now_y {
        format!("{d} {}", MONTHS[(m - 1) as usize])
    } else {
        format!("{d} {} {y}", MONTHS[(m - 1) as usize])
    }
}

/// `Mon 23 Nov 2026, 09:41` for the reader header.
pub fn full_date_label(ts: i64, offset_min: i64) -> String {
    let local = ts + offset_min * 60;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{} {d} {} {y}, {:02}:{:02}",
        WEEKDAYS[days.rem_euclid(7) as usize],
        MONTHS[(m - 1) as usize],
        secs / 3600,
        (secs % 3600) / 60
    )
}

/// A local wall-clock moment: `add_days` after today at `hour`:00, for a viewer
/// whose UTC offset is `offset_min`.
pub fn local_time_at(now: i64, offset_min: i64, add_days: i64, hour: u32) -> i64 {
    let today = (now + offset_min * 60).div_euclid(86_400);
    (today + add_days) * 86_400 + i64::from(hour) * 3600 - offset_min * 60
}

/// `(label, unix time)` choices for Snooze, nearest first. Later today is only
/// offered before 15:00; the weekend and next-week choices are skipped when they
/// would repeat Tomorrow morning.
pub fn snooze_presets(now: i64, offset_min: i64) -> Vec<(&'static str, i64)> {
    let local = now + offset_min * 60;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    // 0 = Thursday, 2 = Saturday, 4 = Monday (see WEEKDAYS).
    let idx = days.rem_euclid(7);
    let until = |target: i64| {
        let d = (target - idx).rem_euclid(7);
        if d == 0 { 7 } else { d }
    };
    let mut out = Vec::new();
    if secs < 15 * 3600 {
        out.push(("Later today", now + 3 * 3600));
    }
    out.push(("Tomorrow morning", local_time_at(now, offset_min, 1, 9)));
    let to_sat = until(2);
    if to_sat > 1 {
        out.push(("This weekend", local_time_at(now, offset_min, to_sat, 9)));
    }
    let to_mon = until(4);
    if to_mon > 1 {
        out.push(("Next week", local_time_at(now, offset_min, to_mon, 9)));
    }
    out
}

/// A folder whose last path segment is one of `names` (case-insensitive), such as
/// Archive or Trash.
pub fn special_folder<'a>(folders: &'a [String], names: &[&str]) -> Option<&'a str> {
    let last = |f: &str| {
        f.rsplit(['/', '.'])
            .next()
            .unwrap_or(f)
            .to_ascii_lowercase()
    };
    folders
        .iter()
        .find(|f| names.iter().any(|n| last(f) == *n))
        .map(String::as_str)
}

// ---------------------------------------------------- senders, avatars ----

/// The display part of `Jane Doe <jane@x.example>`, else the address itself.
pub fn sender_name(from: &str) -> String {
    let from = from.trim();
    if let (Some(open), Some(close)) = (from.rfind('<'), from.rfind('>'))
        && open < close
    {
        let name = from[..open].trim().trim_matches('"').trim();
        if !name.is_empty() {
            return name.to_owned();
        }
        return from[open + 1..close].trim().to_owned();
    }
    from.to_owned()
}

/// The bare address of `Jane Doe <jane@x.example>`.
pub fn sender_email(from: &str) -> String {
    let from = from.trim();
    if let (Some(open), Some(close)) = (from.rfind('<'), from.rfind('>'))
        && open < close
    {
        return from[open + 1..close].trim().to_ascii_lowercase();
    }
    from.to_ascii_lowercase()
}

/// One or two capital letters for an avatar.
pub fn initials(name: &str) -> String {
    let name = name.trim();
    let base = name.split('@').next().unwrap_or(name);
    let words: Vec<&str> = base
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let mut out = String::new();
    match words.as_slice() {
        [] => out.push('?'),
        [only] => out.extend(only.chars().take(2).flat_map(char::to_uppercase)),
        [first, .., last] => {
            out.extend(first.chars().take(1).flat_map(char::to_uppercase));
            out.extend(last.chars().take(1).flat_map(char::to_uppercase));
        }
    }
    out
}

/// A stable hue (0-359) for a sender, so each person keeps one avatar colour.
pub fn avatar_hue(seed: &str) -> u16 {
    let mut hash: u32 = 2_166_136_261;
    for b in seed.trim().to_ascii_lowercase().bytes() {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(16_777_619);
    }
    (hash % 360) as u16
}

// ------------------------------------------------------------ threads ----

/// Consecutive-by-first-appearance groups: `rows` are `(uid, thread_key)` in list
/// order (newest first). Each group keeps the order its messages appear in.
pub fn group_threads(rows: &[(u32, String)]) -> Vec<(String, Vec<u32>)> {
    let mut groups: Vec<(String, Vec<u32>)> = Vec::new();
    for (uid, key) in rows {
        if key.is_empty() {
            groups.push((format!("u-{uid}"), vec![*uid]));
            continue;
        }
        match groups.iter_mut().find(|(k, _)| k == key) {
            Some((_, uids)) => uids.push(*uid),
            None => groups.push((key.clone(), vec![*uid])),
        }
    }
    groups
}

// ---------------------------------------------------------- selection ----

/// Multi-select state: Ctrl/Cmd-click toggles, Shift-click extends from the anchor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    picked: BTreeSet<u32>,
    anchor: Option<u32>,
}

impl Selection {
    pub fn contains(&self, uid: u32) -> bool {
        self.picked.contains(&uid)
    }

    pub fn len(&self) -> usize {
        self.picked.len()
    }

    pub fn is_empty(&self) -> bool {
        self.picked.is_empty()
    }

    pub fn uids(&self) -> Vec<u32> {
        self.picked.iter().copied().collect()
    }

    pub fn clear(&mut self) {
        self.picked.clear();
        self.anchor = None;
    }

    pub fn toggle(&mut self, uid: u32) {
        if !self.picked.remove(&uid) {
            self.picked.insert(uid);
        }
        self.anchor = Some(uid);
    }

    /// Select everything between the anchor and `target` in `order` (the visible
    /// order). Without an anchor this just selects `target`.
    pub fn extend_to(&mut self, order: &[u32], target: u32) {
        let Some(anchor) = self.anchor else {
            self.toggle(target);
            return;
        };
        let (Some(a), Some(b)) = (
            order.iter().position(|u| *u == anchor),
            order.iter().position(|u| *u == target),
        ) else {
            self.toggle(target);
            return;
        };
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        self.picked.extend(order[lo..=hi].iter().copied());
    }

    pub fn select_all(&mut self, order: &[u32]) {
        self.picked = order.iter().copied().collect();
        self.anchor = order.first().copied();
    }

    /// Forget uids that left the list (moved, deleted, filtered out).
    pub fn retain_in(&mut self, order: &[u32]) {
        self.picked.retain(|u| order.contains(u));
        if let Some(a) = self.anchor
            && !order.contains(&a)
        {
            self.anchor = None;
        }
    }
}

// ---------------------------------------------------------- shortcuts ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailKey {
    NextMessage,
    PrevMessage,
    OpenMessage,
    Compose,
    Reply,
    ReplyAll,
    Forward,
    Archive,
    Delete,
    ToggleStar,
    ToggleRead,
    Snooze,
    SelectToggle,
    SelectAll,
    FocusSearch,
    Undo,
    Escape,
    Help,
    GoInbox,
}

/// Map a key press to a mail action. `typing` is true while focus is in an input,
/// textarea or editable element: then only Escape and Ctrl/Cmd chords apply.
pub fn map_key(key: &str, shift: bool, ctrl: bool, alt: bool, typing: bool) -> Option<MailKey> {
    if alt {
        return None;
    }
    if ctrl {
        return match key.to_ascii_lowercase().as_str() {
            "a" if !typing => Some(MailKey::SelectAll),
            "z" if !typing => Some(MailKey::Undo),
            _ => None,
        };
    }
    if key == "Escape" {
        return Some(MailKey::Escape);
    }
    if typing {
        return None;
    }
    match (key, shift) {
        ("j" | "ArrowDown", false) => Some(MailKey::NextMessage),
        ("k" | "ArrowUp", false) => Some(MailKey::PrevMessage),
        ("Enter" | "o", false) => Some(MailKey::OpenMessage),
        ("c", false) => Some(MailKey::Compose),
        ("r", false) => Some(MailKey::Reply),
        ("a" | "R", _) => Some(MailKey::ReplyAll),
        ("f" | "F", _) => Some(MailKey::Forward),
        ("e", false) => Some(MailKey::Archive),
        ("#" | "Delete", _) => Some(MailKey::Delete),
        ("s", false) => Some(MailKey::ToggleStar),
        ("u", false) => Some(MailKey::ToggleRead),
        ("z", false) => Some(MailKey::Snooze),
        ("x", false) => Some(MailKey::SelectToggle),
        ("/", false) => Some(MailKey::FocusSearch),
        ("?", _) => Some(MailKey::Help),
        ("g", false) => Some(MailKey::GoInbox),
        _ => None,
    }
}

/// `(keys, what it does)` rows for the shortcut cheat sheet.
pub const MAIL_SHORTCUTS: &[(&str, &str)] = &[
    ("j / k", "Next / previous message"),
    ("Enter", "Open the selected message"),
    ("c", "Compose a new message"),
    ("r / a", "Reply / reply all"),
    ("f", "Forward"),
    ("e", "Archive"),
    ("#", "Delete"),
    ("s", "Star or unstar"),
    ("u", "Mark read or unread"),
    ("z", "Snooze"),
    ("x", "Select the message"),
    ("Ctrl+A", "Select all in this list"),
    ("Ctrl+Z", "Undo the last action"),
    ("/", "Search"),
    ("g", "Go to the inbox"),
    ("Esc", "Close, clear the selection"),
    ("?", "Show this list"),
];

// -------------------------------------------------------- recipients ----

/// Most files one message may carry.
pub const MAX_ATTACH_FILES: usize = 10;
/// Raw size of all attachments together. The message is base64 encoded and the
/// server caps it at 25 MiB, so this leaves room for the body and encoding.
pub const MAX_ATTACH_TOTAL_BYTES: u64 = 18 * 1024 * 1024;

/// Split what was typed or pasted into separate recipients at commas, semicolons
/// and line breaks, but not inside quotes or `<...>` (so `"Doe, Jane" <j@x.example>`
/// stays one entry).
pub fn split_recipient_input(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut angle = false;
    for c in text.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                current.push(c);
            }
            '<' if !in_quote => {
                angle = true;
                current.push(c);
            }
            '>' if !in_quote => {
                angle = false;
                current.push(c);
            }
            ',' | ';' | '\n' | '\r' if !in_quote && !angle => {
                let piece = current.trim();
                if !piece.is_empty() {
                    out.push(piece.to_owned());
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }
    let piece = current.trim();
    if !piece.is_empty() {
        out.push(piece.to_owned());
    }
    out
}

fn address_parts(entry: &str) -> Option<(String, String)> {
    let addr = sender_email(entry);
    if addr.is_empty()
        || addr.len() > 254
        || addr.chars().any(|c| {
            c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';' | '"')
        })
    {
        return None;
    }
    let (local, domain) = addr.split_once('@')?;
    (!local.is_empty() && !domain.is_empty() && !domain.contains('@'))
        .then(|| (local.to_owned(), domain.to_owned()))
}

/// Something that could be an address (`bob@host`, or `Name <bob@host>`).
pub fn is_plausible_recipient(entry: &str) -> bool {
    address_parts(entry).is_some()
}

/// A plausible address whose domain looks finished (`bob@example.org`), used to
/// decide whether a space or a pause means "this one is done".
pub fn is_complete_address(entry: &str) -> bool {
    address_parts(entry).is_some_and(|(_, domain)| {
        !domain.starts_with('.')
            && !domain.ends_with('.')
            && domain
                .rsplit_once('.')
                .is_some_and(|(_, tld)| tld.len() >= 2)
    })
}

/// Add one or more recipients from `text`, skipping duplicates (compared by bare
/// address, case-insensitively). Returns how many were added.
pub fn add_recipients(list: &mut Vec<String>, text: &str) -> usize {
    let mut added = 0;
    for entry in split_recipient_input(text) {
        let key = sender_email(&entry);
        if !list.iter().any(|have| sender_email(have) == key) {
            list.push(entry);
            added += 1;
        }
    }
    added
}

/// Entries that can be turned into chips now, and the unfinished rest.
pub fn drain_plausible(text: &str) -> (Vec<String>, String) {
    let mut accepted = Vec::new();
    let mut rest = Vec::new();
    for entry in split_recipient_input(text) {
        if is_plausible_recipient(&entry) {
            accepted.push(entry);
        } else {
            rest.push(entry);
        }
    }
    (accepted, rest.join(", "))
}

pub fn join_recipients(list: &[String]) -> String {
    list.join(", ")
}

/// The text of a recipient chip: the name when there is one, else the address.
pub fn recipient_label(entry: &str) -> String {
    let name = sender_name(entry);
    if name.trim().is_empty() {
        entry.trim().to_owned()
    } else {
        name
    }
}

/// Whether one more file of `size` bytes fits next to what is already attached.
pub fn check_attachment(count: usize, total_bytes: u64, size: u64) -> Result<(), String> {
    if size == 0 {
        return Err("That file is empty.".to_owned());
    }
    if count >= MAX_ATTACH_FILES {
        return Err(format!(
            "A message can carry at most {MAX_ATTACH_FILES} files."
        ));
    }
    if total_bytes.saturating_add(size) > MAX_ATTACH_TOTAL_BYTES {
        return Err(format!(
            "Attachments are limited to {} MB in total.",
            MAX_ATTACH_TOTAL_BYTES / (1024 * 1024)
        ));
    }
    Ok(())
}

// ------------------------------------------------ recipient autocomplete ----

/// Split a recipients field at the entry being typed: `("a@x.example, ", "bo")`.
pub fn split_last_token(field: &str) -> (&str, &str) {
    let cut = field.rfind([',', ';']).map_or(0, |i| i + 1);
    let (head, tail) = field.split_at(cut);
    (head, tail.trim_start())
}

/// Replace the entry being typed with a chosen address and leave the cursor ready
/// for the next one.
pub fn apply_suggestion(field: &str, email: &str) -> String {
    let (head, _) = split_last_token(field);
    let head = head.trim_end();
    if head.is_empty() {
        format!("{email}, ")
    } else {
        format!("{head} {email}, ")
    }
}

/// The text to search contacts for, or `None` when it is too short or already an
/// address.
pub fn suggestion_query(field: &str) -> Option<String> {
    let (_, token) = split_last_token(field);
    let token = token.trim();
    (token.chars().count() >= 2 && !token.contains(['@', '<', '>'])).then(|| token.to_owned())
}

// ------------------------------------------------------------ autosave ----

/// Save a draft once edits stopped for `quiet_secs`, and only if something changed
/// since the last save.
pub fn autosave_due(last_change: i64, last_save: i64, now: i64, quiet_secs: i64) -> bool {
    last_change > last_save && now - last_change >= quiet_secs
}

// ------------------------------------------------------------- layout ----

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Density {
    #[default]
    Comfortable,
    Compact,
}

impl Density {
    pub fn class(self) -> &'static str {
        match self {
            Density::Comfortable => "density-comfortable",
            Density::Compact => "density-compact",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Density::Comfortable => "Comfortable",
            Density::Compact => "Compact",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Density::Comfortable => Density::Compact,
            Density::Compact => Density::Comfortable,
        }
    }
}

pub const SIDEBAR_RANGE: (i32, i32) = (180, 320);
pub const LIST_RANGE: (i32, i32) = (260, 560);
pub const DEFAULT_SIDEBAR: i32 = 220;
pub const DEFAULT_LIST: i32 = 340;
/// The reader never gets narrower than this.
pub const READER_MIN: i32 = 360;

/// Keep both panes inside their ranges and leave the reader at least
/// [`READER_MIN`] wide inside a window `total` pixels wide.
pub fn clamp_panes(sidebar: i32, list: i32, total: i32) -> (i32, i32) {
    let sidebar = sidebar.clamp(SIDEBAR_RANGE.0, SIDEBAR_RANGE.1);
    let room = (total - sidebar - READER_MIN).max(LIST_RANGE.0);
    let list = list.clamp(LIST_RANGE.0, LIST_RANGE.1.min(room));
    (sidebar, list)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-25 10:00:00 UTC
    const NOW: i64 = 1_795_600_800;

    #[test]
    fn civil_round_trip() {
        for days in [-1000_i64, 0, 365, 20_000, 20_597, 40_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(days_from_civil(2026, 11, 25) * 86_400 + 10 * 3600, NOW);
    }

    #[test]
    fn rfc2822_and_iso_dates_parse() {
        let base = days_from_civil(2026, 11, 23) * 86_400;
        assert_eq!(
            parse_mail_date("Mon, 23 Nov 2026 09:00:00 +0000"),
            Some(base + 9 * 3600)
        );
        assert_eq!(
            parse_mail_date("Mon, 23 Nov 2026 09:00:00 +0100 (CET)"),
            Some(base + 8 * 3600)
        );
        assert_eq!(
            parse_mail_date("23 Nov 2026 09:00 -0500"),
            Some(base + 14 * 3600)
        );
        assert_eq!(parse_mail_date("2026-11-23"), Some(base));
        assert_eq!(
            parse_mail_date("2026-11-23T09:00:00Z"),
            Some(base + 9 * 3600)
        );
        assert_eq!(
            parse_mail_date("2026-11-23T09:00:00+02:00"),
            Some(base + 7 * 3600)
        );
        assert_eq!(parse_mail_date("yesterday-ish"), None);
        assert_eq!(parse_mail_date("32 Nov 2026 09:00 +0000"), None);
        assert_eq!(parse_mail_date(""), None);
    }

    #[test]
    fn buckets_and_labels() {
        let h = |hours: i64| NOW - hours * 3600;
        assert_eq!(day_bucket(h(1), NOW, 0), DayBucket::Today);
        assert_eq!(day_bucket(h(11), NOW, 0), DayBucket::Yesterday);
        assert_eq!(day_bucket(h(24 * 4), NOW, 0), DayBucket::ThisWeek);
        assert_eq!(day_bucket(h(24 * 20), NOW, 0), DayBucket::ThisMonth);
        assert_eq!(day_bucket(h(24 * 90), NOW, 0), DayBucket::Earlier);
        assert_eq!(day_bucket(NOW + 5000, NOW, 0), DayBucket::Today);
        // The viewer's offset moves the midnight boundary.
        assert_eq!(day_bucket(h(2), NOW, 0), DayBucket::Today);
        assert_eq!(day_bucket(h(2), NOW, -9 * 60), DayBucket::Yesterday);

        assert_eq!(short_time_label(NOW - 3600, NOW, 0), "09:00");
        assert_eq!(short_time_label(NOW - 3600, NOW, 60), "10:00");
        assert_eq!(short_time_label(NOW - 86_400 * 2, NOW, 0), "Mon");
        assert_eq!(short_time_label(NOW - 86_400 * 20, NOW, 0), "5 Nov");
        assert_eq!(short_time_label(NOW - 86_400 * 400, NOW, 0), "21 Oct 2025");
        assert_eq!(full_date_label(NOW, 0), "Wed 25 Nov 2026, 10:00");
        assert_eq!(DayBucket::ThisWeek.label(), "This week");
    }

    #[test]
    fn snooze_presets_follow_the_viewers_clock() {
        // Wed 25 Nov 2026 10:00 UTC.
        let p = snooze_presets(NOW, 0);
        let labels: Vec<&str> = p.iter().map(|(l, _)| *l).collect();
        assert_eq!(
            labels,
            vec![
                "Later today",
                "Tomorrow morning",
                "This weekend",
                "Next week"
            ]
        );
        assert_eq!(p[0].1, NOW + 3 * 3600);
        assert_eq!(full_date_label(p[1].1, 0), "Thu 26 Nov 2026, 09:00");
        assert_eq!(full_date_label(p[2].1, 0), "Sat 28 Nov 2026, 09:00");
        assert_eq!(full_date_label(p[3].1, 0), "Mon 30 Nov 2026, 09:00");
        // 17:00 local (UTC+7): no "Later today", and 09:00 means 02:00 UTC.
        let late = snooze_presets(NOW, 7 * 60);
        assert_eq!(late[0].0, "Tomorrow morning");
        assert_eq!(full_date_label(late[0].1, 7 * 60), "Thu 26 Nov 2026, 09:00");
        // On a Friday, tomorrow is the weekend; on a Sunday, tomorrow is Monday.
        let fri = snooze_presets(NOW + 86_400 * 2, 0);
        assert!(!fri.iter().any(|(l, _)| *l == "This weekend"));
        let sun = snooze_presets(NOW + 86_400 * 4, 0);
        assert!(!sun.iter().any(|(l, _)| *l == "Next week"));
        assert!(sun.iter().any(|(l, _)| *l == "This weekend"));
    }

    #[test]
    fn archive_and_trash_folders_are_found() {
        let folders: Vec<String> = ["INBOX", "INBOX.Archive", "INBOX.Trash", "Sent"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            special_folder(&folders, &["archive", "archives"]),
            Some("INBOX.Archive")
        );
        assert_eq!(
            special_folder(&folders, &["trash", "deleted items"]),
            Some("INBOX.Trash")
        );
        assert_eq!(special_folder(&folders, &["spam", "junk"]), None);
    }

    #[test]
    fn senders_and_avatars() {
        assert_eq!(sender_name("\"Doe, Jane\" <Jane@X.example>"), "Doe, Jane");
        assert_eq!(sender_name("<solo@x.example>"), "solo@x.example");
        assert_eq!(sender_name("bare@x.example"), "bare@x.example");
        assert_eq!(sender_email("Jane <Jane@X.example>"), "jane@x.example");
        assert_eq!(initials("Jane Doe"), "JD");
        assert_eq!(initials("Jean-Luc Picard"), "JP");
        assert_eq!(initials("anna@example.example"), "AN");
        assert_eq!(initials("madonna"), "MA");
        assert_eq!(initials(""), "?");
        assert_eq!(avatar_hue("A@x.example"), avatar_hue(" a@x.example "));
        assert!(avatar_hue("anyone") < 360);
        assert_ne!(avatar_hue("a@x.example"), avatar_hue("b@x.example"));
    }

    #[test]
    fn threads_group_in_list_order() {
        let rows = vec![
            (9, "t-a".to_owned()),
            (8, "t-b".to_owned()),
            (7, "t-a".to_owned()),
            (6, String::new()),
            (5, "t-a".to_owned()),
        ];
        let groups = group_threads(&rows);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0], ("t-a".to_owned(), vec![9, 7, 5]));
        assert_eq!(groups[1], ("t-b".to_owned(), vec![8]));
        assert_eq!(groups[2], ("u-6".to_owned(), vec![6]));
    }

    #[test]
    fn selection_toggle_range_and_prune() {
        let order = [10, 9, 8, 7, 6];
        let mut sel = Selection::default();
        sel.toggle(9);
        sel.extend_to(&order, 6);
        assert_eq!(sel.uids(), vec![6, 7, 8, 9]);
        sel.toggle(7);
        assert!(!sel.contains(7));
        assert_eq!(sel.len(), 3);
        sel.retain_in(&[10, 9, 8]);
        assert_eq!(sel.uids(), vec![8, 9]);
        sel.select_all(&order);
        assert_eq!(sel.len(), 5);
        sel.clear();
        assert!(sel.is_empty());
        sel.extend_to(&order, 8);
        assert_eq!(sel.uids(), vec![8]);
        let mut gone = Selection::default();
        gone.toggle(99);
        gone.extend_to(&order, 7);
        assert!(gone.contains(7));
    }

    #[test]
    fn keys_map_only_when_not_typing() {
        assert_eq!(
            map_key("j", false, false, false, false),
            Some(MailKey::NextMessage)
        );
        assert_eq!(map_key("j", false, false, false, true), None);
        assert_eq!(
            map_key("Escape", false, false, false, true),
            Some(MailKey::Escape)
        );
        assert_eq!(
            map_key("c", false, false, false, false),
            Some(MailKey::Compose)
        );
        assert_eq!(
            map_key("R", true, false, false, false),
            Some(MailKey::ReplyAll)
        );
        assert_eq!(
            map_key("r", false, false, false, false),
            Some(MailKey::Reply)
        );
        assert_eq!(
            map_key("a", false, false, false, false),
            Some(MailKey::ReplyAll)
        );
        assert_eq!(
            map_key("#", true, false, false, false),
            Some(MailKey::Delete)
        );
        assert_eq!(
            map_key("a", false, true, false, false),
            Some(MailKey::SelectAll)
        );
        assert_eq!(map_key("a", false, true, false, true), None);
        assert_eq!(map_key("z", false, true, false, false), Some(MailKey::Undo));
        assert_eq!(map_key("j", false, false, true, false), None);
        assert_eq!(map_key("?", true, false, false, false), Some(MailKey::Help));
        assert_eq!(map_key("q", false, false, false, false), None);
    }

    #[test]
    fn typed_and_pasted_recipients_split_cleanly() {
        assert_eq!(
            split_recipient_input(
                "a@x.example, \"Doe, Jane\" <j@x.example>; b@y.example\nc@z.example"
            ),
            vec![
                "a@x.example",
                "\"Doe, Jane\" <j@x.example>",
                "b@y.example",
                "c@z.example"
            ]
        );
        assert!(split_recipient_input("  , ; ").is_empty());
        assert_eq!(
            split_recipient_input("Name <a@x.example, b@y.example>").len(),
            1
        );
    }

    #[test]
    fn plausible_versus_complete_addresses() {
        assert!(is_plausible_recipient("bob@host"));
        assert!(!is_complete_address("bob@host"));
        assert!(is_complete_address("bob@example.org"));
        assert!(is_complete_address("Bob <bob@example.org>"));
        assert!(!is_complete_address("bob@example."));
        assert!(!is_plausible_recipient("bob"));
        assert!(!is_plausible_recipient("a@b@c"));
        assert!(!is_plausible_recipient("bob @example.org"));
        assert!(!is_plausible_recipient("@example.org"));
        assert!(!is_plausible_recipient(""));
    }

    #[test]
    fn adding_recipients_skips_duplicates_and_keeps_names() {
        let mut list = vec!["Bob <bob@example.org>".to_owned()];
        assert_eq!(
            add_recipients(&mut list, "BOB@example.org, carol@example.org"),
            1
        );
        assert_eq!(list, vec!["Bob <bob@example.org>", "carol@example.org"]);
        assert_eq!(add_recipients(&mut list, ""), 0);
        assert_eq!(
            join_recipients(&list),
            "Bob <bob@example.org>, carol@example.org"
        );
        assert_eq!(recipient_label("Bob <bob@example.org>"), "Bob");
        assert_eq!(recipient_label("carol@example.org"), "carol@example.org");
    }

    #[test]
    fn draining_keeps_unfinished_text() {
        let (done, rest) = drain_plausible("a@x.example, b@y.example, ali");
        assert_eq!(done, vec!["a@x.example", "b@y.example"]);
        assert_eq!(rest, "ali");
        let (done, rest) = drain_plausible("");
        assert!(done.is_empty() && rest.is_empty());
    }

    #[test]
    fn attachment_limits() {
        assert!(check_attachment(0, 0, 1024).is_ok());
        assert!(check_attachment(0, 0, 0).is_err());
        assert!(check_attachment(MAX_ATTACH_FILES, 0, 10).is_err());
        assert!(check_attachment(1, MAX_ATTACH_TOTAL_BYTES - 5, 10).is_err());
        assert!(check_attachment(1, MAX_ATTACH_TOTAL_BYTES - 10, 10).is_ok());
        assert!(check_attachment(0, 0, u64::MAX).is_err());
    }

    #[test]
    fn recipient_tokens() {
        assert_eq!(split_last_token("a@x.example, bo"), ("a@x.example,", "bo"));
        assert_eq!(split_last_token("bo"), ("", "bo"));
        assert_eq!(split_last_token(""), ("", ""));
        assert_eq!(suggestion_query("a@x.example, bo"), Some("bo".to_owned()));
        assert_eq!(suggestion_query("b"), None);
        assert_eq!(suggestion_query("bob@"), None);
        assert_eq!(
            apply_suggestion("a@x.example, bo", "bob@y.example"),
            "a@x.example, bob@y.example, "
        );
        assert_eq!(apply_suggestion("bo", "bob@y.example"), "bob@y.example, ");
    }

    #[test]
    fn autosave_waits_for_quiet_and_changes() {
        assert!(!autosave_due(100, 100, 200, 3));
        assert!(!autosave_due(100, 50, 102, 3));
        assert!(autosave_due(100, 50, 103, 3));
        assert!(!autosave_due(0, 0, 10, 3));
    }

    #[test]
    fn density_and_panes() {
        assert_eq!(Density::default(), Density::Comfortable);
        assert_eq!(Density::Comfortable.toggled(), Density::Compact);
        assert_eq!(Density::Compact.class(), "density-compact");
        assert_eq!(clamp_panes(100, 100, 1400), (180, 260));
        assert_eq!(clamp_panes(220, 340, 1400), (220, 340));
        assert_eq!(clamp_panes(900, 900, 1400), (320, 560));
        // A narrow window shrinks the list so the reader keeps its minimum.
        let (s, l) = clamp_panes(220, 560, 1000);
        assert_eq!(s, 220);
        assert!(s + l + READER_MIN <= 1000 || l == LIST_RANGE.0);
        assert_eq!(l, 420);
    }
}
