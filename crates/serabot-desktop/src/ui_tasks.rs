//! The Tasks tab (0.25.0): the acting user's personal tasks in three views —
//! a todo list, a month calendar and a Kanban board — plus the opt-in sync
//! with Google Tasks / Microsoft To Do / Google Calendar / Outlook Calendar.
//! All data is the acting user's own (`/v1/tasks*`), loaded through the
//! session's worker so no network call runs on the UI thread.

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

const VIEWS: [TaskView; 3] = [TaskView::Todo, TaskView::Calendar, TaskView::Kanban];

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

/// The current UTC (year, month).
fn today() -> (i64, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    civil_from_days(days_from_epoch(secs))
}

#[component]
pub fn TasksView(mut session: Signal<Session>) -> Element {
    let mut view = use_signal(|| TaskView::Todo);
    let mut new_title = use_signal(String::new);
    let mut new_due = use_signal(String::new);
    let mut month_offset = use_signal(|| 0i64);

    // Load once when the tab opens.
    use_effect(move || {
        let mut s = session.write();
        s.load_tasks();
        s.load_task_prefs();
    });

    let (tasks, prefs, sync, events) = {
        let s = session.read();
        (
            s.state.tasks.clone(),
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
    let (year, month) = {
        let (y, m, _) = today();
        let total = y * 12 + (m as i64 - 1) + *month_offset.read();
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
                        disabled: current_provider == "none" || needs_sign_in,
                        onclick: move |_| session.write().run_task_sync(),
                        "Sync now"
                    }
                }
            }
            if needs_sign_in {
                div { class: "tasks-notice",
                    "Sign in with {sign_in_family} in Connectors to sync {provider_label}."
                }
            }
            match *view.read() {
                TaskView::Todo => rsx! {
                    div { class: "tasks-add",
                        input {
                            class: "input",
                            placeholder: "Add a task…",
                            value: "{new_title}",
                            oninput: move |evt| new_title.set(evt.value()),
                            onkeydown: move |evt| {
                                if evt.key() == Key::Enter {
                                    let title = new_title.read().trim().to_owned();
                                    let due = new_due.read().trim().to_owned();
                                    if !title.is_empty() {
                                        session.write().add_task(title, (!due.is_empty()).then_some(due));
                                        new_title.set(String::new());
                                        new_due.set(String::new());
                                    }
                                }
                            }
                        }
                        input {
                            class: "input tasks-due",
                            placeholder: "Due (YYYY-MM-DD)",
                            value: "{new_due}",
                            oninput: move |evt| new_due.set(evt.value())
                        }
                        button {
                            class: "btn primary sm",
                            onclick: move |_| {
                                let title = new_title.read().trim().to_owned();
                                let due = new_due.read().trim().to_owned();
                                if !title.is_empty() {
                                    session.write().add_task(title, (!due.is_empty()).then_some(due));
                                    new_title.set(String::new());
                                    new_due.set(String::new());
                                }
                            },
                            "Add"
                        }
                    }
                    if tasks.is_empty() {
                        div { class: "empty-note", "No tasks yet. Add one above, or sync a provider." }
                    }
                    for task in tasks.iter().filter(|t| t.status != "done") {
                        TaskRow { session, task: task.clone(), ondelete: true }
                    }
                    if tasks.iter().any(|t| t.status == "done") {
                        details { class: "tasks-done",
                            summary { "Done ({tasks.iter().filter(|t| t.status == \"done\").count()})" }
                            for task in tasks.iter().filter(|t| t.status == "done") {
                                TaskRow { session, task: task.clone(), ondelete: true }
                            }
                        }
                    }
                },
                TaskView::Kanban => rsx! {
                    div { class: "kanban",
                        for column in ["todo", "doing", "done"] {
                            div { class: "kanban-col",
                                h3 { "{column}" }
                                for task in tasks.iter().filter(|t| t.status == column) {
                                    TaskRow { session, task: task.clone(), ondelete: true }
                                }
                            }
                        }
                    }
                },
                TaskView::Calendar => rsx! {
                    div { class: "cal-head",
                        button { class: "btn sm", onclick: move |_| { let current = *month_offset.read(); month_offset.set(current - 1); }, "‹" }
                        strong { "{month_name} {year}" }
                        button { class: "btn sm", onclick: move |_| { let current = *month_offset.read(); month_offset.set(current + 1); }, "›" }
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
                                let day_tasks = tasks_by_day(z);
                                let day_events = events_by_day(z);
                                rsx! {
                                    div { class: "cal-cell",
                                        div { class: "cal-day", "{day}" }
                                        for task in day_tasks {
                                            div { class: "cal-item task", "{task.title}" }
                                        }
                                        for event in day_events {
                                            div { class: "cal-item event", "{event.title}" }
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

#[component]
fn TaskRow(
    mut session: Signal<Session>,
    task: serabot_desktop::TaskEntry,
    ondelete: bool,
) -> Element {
    let status_id = task.id.clone();
    let status = task.status.clone();
    let delete_id = task.id.clone();
    rsx! {
        div { class: "task-row",
            button {
                class: "task-status {task.status}",
                title: "Move to next column",
                onclick: move |_| {
                    session.write().cycle_task_status(status_id.clone(), &status);
                },
                "{task.status}"
            }
            span { class: "task-title", "{task.title}" }
            if let Some(due) = &task.due {
                span { class: "task-due", "{&due[..10]}" }
            }
            if task.remote_provider.is_some() {
                span { class: "task-synced", title: "Synced", "⇄" }
            }
            if ondelete {
                button {
                    class: "btn ghost sm",
                    onclick: move |_| session.write().delete_task(delete_id.clone()),
                    "×"
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
}
