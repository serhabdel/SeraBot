//! Message-list rules: day separators, the working line, the unseen-message
//! badge on the scroll-to-bottom button, and the empty-state suggestions.
//! Dates are UTC `YYYY-MM-DD` prefixes, the same convention `msg_stamp` uses.

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Days since 1970-01-01 for a civil date (proleptic Gregorian).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let year_of_era = y - era * 400;
    let shifted_month = (month as i64 + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The civil date for a count of days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = (if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    }) as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Parse `YYYY-MM-DD` (the first ten characters of a timestamp).
fn parse_date(text: &str) -> Option<(i64, u32, u32)> {
    let bytes = text.as_bytes();
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i64 = text.get(0..4)?.parse().ok()?;
    let month: u32 = text.get(5..7)?.parse().ok()?;
    let day: u32 = text.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some((year, month, day))
}

/// The UTC date a message was made on, as `YYYY-MM-DD`. Optimistic messages
/// (`Just now`) belong to `today`. Unparseable stamps have no day.
pub fn day_key(created_at: &str, today: &str) -> Option<String> {
    if created_at == "Just now" {
        return Some(today.to_owned());
    }
    let (year, month, day) = parse_date(created_at)?;
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

/// Heading for a day separator: `Today`, `Yesterday`, `Oct 30`, or
/// `Oct 30, 2025` when the year differs from today's.
pub fn day_heading(key: &str, today: &str) -> String {
    let Some((year, month, day)) = parse_date(key) else {
        return key.to_owned();
    };
    let Some((today_year, today_month, today_day)) = parse_date(today) else {
        return key.to_owned();
    };
    let day_number = days_from_civil(year, month, day);
    let today_number = days_from_civil(today_year, today_month, today_day);
    if day_number == today_number {
        return "Today".into();
    }
    if day_number + 1 == today_number {
        return "Yesterday".into();
    }
    let month_name = MONTHS[(month - 1) as usize];
    if year == today_year {
        format!("{month_name} {day}")
    } else {
        format!("{month_name} {day}, {year}")
    }
}

/// The day before a `YYYY-MM-DD` key, for tests and the separator logic.
pub fn previous_day(key: &str) -> Option<String> {
    let (year, month, day) = parse_date(key)?;
    let (y, m, d) = civil_from_days(days_from_civil(year, month, day) - 1);
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// The working line under the transcript: `Main is working`, plus the
/// current step when there is one (`Main is working: Searching the web`).
pub fn working_line(bot_name: &str, step: Option<&str>) -> String {
    match step.map(str::trim).filter(|step| !step.is_empty()) {
        Some(step) => format!("{bot_name} is working: {step}"),
        None => format!("{bot_name} is working"),
    }
}

/// The badge on the scroll-to-bottom button: nothing at zero, `99+` past 99.
pub fn unseen_badge(count: usize) -> Option<String> {
    match count {
        0 => None,
        1..=99 => Some(count.to_string()),
        _ => Some("99+".into()),
    }
}

const MAIN_SUGGESTIONS: &[&str] = &[
    "Plan my week from my deadlines",
    "Summarize the files in my inbox",
    "What should I do first today?",
    "Draft a short reply to my latest message",
];
const ANALYST_SUGGESTIONS: &[&str] = &[
    "Summarize the spreadsheet I attached",
    "Find the totals that changed this month",
    "Check this ledger for gaps",
];
const MAKER_SUGGESTIONS: &[&str] = &[
    "Draft a two-page brief from my notes",
    "Make a five-slide deck from this outline",
    "Turn this into a one-page PDF",
];
const WATCHER_SUGGESTIONS: &[&str] = &[
    "Tell me what changed since yesterday",
    "Watch this page for updates",
];
const LEGAL_SUGGESTIONS: &[&str] = &[
    "Explain the key clauses in this contract",
    "List the deadlines in my documents",
];
const AUDITOR_SUGGESTIONS: &[&str] = &[
    "Run the audit checklist on these statements",
    "Check this bank reconciliation for exceptions",
    "List the evidence I need for a payroll review",
];
const COMPTABLE_SUGGESTIONS: &[&str] = &[
    "Reconcile this bank statement with my ledger",
    "Estimate my IS for 2026",
    "Prepare this month's payroll summary",
    "Explain the TVA return for this quarter",
];
const MARKETER_SUGGESTIONS: &[&str] = &[
    "Draft a launch plan for my new service",
    "Write three social posts for this offer",
    "Outline an SEO brief for my product page",
];
const MARKETS_SUGGESTIONS: &[&str] = &[
    "Research the margin trend of this listed company",
    "Sketch a DCF range with every assumption written out",
    "Explain the currency exposure in my portfolio",
];
const COURIER_SUGGESTIONS: &[&str] = &[
    "Draft a reminder for a client who is late on payment",
    "Write a follow-up after our meeting on Tuesday",
    "Send the signed quote to my own Telegram chat",
];
const INBOX_SUGGESTIONS: &[&str] = &[
    "Rank my unread messages by priority",
    "Summarize the thread about the delayed delivery",
    "Draft a polite reply to my latest invoice question",
];
const WHATSAPP_SUGGESTIONS: &[&str] = &[
    "Draft a reminder about tomorrow's appointment",
    "Write a short follow-up for a supplier who has not replied",
    "Draft a message to confirm my appointment changes",
];
const GENERAL_SUGGESTIONS: &[&str] = &[
    "Summarize the attached file",
    "Help me plan the next step",
    "Explain this in plain words",
    "Draft a short message for me",
];

/// Starter prompts for an empty chat, tailored to the bot it talks to.
/// Unknown bots and group rooms get the general set.
pub fn suggestions(bot_id: Option<&str>) -> &'static [&'static str] {
    match bot_id {
        Some("main") => MAIN_SUGGESTIONS,
        Some("analyst") => ANALYST_SUGGESTIONS,
        Some("maker") => MAKER_SUGGESTIONS,
        Some("watcher") => WATCHER_SUGGESTIONS,
        Some("legal") => LEGAL_SUGGESTIONS,
        Some("auditor") => AUDITOR_SUGGESTIONS,
        Some("comptable") => COMPTABLE_SUGGESTIONS,
        Some("marketer") => MARKETER_SUGGESTIONS,
        Some("markets") => MARKETS_SUGGESTIONS,
        Some("courier") => COURIER_SUGGESTIONS,
        Some("inbox") => INBOX_SUGGESTIONS,
        Some("whatsapp") => WHATSAPP_SUGGESTIONS,
        _ => GENERAL_SUGGESTIONS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_days_round_trip_and_anchor_on_the_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        for days in [-800_000_i64, -1, 0, 1, 59, 60, 365, 10_957, 20_000, 800_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "{y}-{m}-{d}");
        }
        // Leap day handling: 2024 is a leap year, 2100 is not.
        assert_eq!(
            days_from_civil(2024, 3, 1) - days_from_civil(2024, 2, 28),
            2
        );
        assert_eq!(
            days_from_civil(2100, 3, 1) - days_from_civil(2100, 2, 28),
            1
        );
    }

    #[test]
    fn day_headings_say_today_yesterday_or_the_date() {
        let today = "2026-11-07";
        assert_eq!(day_heading("2026-11-07", today), "Today");
        assert_eq!(day_heading("2026-11-06", today), "Yesterday");
        assert_eq!(day_heading("2026-10-30", today), "Oct 30");
        assert_eq!(day_heading("2025-12-31", today), "Dec 31, 2025");
        // Month and year boundaries.
        assert_eq!(day_heading("2026-10-31", "2026-11-01"), "Yesterday");
        assert_eq!(day_heading("2026-01-01", "2026-01-02"), "Yesterday");
        assert_eq!(previous_day("2026-03-01").as_deref(), Some("2026-02-28"));
        assert_eq!(previous_day("2024-03-01").as_deref(), Some("2024-02-29"));
    }

    #[test]
    fn day_keys_group_messages_and_skip_unparseable_stamps() {
        let today = "2026-11-07";
        assert_eq!(day_key("Just now", today).as_deref(), Some(today));
        assert_eq!(
            day_key("2026-11-06T23:59:59Z", today).as_deref(),
            Some("2026-11-06")
        );
        assert_eq!(day_key("nope", today), None);
        assert_eq!(day_key("2026-13-01T00:00:00Z", today), None);
        assert_eq!(day_heading("garbage", today), "garbage");
    }

    #[test]
    fn working_line_names_the_bot_and_the_current_step() {
        assert_eq!(working_line("Main", None), "Main is working");
        assert_eq!(working_line("Main", Some("  ")), "Main is working");
        assert_eq!(
            working_line("Analyst", Some("Reading the ledger")),
            "Analyst is working: Reading the ledger"
        );
    }

    #[test]
    fn unseen_badge_caps_at_99_plus() {
        assert_eq!(unseen_badge(0), None);
        assert_eq!(unseen_badge(3).as_deref(), Some("3"));
        assert_eq!(unseen_badge(99).as_deref(), Some("99"));
        assert_eq!(unseen_badge(100).as_deref(), Some("99+"));
    }

    #[test]
    fn suggestions_are_tailored_and_fall_back_for_others() {
        assert!(suggestions(Some("main")).contains(&"Plan my week from my deadlines"));
        assert!(
            suggestions(Some("analyst"))
                .iter()
                .any(|s| s.contains("spreadsheet"))
        );
        assert_eq!(suggestions(Some("unknown-bot")), GENERAL_SUGGESTIONS);
        assert_eq!(suggestions(None), GENERAL_SUGGESTIONS);
        for set in [
            suggestions(Some("main")),
            suggestions(Some("analyst")),
            suggestions(Some("maker")),
            suggestions(Some("watcher")),
            suggestions(Some("legal")),
            suggestions(Some("auditor")),
            suggestions(Some("comptable")),
            suggestions(Some("marketer")),
            suggestions(Some("markets")),
            suggestions(Some("courier")),
            suggestions(Some("inbox")),
            suggestions(Some("whatsapp")),
            suggestions(None),
        ] {
            assert!((2..=4).contains(&set.len()));
            assert!(set.iter().all(|text| !text.contains('\u{2192}')));
        }
        assert!(
            suggestions(Some("comptable"))
                .iter()
                .any(|s| s.contains("ledger"))
        );
    }

    /// Every template in `bots/` has its own starter prompts, so no shipped
    /// bot falls back to the general set. The directory is read, not listed
    /// by hand, so a new template without prompts fails here.
    #[test]
    fn every_shipped_bot_has_its_own_starter_prompts() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bots");
        let mut checked = 0usize;
        for entry in std::fs::read_dir(&root).expect("bots directory") {
            let path = entry.expect("bots entry").path();
            if path.extension().and_then(|value| value.to_str()) != Some("toml") {
                continue;
            }
            let id = path.file_stem().and_then(|stem| stem.to_str()).unwrap();
            let set = suggestions(Some(id));
            assert_ne!(set, GENERAL_SUGGESTIONS, "{id} has no starter prompts");
            assert!((2..=4).contains(&set.len()), "{id}: {} prompts", set.len());
            assert!(set.iter().all(|text| !text.trim().is_empty()), "{id}");
            checked += 1;
        }
        assert!(checked >= 12, "expected the twelve shipped bot files");
    }
}
