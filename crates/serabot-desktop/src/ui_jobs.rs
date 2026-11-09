//! The Jobs/Activity panel in the chat's main view (0.28.0): one collapsible
//! list of the ongoing work — the running/queued turn, background media jobs
//! and handoffs to other bots — built by `serabot_desktop::jobs` from the
//! state the event stream and the media-jobs poll already keep.
//!
//! Motion follows the console's existing rules: the panel is a plain
//! interruptible `<details>` (no springs), the running rows use the same
//! `pill-dot live` spinner the activity rows use, and the reduced-motion
//! stylesheet stills it.

use crate::ui::Icon;
use dioxus::prelude::*;
use serabot_desktop::jobs::{self, JobItem, JobState};
use serabot_desktop::session::Session;
use std::time::Instant;

fn state_class(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "job queued",
        JobState::Running => "job running",
        JobState::Waiting => "job waiting",
        JobState::Finishing => "job finishing",
        JobState::Done => "job done",
        JobState::Failed => "job failed",
    }
}

fn state_mark(state: JobState) -> Element {
    match state {
        JobState::Running | JobState::Finishing => rsx! {
            span { class: "pill-dot live", "aria-hidden": "true" }
        },
        JobState::Done => rsx! { Icon { name: "check", size: 12 } },
        JobState::Failed => rsx! { Icon { name: "x", size: 12 } },
        JobState::Waiting => rsx! { Icon { name: "lock", size: 12 } },
        JobState::Queued => rsx! { span { class: "plan-dot", "aria-hidden": "true" } },
    }
}

/// One row of the panel.
#[component]
fn JobRow(item: JobItem, session: Signal<Session>) -> Element {
    let trace = item.trace_conversation.clone();
    rsx! {
        div { class: "{state_class(item.state)}", role: "listitem",
            span { class: "job-mark", {state_mark(item.state)} }
            span { class: "job-icon", Icon { name: item.icon().to_owned(), size: 13 } }
            span { class: "job-title", title: "{item.title}", "{item.title}" }
            span { class: "job-meta", "{item.bot} \u{b7} {item.state.label()}" }
            if let Some(conversation) = trace {
                button {
                    class: "btn ghost sm job-trace",
                    title: "Open chat for trace context",
                    onclick: move |_| session.write().open_conversation(conversation.clone()),
                    "Trace"
                }
            }
        }
    }
}

/// The panel itself. Empty while nothing is running or finishing; the summary
/// line always says what is going on.
#[component]
pub fn JobsPanel(session: Signal<Session>) -> Element {
    let items = {
        let state = session.read();
        jobs::build(
            state.state.task.as_ref(),
            &state.state.media_jobs,
            &state.state.working,
            &state.state.activity,
            &state.conversation,
            &state.state.jobs_reliability,
            Instant::now(),
        )
    };
    if items.is_empty() {
        return rsx! {};
    }
    let active = items.iter().filter(|item| item.is_active()).count();
    let headline = jobs::headline(&items);
    rsx! {
        details { class: "jobs-panel", open: active > 0,
            summary { class: "jobs-summary", "aria-label": "{headline}",
                span { class: if active > 0 { "pill-dot live" } else { "plan-dot" }, "aria-hidden": "true" }
                span { class: "jobs-headline", "{headline}" }
                Icon { name: "chevron-right", size: 12 }
            }
            div { class: "jobs-list", role: "list", "aria-label": "Ongoing work",
                for item in items {
                    JobRow { key: "{item.id}", item, session }
                }
            }
        }
    }
}
