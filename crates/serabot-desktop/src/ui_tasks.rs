//! The Tasks tab (0.25.0): the acting user's personal tasks in three views —
//! a todo list, a month calendar and a Kanban board — plus the opt-in sync
//! with Google Tasks / Microsoft To Do / Google Calendar / Outlook Calendar.
//! All data is the acting user's own (`/v1/tasks*`), loaded through the
//! session's worker so no network call runs on the UI thread.

use crate::ui::Icon;
use dioxus::prelude::*;
use serabot_desktop::session::Session;

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

const VIEWS: [TaskView; 3] = [TaskView::Todo, TaskView::Calendar, TaskView::Kanban];
const STATUS_FILTERS: [TaskStatusFilter; 4] = [
    TaskStatusFilter::All,
    TaskStatusFilter::Todo,
    TaskStatusFilter::Doing,
    TaskStatusFilter::Done,
];

const SYNC_PROVIDERS: [(&str, &str); 5] = [
    ("none", "No sync"),
    ("google_tasks", "Google Tasks"),
    ("microsoft_todo", "Microsoft To Do"),
    ("google_calendar", "Google Calendar"),
    ("outlook_calendar", "Outlook Calendar"),
];

// --- tiny civil-date helpers (no extra dependency) ---------------------------

/// Days since 1970-01-01 from a Unix timestamp (UTC).
fn days_from_epoch(secs: u64) -> i64 {
    (secs / 86_400) as i64
}

/// (year, month, day) for a day count since 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
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

/// Weekday (0 = Monday) for a day count since 1970-01-01 (a Thursday).
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

/// `YYYY-MM-DD` for a day count.
fn iso_date(z: i64) -> String {
    let (y, m, d) = civil_from_days(z);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The current UTC (year, month, day).
fn today() -> (i64, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    civil_from_days(days_from_epoch(secs))
}

/// Safely extract up to `YYYY-MM-DD` (10 characters) without panicking on
/// short or non-ASCII strings.
fn format_due_date(due: &str) -> String {
    let trimmed = due.trim();
    let date_prefix: String = trimmed.chars().take(10).collect();
    if is_valid_iso_date(&date_prefix) {
        date_prefix
    } else {
        trimmed.chars().take(20).collect()
    }
}

/// Validate a calendar date `YYYY-MM-DD`. Empty is treated as "no due date".
fn is_valid_iso_date(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let Ok(year) = raw[..4].parse::<i64>() else {
        return false;
    };
    let Ok(month) = raw[5..7].parse::<u32>() else {
        return false;
    };
    let Ok(day) = raw[8..10].parse::<u32>() else {
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

fn task_matches_filter(
    task: &serabot_desktop::TaskEntry,
    query: &str,
    filter: TaskStatusFilter,
) -> bool {
    if !filter.matches(&task.status) {
        return false;
    }
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return true;
    }
    task.title.to_ascii_lowercase().contains(&needle)
        || task.notes.to_ascii_lowercase().contains(&needle)
}

fn next_status_label(status: &str) -> &'static str {
    match status {
        "todo" => "Doing",
        "doing" => "Done",
        _ => "To do",
    }
}

fn column_heading(status: &str) -> &'static str {
    match status {
        "todo" => "To do",
        "doing" => "Doing",
        "done" => "Done",
        _ => "Tasks",
    }
}

#[component]
pub fn TasksView(mut session: Signal<Session>) -> Element {
    let mut view = use_signal(|| TaskView::Todo);
    let mut new_title = use_signal(String::new);
    let mut new_due = use_signal(String::new);
    let mut due_error = use_signal(|| Option::<String>::None);
    let mut query = use_signal(String::new);
    let mut status_filter = use_signal(|| TaskStatusFilter::All);
    let mut month_offset = use_signal(|| 0i64);

    // Load once when the tab opens.
    use_effect(move || {
        let mut s = session.write();
        s.load_tasks();
        s.load_task_prefs();
    });

    let (tasks, tasks_loaded, prefs, sync, events) = {
        let s = session.read();
        (
            s.state.tasks.clone(),
            s.state.tasks_loaded,
            s.state.task_prefs.clone(),
            s.state.task_sync.clone(),
            s.state.task_events.clone(),
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

    // The calendar view's month range, loaded when the month changes.
    let (today_y, today_m, today_d) = today();
    let today_iso = format!("{today_y:04}-{today_m:02}-{today_d:02}");
    let (year, month) = {
        let total = today_y * 12 + (today_m as i64 - 1) + *month_offset.read();
        (total.div_euclid(12), (total.rem_euclid(12) + 1) as u32)
    };
    {
        let mut session = session;
        use_effect(move || {
            let first = days_from_civil(year, month, 1);
            let (ny, nm) = {
                let total = year * 12 + (month as i64 - 1) + 1;
                (total.div_euclid(12), (total.rem_euclid(12) + 1) as u32)
            };
            let last = days_from_civil(ny, nm, 1);
            session
                .write()
                .load_task_calendar(iso_date(first), iso_date(last));
        });
    }

    let sign_in_family = if current_provider.starts_with("google") {
        "Google"
    } else {
        "Microsoft"
    };
    let provider_label = current_provider.replace('_', " ");

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

    let tasks_by_day = |z: i64| -> Vec<&serabot_desktop::TaskEntry> {
        let day = iso_date(z);
        tasks
            .iter()
            .filter(|t| {
                t.due
                    .as_deref()
                    .map(|d| d.starts_with(&day))
                    .unwrap_or(false)
            })
            .collect()
    };
    let events_by_day = |z: i64| -> Vec<&serabot_desktop::TaskEvent> {
        let day = iso_date(z);
        events
            .iter()
            .filter(|e| e.start.starts_with(&day))
            .collect()
    };

    let needle = query.read().clone();
    let filter = *status_filter.read();
    let filtered_tasks: Vec<serabot_desktop::TaskEntry> = tasks
        .iter()
        .filter(|t| task_matches_filter(t, &needle, filter))
        .cloned()
        .collect();
    let open_tasks: Vec<serabot_desktop::TaskEntry> = filtered_tasks
        .iter()
        .filter(|t| t.status != "done")
        .cloned()
        .collect();
    let done_tasks: Vec<serabot_desktop::TaskEntry> = filtered_tasks
        .iter()
        .filter(|t| t.status == "done")
        .cloned()
        .collect();
    let title_ready = !new_title.read().trim().is_empty();

    fn submit_task(
        mut session: Signal<Session>,
        mut new_title: Signal<String>,
        mut new_due: Signal<String>,
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
    }

    rsx! {
        div { class: "pane tasks-page",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Tasks" }
                    p { class: "pane-sub", "Your todo list, calendar and board. Sync is opt-in per account." }
                }
                div { class: "tasks-head-actions",
                    for v in VIEWS {
                        button {
                            class: if *view.read() == v { "seg on" } else { "seg" },
                            r#type: "button",
                            "aria-pressed": if *view.read() == v { "true" } else { "false" },
                            onclick: move |_| view.set(v),
                            "{v.label()}"
                        }
                    }
                    select {
                        class: "input",
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
                    button {
                        class: "btn primary sm",
                        r#type: "button",
                        disabled: current_provider == "none" || needs_sign_in,
                        onclick: move |_| session.write().run_task_sync(),
                        "Sync now"
                    }
                }
            }
            div { class: "pane-body",
                if needs_sign_in {
                    div { class: "tasks-notice", role: "status",
                        "Sign in with {sign_in_family} in Connectors to sync {provider_label}."
                    }
                }
                if !tasks_loaded && tasks.is_empty() {
                    div { class: "tasks-skeleton", role: "status", "aria-label": "Loading tasks",
                        for n in 0..4 {
                            div { key: "{n}", class: "skeleton", style: "height: 40px; margin-bottom: 8px;" }
                        }
                    }
                } else {
                    match *view.read() {
                    TaskView::Todo => rsx! {
                        div { class: "tasks-add",
                            input {
                                class: "input",
                                placeholder: "Add a task\u{2026}",
                                "aria-label": "New task title",
                                value: "{new_title}",
                                oninput: move |evt| new_title.set(evt.value()),
                                onkeydown: move |evt| {
                                    if evt.key() == Key::Enter {
                                        submit_task(session, new_title, new_due, due_error);
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
                                        submit_task(session, new_title, new_due, due_error);
                                    }
                                }
                            }
                            button {
                                class: "btn primary sm",
                                r#type: "button",
                                disabled: !title_ready,
                                onclick: move |_| submit_task(session, new_title, new_due, due_error),
                                "Add"
                            }
                        }
                        if let Some(msg) = due_error.read().as_ref() {
                            p { class: "tasks-inline-err", role: "alert", "{msg}" }
                        }
                        if !tasks.is_empty() {
                            div { class: "tasks-toolbar",
                                input {
                                    class: "input tasks-search",
                                    r#type: "search",
                                    placeholder: "Filter tasks\u{2026}",
                                    "aria-label": "Filter tasks",
                                    value: "{query}",
                                    oninput: move |evt| query.set(evt.value()),
                                    onkeydown: move |evt: KeyboardEvent| {
                                        if evt.key() == Key::Escape {
                                            query.set(String::new());
                                        }
                                    }
                                }
                                div { class: "tasks-filters", role: "group", "aria-label": "Status filter",
                                    for sf in STATUS_FILTERS {
                                        button {
                                            key: "{sf.label()}",
                                            class: if filter == sf { "seg on" } else { "seg" },
                                            r#type: "button",
                                            onclick: move |_| status_filter.set(sf),
                                            "{sf.label()}"
                                        }
                                    }
                                }
                            }
                        }
                        if tasks.is_empty() {
                            div { class: "empty-note", "No tasks yet. Add one above, or sync a provider." }
                        } else if filtered_tasks.is_empty() {
                            div { class: "empty-note",
                                "No tasks match this filter. "
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    onclick: move |_| {
                                        query.set(String::new());
                                        status_filter.set(TaskStatusFilter::All);
                                    },
                                    "Clear filter"
                                }
                            }
                        } else {
                            for task in open_tasks {
                                TaskRow {
                                    key: "{task.id}",
                                    session,
                                    task,
                                    today_iso: today_iso.clone(),
                                    ondelete: true,
                                }
                            }
                            if !done_tasks.is_empty() {
                                details { class: "tasks-done", open: filter == TaskStatusFilter::Done,
                                    summary { "Done ({done_tasks.len()})" }
                                    for task in done_tasks {
                                        TaskRow {
                                            key: "{task.id}",
                                            session,
                                            task,
                                            today_iso: today_iso.clone(),
                                            ondelete: true,
                                        }
                                    }
                                }
                            }
                        }
                    },
                    TaskView::Kanban => rsx! {
                        if !tasks.is_empty() {
                            div { class: "tasks-toolbar",
                                input {
                                    class: "input tasks-search",
                                    r#type: "search",
                                    placeholder: "Filter board\u{2026}",
                                    "aria-label": "Filter Kanban board",
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
                        div { class: "kanban",
                            for column in ["todo", "doing", "done"] {
                                {
                                    let col_tasks: Vec<serabot_desktop::TaskEntry> = tasks
                                        .iter()
                                        .filter(|t| t.status == column && task_matches_filter(t, &needle, TaskStatusFilter::All))
                                        .cloned()
                                        .collect();
                                    rsx! {
                                        div { key: "{column}", class: "kanban-col",
                                            h3 {
                                                "{column_heading(column)} "
                                                span { class: "kanban-count", "({col_tasks.len()})" }
                                            }
                                            if col_tasks.is_empty() {
                                                p { class: "empty-note kanban-empty", "Nothing here." }
                                            } else {
                                                for task in col_tasks {
                                                    TaskRow {
                                                        key: "{task.id}",
                                                        session,
                                                        task,
                                                        today_iso: today_iso.clone(),
                                                        ondelete: true,
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                    TaskView::Calendar => rsx! {
                        div { class: "cal-head",
                            button {
                                class: "btn sm",
                                r#type: "button",
                                "aria-label": "Previous month",
                                onclick: move |_| { let current = *month_offset.read(); month_offset.set(current - 1); },
                                "‹"
                            }
                            strong { "{month_name} {year}" }
                            button {
                                class: "btn sm",
                                r#type: "button",
                                "aria-label": "Next month",
                                onclick: move |_| { let current = *month_offset.read(); month_offset.set(current + 1); },
                                "›"
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
                        div { class: "cal-grid",
                            for name in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] {
                                div { class: "cal-dow", "{name}" }
                            }
                            for _ in 0..leading {
                                div { class: "cal-cell empty" }
                            }
                            for day in 1..=dim {
                                {
                                    let z = first_day + day as i64 - 1;
                                    let day_iso = iso_date(z);
                                    let is_today = day_iso == today_iso;
                                    let day_tasks = tasks_by_day(z);
                                    let day_events = events_by_day(z);
                                    let total_items = day_tasks.len() + day_events.len();
                                    let extra = total_items.saturating_sub(4);
                                    rsx! {
                                        div {
                                            key: "{day_iso}",
                                            class: if is_today { "cal-cell today" } else { "cal-cell" },
                                            div { class: "cal-day",
                                                span { "{day}" }
                                                button {
                                                    class: "cal-add",
                                                    r#type: "button",
                                                    title: "Add task on {day_iso}",
                                                    "aria-label": "Add task on {day_iso}",
                                                    onclick: move |_| {
                                                        new_due.set(day_iso.clone());
                                                        due_error.set(None);
                                                        view.set(TaskView::Todo);
                                                    },
                                                    Icon { name: "plus", size: 11 }
                                                }
                                            }
                                            for task in day_tasks.into_iter().take(4) {
                                                div { class: "cal-item task", title: "{task.title}", "{task.title}" }
                                            }
                                            for event in day_events.into_iter().take(4) {
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
                    },
                }
            }
            }
        }
    }
}

#[component]
fn TaskRow(
    mut session: Signal<Session>,
    task: serabot_desktop::TaskEntry,
    today_iso: String,
    ondelete: bool,
) -> Element {
    let mut confirming = use_signal(|| false);
    let status_id = task.id.clone();
    let status = task.status.clone();
    let delete_id = task.id.clone();
    let due_text = task.due.as_deref().map(format_due_date);
    let overdue = is_overdue(task.due.as_deref(), &task.status, &today_iso);
    let due_class = if overdue {
        "task-due overdue"
    } else {
        "task-due"
    };
    let next_label = next_status_label(&task.status);
    rsx! {
        div { class: "task-row",
            button {
                class: "task-status {task.status}",
                r#type: "button",
                title: "Move to {next_label}",
                onclick: move |_| {
                    session.write().cycle_task_status(status_id.clone(), &status);
                },
                "{task.status}"
            }
            div { class: "task-main",
                span { class: "task-title", "{task.title}" }
                if !task.notes.trim().is_empty() {
                    span { class: "task-notes", "{task.notes}" }
                }
            }
            if let Some(due) = due_text {
                span { class: "{due_class}", title: if overdue { "Overdue" } else { "Due date" }, "{due}" }
            }
            if task.remote_provider.is_some() {
                span { class: "task-synced", title: "Synced", "⇄" }
            }
            if ondelete {
                if confirming() {
                    button {
                        class: "btn danger sm",
                        r#type: "button",
                        "aria-label": "Confirm delete {task.title}",
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
                        "×"
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_date_round_trips() {
        // 1970-01-01 was a Thursday.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(weekday_mon0(0), 3);
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(civil_from_days(days_from_civil(2026, 11, 8)), (2026, 11, 8));
        assert_eq!(iso_date(days_from_civil(2026, 2, 29 - 1)), "2026-02-28");
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2028, 2), 29);
        assert_eq!(days_in_month(2026, 11), 30);
        // Month math across a year boundary.
        let total: i64 = 2026 * 12 + 11; // December 2026
        assert_eq!(total.div_euclid(12), 2026);
        let next: i64 = total + 1;
        assert_eq!((next.div_euclid(12), next.rem_euclid(12) + 1), (2027, 1));
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
}
