//! The Tasks tab (0.25.0, redesigned 0.45.0): the acting user's personal tasks
//! in three views — Todo, Calendar (with day inspector), and Kanban — plus
//! opt-in sync with Google Tasks / Microsoft To Do / Google Calendar / Outlook
//! Calendar and full task editing (`PATCH /v1/tasks/{id}`).

use crate::ui::Icon;
use dioxus::prelude::*;
use serabot_desktop::session::{Section, Session};
use serabot_desktop::{TaskEntry, TaskEvent, TaskUpdate};

/// The three views of the tab.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskView {
    Todo,
    Calendar,
    Kanban,
}

impl TaskView {
    fn label(self) -> &'static str {
        match self {
            Self::Todo => "Todo",
            Self::Calendar => "Calendar",
            Self::Kanban => "Kanban",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Todo => "list",
            Self::Calendar => "workflows",
            Self::Kanban => "grid",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskStatusFilter {
    All,
    Todo,
    Doing,
    Done,
}

impl TaskStatusFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Todo => "To do",
            Self::Doing => "Doing",
            Self::Done => "Done",
        }
    }

    fn matches(self, status: &str) -> bool {
        match self {
            Self::All => true,
            Self::Todo => status == "todo",
            Self::Doing => status == "doing",
            Self::Done => status == "done",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskDueFilter {
    All,
    Overdue,
    Today,
    Upcoming,
    Anytime,
}

impl TaskDueFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "Any due",
            Self::Overdue => "Overdue",
            Self::Today => "Today",
            Self::Upcoming => "Upcoming",
            Self::Anytime => "No due date",
        }
    }

    fn matches(self, task: &TaskEntry, today_iso: &str) -> bool {
        match self {
            Self::All => true,
            Self::Overdue => is_overdue(task.due.as_deref(), &task.status, today_iso),
            Self::Today => task
                .due
                .as_deref()
                .map(format_due_date)
                .is_some_and(|d| is_valid_iso_date(&d) && d == today_iso),
            Self::Upcoming => task
                .due
                .as_deref()
                .map(format_due_date)
                .is_some_and(|d| is_valid_iso_date(&d) && d.as_str() > today_iso),
            Self::Anytime => task
                .due
                .as_deref()
                .map(format_due_date)
                .is_none_or(|d| d.is_empty()),
        }
    }
}

const VIEWS: [TaskView; 3] = [TaskView::Todo, TaskView::Calendar, TaskView::Kanban];
const STATUS_FILTERS: [TaskStatusFilter; 4] = [
    TaskStatusFilter::All,
    TaskStatusFilter::Todo,
    TaskStatusFilter::Doing,
    TaskStatusFilter::Done,
];
const DUE_FILTERS: [TaskDueFilter; 5] = [
    TaskDueFilter::All,
    TaskDueFilter::Overdue,
    TaskDueFilter::Today,
    TaskDueFilter::Upcoming,
    TaskDueFilter::Anytime,
];

const SYNC_PROVIDERS: [(&str, &str); 5] = [
    ("none", "No sync"),
    ("google_tasks", "Google Tasks"),
    ("microsoft_todo", "Microsoft To Do"),
    ("google_calendar", "Google Calendar"),
    ("outlook_calendar", "Outlook Calendar"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct TaskDraft {
    id: String,
    title: String,
    notes: String,
    status: String,
    due: String,
    error: Option<String>,
}

impl TaskDraft {
    fn from_entry(task: &TaskEntry) -> Self {
        Self {
            id: task.id.clone(),
            title: task.title.clone(),
            notes: task.notes.clone(),
            status: task.status.clone(),
            due: task.due.as_deref().map(format_due_date).unwrap_or_default(),
            error: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TaskStats {
    total: usize,
    open: usize,
    todo: usize,
    doing: usize,
    done: usize,
    due_today: usize,
    overdue: usize,
    upcoming_events: usize,
}

impl TaskStats {
    fn completion_pct(self) -> usize {
        (self.done * 100).checked_div(self.total).unwrap_or(0)
    }
}

fn compute_stats(tasks: &[TaskEntry], events: &[TaskEvent], today_iso: &str) -> TaskStats {
    let mut stats = TaskStats {
        total: tasks.len(),
        ..Default::default()
    };
    for task in tasks {
        match task.status.as_str() {
            "todo" => {
                stats.todo += 1;
                stats.open += 1;
            }
            "doing" => {
                stats.doing += 1;
                stats.open += 1;
            }
            "done" => stats.done += 1,
            _ => {
                stats.open += 1;
            }
        }
        if task.status != "done"
            && let Some(due) = task.due.as_deref()
        {
            let formatted = format_due_date(due);
            if is_valid_iso_date(&formatted) {
                if formatted.as_str() < today_iso {
                    stats.overdue += 1;
                } else if formatted.as_str() == today_iso {
                    stats.due_today += 1;
                }
            }
        }
    }
    stats.upcoming_events = events
        .iter()
        .filter(|event| {
            let day = format_due_date(&event.start);
            is_valid_iso_date(&day) && day.as_str() >= today_iso
        })
        .count();
    stats
}

// --- tiny civil-date helpers (no extra dependency) ---------------------------

fn days_from_epoch(secs: u64) -> i64 {
    (secs / 86_400) as i64
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn weekday_mon0(z: i64) -> u32 {
    ((z + 3).rem_euclid(7)) as u32
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn iso_date(z: i64) -> String {
    let (y, m, d) = civil_from_days(z);
    format!("{y:04}-{m:02}-{d:02}")
}

fn today() -> (i64, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    civil_from_days(days_from_epoch(secs))
}

fn date_offset_iso(today_z: i64, offset_days: i64) -> String {
    iso_date(today_z + offset_days)
}

fn calendar_month_range(today_y: i64, today_m: u32, offset: i64) -> (i64, u32, String, String) {
    let total = today_y * 12 + (today_m as i64 - 1) + offset;
    let year = total.div_euclid(12);
    let month = (total.rem_euclid(12) + 1) as u32;
    let first = days_from_civil(year, month, 1);
    let next_total = year * 12 + (month as i64 - 1) + 1;
    let ny = next_total.div_euclid(12);
    let nm = (next_total.rem_euclid(12) + 1) as u32;
    let last = days_from_civil(ny, nm, 1);
    (year, month, iso_date(first), iso_date(last))
}

fn format_due_date(due: &str) -> String {
    let trimmed = due.trim();
    let date_prefix: String = trimmed.chars().take(10).collect();
    if is_valid_iso_date(&date_prefix) {
        date_prefix
    } else {
        trimmed.chars().take(20).collect()
    }
}

fn month_short_name(month: u32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "",
    }
}

fn format_day_heading(day_iso: &str, today_iso: &str) -> String {
    if !is_valid_iso_date(day_iso) {
        return day_iso.to_owned();
    }
    let Ok(year) = day_iso[..4].parse::<i64>() else {
        return day_iso.to_owned();
    };
    let Ok(month) = day_iso[5..7].parse::<u32>() else {
        return day_iso.to_owned();
    };
    let Ok(day) = day_iso[8..10].parse::<u32>() else {
        return day_iso.to_owned();
    };
    let weekday = match weekday_mon0(days_from_civil(year, month, day)) {
        0 => "Mon",
        1 => "Tue",
        2 => "Wed",
        3 => "Thu",
        4 => "Fri",
        5 => "Sat",
        _ => "Sun",
    };
    let label = format!("{weekday}, {} {day}", month_short_name(month));
    if day_iso == today_iso {
        format!("Today \u{00b7} {label}")
    } else {
        format!("{label}, {year}")
    }
}

fn extract_hhmm(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    if trimmed.len() >= 16
        && trimmed.as_bytes().get(10).copied() == Some(b'T')
        && trimmed.as_bytes().get(13).copied() == Some(b':')
    {
        let hhmm = &trimmed[11..16];
        if hhmm.bytes().enumerate().all(|(i, b)| {
            if i == 2 {
                b == b':'
            } else {
                b.is_ascii_digit()
            }
        }) {
            return Some(hhmm);
        }
    }
    None
}

fn format_event_time(event: &TaskEvent) -> String {
    if event.all_day {
        return "All day".to_owned();
    }
    match (extract_hhmm(&event.start), extract_hhmm(&event.end)) {
        (Some(start), Some(end)) => format!("{start} \u{2013} {end}"),
        (Some(start), None) => start.to_owned(),
        _ => format!("{} \u{2013} {}", event.start.trim(), event.end.trim()),
    }
}

fn event_provider_label(provider: &str) -> &'static str {
    match provider.trim() {
        "google_calendar" | "google_tasks" | "google" => "Google Calendar",
        "outlook_calendar" | "microsoft_todo" | "microsoft" => "Outlook Calendar",
        "" => "Calendar",
        _ => "Synced",
    }
}

fn is_valid_iso_date(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let (Some(y), Some(m), Some(d)) = (raw.get(..4), raw.get(5..7), raw.get(8..10)) else {
        return false;
    };
    let (Ok(year), Ok(month), Ok(day)) = (y.parse::<i64>(), m.parse::<u32>(), d.parse::<u32>())
    else {
        return false;
    };
    (1..=12).contains(&month) && (1..=days_in_month(year, month)).contains(&day)
}

fn is_valid_due_input(raw: &str) -> bool {
    let trimmed = raw.trim();
    trimmed.is_empty() || is_valid_iso_date(trimmed)
}

fn is_overdue(due: Option<&str>, status: &str, today_iso: &str) -> bool {
    if status == "done" {
        return false;
    }
    let Some(due) = due else {
        return false;
    };
    let formatted = format_due_date(due);
    is_valid_iso_date(&formatted) && formatted.as_str() < today_iso
}

fn due_badge_class(due: &str, status: &str, today_iso: &str) -> &'static str {
    if status == "done" {
        "tasks-due-badge"
    } else if is_overdue(Some(due), status, today_iso) {
        "tasks-due-badge overdue"
    } else if format_due_date(due) == today_iso {
        "tasks-due-badge today"
    } else {
        "tasks-due-badge upcoming"
    }
}

fn status_label(status: &str) -> &'static str {
    match status {
        "todo" => "To do",
        "doing" => "Doing",
        "done" => "Done",
        _ => "Task",
    }
}

fn source_label(remote_provider: Option<&str>) -> &'static str {
    match remote_provider {
        Some("google_tasks") | Some("google_calendar") | Some("google") => "Google Tasks",
        Some("microsoft_todo") | Some("outlook_calendar") | Some("microsoft") => "Microsoft To Do",
        Some(_) => "Synced",
        None => "Local",
    }
}

fn task_matches_filter(
    task: &TaskEntry,
    query: &str,
    status_filter: TaskStatusFilter,
    due_filter: TaskDueFilter,
    today_iso: &str,
) -> bool {
    if !status_filter.matches(&task.status) || !due_filter.matches(task, today_iso) {
        return false;
    }
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return true;
    }
    task.title.to_ascii_lowercase().contains(&needle)
        || task.notes.to_ascii_lowercase().contains(&needle)
}

#[derive(Clone, Debug, Default)]
struct TodoGroups {
    overdue_or_today: Vec<TaskEntry>,
    upcoming: Vec<TaskEntry>,
    anytime: Vec<TaskEntry>,
    completed: Vec<TaskEntry>,
}

fn group_todo_tasks(tasks: &[TaskEntry], today_iso: &str) -> TodoGroups {
    let mut groups = TodoGroups::default();
    for task in tasks {
        if task.status == "done" {
            groups.completed.push(task.clone());
            continue;
        }
        match task.due.as_deref().map(format_due_date) {
            Some(due) if is_valid_iso_date(&due) && due.as_str() <= today_iso => {
                groups.overdue_or_today.push(task.clone());
            }
            Some(due) if !due.is_empty() => {
                groups.upcoming.push(task.clone());
            }
            _ => {
                groups.anytime.push(task.clone());
            }
        }
    }
    groups
}

fn column_heading(status: &str) -> &'static str {
    match status {
        "todo" => "To do",
        "doing" => "Doing",
        "done" => "Done",
        _ => "Tasks",
    }
}

fn left_status(status: &str) -> Option<&'static str> {
    match status {
        "doing" => Some("todo"),
        "done" => Some("doing"),
        _ => None,
    }
}

fn right_status(status: &str) -> Option<&'static str> {
    match status {
        "todo" => Some("doing"),
        "doing" => Some("done"),
        _ => None,
    }
}

#[component]
pub fn TasksView(mut session: Signal<Session>) -> Element {
    let (today_y, today_m, today_d) = today();
    let today_z = days_from_civil(today_y, today_m, today_d);
    let today_iso = format!("{today_y:04}-{today_m:02}-{today_d:02}");

    let mut view = use_signal(|| TaskView::Todo);
    let mut composer_open = use_signal(|| false);
    let mut new_title = use_signal(String::new);
    let mut new_due = use_signal(String::new);
    let mut new_notes = use_signal(String::new);
    let mut show_notes = use_signal(|| false);
    let mut due_error = use_signal(|| Option::<String>::None);
    let mut query = use_signal(String::new);
    let mut status_filter = use_signal(|| TaskStatusFilter::All);
    let mut due_filter = use_signal(|| TaskDueFilter::All);
    let mut month_offset = use_signal(|| 0i64);
    let mut selected_day = use_signal({
        let today_iso = today_iso.clone();
        move || today_iso
    });
    let editing_task = use_signal(|| Option::<TaskDraft>::None);

    // Load once when the tab opens.
    use_effect(move || {
        let mut s = session.write();
        s.load_tasks();
        s.load_task_prefs();
    });

    let (tasks, tasks_loaded, prefs, sync, events, cal_warnings) = {
        let s = session.read();
        (
            s.state.tasks.clone(),
            s.state.tasks_loaded,
            s.state.task_prefs.clone(),
            s.state.task_sync.clone(),
            s.state.task_events.clone(),
            s.state.task_calendar_warnings.clone(),
        )
    };
    let current_provider = sync
        .as_ref()
        .map(|status| status.prefs.sync_provider.clone())
        .or_else(|| prefs.map(|p| p.sync_provider))
        .unwrap_or_else(|| "none".into());
    let signed_in_google = sync
        .as_ref()
        .and_then(|s| s.signed_in.get("tasks_google"))
        .copied()
        .unwrap_or(false);
    let signed_in_microsoft = sync
        .as_ref()
        .and_then(|s| s.signed_in.get("tasks_microsoft"))
        .copied()
        .unwrap_or(false);
    let needs_sign_in = matches!(
        current_provider.as_str(),
        "google_tasks" | "google_calendar"
    ) && !signed_in_google
        || matches!(
            current_provider.as_str(),
            "microsoft_todo" | "outlook_calendar"
        ) && !signed_in_microsoft;

    let (year, month, _, _) = calendar_month_range(today_y, today_m, *month_offset.read());
    {
        let mut session = session;
        use_effect(move || {
            let _active_view = *view.read();
            let offset = *month_offset.read();
            let (_, _, from, to) = calendar_month_range(today_y, today_m, offset);
            session.write().load_task_calendar(from, to);
        });
    }

    let sign_in_family = if current_provider.starts_with("google") {
        "Google"
    } else {
        "Microsoft"
    };
    let provider_label = current_provider.replace('_', " ");
    let (health_class, health_text, health_icon) = if current_provider == "none" {
        ("tasks-health-badge local", "Local only", "lock")
    } else if needs_sign_in {
        ("tasks-health-badge warn", "Needs sign-in", "alert-circle")
    } else {
        ("tasks-health-badge ok", "Connected & synced", "check")
    };

    let month_name = [
        "",
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ][month as usize];
    let first_day = days_from_civil(year, month, 1);
    let dim = days_in_month(year, month);
    let leading = weekday_mon0(first_day);

    let tasks_by_day = |day_iso: &str| -> Vec<TaskEntry> {
        tasks
            .iter()
            .filter(|t| {
                t.due
                    .as_deref()
                    .map(|d| d.starts_with(day_iso))
                    .unwrap_or(false)
            })
            .cloned()
            .collect()
    };
    let events_by_day = |day_iso: &str| -> Vec<TaskEvent> {
        events
            .iter()
            .filter(|e| e.start.starts_with(day_iso))
            .cloned()
            .collect()
    };

    let stats = compute_stats(&tasks, &events, &today_iso);
    let pct = stats.completion_pct();

    let needle = query.read().clone();
    let s_filter = *status_filter.read();
    let d_filter = *due_filter.read();
    let filtered_tasks: Vec<TaskEntry> = tasks
        .iter()
        .filter(|t| task_matches_filter(t, &needle, s_filter, d_filter, &today_iso))
        .cloned()
        .collect();
    let kanban_tasks: Vec<TaskEntry> = tasks
        .iter()
        .filter(|t| task_matches_filter(t, &needle, TaskStatusFilter::All, d_filter, &today_iso))
        .cloned()
        .collect();
    let groups = group_todo_tasks(&filtered_tasks, &today_iso);
    let title_ready = !new_title.read().trim().is_empty();
    let active_view = *view.read();
    let has_active_filters = !needle.trim().is_empty()
        || s_filter != TaskStatusFilter::All
        || d_filter != TaskDueFilter::All;

    let today_chip = date_offset_iso(today_z, 0);
    let tomorrow_chip = date_offset_iso(today_z, 1);
    let next_week_chip = date_offset_iso(today_z, 7);

    fn submit_task(
        mut session: Signal<Session>,
        mut new_title: Signal<String>,
        mut new_due: Signal<String>,
        mut new_notes: Signal<String>,
        mut due_error: Signal<Option<String>>,
    ) {
        let title = new_title.read().trim().to_owned();
        let due = new_due.read().trim().to_owned();
        if title.is_empty() {
            return;
        }
        if !is_valid_due_input(&due) {
            due_error.set(Some("Use YYYY-MM-DD for the due date.".into()));
            return;
        }
        due_error.set(None);
        session
            .write()
            .add_task(title, (!due.is_empty()).then_some(due));
        new_title.set(String::new());
        new_due.set(String::new());
        new_notes.set(String::new());
    }

    rsx! {
        div { class: "pane tasks-page",
            div { class: "pane-head tasks-hero-head",
                div { class: "tasks-hero-copy",
                    h1 { class: "pane-title", "Tasks" }
                    p { class: "muted", "Plan your day, organize boards, and review your calendar in one place." }
                }
                div { class: "pane-actions tasks-head-actions",
                    div { class: "seg tasks-view-seg", role: "group", "aria-label": "Task view",
                        for v in VIEWS {
                            button {
                                key: "{v.label()}",
                                class: if active_view == v { "seg-btn active" } else { "seg-btn" },
                                r#type: "button",
                                "aria-pressed": if active_view == v { "true" } else { "false" },
                                onclick: move |_| view.set(v),
                                Icon { name: v.icon(), size: 13 }
                                span { "{v.label()}" }
                            }
                        }
                    }
                    button {
                        class: "btn primary sm",
                        r#type: "button",
                        onclick: move |_| {
                            if *view.read() == TaskView::Calendar {
                                view.set(TaskView::Todo);
                            }
                            composer_open.set(true);
                        },
                        Icon { name: "plus", size: 14 }
                        "New task"
                    }
                }
            }
            div { class: "pane-body",
                if !tasks_loaded && tasks.is_empty() {
                    div { class: "tasks-skeleton", role: "status", "aria-label": "Loading tasks",
                        div { class: "tasks-stat-tiles",
                            for n in 0..5 {
                                div { key: "sk-tile-{n}", class: "skeleton tasks-skeleton-tile" }
                            }
                        }
                        for n in 0..4 {
                            div { key: "{n}", class: "skeleton tasks-skeleton-row" }
                        }
                    }
                } else {
                    // Compact Stat Tiles + Completion Progress
                    section { class: "tasks-hero", role: "region", "aria-label": "Task metrics",
                        div { class: "tasks-stat-tiles",
                            div { class: "stat-tile tasks-tile total",
                                span { class: "stat-tile-value", "{stats.total}" }
                                span { class: "stat-tile-label", "Total tasks" }
                                span { class: "stat-tile-sub", "{stats.open} open" }
                            }
                            div { class: "stat-tile tasks-tile doing",
                                span { class: "stat-tile-value", "{stats.doing}" }
                                span { class: "stat-tile-label", "In progress" }
                                span { class: "stat-tile-sub", "{stats.todo} queued" }
                            }
                            div {
                                class: if stats.overdue > 0 { "stat-tile tasks-tile overdue" } else { "stat-tile tasks-tile today" },
                                span { class: "stat-tile-value",
                                    if stats.overdue > 0 { "{stats.overdue}" } else { "{stats.due_today}" }
                                }
                                span { class: "stat-tile-label",
                                    if stats.overdue > 0 { "Overdue" } else { "Due today" }
                                }
                                span { class: "stat-tile-sub",
                                    if stats.overdue > 0 { "{stats.due_today} due today" } else { "On track" }
                                }
                            }
                            div { class: "stat-tile tasks-tile done",
                                span { class: "stat-tile-value", "{stats.done}" }
                                span { class: "stat-tile-label", "Done" }
                                span { class: "stat-tile-sub", "{pct}% complete" }
                            }
                            div { class: "stat-tile tasks-tile events",
                                span { class: "stat-tile-value", "{stats.upcoming_events}" }
                                span { class: "stat-tile-label", "Upcoming events" }
                                span { class: "stat-tile-sub",
                                    if events.is_empty() { "No synced events" } else { "{events.len()} this month" }
                                }
                            }
                        }
                        if stats.total > 0 {
                            div { class: "tasks-progress-wrap",
                                div { class: "tasks-progress-label",
                                    span { "Completion \u{2014} {stats.done} of {stats.total} tasks finished" }
                                    span { class: "mono", "{pct}%" }
                                }
                                div { class: "tasks-progress-bar", role: "progressbar", "aria-valuenow": "{pct}", "aria-valuemin": "0", "aria-valuemax": "100",
                                    div { class: "tasks-progress-fill", style: "width: {pct}%;" }
                                }
                            }
                        }
                    }

                    // Unified Sync & Search Control Strip
                    div { class: "card tasks-sync-bar",
                        div { class: "tasks-sync-controls",
                            select {
                                class: "select tasks-provider-select",
                                "aria-label": "Sync provider",
                                onchange: move |evt| {
                                    let provider = evt.value();
                                    session.write().set_task_sync_provider(provider);
                                },
                                for (value, label) in SYNC_PROVIDERS {
                                    option {
                                        value: "{value}",
                                        selected: current_provider == value,
                                        "{label}"
                                    }
                                }
                            }
                            span { class: "{health_class}",
                                Icon { name: health_icon, size: 12 }
                                "{health_text}"
                            }
                            button {
                                class: "btn sm",
                                r#type: "button",
                                disabled: current_provider == "none" || needs_sign_in,
                                onclick: move |_| {
                                    let (_, _, from, to) =
                                        calendar_month_range(today_y, today_m, *month_offset.read());
                                    let mut s = session.write();
                                    s.run_task_sync();
                                    s.load_task_calendar(from, to);
                                },
                                Icon { name: "refresh", size: 13 }
                                "Sync now"
                            }
                        }
                        if !tasks.is_empty() || !needle.is_empty() {
                            div { class: "tasks-search-wrap",
                                input {
                                    class: "input tasks-search",
                                    r#type: "search",
                                    placeholder: "Search tasks\u{2026}",
                                    "aria-label": "Search tasks",
                                    value: "{query}",
                                    oninput: move |evt| query.set(evt.value()),
                                    onkeydown: move |evt: KeyboardEvent| {
                                        if evt.key() == Key::Escape {
                                            query.set(String::new());
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if needs_sign_in {
                        div { class: "banner warn tasks-notice", role: "status",
                            span { "Sign in with {sign_in_family} in Integrations to sync {provider_label}." }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                onclick: move |_| session.write().open_section(Section::Integrations),
                                "Open Integrations"
                            }
                        }
                    }

                    if active_view != TaskView::Calendar && (composer_open() || tasks.is_empty()) {
                        div { class: "card tasks-composer",
                            div { class: "tasks-composer-row",
                                input {
                                    class: "input",
                                    placeholder: "What needs to get done?\u{2026}",
                                    "aria-label": "New task title",
                                    value: "{new_title}",
                                    oninput: move |evt| new_title.set(evt.value()),
                                    onkeydown: move |evt| {
                                        if evt.key() == Key::Enter {
                                            submit_task(session, new_title, new_due, new_notes, due_error);
                                        }
                                    }
                                }
                                input {
                                    class: "input tasks-due",
                                    r#type: "date",
                                    title: "Optional due date (YYYY-MM-DD)",
                                    "aria-label": "Due date",
                                    value: "{new_due}",
                                    oninput: move |evt| {
                                        due_error.set(None);
                                        new_due.set(evt.value());
                                    },
                                    onkeydown: move |evt| {
                                        if evt.key() == Key::Enter {
                                            submit_task(session, new_title, new_due, new_notes, due_error);
                                        }
                                    }
                                }
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    onclick: move |_| show_notes.set(!show_notes()),
                                    if show_notes() { "Hide notes" } else { "+ Notes" }
                                }
                                button {
                                    class: "btn primary sm",
                                    r#type: "button",
                                    disabled: !title_ready,
                                    onclick: move |_| submit_task(session, new_title, new_due, new_notes, due_error),
                                    Icon { name: "plus", size: 14 }
                                    "Add task"
                                }
                            }
                            div { class: "tasks-date-chips",
                                span { class: "tasks-date-label", "Quick due:" }
                                button {
                                    class: if *new_due.read() == today_chip { "tasks-date-chip active" } else { "tasks-date-chip" },
                                    r#type: "button",
                                    onclick: {
                                        let d = today_chip.clone();
                                        move |_| { due_error.set(None); new_due.set(d.clone()); }
                                    },
                                    "Today"
                                }
                                button {
                                    class: if *new_due.read() == tomorrow_chip { "tasks-date-chip active" } else { "tasks-date-chip" },
                                    r#type: "button",
                                    onclick: {
                                        let d = tomorrow_chip.clone();
                                        move |_| { due_error.set(None); new_due.set(d.clone()); }
                                    },
                                    "Tomorrow"
                                }
                                button {
                                    class: if *new_due.read() == next_week_chip { "tasks-date-chip active" } else { "tasks-date-chip" },
                                    r#type: "button",
                                    onclick: {
                                        let d = next_week_chip.clone();
                                        move |_| { due_error.set(None); new_due.set(d.clone()); }
                                    },
                                    "Next week"
                                }
                                if !new_due.read().is_empty() {
                                    button {
                                        class: "tasks-date-chip",
                                        r#type: "button",
                                        onclick: move |_| new_due.set(String::new()),
                                        "Clear"
                                    }
                                }
                                if !tasks.is_empty() {
                                    button {
                                        class: "btn ghost sm tasks-composer-hide",
                                        r#type: "button",
                                        onclick: move |_| composer_open.set(false),
                                        "Close"
                                    }
                                }
                            }
                            if show_notes() {
                                div { class: "tasks-composer-notes-wrap",
                                    input {
                                        class: "input",
                                        placeholder: "Optional notes (edit after adding if needed)\u{2026}",
                                        value: "{new_notes}",
                                        oninput: move |evt| new_notes.set(evt.value()),
                                    }
                                }
                            }
                            if let Some(msg) = due_error.read().as_ref() {
                                p { class: "tasks-inline-err", role: "alert", "{msg}" }
                            }
                        }
                    }

                    if active_view != TaskView::Calendar && !tasks.is_empty() {
                        div { class: "tasks-toolbar",
                            if active_view == TaskView::Todo {
                                div { class: "seg tasks-filters", role: "group", "aria-label": "Status filter",
                                    for sf in STATUS_FILTERS {
                                        button {
                                            key: "{sf.label()}",
                                            class: if s_filter == sf { "seg-btn active" } else { "seg-btn" },
                                            "aria-pressed": if s_filter == sf { "true" } else { "false" },
                                            r#type: "button",
                                            onclick: move |_| status_filter.set(sf),
                                            "{sf.label()}"
                                        }
                                    }
                                }
                            }
                            div { class: "seg tasks-due-filters", role: "group", "aria-label": "Due window filter",
                                for df in DUE_FILTERS {
                                    button {
                                        key: "{df.label()}",
                                        class: if d_filter == df { "seg-btn active" } else { "seg-btn" },
                                        "aria-pressed": if d_filter == df { "true" } else { "false" },
                                        r#type: "button",
                                        onclick: move |_| due_filter.set(df),
                                        "{df.label()}"
                                    }
                                }
                            }
                            if has_active_filters {
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    onclick: move |_| {
                                        query.set(String::new());
                                        status_filter.set(TaskStatusFilter::All);
                                        due_filter.set(TaskDueFilter::All);
                                    },
                                    "Reset filters"
                                }
                            }
                        }
                    }

                    match active_view {
                        TaskView::Todo => rsx! {
                            if tasks.is_empty() {
                                div { class: "empty tasks-empty",
                                    span { class: "empty-icon", Icon { name: "check", size: 20 } }
                                    h2 { class: "empty-title", "No tasks yet" }
                                    p { class: "empty-text",
                                        "Add your first task above, or choose a sync provider to import the ones you already have."
                                    }
                                }
                            } else if filtered_tasks.is_empty() {
                                div { class: "empty tasks-empty",
                                    span { class: "empty-icon", Icon { name: "search", size: 20 } }
                                    h2 { class: "empty-title", "No tasks match" }
                                    p { class: "empty-text", "Try a different search, status, or due-date filter." }
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        onclick: move |_| {
                                            query.set(String::new());
                                            status_filter.set(TaskStatusFilter::All);
                                            due_filter.set(TaskDueFilter::All);
                                        },
                                        "Clear filters"
                                    }
                                }
                            } else {
                                if !groups.overdue_or_today.is_empty() {
                                    TaskGroupSection {
                                        title: "Overdue & Due Today".to_owned(),
                                        tasks: groups.overdue_or_today,
                                        today_iso: today_iso.clone(),
                                        session,
                                        editing_task,
                                    }
                                }
                                if !groups.upcoming.is_empty() {
                                    TaskGroupSection {
                                        title: "Upcoming".to_owned(),
                                        tasks: groups.upcoming,
                                        today_iso: today_iso.clone(),
                                        session,
                                        editing_task,
                                    }
                                }
                                if !groups.anytime.is_empty() {
                                    TaskGroupSection {
                                        title: "Anytime".to_owned(),
                                        tasks: groups.anytime,
                                        today_iso: today_iso.clone(),
                                        session,
                                        editing_task,
                                    }
                                }
                                if !groups.completed.is_empty() {
                                    details { class: "tasks-done", open: s_filter == TaskStatusFilter::Done,
                                        summary {
                                            Icon { name: "chevron-right", size: 14 }
                                            "Completed"
                                            span { class: "tasks-section-count", "{groups.completed.len()}" }
                                        }
                                        div { class: "card-grid tasks-list",
                                            for task in groups.completed {
                                                TaskCard {
                                                    key: "{task.id}",
                                                    session,
                                                    task,
                                                    today_iso: today_iso.clone(),
                                                    editing_task,
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        TaskView::Kanban => rsx! {
                            if tasks.is_empty() {
                                div { class: "empty tasks-empty",
                                    span { class: "empty-icon", Icon { name: "grid", size: 20 } }
                                    h2 { class: "empty-title", "Board is empty" }
                                    p { class: "empty-text",
                                        "Create a task to organize work across To do, Doing, and Done columns."
                                    }
                                    button {
                                        class: "btn primary sm",
                                        r#type: "button",
                                        onclick: move |_| composer_open.set(true),
                                        Icon { name: "plus", size: 14 }
                                        "New task"
                                    }
                                }
                            } else if kanban_tasks.is_empty() {
                                div { class: "empty tasks-empty",
                                    span { class: "empty-icon", Icon { name: "search", size: 20 } }
                                    h2 { class: "empty-title", "No board items match" }
                                    p { class: "empty-text", "Try clearing your search or due-date filter." }
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        onclick: move |_| {
                                            query.set(String::new());
                                            due_filter.set(TaskDueFilter::All);
                                        },
                                        "Clear filters"
                                    }
                                }
                            } else {
                                div { class: "tasks-kanban",
                                    for column in ["todo", "doing", "done"] {
                                        {
                                            let col_tasks: Vec<TaskEntry> = kanban_tasks
                                                .iter()
                                                .filter(|t| t.status == column)
                                                .cloned()
                                                .collect();
                                            let col_class = format!("tasks-kanban-col {column}");
                                            rsx! {
                                                div { key: "{column}", class: "{col_class}",
                                                    div { class: "tasks-kanban-head",
                                                        div { class: "tasks-kanban-title-wrap",
                                                            span { class: "tasks-kanban-dot {column}" }
                                                            h3 { "{column_heading(column)}" }
                                                        }
                                                        span { class: "kanban-count", "{col_tasks.len()}" }
                                                    }
                                                    if col_tasks.is_empty() {
                                                        p { class: "kanban-empty", "No {column_heading(column).to_ascii_lowercase()} tasks." }
                                                    } else {
                                                        div { class: "card-grid tasks-list",
                                                            for task in col_tasks {
                                                                KanbanTaskCard {
                                                                    key: "{task.id}",
                                                                    session,
                                                                    task,
                                                                    today_iso: today_iso.clone(),
                                                                    editing_task,
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        TaskView::Calendar => {
                            let cal_status = match (signed_in_google, signed_in_microsoft) {
                                (true, true) => "Google & Outlook Calendars connected",
                                (true, false) => "Google Calendar connected",
                                (false, true) => "Outlook Calendar connected",
                                (false, false) => "No external calendar connected \u{2014} showing task due dates",
                            };
                            let has_external_cal = signed_in_google || signed_in_microsoft;
                            let can_sync_cal = current_provider != "none" && !needs_sign_in;
                            rsx! {
                                div { class: "card tasks-cal-sync-strip",
                                    span {
                                        class: if has_external_cal { "tasks-health-badge ok" } else { "tasks-health-badge warn" },
                                        Icon { name: if has_external_cal { "check" } else { "alert-circle" }, size: 12 }
                                        "{cal_status}"
                                    }
                                    div { class: "tasks-cal-sync-actions",
                                        button {
                                            class: "btn ghost sm",
                                            r#type: "button",
                                            onclick: move |_| {
                                                let (_, _, from, to) =
                                                    calendar_month_range(today_y, today_m, *month_offset.read());
                                                let mut s = session.write();
                                                if can_sync_cal {
                                                    s.run_task_sync();
                                                }
                                                s.load_task_calendar(from, to);
                                            },
                                            Icon { name: "refresh", size: 13 }
                                            "Sync calendars"
                                        }
                                        if !has_external_cal {
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                onclick: move |_| session.write().open_section(Section::Integrations),
                                                "Open Integrations"
                                            }
                                        }
                                    }
                                }
                                for warn_item in cal_warnings.iter() {
                                    div { class: "banner warn tasks-cal-warn", role: "alert",
                                        Icon { name: "alert-circle", size: 15 }
                                        div { class: "grow",
                                            strong { "{event_provider_label(&warn_item.provider)}: " }
                                            span { "{warn_item.message}" }
                                            if !warn_item.hint.is_empty() {
                                                div { class: "tiny muted", "{warn_item.hint}" }
                                            }
                                        }
                                        button {
                                            class: "btn ghost sm",
                                            r#type: "button",
                                            onclick: move |_| session.write().open_section(Section::Integrations),
                                            "Open Integrations"
                                        }
                                    }
                                }
                                div { class: "tasks-cal-layout",
                                    div { class: "card tasks-cal-board",
                                        div { class: "cal-head",
                                            div { class: "cal-nav",
                                                button {
                                                    class: "btn sm",
                                                    r#type: "button",
                                                    "aria-label": "Previous month",
                                                    onclick: move |_| { let current = *month_offset.read(); month_offset.set(current - 1); },
                                                    Icon { name: "chevron-left", size: 14 }
                                                }
                                                strong { "{month_name} {year}" }
                                                button {
                                                    class: "btn sm",
                                                    r#type: "button",
                                                    "aria-label": "Next month",
                                                    onclick: move |_| { let current = *month_offset.read(); month_offset.set(current + 1); },
                                                    Icon { name: "chevron-right", size: 14 }
                                                }
                                                if *month_offset.read() != 0 {
                                                    button {
                                                        class: "btn ghost sm",
                                                        r#type: "button",
                                                        onclick: move |_| month_offset.set(0),
                                                        "Today"
                                                    }
                                                }
                                            }
                                            div { class: "tasks-cal-legend",
                                                span { class: "tasks-cal-legend-item",
                                                    span { class: "tasks-cal-dot task" }
                                                    "Tasks"
                                                }
                                                span { class: "tasks-cal-legend-item",
                                                    span { class: "tasks-cal-dot event" }
                                                    "Events"
                                                }
                                            }
                                        }
                                        div { class: "tasks-cal-grid",
                                            for name in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] {
                                                div { class: "cal-dow", "{name}" }
                                            }
                                            for _ in 0..leading {
                                                div { class: "tasks-cal-cell empty" }
                                            }
                                            for day in 1..=dim {
                                                {
                                                    let z = first_day + day as i64 - 1;
                                                    let day_iso = iso_date(z);
                                                    let is_today = day_iso == today_iso;
                                                    let is_selected = day_iso == *selected_day.read();
                                                    let day_tasks = tasks_by_day(&day_iso);
                                                    let day_events = events_by_day(&day_iso);
                                                    let cell_class = match (is_today, is_selected) {
                                                        (true, true) => "tasks-cal-cell today selected",
                                                        (true, false) => "tasks-cal-cell today",
                                                        (false, true) => "tasks-cal-cell selected",
                                                        (false, false) => "tasks-cal-cell",
                                                    };
                                                    let extra = (day_tasks.len() + day_events.len()).saturating_sub(3);
                                                    let click_iso = day_iso.clone();
                                                    let key_iso = day_iso.clone();
                                                    rsx! {
                                                        div {
                                                            key: "{day_iso}",
                                                            class: "{cell_class}",
                                                            role: "button",
                                                            tabindex: "0",
                                                            "aria-pressed": if is_selected { "true" } else { "false" },
                                                            "aria-label": "{day_iso}",
                                                            onclick: move |_| selected_day.set(click_iso.clone()),
                                                            onkeydown: move |evt: KeyboardEvent| {
                                                                if evt.key() == Key::Enter || evt.key() == Key::Character(" ".into()) {
                                                                    selected_day.set(key_iso.clone());
                                                                }
                                                            },
                                                            div { class: "cal-day",
                                                                span { "{day}" }
                                                                if !day_tasks.is_empty() || !day_events.is_empty() {
                                                                    span { class: "tasks-cal-dots",
                                                                        for _ in 0..day_tasks.len().min(3) {
                                                                            span { class: "tasks-cal-dot task" }
                                                                        }
                                                                        for _ in 0..day_events.len().min(3) {
                                                                            span { class: "tasks-cal-dot event" }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            div { class: "cal-items",
                                                                for task in day_tasks.into_iter().take(2) {
                                                                    div { class: "cal-item task", title: "{task.title}", "{task.title}" }
                                                                }
                                                                for event in day_events.into_iter().take(1) {
                                                                    div { class: "cal-item event", title: "{event.title}", "{event.title}" }
                                                                }
                                                                if extra > 0 {
                                                                    div { class: "cal-more", "+{extra} more" }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    // Selected Day Inspector Side Panel
                                    {
                                        let active_day = selected_day.read().clone();
                                        let active_day_label = format_day_heading(&active_day, &today_iso);
                                        let sel_tasks = tasks_by_day(&active_day);
                                        let sel_events = events_by_day(&active_day);
                                        let add_day = active_day.clone();
                                        rsx! {
                                            aside { class: "card tasks-cal-sidebar", "aria-label": "Selected day details",
                                                div { class: "tasks-cal-sidebar-head",
                                                    div { class: "tasks-cal-sidebar-copy",
                                                        strong { "{active_day_label}" }
                                                        span { class: "tasks-cal-sidebar-sub",
                                                            "{sel_tasks.len()} tasks \u{00b7} {sel_events.len()} events"
                                                        }
                                                    }
                                                    button {
                                                        class: "btn primary sm",
                                                        r#type: "button",
                                                        onclick: move |_| {
                                                            new_due.set(add_day.clone());
                                                            due_error.set(None);
                                                            composer_open.set(true);
                                                            view.set(TaskView::Todo);
                                                        },
                                                        Icon { name: "plus", size: 12 }
                                                        "Add task"
                                                    }
                                                }
                                                div { class: "tasks-cal-sidebar-list",
                                                    if sel_tasks.is_empty() && sel_events.is_empty() {
                                                        div { class: "empty compact tasks-cal-empty",
                                                            span { class: "empty-icon", Icon { name: "workflows", size: 18 } }
                                                            h3 { class: "empty-title", "Nothing scheduled" }
                                                            p { class: "empty-text", "No tasks due or calendar events on this day." }
                                                        }
                                                    }
                                                    if !sel_events.is_empty() {
                                                        div { class: "tasks-cal-group",
                                                            span { class: "tasks-cal-group-label", "Calendar events" }
                                                            for event in sel_events {
                                                                {
                                                                    let time_label = format_event_time(&event);
                                                                    let provider = event_provider_label(&event.provider);
                                                                    rsx! {
                                                                        div { key: "{event.id}", class: "tasks-cal-event-item",
                                                                            div { class: "tasks-cal-event-head",
                                                                                strong { class: "tasks-cal-event-title", "{event.title}" }
                                                                                span { class: "tasks-source-tag", "{provider}" }
                                                                            }
                                                                            span { class: "tasks-cal-event-time", "{time_label}" }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                    if !sel_tasks.is_empty() {
                                                        div { class: "tasks-cal-group",
                                                            span { class: "tasks-cal-group-label", "Tasks due" }
                                                            for task in sel_tasks {
                                                                TaskCard {
                                                                    key: "cal-{task.id}",
                                                                    session,
                                                                    task,
                                                                    today_iso: today_iso.clone(),
                                                                    editing_task,
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                    }
                }
            }

            if let Some(draft) = editing_task.read().clone() {
                TaskEditorModal {
                    session,
                    draft,
                    editing_task,
                }
            }
        }
    }
}

#[component]
fn TaskGroupSection(
    title: String,
    tasks: Vec<TaskEntry>,
    today_iso: String,
    session: Signal<Session>,
    editing_task: Signal<Option<TaskDraft>>,
) -> Element {
    rsx! {
        section { class: "tasks-section",
            div { class: "tasks-section-head",
                h3 { class: "tasks-section-title", "{title}" }
                span { class: "tasks-section-count", "{tasks.len()}" }
            }
            div { class: "card-grid tasks-list",
                for task in tasks {
                    TaskCard {
                        key: "{task.id}",
                        session,
                        task,
                        today_iso: today_iso.clone(),
                        editing_task,
                    }
                }
            }
        }
    }
}

#[component]
fn TaskCard(
    mut session: Signal<Session>,
    task: TaskEntry,
    today_iso: String,
    mut editing_task: Signal<Option<TaskDraft>>,
) -> Element {
    let mut confirming = use_signal(|| false);
    let status_id = task.id.clone();
    let status = task.status.clone();
    let delete_id = task.id.clone();
    let due_text = task.due.as_deref().map(format_due_date);
    let badge_class = due_text
        .as_deref()
        .map(|d| due_badge_class(d, &task.status, &today_iso))
        .unwrap_or("tasks-due-badge");
    let edit_entry = task.clone();
    let src = source_label(task.remote_provider.as_deref());
    let st_label = status_label(&task.status);

    rsx! {
        div { class: "card tasks-card",
            div { class: "tasks-card-head",
                button {
                    class: "tasks-card-check {task.status}",
                    r#type: "button",
                    title: "Cycle status",
                    onclick: move |_| {
                        session.write().cycle_task_status(status_id.clone(), &status);
                    },
                    if task.status == "done" {
                        Icon { name: "check", size: 12 }
                    }
                    "{st_label}"
                }
                div { class: "tasks-card-body",
                    span { class: "tasks-card-title", "{task.title}" }
                    if !task.notes.trim().is_empty() {
                        p { class: "tasks-card-notes", "{task.notes}" }
                    }
                    div { class: "tasks-card-meta",
                        if let Some(due) = due_text {
                            span { class: "{badge_class}", "{due}" }
                        }
                        span { class: "tasks-source-tag", "{src}" }
                    }
                }
                div { class: "tasks-card-actions",
                    if confirming() {
                        button {
                            class: "btn danger sm",
                            r#type: "button",
                            onclick: move |_| {
                                confirming.set(false);
                                session.write().delete_task(delete_id.clone());
                            },
                            "Delete"
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| confirming.set(false),
                            "Keep"
                        }
                    } else {
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            title: "Edit task",
                            onclick: move |_| editing_task.set(Some(TaskDraft::from_entry(&edit_entry))),
                            Icon { name: "edit", size: 13 }
                            "Edit"
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            title: "Delete task",
                            "aria-label": "Delete {task.title}",
                            onclick: move |_| confirming.set(true),
                            Icon { name: "trash", size: 13 }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn KanbanTaskCard(
    mut session: Signal<Session>,
    task: TaskEntry,
    today_iso: String,
    mut editing_task: Signal<Option<TaskDraft>>,
) -> Element {
    let mut confirming = use_signal(|| false);
    let due_text = task.due.as_deref().map(format_due_date);
    let badge_class = due_text
        .as_deref()
        .map(|d| due_badge_class(d, &task.status, &today_iso))
        .unwrap_or("tasks-due-badge");
    let prev = left_status(&task.status);
    let next = right_status(&task.status);
    let edit_entry = task.clone();
    let delete_id = task.id.clone();

    rsx! {
        div { class: "card tasks-kanban-card",
            div { class: "tasks-card-body",
                strong { class: "tasks-card-title", "{task.title}" }
                if !task.notes.trim().is_empty() {
                    p { class: "tasks-card-notes", "{task.notes}" }
                }
                div { class: "tasks-card-meta",
                    if let Some(due) = due_text {
                        span { class: "{badge_class}", "{due}" }
                    }
                    span { class: "tasks-source-tag", "{source_label(task.remote_provider.as_deref())}" }
                }
            }
            div { class: "tasks-card-actions",
                if let Some(target) = prev {
                    {
                        let id = task.id.clone();
                        rsx! {
                            button {
                                class: "tasks-kanban-move",
                                r#type: "button",
                                title: "Move to {column_heading(target)}",
                                "aria-label": "Move to {column_heading(target)}",
                                onclick: move |_| {
                                    session.write().update_task(
                                        id.clone(),
                                        TaskUpdate {
                                            status: Some(target.to_owned()),
                                            ..Default::default()
                                        },
                                    );
                                },
                                Icon { name: "chevron-left", size: 13 }
                            }
                        }
                    }
                }
                if let Some(target) = next {
                    {
                        let id = task.id.clone();
                        rsx! {
                            button {
                                class: "tasks-kanban-move",
                                r#type: "button",
                                title: "Move to {column_heading(target)}",
                                "aria-label": "Move to {column_heading(target)}",
                                onclick: move |_| {
                                    session.write().update_task(
                                        id.clone(),
                                        TaskUpdate {
                                            status: Some(target.to_owned()),
                                            ..Default::default()
                                        },
                                    );
                                },
                                Icon { name: "chevron-right", size: 13 }
                            }
                        }
                    }
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| editing_task.set(Some(TaskDraft::from_entry(&edit_entry))),
                    Icon { name: "edit", size: 12 }
                    "Edit"
                }
                if confirming() {
                    button {
                        class: "btn danger sm",
                        r#type: "button",
                        onclick: move |_| {
                            confirming.set(false);
                            session.write().delete_task(delete_id.clone());
                        },
                        "Delete"
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| confirming.set(false),
                        "Keep"
                    }
                } else {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        title: "Delete task",
                        "aria-label": "Delete {task.title}",
                        onclick: move |_| confirming.set(true),
                        Icon { name: "trash", size: 12 }
                    }
                }
            }
        }
    }
}

#[component]
fn TaskEditorModal(
    mut session: Signal<Session>,
    draft: TaskDraft,
    mut editing_task: Signal<Option<TaskDraft>>,
) -> Element {
    let mut title = use_signal(|| draft.title.clone());
    let mut notes = use_signal(|| draft.notes.clone());
    let mut status = use_signal(|| draft.status.clone());
    let mut due = use_signal(|| draft.due.clone());
    let mut error = use_signal(|| draft.error.clone());
    let task_id = draft.id.clone();

    rsx! {
        div {
            class: "tasks-editor-overlay",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "Edit task",
            onclick: move |_| editing_task.set(None),
            div {
                class: "tasks-editor-modal",
                onclick: move |evt| evt.stop_propagation(),
                div { class: "tasks-editor-head",
                    h3 { "Edit Task" }
                    button {
                        class: "icon-btn",
                        r#type: "button",
                        "aria-label": "Close editor",
                        onclick: move |_| editing_task.set(None),
                        Icon { name: "x", size: 15 }
                    }
                }
                div { class: "tasks-editor-body",
                    div { class: "field",
                        label { class: "field-label", "Title" }
                        input {
                            class: "input",
                            value: "{title}",
                            oninput: move |evt| {
                                error.set(None);
                                title.set(evt.value());
                            },
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "Status" }
                        select {
                            class: "select",
                            onchange: move |evt| status.set(evt.value()),
                            for (val, lbl) in [("todo", "To do"), ("doing", "Doing"), ("done", "Done")] {
                                option {
                                    value: "{val}",
                                    selected: *status.read() == val,
                                    "{lbl}"
                                }
                            }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "Due date" }
                        div { class: "tasks-composer-row",
                            input {
                                class: "input",
                                r#type: "date",
                                value: "{due}",
                                oninput: move |evt| {
                                    error.set(None);
                                    due.set(evt.value());
                                },
                            }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                onclick: move |_| due.set(String::new()),
                                "Clear date"
                            }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "Notes" }
                        textarea {
                            class: "textarea",
                            rows: "3",
                            value: "{notes}",
                            oninput: move |evt| notes.set(evt.value()),
                        }
                    }
                    if let Some(msg) = error.read().as_ref() {
                        p { class: "tasks-inline-err", role: "alert", "{msg}" }
                    }
                }
                div { class: "tasks-editor-foot",
                    button {
                        class: "btn ghost",
                        r#type: "button",
                        onclick: move |_| editing_task.set(None),
                        "Cancel"
                    }
                    button {
                        class: "btn primary",
                        r#type: "button",
                        onclick: move |_| {
                            let t = title.read().trim().to_owned();
                            if t.is_empty() {
                                error.set(Some("Title cannot be empty.".into()));
                                return;
                            }
                            let d = due.read().trim().to_owned();
                            if !is_valid_due_input(&d) {
                                error.set(Some("Use YYYY-MM-DD for the due date.".into()));
                                return;
                            }
                            let update = TaskUpdate {
                                title: Some(t),
                                notes: Some(notes.read().clone()),
                                status: Some(status.read().clone()),
                                due: Some((!d.is_empty()).then_some(d)),
                            };
                            session.write().update_task(task_id.clone(), update);
                            editing_task.set(None);
                        },
                        "Save changes"
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_task(id: &str, status: &str, due: Option<&str>) -> TaskEntry {
        TaskEntry {
            id: id.into(),
            title: format!("Task {id}"),
            notes: String::new(),
            status: status.into(),
            due: due.map(String::from),
            position: 0,
            remote_provider: None,
            remote_id: None,
            created_at: "2026-11-15T00:00:00Z".into(),
            updated_at: "2026-11-15T00:00:00Z".into(),
        }
    }

    fn sample_event(id: &str, start: &str, end: &str, all_day: bool, provider: &str) -> TaskEvent {
        TaskEvent {
            id: id.into(),
            title: format!("Event {id}"),
            start: start.into(),
            end: end.into(),
            all_day,
            provider: provider.into(),
        }
    }

    #[test]
    fn civil_date_round_trips() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(weekday_mon0(0), 3);
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(civil_from_days(days_from_civil(2026, 11, 8)), (2026, 11, 8));
        assert_eq!(iso_date(days_from_civil(2026, 2, 29 - 1)), "2026-02-28");
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2028, 2), 29);
        assert_eq!(days_in_month(2026, 11), 30);
    }

    #[test]
    fn due_date_formatting_never_panics_on_short_or_multibyte_values() {
        assert_eq!(format_due_date("2026-11-15T09:00:00Z"), "2026-11-15");
        assert_eq!(format_due_date("2026-11"), "2026-11");
        assert_eq!(format_due_date("éééééé"), "éééééé");
        assert!(is_valid_due_input(""));
        assert!(is_valid_due_input("2028-02-29"));
        assert!(!is_valid_due_input("2026-02-29"));
        assert!(!is_valid_due_input("not-a-date"));
        assert!(is_overdue(
            Some("2026-11-10T10:00:00Z"),
            "todo",
            "2026-11-14"
        ));
        assert!(!is_overdue(Some("2026-11-10"), "done", "2026-11-14"));
        assert!(!is_overdue(Some("2026-11-20"), "todo", "2026-11-14"));
    }

    #[test]
    fn compute_stats_and_grouping_classify_tasks_and_events() {
        let tasks = vec![
            sample_task("1", "todo", Some("2026-11-10")),
            sample_task("2", "doing", Some("2026-11-15")),
            sample_task("3", "todo", Some("2026-11-20")),
            sample_task("4", "todo", None),
            sample_task("5", "done", Some("2026-11-10")),
        ];
        let events = vec![
            sample_event(
                "e1",
                "2026-11-14T09:00:00Z",
                "2026-11-14T10:00:00Z",
                false,
                "google_calendar",
            ),
            sample_event(
                "e2",
                "2026-11-15T09:30:00Z",
                "2026-11-15T10:30:00Z",
                false,
                "google_calendar",
            ),
            sample_event("e3", "2026-11-18", "2026-11-18", true, "outlook_calendar"),
        ];
        let stats = compute_stats(&tasks, &events, "2026-11-15");
        assert_eq!(stats.total, 5);
        assert_eq!(stats.open, 4);
        assert_eq!(stats.todo, 3);
        assert_eq!(stats.doing, 1);
        assert_eq!(stats.done, 1);
        assert_eq!(stats.due_today, 1);
        assert_eq!(stats.overdue, 1);
        assert_eq!(stats.upcoming_events, 2);
        assert_eq!(stats.completion_pct(), 20);

        let groups = group_todo_tasks(&tasks, "2026-11-15");
        assert_eq!(groups.overdue_or_today.len(), 2);
        assert_eq!(groups.upcoming.len(), 1);
        assert_eq!(groups.anytime.len(), 1);
        assert_eq!(groups.completed.len(), 1);
    }

    #[test]
    fn due_filters_and_calendar_formatters_work() {
        let overdue = sample_task("1", "todo", Some("2026-11-10"));
        let today = sample_task("2", "doing", Some("2026-11-15"));
        let upcoming = sample_task("3", "todo", Some("2026-11-20"));
        let anytime = sample_task("4", "todo", None);
        let done_old = sample_task("5", "done", Some("2026-11-10"));

        assert!(task_matches_filter(
            &overdue,
            "",
            TaskStatusFilter::All,
            TaskDueFilter::Overdue,
            "2026-11-15"
        ));
        assert!(!task_matches_filter(
            &done_old,
            "",
            TaskStatusFilter::All,
            TaskDueFilter::Overdue,
            "2026-11-15"
        ));
        assert!(task_matches_filter(
            &today,
            "",
            TaskStatusFilter::Doing,
            TaskDueFilter::Today,
            "2026-11-15"
        ));
        assert!(task_matches_filter(
            &upcoming,
            "",
            TaskStatusFilter::All,
            TaskDueFilter::Upcoming,
            "2026-11-15"
        ));
        assert!(task_matches_filter(
            &anytime,
            "",
            TaskStatusFilter::All,
            TaskDueFilter::Anytime,
            "2026-11-15"
        ));

        let timed = sample_event(
            "e1",
            "2026-11-15T09:00:00Z",
            "2026-11-15T10:30:00Z",
            false,
            "google_calendar",
        );
        let all_day = sample_event("e2", "2026-11-16", "2026-11-16", true, "outlook_calendar");
        assert_eq!(format_event_time(&timed), "09:00 \u{2013} 10:30");
        assert_eq!(format_event_time(&all_day), "All day");
        assert_eq!(event_provider_label(&timed.provider), "Google Calendar");
        assert_eq!(event_provider_label(&all_day.provider), "Outlook Calendar");
        assert_eq!(
            format_day_heading("2026-11-15", "2026-11-15"),
            "Today \u{00b7} Sun, Nov 15"
        );
        assert_eq!(
            format_day_heading("2026-11-16", "2026-11-15"),
            "Mon, Nov 16, 2026"
        );
    }
}
