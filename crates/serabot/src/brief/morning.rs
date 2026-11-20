//! Member morning brief (0.70): the short daily message sent on the member's
//! alert channels. It extends the watcher brief in the parent module (same
//! deadline and opportunity rows) with calendar, tasks, jobs, mail, cost and
//! alerts. Deterministic text, no model call, bounded, no secrets.

use super::*;

/// Most lines per section of the member brief.
const BRIEF_SECTION_ITEMS: usize = 3;
/// Hard bound for the member brief text.
const MEMBER_BRIEF_MAX_CHARS: usize = 1800;

/// Read-only data a member brief takes from connected services. `None` means
/// "not connected or not reachable": the section is left out silently.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BriefFeeds {
    /// Today's calendar events, one printable line each (`09:30 Standup`).
    pub calendar: Option<Vec<String>>,
    /// New mail in the last 24 hours.
    pub mail: Option<MailPeek>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailPeek {
    pub new_count: usize,
    /// Up to three subjects, newest first.
    pub subjects: Vec<String>,
}

/// One printable line, free of control characters and bounded: feed text
/// (event titles, mail subjects) is untrusted data.
fn feed_line(value: &str, max: usize) -> String {
    let flat: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    truncate_chars(&flat.split_whitespace().collect::<Vec<_>>().join(" "), max)
}

/// Today's events from a Google (`items`) or Outlook (`value`) calendar
/// reply, as `HH:MM title` lines in `tz`, in time order.
pub fn calendar_lines(reply: &serde_json::Value, today: NaiveDate, tz: Tz) -> Vec<String> {
    let items = reply
        .get("items")
        .or_else(|| reply.get("value"))
        .and_then(serde_json::Value::as_array);
    let mut events: Vec<(String, String)> = Vec::new();
    for item in items.into_iter().flatten() {
        let title = item
            .get("summary")
            .or_else(|| item.get("subject"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("(busy)");
        let start = item.get("start");
        if let Some(date) = start
            .and_then(|start| start.get("date"))
            .and_then(serde_json::Value::as_str)
        {
            if date == today.format("%Y-%m-%d").to_string() {
                events.push(("00:00".into(), format!("all day  {}", feed_line(title, 80))));
            }
            continue;
        }
        let Some(raw) = start
            .and_then(|start| start.get("dateTime"))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        // Outlook sends UTC without an offset; Google sends an offset.
        let instant = DateTime::parse_from_rfc3339(raw)
            .map(|value| value.with_timezone(&Utc))
            .or_else(|_| {
                NaiveDateTime::parse_from_str(raw.trim_end_matches('Z'), "%Y-%m-%dT%H:%M:%S%.f")
                    .map(|naive| naive.and_utc())
            });
        let Ok(instant) = instant else {
            continue;
        };
        let local = instant.with_timezone(&tz);
        if local.date_naive() != today {
            continue;
        }
        let time = local.format("%H:%M").to_string();
        events.push((time.clone(), format!("{time}  {}", feed_line(title, 80))));
    }
    events.sort();
    events.into_iter().map(|(_, line)| line).collect()
}

/// New Outlook mail (`value`: `subject`, `receivedDateTime`) in the 24 hours
/// before `now`.
pub fn mail_peek(reply: &serde_json::Value, now: DateTime<Utc>) -> MailPeek {
    let since = now - Duration::hours(24);
    let mut rows: Vec<(DateTime<Utc>, String)> = Vec::new();
    for item in reply
        .get("value")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let received = item
            .get("receivedDateTime")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_utc);
        let Some(received) = received else {
            continue;
        };
        if received < since || received > now + Duration::minutes(5) {
            continue;
        }
        let subject = item
            .get("subject")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("(no subject)");
        rows.push((received, feed_line(subject, 80)));
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    MailPeek {
        new_count: rows.len(),
        subjects: rows
            .into_iter()
            .take(BRIEF_SECTION_ITEMS)
            .map(|(_, subject)| subject)
            .collect(),
    }
}

/// Read today's events and recent mail for one member from the connectors
/// they connected themselves. Failures and missing connections leave the
/// field `None`; nothing here writes or sends.
pub async fn collect_feeds(
    personal: &crate::personal::PersonalConnectors,
    user_id: &str,
    now: DateTime<Utc>,
    tz: Tz,
) -> BriefFeeds {
    use crate::connectors::ConnectorKind;
    let today = now.with_timezone(&tz).date_naive();
    let args = serde_json::json!({"page_size": 25});
    let mut feeds = BriefFeeds::default();
    for kind in [
        ConnectorKind::GoogleCalendar,
        ConnectorKind::OutlookCalendar,
    ] {
        if !personal.connected(user_id, kind).unwrap_or(false) {
            continue;
        }
        if let Ok(reply) = personal.read(user_id, kind, &args).await {
            let lines = calendar_lines(&reply, today, tz);
            feeds.calendar.get_or_insert_with(Vec::new).extend(lines);
        }
    }
    if let Some(lines) = feeds.calendar.as_mut() {
        lines.sort();
    }
    if personal
        .connected(user_id, ConnectorKind::OutlookMail)
        .unwrap_or(false)
        && let Ok(reply) = personal
            .read(user_id, ConnectorKind::OutlookMail, &args)
            .await
    {
        feeds.mail = Some(mail_peek(&reply, now));
    }
    feeds
}

/// New job matches since `since`: the job watcher's `job_seen` rows marked
/// as matching the member's profile (migration 0048).
fn job_matches_since(
    store: &Store,
    user_id: &str,
    since: DateTime<Utc>,
) -> Option<(i64, Vec<String>)> {
    let connection = store.connection().ok()?;
    let cutoff = since.to_rfc3339();
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM job_seen WHERE user_id = ?1 AND matched = 1 AND first_seen >= ?2",
            params![user_id, cutoff],
            |row| row.get(0),
        )
        .ok()?;
    if count == 0 {
        return None;
    }
    let mut titles = Vec::new();
    if let Ok(mut statement) = connection.prepare(
        "SELECT title FROM job_seen WHERE user_id = ?1 AND matched = 1 AND first_seen >= ?2
         ORDER BY first_seen DESC LIMIT 3",
    ) && let Ok(rows) = statement.query_map(params![user_id, cutoff], |row| {
        row.get::<_, Option<String>>(0)
    }) {
        titles = rows
            .flatten()
            .flatten()
            .map(|title| feed_line(&title, 80))
            .collect();
    }
    Some((count, titles))
}

fn more(total: usize) -> Option<String> {
    (total > BRIEF_SECTION_ITEMS).then(|| format!("  … {} more", total - BRIEF_SECTION_ITEMS))
}

/// The morning brief for one member: a short, deterministic message from
/// their own rows plus whatever `feeds` carries. A section with nothing to
/// say is left out.
pub fn morning_brief_text(
    store: &Store,
    user_id: &str,
    now: DateTime<Utc>,
    tz: Tz,
    feeds: &BriefFeeds,
) -> Result<String> {
    let local_now = now.with_timezone(&tz);
    let today = local_now.date_naive();
    let parts = collect_parts(store, user_id, now, tz, 24)?;
    let mut lines = vec![format!(
        "☀️ SeraBot — morning brief, {}",
        local_now.format("%a %d %b")
    )];

    let mut deadline_lines: Vec<String> = parts
        .overdue
        .iter()
        .map(|item| format!("overdue: {}", item.line))
        .collect();
    deadline_lines.extend(parts.due_soon.iter().map(|item| item.line.clone()));
    if !deadline_lines.is_empty() {
        lines.push(format!("📌 Deadlines ({})", deadline_lines.len()));
        for line in deadline_lines.iter().take(BRIEF_SECTION_ITEMS) {
            lines.push(format!("- {}", truncate_chars(line, 160)));
        }
        lines.extend(more(deadline_lines.len()));
    }
    let new_total = parts.new_items.len() + parts.changed_items.len();
    if new_total > 0 {
        lines.push(format!("🔎 Opportunities: {new_total} new or changed"));
        for item in parts
            .new_items
            .iter()
            .chain(parts.changed_items.iter())
            .take(BRIEF_SECTION_ITEMS)
        {
            lines.push(format!(
                "- {}",
                truncate_chars(&opportunity_line(item), 160)
            ));
        }
        lines.extend(more(new_total));
    }

    if let Some(events) = feeds.calendar.as_ref().filter(|events| !events.is_empty()) {
        lines.push(format!("📅 Today ({})", events.len()));
        for event in events.iter().take(5) {
            lines.push(format!("- {event}"));
        }
        if events.len() > 5 {
            lines.push(format!("  … {} more", events.len() - 5));
        }
    }

    let tasks = crate::personal_tasks::list(store, user_id, None, None, None, 500)
        .unwrap_or_default()
        .into_iter()
        .filter(|task| task.status != "done")
        .filter(|task| {
            task.due
                .as_deref()
                .and_then(|due| due.get(..10))
                .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
                .is_some_and(|date| date <= today)
        })
        .collect::<Vec<_>>();
    if !tasks.is_empty() {
        lines.push(format!("✅ Tasks due ({})", tasks.len()));
        for task in tasks.iter().take(BRIEF_SECTION_ITEMS) {
            lines.push(format!("- {}", feed_line(&task.title, 120)));
        }
        lines.extend(more(tasks.len()));
    }

    if let Some((count, titles)) = job_matches_since(store, user_id, now - Duration::hours(24)) {
        lines.push(format!("💼 New job matches: {count}"));
        for title in titles {
            lines.push(format!("- {title}"));
        }
    }

    if let Some(mail) = feeds.mail.as_ref().filter(|mail| mail.new_count > 0) {
        lines.push(format!("✉️ New mail (24 h): {}", mail.new_count));
        for subject in &mail.subjects {
            lines.push(format!("- {subject}"));
        }
    }

    let month = store.cost_report(Some(user_id), crate::costs::month_start(now))?;
    match store.monthly_budget(user_id)? {
        Some(budget) => {
            let percent = if budget > 0.0 {
                ((month.usd / budget) * 100.0).round() as i64
            } else {
                0
            };
            lines.push(format!(
                "💶 This month: {} of {} ({percent}%)",
                crate::costs::usd(month.usd),
                crate::costs::usd(budget)
            ));
        }
        None if month.usd > 0.0 => {
            lines.push(format!("💶 This month: {}", crate::costs::usd(month.usd)));
        }
        None => {}
    }

    let day_ago = now - Duration::hours(24);
    let mut kinds: Vec<String> = store
        .recent_alerts(user_id, 100)?
        .into_iter()
        .filter(|alert| {
            parse_utc(&alert.at).is_some_and(|at| at >= day_ago)
                && !matches!(
                    alert.kind.as_str(),
                    "brief" | "digest" | "triage" | "memory_review"
                )
        })
        .map(|alert| alert.kind)
        .collect();
    if !kinds.is_empty() {
        let total = kinds.len();
        kinds.sort();
        kinds.dedup();
        lines.push(format!(
            "⚠️ Alerts in the last 24 h: {total} ({})",
            kinds.join(", ")
        ));
    }

    if lines.len() == 1 {
        lines.push("Nothing needs you this morning.".into());
    }
    let text = lines.join("\n");
    if text.chars().count() > MEMBER_BRIEF_MAX_CHARS {
        let mut cut = truncate_chars(&text, MEMBER_BRIEF_MAX_CHARS - 12);
        if let Some(newline) = cut.rfind('\n') {
            cut.truncate(newline);
        }
        cut.push_str("\n[trimmed]");
        return Ok(cut);
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::new_id;
    use chrono::TimeZone;

    fn setup() -> (Store, String, String) {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let bob = store
            .create_user("microsoft:bob", "bob@example.example", "Bob")
            .unwrap()
            .id;
        (store, admin, bob)
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 20, 6, 0, 0).unwrap()
    }

    fn utc() -> Tz {
        "UTC".parse().unwrap()
    }

    fn opportunity_with_deadline(store: &Store, user: &str, title: &str, due: &str) {
        let id = new_id();
        let connection = store.connection().unwrap();
        connection
            .execute(
                "INSERT INTO opportunities(id, source, external_id, title, summary, url, status,
                    evidence, revision, first_seen, last_seen, user_id)
                 VALUES (?1, 'src', ?2, ?3, '', NULL, 'open', '', 1, ?4, ?4, ?5)",
                params![id, format!("ext-{title}"), title, utc_string(now()), user],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO deadlines(id, opportunity_id, due_date, due_time, timezone,
                    precision, source_text, status, observed_at, user_id)
                 VALUES (?1, ?2, ?3, NULL, NULL, 'date', '', 'upcoming', ?5, ?4)",
                params![new_id(), id, due, user, utc_string(now())],
            )
            .unwrap();
    }

    #[test]
    fn an_empty_brief_says_so_and_stays_short() {
        let (store, admin, _) = setup();
        let text =
            morning_brief_text(&store, &admin, now(), utc(), &BriefFeeds::default()).unwrap();
        assert!(text.contains("morning brief"));
        assert!(text.contains("Nothing needs you"));
        assert!(!text.contains("Deadlines"));
        assert!(!text.contains("Today"));
        assert!(!text.contains("mail"));
    }

    #[test]
    fn each_section_appears_only_with_data_and_stays_member_scoped() {
        let (store, admin, bob) = setup();
        opportunity_with_deadline(&store, &admin, "Grant call", "2026-11-23");
        opportunity_with_deadline(&store, &bob, "Bob secret grant", "2026-11-23");
        crate::personal_tasks::create(
            &store,
            &admin,
            &crate::personal_tasks::NewTask {
                title: "File the form".into(),
                notes: None,
                status: None,
                due: Some("2026-11-20".into()),
            },
        )
        .unwrap();
        store.set_monthly_budget(&admin, Some(10.0)).unwrap();
        let feeds = BriefFeeds {
            calendar: Some(vec!["09:30  Standup".into()]),
            mail: Some(MailPeek {
                new_count: 2,
                subjects: vec!["Invoice".into(), "Lunch".into()],
            }),
        };
        let text = morning_brief_text(&store, &admin, now(), utc(), &feeds).unwrap();
        assert!(text.contains("Deadlines (1)"));
        assert!(text.contains("Grant call"));
        assert!(!text.contains("Bob secret grant"));
        assert!(text.contains("Today (1)"));
        assert!(text.contains("09:30  Standup"));
        assert!(text.contains("Tasks due (1)"));
        assert!(text.contains("File the form"));
        assert!(text.contains("New mail (24 h): 2"));
        assert!(text.contains("This month: $0.00 of $10.00") || text.contains("of $10"));
        assert!(!text.contains("Nothing needs you"));
        // Without feeds, calendar and mail vanish silently.
        let bare =
            morning_brief_text(&store, &admin, now(), utc(), &BriefFeeds::default()).unwrap();
        assert!(!bare.contains("Standup"));
        assert!(!bare.contains("New mail"));
    }

    #[test]
    fn job_matches_come_from_the_job_watcher() {
        let (store, admin, _) = setup();
        let text =
            morning_brief_text(&store, &admin, now(), utc(), &BriefFeeds::default()).unwrap();
        assert!(!text.contains("job matches"));
        let connection = store.connection().unwrap();
        connection
            .execute(
                "INSERT INTO job_seen(user_id, url_hash, title_hash, source_key, title, employer,
                                      city, deadline, url, matched, first_seen)
                 VALUES (?1, 'u1', 't1', 'rekrute', 'Rust engineer', 'Acme', 'Rabat', NULL,
                         'https://example.example/j1', 1, ?2),
                        (?1, 'u2', 't2', 'rekrute', 'Not a match', 'Acme', 'Rabat', NULL,
                         'https://example.example/j2', 0, ?2)",
                params![admin, (now() - Duration::hours(2)).to_rfc3339()],
            )
            .unwrap();
        drop(connection);
        let text =
            morning_brief_text(&store, &admin, now(), utc(), &BriefFeeds::default()).unwrap();
        assert!(text.contains("New job matches: 1"), "{text}");
        assert!(text.contains("Rust engineer"));
        assert!(!text.contains("Not a match"));
    }

    #[test]
    fn alerts_of_the_last_day_are_counted_without_their_text() {
        let (store, admin, _) = setup();
        store
            .record_audit(
                &admin,
                "alert.sent",
                None,
                None,
                None,
                None,
                None,
                Some(&serde_json::json!({"kind": "failover", "text": "private words"})),
            )
            .unwrap();
        let at = Utc::now();
        let text = morning_brief_text(&store, &admin, at, utc(), &BriefFeeds::default()).unwrap();
        assert!(text.contains("Alerts in the last 24 h: 1 (failover)"));
        assert!(!text.contains("private words"));
    }

    #[test]
    fn the_text_is_bounded() {
        let (store, admin, _) = setup();
        let feeds = BriefFeeds {
            calendar: Some(
                (0..200)
                    .map(|i| format!("10:00  {}", "x".repeat(70 + i % 5)))
                    .collect(),
            ),
            mail: None,
        };
        let text = morning_brief_text(&store, &admin, now(), utc(), &feeds).unwrap();
        assert!(text.chars().count() <= MEMBER_BRIEF_MAX_CHARS);
    }

    #[test]
    fn calendar_and_mail_replies_are_parsed_defensively() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        let reply = serde_json::json!({"value": [
            {"subject": "Later", "start": {"dateTime": "2026-11-20T15:00:00.0000000"}},
            {"subject": "Early\nmeeting", "start": {"dateTime": "2026-11-20T08:15:00.0000000"}},
            {"subject": "Tomorrow", "start": {"dateTime": "2026-11-21T08:15:00.0000000"}},
            {"subject": "broken", "start": {}},
        ]});
        assert_eq!(
            calendar_lines(&reply, today, utc()),
            vec![
                "08:15  Early meeting".to_string(),
                "15:00  Later".to_string()
            ]
        );
        let google = serde_json::json!({"items": [
            {"summary": "Holiday", "start": {"date": "2026-11-20"}},
            {"summary": "Call", "start": {"dateTime": "2026-11-20T09:00:00+01:00"}},
        ]});
        let lines = calendar_lines(&google, today, utc());
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().any(|line| line.starts_with("08:00")));
        let mail = serde_json::json!({"value": [
            {"subject": "Fresh", "receivedDateTime": "2026-11-20T05:00:00Z"},
            {"subject": "Old", "receivedDateTime": "2026-11-18T05:00:00Z"},
        ]});
        let peek = mail_peek(&mail, now());
        assert_eq!(peek.new_count, 1);
        assert_eq!(peek.subjects, vec!["Fresh".to_string()]);
    }
}
