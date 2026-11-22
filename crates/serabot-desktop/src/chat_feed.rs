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

/// The working line with the single running step's live elapsed time
/// (0.74.3): `Main is working: Searching the web · 4s`. Without an elapsed
/// time it is the plain [`working_line`].
pub fn working_line_elapsed(bot_name: &str, step: Option<&str>, elapsed: Option<&str>) -> String {
    let line = working_line(bot_name, step);
    match elapsed.map(str::trim).filter(|elapsed| !elapsed.is_empty()) {
        Some(elapsed) => format!("{line} \u{b7} {elapsed}"),
        None => line,
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

/// Lower-cased text a transcript search looks in, built once per row: the
/// message and who said it.
pub fn search_haystack(content: &str, speaker: &str) -> String {
    let mut text = content.to_lowercase();
    text.push('\n');
    text.push_str(&speaker.to_lowercase());
    text
}

/// Whether a row matches a trimmed, lower-cased search needle.
pub fn row_matches(haystack: &str, needle: &str) -> bool {
    !needle.is_empty() && haystack.contains(needle)
}

/// Forget the rows that already had their entrance animation once the set has
/// outgrown the transcript, so a long-lived chat does not keep every id it
/// ever saw. Rows still on screen are kept (no animation replays).
pub fn trim_entered<'a>(
    entered: &mut std::collections::HashSet<String>,
    live: impl IntoIterator<Item = &'a str>,
    rows: usize,
) {
    if entered.len() <= rows.saturating_add(ENTERED_SLACK) {
        return;
    }
    let live: std::collections::HashSet<&str> = live.into_iter().collect();
    entered.retain(|id| live.contains(id.as_str()));
}

/// How far the entrance-animation set may run ahead of the rows on screen.
const ENTERED_SLACK: usize = 64;

/// A fingerprint of everything a transcript row is drawn from, so the view
/// rebuilds its rows only when one of them changed. `context` carries what
/// the rows depend on besides the messages (speaker names, group mode, ...).
/// `ledger_revision` is the [`TurnLedger`](crate::turn_trace::TurnLedger)
/// revision, and `activity` the live turn's steps: a finished turn's trace
/// attaches to its reply, and a step starting, running or finishing changes
/// what the rows show, so both must invalidate the cache (0.74.3).
pub fn transcript_fingerprint(
    messages: &[crate::Message],
    context: impl std::hash::Hash,
    ledger_revision: u64,
    activity: &crate::activity::Activity,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    context.hash(&mut hasher);
    messages.len().hash(&mut hasher);
    for message in messages {
        message.id.hash(&mut hasher);
        (message.role == crate::Role::User).hash(&mut hasher);
        message.content.hash(&mut hasher);
        message.created_at.hash(&mut hasher);
        message.name.hash(&mut hasher);
        message.bot.hash(&mut hasher);
        message.limit_reached.hash(&mut hasher);
        message.turn_id.hash(&mut hasher);
        message.attachments.len().hash(&mut hasher);
    }
    ledger_revision.hash(&mut hasher);
    activity.task_id.hash(&mut hasher);
    activity.steps.len().hash(&mut hasher);
    activity.dropped.hash(&mut hasher);
    for step in &activity.steps {
        step.call_id.hash(&mut hasher);
        step.label.hash(&mut hasher);
        step.icon.hash(&mut hasher);
        // `StepPhase` has no `Hash`: its discriminant stands in.
        (match step.phase {
            crate::activity::StepPhase::Running => 0u8,
            crate::activity::StepPhase::Progress => 1u8,
            crate::activity::StepPhase::Done => 2u8,
            crate::activity::StepPhase::Failed => 3u8,
            crate::activity::StepPhase::WaitingApproval => 4u8,
        })
        .hash(&mut hasher);
        step.duration_ms.hash(&mut hasher);
        step.approval_id.hash(&mut hasher);
        step.error.hash(&mut hasher);
        step.cards.len().hash(&mut hasher);
    }
    hasher.finish()
}

/// What the live region says when a reply lands.
pub fn reply_announcement(speaker: &str) -> String {
    let speaker = speaker.trim();
    if speaker.is_empty() {
        "A reply arrived".into()
    } else {
        format!("{speaker} replied")
    }
}

/// The unread badge in the rail: nothing at zero, `99+` past 99.
pub fn unread_badge(count: i64) -> Option<String> {
    match count {
        i64::MIN..=0 => None,
        1..=99 => Some(count.to_string()),
        _ => Some("99+".into()),
    }
}

/// What a screen reader says for an unread badge.
pub fn unread_label(count: i64) -> String {
    match count {
        i64::MIN..=0 => "No unread messages".into(),
        1 => "1 unread message".into(),
        2..=99 => format!("{count} unread messages"),
        _ => "More than 99 unread messages".into(),
    }
}

/// A bot's badge: its main chat plus every other chat of that bot.
pub fn rolled_up_unread<'a>(primary: i64, others: impl IntoIterator<Item = &'a i64>) -> i64 {
    others.into_iter().fold(primary.max(0), |total, count| {
        total.saturating_add((*count).max(0))
    })
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
const HOSTINGER_SUGGESTIONS: &[&str] = &[
    "Triage my Hostinger inbox and summarize what needs a reply",
    "Read my latest client message and draft a reply",
    "Check my Hostinger mailbox for new invoices",
];
const MAILBOX_SUGGESTIONS: &[&str] = &[
    "Triage all my connected mailboxes at once",
    "What needs a reply in my work account today?",
    "Draft a reply to the latest message in the shop mailbox",
];
const WHATSAPP_SUGGESTIONS: &[&str] = &[
    "Draft a reminder about tomorrow's appointment",
    "Write a short follow-up for a supplier who has not replied",
    "Draft a message to confirm my appointment changes",
];
const JOBFINDER_SUGGESTIONS: &[&str] = &[
    "Find remote Rust or backend roles open this week",
    "Tailor my CV for this job description",
    "Draft a concise cover letter and application checklist",
];
const RESEARCHER_SUGGESTIONS: &[&str] = &[
    "Research the pros and cons of this approach, with sources",
    "Compare three public sources on this topic and note where they disagree",
    "Fact-check these claims and cite what you find",
];
const WRITER_SUGGESTIONS: &[&str] = &[
    "Draft a 600-word article from this outline",
    "Edit this text for clarity and keep my voice",
    "Proofread this letter and list what you changed",
];
const TRANSLATOR_SUGGESTIONS: &[&str] = &[
    "Translate this text into French and keep the tone",
    "Translate this document and build a glossary of key terms",
    "Rewrite this translation in a more formal register",
];
const PLANNER_SUGGESTIONS: &[&str] = &[
    "Plan my week from my tasks and calendar",
    "Break this goal into next actions with time estimates",
    "Review what is overdue and propose a catch-up plan",
];
const SUPPORT_SUGGESTIONS: &[&str] = &[
    "Draft a calm reply to this customer complaint",
    "Write FAQ answers from our policy document",
    "Suggest the next step for this support request",
];
const PRODUCT_SUGGESTIONS: &[&str] = &[
    "Write a PRD from these notes",
    "Turn this feature idea into user stories with acceptance criteria",
    "Draft customer-facing release notes from this change list",
];
const TUTOR_SUGGESTIONS: &[&str] = &[
    "Explain this concept step by step with an example",
    "Quiz me on this chapter, one question at a time",
    "Build a two-week study plan for this topic",
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
        Some("hostinger") => HOSTINGER_SUGGESTIONS,
        Some("mailbox") => MAILBOX_SUGGESTIONS,
        Some("whatsapp") => WHATSAPP_SUGGESTIONS,
        Some("jobfinder") => JOBFINDER_SUGGESTIONS,
        Some("researcher") => RESEARCHER_SUGGESTIONS,
        Some("writer") => WRITER_SUGGESTIONS,
        Some("translator") => TRANSLATOR_SUGGESTIONS,
        Some("planner") => PLANNER_SUGGESTIONS,
        Some("support") => SUPPORT_SUGGESTIONS,
        Some("product") => PRODUCT_SUGGESTIONS,
        Some("tutor") => TUTOR_SUGGESTIONS,
        _ => GENERAL_SUGGESTIONS,
    }
}

const MAIN_QUICK_CHIPS: &[&str] = &[
    "Check today's tasks & calendar",
    "List my OneDrive files",
    "Ask Vault for a secret safely",
];
const WHATSAPP_QUICK_CHIPS: &[&str] = &[
    "Check my WhatsApp number status",
    "List approved WhatsApp templates",
    "Send a WhatsApp update",
];
const ANALYST_QUICK_CHIPS: &[&str] = &[
    "Analyze my uploaded spreadsheet",
    "Compare monthly totals",
    "Flag missing ledger rows",
];
const MAKER_QUICK_CHIPS: &[&str] = &[
    "Create an executive PDF brief",
    "Build a presentation deck",
    "Format a clean summary doc",
];
const WATCHER_QUICK_CHIPS: &[&str] = &[
    "Check my tracked deadlines",
    "Monitor a webpage for changes",
    "Show today's reminders",
];
const COMPTABLE_QUICK_CHIPS: &[&str] = &[
    "Review bank reconciliation",
    "Check invoice & TVA totals",
    "Summarize payroll entries",
];
const RESEARCHER_QUICK_CHIPS: &[&str] = &[
    "Compare sources with citations",
    "Fact-check a claim",
    "Summarize recent news on a topic",
];
const GENERAL_QUICK_CHIPS: &[&str] = &[
    "Summarize an attached file",
    "Break this goal into next steps",
    "Draft a concise message",
];

/// Short action chips shown above starter prompts in an empty chat, tailored to the open bot.
pub fn quick_chips(bot_id: Option<&str>) -> &'static [&'static str] {
    match bot_id {
        Some("main") | None => MAIN_QUICK_CHIPS,
        Some("whatsapp") => WHATSAPP_QUICK_CHIPS,
        Some("analyst") | Some("auditor") => ANALYST_QUICK_CHIPS,
        Some("maker") | Some("writer") => MAKER_QUICK_CHIPS,
        Some("watcher") | Some("planner") => WATCHER_QUICK_CHIPS,
        Some("comptable") => COMPTABLE_QUICK_CHIPS,
        Some("researcher") | Some("markets") => RESEARCHER_QUICK_CHIPS,
        Some(_) => GENERAL_QUICK_CHIPS,
    }
}

/// Library artifacts grouped by the assistant message of the turn that
/// produced them, plus any artifacts from an in-progress turn (or a
/// conversation with no assistant reply yet).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssignedLibrary {
    pub by_message: std::collections::BTreeMap<String, Vec<crate::LibraryEntry>>,
    pub trailing: Vec<crate::LibraryEntry>,
}

/// Assign each library artifact to the assistant message of the user turn
/// whose time window `[turn_user_ts, next_user_ts)` produced it.
///
/// Artifacts are sorted oldest-first by `(created_at, hash)` so each turn's
/// attachments render in chronological creation order. Artifacts from an
/// active turn that has no assistant reply yet go into `trailing`.
pub fn assign_library_to_messages(
    messages: &[crate::Message],
    library: &[crate::LibraryEntry],
) -> AssignedLibrary {
    let mut assigned = AssignedLibrary::default();
    if library.is_empty() {
        return assigned;
    }

    let mut sorted = library.to_vec();
    sorted.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.hash.cmp(&b.hash))
    });

    struct Turn<'a> {
        start_ts: Option<&'a str>,
        last_assistant_id: Option<&'a str>,
    }

    let mut leading_assistant_id: Option<&str> = None;
    let mut turns: Vec<Turn<'_>> = Vec::new();

    for msg in messages {
        match msg.role {
            crate::Role::User => {
                let start_ts = (msg.created_at != "Just now").then_some(msg.created_at.as_str());
                turns.push(Turn {
                    start_ts,
                    last_assistant_id: None,
                });
            }
            crate::Role::Assistant => {
                if let Some(turn) = turns.last_mut() {
                    turn.last_assistant_id = Some(msg.id.as_str());
                } else {
                    leading_assistant_id = Some(msg.id.as_str());
                }
            }
        }
    }

    for entry in sorted {
        let ts = entry.created_at.as_str();
        let target_id = if turns.is_empty() {
            leading_assistant_id
        } else {
            let matching_turn = turns
                .iter()
                .rfind(|turn| turn.start_ts.is_some_and(|start| start <= ts));
            match matching_turn {
                Some(turn) => turn.last_assistant_id,
                None => leading_assistant_id.or_else(|| turns[0].last_assistant_id),
            }
        };

        if let Some(id) = target_id {
            assigned
                .by_message
                .entry(id.to_owned())
                .or_default()
                .push(entry);
        } else {
            assigned.trailing.push(entry);
        }
    }

    assigned
}

#[cfg(test)]
mod tests {
    fn synthetic(id: &str, text: &str) -> crate::Message {
        crate::Message {
            id: id.into(),
            role: crate::Role::Assistant,
            content: text.into(),
            created_at: "2026-10-29T10:00:00.000Z".into(),
            name: None,
            bot: None,
            attachments: Vec::new(),
            limit_reached: false,
            turn_id: None,
        }
    }

    #[test]
    fn the_fingerprint_changes_with_content_rows_and_context_only() {
        let rows = vec![synthetic("m1", "hello"), synthetic("m2", "world")];
        let activity = crate::activity::Activity::default();
        let base = transcript_fingerprint(&rows, ("dir:main", true), 7, &activity);
        assert_eq!(
            base,
            transcript_fingerprint(&rows.clone(), ("dir:main", true), 7, &activity)
        );
        let mut grown = rows.clone();
        grown[1].content.push_str(" and more");
        assert_ne!(
            base,
            transcript_fingerprint(&grown, ("dir:main", true), 7, &activity)
        );
        let mut longer = rows.clone();
        longer.push(synthetic("m3", "x"));
        assert_ne!(
            base,
            transcript_fingerprint(&longer, ("dir:main", true), 7, &activity)
        );
        assert_ne!(
            base,
            transcript_fingerprint(&rows, ("dir:main", false), 7, &activity)
        );
        let mut limited = rows.clone();
        limited[0].limit_reached = true;
        assert_ne!(
            base,
            transcript_fingerprint(&limited, ("dir:main", true), 7, &activity)
        );
        // A turn-ledger change (a finished turn's trace landing) and a step
        // starting or changing phase both invalidate the rows.
        assert_ne!(
            base,
            transcript_fingerprint(&rows, ("dir:main", true), 8, &activity)
        );
        let mut stepped = crate::activity::Activity::default();
        stepped.apply(
            "turn.step",
            &serde_json::json!({"task_id": "t1", "call_id": "c1", "phase": "running",
               "label": "Searching the web", "icon": "globe"}),
        );
        assert_ne!(
            base,
            transcript_fingerprint(&rows, ("dir:main", true), 7, &stepped)
        );
        stepped.apply(
            "turn.step",
            &serde_json::json!({"task_id": "t1", "call_id": "c1", "phase": "done",
               "label": "Searching the web", "icon": "globe", "duration_ms": 1200}),
        );
        assert_ne!(
            base,
            transcript_fingerprint(&rows, ("dir:main", true), 7, &stepped)
        );
    }

    #[test]
    fn working_line_with_elapsed_keeps_the_plain_line_without_one() {
        assert_eq!(
            working_line_elapsed("Main", Some("Searching the web"), Some("4s")),
            "Main is working: Searching the web \u{b7} 4s"
        );
        assert_eq!(
            working_line_elapsed("Main", Some("Searching the web"), None),
            "Main is working: Searching the web"
        );
        assert_eq!(
            working_line_elapsed("Main", None, Some("4s")),
            "Main is working \u{b7} 4s"
        );
        assert_eq!(working_line_elapsed("Main", None, None), "Main is working");
    }

    #[test]
    fn search_uses_one_lowercase_haystack_per_row() {
        let hay = search_haystack("Quarterly REPORT is ready", "Analyst");
        assert!(row_matches(&hay, "report"));
        assert!(row_matches(&hay, "analyst"));
        assert!(!row_matches(&hay, "invoice"));
        assert!(!row_matches(&hay, ""), "an empty needle matches nothing");
    }

    #[test]
    fn the_entered_set_is_trimmed_to_the_rows_on_screen_once_it_outgrows_them() {
        let mut entered: std::collections::HashSet<String> =
            (0..200).map(|n| format!("old-{n}")).collect();
        entered.insert("live-1".into());
        trim_entered(&mut entered, ["live-1", "live-2"], 2);
        assert_eq!(entered.len(), 1);
        assert!(entered.contains("live-1"));
        // Within the slack nothing is dropped.
        let mut small: std::collections::HashSet<String> =
            ["a".to_owned(), "b".to_owned()].into_iter().collect();
        trim_entered(&mut small, ["a"], 1);
        assert_eq!(small.len(), 2);
    }

    #[test]
    fn a_finished_reply_is_announced_by_speaker() {
        assert_eq!(reply_announcement("Analyst"), "Analyst replied");
        assert_eq!(reply_announcement("  "), "A reply arrived");
    }

    #[test]
    fn unread_badges_cap_at_99_and_roll_up_child_chats() {
        assert_eq!(unread_badge(0), None);
        assert_eq!(unread_badge(-3), None);
        assert_eq!(unread_badge(7).as_deref(), Some("7"));
        assert_eq!(unread_badge(99).as_deref(), Some("99"));
        assert_eq!(unread_badge(100).as_deref(), Some("99+"));
        assert_eq!(unread_label(1), "1 unread message");
        assert_eq!(unread_label(4), "4 unread messages");
        assert_eq!(unread_label(250), "More than 99 unread messages");
        assert_eq!(rolled_up_unread(4, [&2, &0, &-1]), 6);
        assert_eq!(rolled_up_unread(0, std::iter::empty::<&i64>()), 0);
    }

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
            suggestions(Some("jobfinder")),
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

    fn test_msg(id: &str, role: crate::Role, created_at: &str) -> crate::Message {
        crate::Message {
            id: id.into(),
            role,
            content: String::new(),
            created_at: created_at.into(),
            name: None,
            bot: None,
            attachments: Vec::new(),
            limit_reached: false,
            turn_id: None,
        }
    }

    fn test_entry(hash: &str, created_at: &str) -> crate::LibraryEntry {
        crate::LibraryEntry {
            action_id: format!("act-{hash}"),
            tool: "maker".into(),
            hash: hash.into(),
            filename: Some(format!("{hash}.png")),
            title: None,
            mime: Some("image/png".into()),
            created_at: created_at.into(),
        }
    }

    #[test]
    fn assign_library_groups_by_turn_in_chronological_order() {
        let messages = vec![
            test_msg("u1", crate::Role::User, "2026-11-15T10:00:00Z"),
            test_msg("a1", crate::Role::Assistant, "2026-11-15T10:01:05Z"),
            test_msg("u2", crate::Role::User, "2026-11-15T11:40:00Z"),
            test_msg("a2", crate::Role::Assistant, "2026-11-15T11:45:00Z"),
        ];
        // Provided newest-first (as the API returns them).
        let library = vec![
            test_entry("img4", "2026-11-15T11:44:00Z"),
            test_entry("img2", "2026-11-15T11:43:10Z"),
            test_entry("img3", "2026-11-15T11:43:30Z"),
            test_entry("img1", "2026-11-15T11:43:00Z"),
            test_entry("deck", "2026-11-15T10:01:00Z"),
        ];

        let assigned = assign_library_to_messages(&messages, &library);
        assert!(assigned.trailing.is_empty());
        assert_eq!(
            assigned
                .by_message
                .get("a1")
                .unwrap()
                .iter()
                .map(|e| e.hash.as_str())
                .collect::<Vec<_>>(),
            vec!["deck"]
        );
        assert_eq!(
            assigned
                .by_message
                .get("a2")
                .unwrap()
                .iter()
                .map(|e| e.hash.as_str())
                .collect::<Vec<_>>(),
            vec!["img1", "img2", "img3", "img4"]
        );
    }

    #[test]
    fn assign_library_puts_active_turn_without_reply_in_trailing() {
        let messages = vec![
            test_msg("u1", crate::Role::User, "2026-11-15T10:00:00Z"),
            test_msg("a1", crate::Role::Assistant, "2026-11-15T10:01:05Z"),
            test_msg("u2", crate::Role::User, "2026-11-15T11:40:00Z"),
        ];
        let library = vec![
            test_entry("deck", "2026-11-15T10:01:00Z"),
            test_entry("wip1", "2026-11-15T11:41:00Z"),
            test_entry("wip2", "2026-11-15T11:42:00Z"),
        ];

        let assigned = assign_library_to_messages(&messages, &library);
        assert_eq!(assigned.by_message.get("a1").unwrap().len(), 1);
        assert_eq!(
            assigned
                .trailing
                .iter()
                .map(|e| e.hash.as_str())
                .collect::<Vec<_>>(),
            vec!["wip1", "wip2"]
        );
    }

    #[test]
    fn assign_library_attaches_post_reply_artifact_before_next_user_turn() {
        let messages = vec![
            test_msg("u1", crate::Role::User, "2026-11-15T10:00:00Z"),
            test_msg("a1", crate::Role::Assistant, "2026-11-15T10:01:05Z"),
            test_msg("u2", crate::Role::User, "2026-11-15T11:40:00Z"),
            test_msg("a2", crate::Role::Assistant, "2026-11-15T11:45:00Z"),
        ];
        let library = vec![test_entry("late", "2026-11-15T10:01:08Z")];

        let assigned = assign_library_to_messages(&messages, &library);
        assert!(assigned.trailing.is_empty());
        assert_eq!(
            assigned
                .by_message
                .get("a1")
                .unwrap()
                .iter()
                .map(|e| e.hash.as_str())
                .collect::<Vec<_>>(),
            vec!["late"]
        );
        assert!(!assigned.by_message.contains_key("a2"));
    }

    #[test]
    fn assign_library_handles_empty_inputs_and_optimistic_user_message() {
        let empty = assign_library_to_messages(&[], &[]);
        assert!(empty.by_message.is_empty());
        assert!(empty.trailing.is_empty());

        let only_lib = assign_library_to_messages(&[], &[test_entry("a", "2026-11-15T10:00:00Z")]);
        assert!(only_lib.by_message.is_empty());
        assert_eq!(only_lib.trailing.len(), 1);

        let messages = vec![
            test_msg("u1", crate::Role::User, "2026-11-15T10:00:00Z"),
            test_msg("a1", crate::Role::Assistant, "2026-11-15T10:01:05Z"),
            test_msg("opt-1", crate::Role::User, "Just now"),
        ];
        let library = vec![test_entry("deck", "2026-11-15T10:01:00Z")];
        let assigned = assign_library_to_messages(&messages, &library);
        assert!(assigned.trailing.is_empty());
        assert_eq!(assigned.by_message.get("a1").unwrap().len(), 1);
    }
}
