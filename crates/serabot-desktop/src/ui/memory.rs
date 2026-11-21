use super::*;

/// Maximum characters accepted in one memory entry on the client.
const MAX_MEMORY_CHARS: usize = 4_000;

/// Primary server filter chips on the Memory page: `(filter key, label)`.
pub(super) const MEMORY_FILTERS: &[(&str, &str)] = &[
    ("all", "All"),
    ("saved", "Saved"),
    ("learned", "Learned"),
    ("superseded", "Superseded"),
    ("review", "To review"),
];

/// Client-side date window options for loaded memory facts.
pub(super) const MEMORY_DATE_RANGES: &[(&str, &str)] = &[
    ("all", "Any date"),
    ("today", "Today"),
    ("7d", "Last 7 days"),
    ("30d", "Last 30 days"),
];

pub(super) fn validate_memory_content(text: &str) -> Result<(), &'static str> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("Write something to remember first.");
    }
    if trimmed.chars().count() > MAX_MEMORY_CHARS {
        return Err("Memories are limited to 4,000 characters.");
    }
    Ok(())
}

pub(super) fn memory_empty_copy(filter: &str, query: &str) -> (&'static str, String, bool) {
    let has_filter = filter != "all" || !query.trim().is_empty();
    if has_filter {
        (
            "No matching memories",
            "Nothing matched that filter or search. Clear filters to see everything.".to_owned(),
            true,
        )
    } else {
        (
            "No memories saved yet",
            "Durable facts you add here reach the orchestrator.".to_owned(),
            false,
        )
    }
}

pub(super) fn memory_empty_copy_with_facets(
    filter: &str,
    query: &str,
    kind_filter: &str,
    bot_filter: &str,
    date_filter: &str,
) -> (&'static str, String, bool) {
    if kind_filter != "all" || bot_filter != "all" || date_filter != "all" {
        return (
            "No matching memories",
            "Nothing matched the selected kind, bot, or date filters. Clear filters to see everything."
                .to_owned(),
            true,
        );
    }
    memory_empty_copy(filter, query)
}

fn ymd_to_days(date: &str) -> Option<i64> {
    let s = date.trim();
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i32 = s[0..4].parse().ok()?;
    let m: u32 = s[5..7].parse().ok()?;
    let d: u32 = s[8..10].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = i64::from(y) - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(m);
    let d = i64::from(d);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// Summary metrics for the Memory page stat tiles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MemorySummaryStats {
    pub active_facts: i64,
    pub saved_count: i64,
    pub learned_count: i64,
    pub bot_count: usize,
    pub top_bot_hint: String,
    pub recent_7d: usize,
    pub flagged_total: usize,
    pub conflict_count: usize,
    pub pending_count: i64,
}

pub(super) fn compute_memory_summary(
    stats: Option<&MemoryStatsView>,
    entries: &[MemoryEntry],
    today: &str,
    review_summary: Option<(usize, usize)>,
) -> MemorySummaryStats {
    let active_facts = stats
        .map(|s| s.active)
        .unwrap_or_else(|| entries.iter().filter(|m| m.status != "superseded").count() as i64);
    let saved_count = stats
        .and_then(|s| s.by_origin.get("explicit").copied())
        .unwrap_or_else(|| entries.iter().filter(|m| m.origin != "extracted").count() as i64);
    let learned_count = stats
        .and_then(|s| s.by_origin.get("extracted").copied())
        .unwrap_or_else(|| entries.iter().filter(|m| m.origin == "extracted").count() as i64);
    let pending_count = stats
        .and_then(|s| s.by_status.get("pending").copied())
        .unwrap_or_else(|| entries.iter().filter(|m| m.status == "pending").count() as i64);

    let mut per_bot: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for entry in entries {
        if let Some(bot) = entry
            .bot
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
        {
            *per_bot.entry(bot.to_owned()).or_insert(0) += 1;
        }
    }
    let bot_count = per_bot.len();
    let top_bot_hint = if let Some((bot, n)) = per_bot.iter().max_by_key(|(_, n)| *n) {
        format!("Most from @{bot} ({n})")
    } else if !entries.is_empty() {
        "Shared across all bots".to_owned()
    } else {
        "Owner-scoped across bots".to_owned()
    };

    let today_days = ymd_to_days(today);
    let recent_7d = match today_days {
        Some(t_days) => entries
            .iter()
            .filter(|m| {
                ymd_to_days(&m.created_at)
                    .map(|c_days| t_days - c_days <= 6)
                    .unwrap_or(false)
            })
            .count(),
        None => 0,
    };

    let (flagged_total, conflict_count) = review_summary.unwrap_or((0, 0));

    MemorySummaryStats {
        active_facts,
        saved_count,
        learned_count,
        bot_count,
        top_bot_hint,
        recent_7d,
        flagged_total,
        conflict_count,
        pending_count,
    }
}

/// Distinct memory kinds and counts sorted alphabetically, for filter pills.
pub(super) fn distinct_memory_kinds(entries: &[MemoryEntry]) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for entry in entries {
        let kind = entry.kind.trim();
        if !kind.is_empty() {
            *counts.entry(kind.to_owned()).or_insert(0) += 1;
        }
    }
    counts.into_iter().collect()
}

/// Distinct bots and counts plus count of owner-wide facts (`bot == None`).
pub(super) fn distinct_memory_bots(entries: &[MemoryEntry]) -> (Vec<(String, usize)>, usize) {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut general = 0usize;
    for entry in entries {
        match entry
            .bot
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
        {
            Some(bot) => *counts.entry(bot.to_owned()).or_insert(0) += 1,
            None => general += 1,
        }
    }
    (counts.into_iter().collect(), general)
}

/// Check whether a memory matches the client-side kind, bot, and date filters.
pub(super) fn matches_memory_facets(
    memory: &MemoryEntry,
    kind_filter: &str,
    bot_filter: &str,
    date_filter: &str,
    today: &str,
) -> bool {
    if kind_filter != "all" && !memory.kind.eq_ignore_ascii_case(kind_filter) {
        return false;
    }
    match bot_filter {
        "all" => {}
        "__general__" => {
            if memory
                .bot
                .as_deref()
                .map(str::trim)
                .is_some_and(|b| !b.is_empty())
            {
                return false;
            }
        }
        target => {
            if memory.bot.as_deref().map(str::trim) != Some(target) {
                return false;
            }
        }
    }
    if date_filter != "all" {
        let Some(t_days) = ymd_to_days(today) else {
            return true;
        };
        let Some(c_days) = ymd_to_days(&memory.created_at) else {
            return false;
        };
        let age = t_days - c_days;
        let max_age = match date_filter {
            "today" => 0,
            "7d" => 6,
            "30d" => 29,
            _ => i64::MAX,
        };
        if age > max_age {
            return false;
        }
    }
    true
}

/// One memory card, prepared before rendering so the markup stays plain.
#[derive(Clone, PartialEq)]
pub(super) struct MemoryRow {
    id: String,
    content: String,
    kind: String,
    source: String,
    origin: &'static str,
    superseded: bool,
    pending: bool,
    delete_label: &'static str,
    bot: Option<String>,
    bot_label: Option<String>,
    importance: String,
    importance_pct: u32,
    created: String,
    created_raw: String,
    recalled: String,
}

pub(super) fn memory_rows(
    memories: &[MemoryEntry],
    bot_names: &std::collections::HashMap<String, String>,
) -> Vec<MemoryRow> {
    memories
        .iter()
        .map(|memory| {
            let bot = memory
                .bot
                .as_deref()
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .map(str::to_owned);
            let bot_label = bot.as_deref().map(|slug| {
                bot_names
                    .get(slug)
                    .cloned()
                    .unwrap_or_else(|| slug.to_owned())
            });
            MemoryRow {
                id: memory.id.clone(),
                content: memory.content.clone(),
                kind: memory.kind.clone(),
                source: memory.source.trim().to_owned(),
                origin: if memory.origin == "extracted" {
                    "Learned"
                } else {
                    "Saved"
                },
                superseded: memory.status == "superseded",
                pending: memory.status == "pending",
                delete_label: if memory.status == "pending" {
                    "Discard"
                } else {
                    "Delete"
                },
                bot,
                bot_label,
                importance: format!(
                    "{:.0}% important",
                    memory.importance.clamp(0.0, 1.0) * 100.0
                ),
                importance_pct: (memory.importance.clamp(0.0, 1.0) * 100.0).round() as u32,
                created: stamp(&memory.created_at),
                created_raw: memory.created_at.clone(),
                recalled: match &memory.last_recalled_at {
                    Some(at) => format!("recalled {} ({}\u{d7})", stamp(at), memory.recall_count),
                    None => "not recalled yet".into(),
                },
            }
        })
        .collect()
}

/// The header line: counts by origin and status, and the last extraction.
pub(super) fn memory_header(stats: Option<&MemoryStatsView>) -> String {
    match stats {
        None => "Owner-scoped memory. Loading counts\u{2026}".into(),
        Some(stats) => {
            let count = |map: &std::collections::BTreeMap<String, i64>, key: &str| {
                map.get(key).copied().unwrap_or(0)
            };
            let last = stats
                .last_extraction_at
                .as_deref()
                .map_or("never".to_owned(), stamp);
            format!(
                "{} active · {} saved · {} learned · {} superseded · last learned: {last}",
                stats.active,
                count(&stats.by_origin, "explicit"),
                count(&stats.by_origin, "extracted"),
                count(&stats.by_status, "superseded"),
            )
        }
    }
}

#[component]
pub(super) fn MemoryView(session: Signal<Session>) -> Element {
    let mut draft_error = use_signal(|| Option::<&'static str>::None);
    let mut composing = use_signal(|| false);
    let mut kind_filter = use_signal(|| "all".to_owned());
    let mut bot_filter = use_signal(|| "all".to_owned());
    let mut date_filter = use_signal(|| "all".to_owned());
    let review_summary = use_signal(|| Option::<(usize, usize)>::None);

    let today = today_utc_date();
    let (raw_memories, memories_loaded, stats_opt, header, app_error, bot_names) = {
        let s = session.read();
        let bot_names: std::collections::HashMap<String, String> = s
            .state
            .listed_bots
            .iter()
            .map(|b| (b.id.clone(), s.bot_display_name(&b.id)))
            .collect();
        (
            s.state.memories.clone(),
            s.state.memories_loaded,
            s.state.memory_stats.clone(),
            memory_header(s.state.memory_stats.as_ref()),
            s.state.error.clone(),
            bot_names,
        )
    };

    let summary_stats =
        compute_memory_summary(stats_opt.as_ref(), &raw_memories, &today, review_summary());
    let available_kinds = distinct_memory_kinds(&raw_memories);
    let (available_bots, general_count) = distinct_memory_bots(&raw_memories);

    let active_kind_filter = kind_filter();
    let active_bot_filter = bot_filter();
    let active_date_filter = date_filter();

    let filtered_entries: Vec<MemoryEntry> = raw_memories
        .iter()
        .filter(|entry| {
            matches_memory_facets(
                entry,
                &active_kind_filter,
                &active_bot_filter,
                &active_date_filter,
                &today,
            )
        })
        .cloned()
        .collect();

    let total_loaded = raw_memories.len();
    let memories = memory_rows(&filtered_entries, &bot_names);
    let shown_count = memories.len();

    let kind = session.read().memory.kind.clone();
    let content = session.read().memory.content.clone();
    let source = session.read().memory.source.clone();
    let query = session.read().memory.query.clone();
    let filter = session.read().memory.filter.clone();
    let confirm = session.read().memory.delete_confirm.clone();
    let editing = session.read().memory.editing.clone();
    let edit_content = session.read().memory.edit_content.clone();
    let memories_empty = memories.is_empty();
    let content_chars = content.trim().chars().count();
    let can_add = validate_memory_content(&content).is_ok();
    let has_facet_filter =
        active_kind_filter != "all" || active_bot_filter != "all" || active_date_filter != "all";
    let (empty_title, empty_text, empty_filtered) = memory_empty_copy_with_facets(
        &filter,
        &query,
        &active_kind_filter,
        &active_bot_filter,
        &active_date_filter,
    );
    let is_composing = composing();

    let flagged_sub = if summary_stats.conflict_count > 0 {
        format!(
            "{} potential conflict{}",
            summary_stats.conflict_count,
            if summary_stats.conflict_count == 1 {
                ""
            } else {
                "s"
            }
        )
    } else if summary_stats.pending_count > 0 {
        format!("{} awaiting approval", summary_stats.pending_count)
    } else if review_summary().is_some() {
        "No conflicts detected".to_owned()
    } else {
        "Weekly audit active".to_owned()
    };

    rsx! {
        div { class: "pane memory-page",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Memory" }
                    p { class: "muted", "{header}" }
                }
                div { class: "page-actions",
                    button {
                        class: "btn ghost",
                        r#type: "button",
                        onclick: move |_| session.write().load_memories(),
                        Icon { name: "refresh", size: 14 }
                        "Refresh"
                    }
                    button {
                        class: if is_composing { "btn" } else { "btn primary" },
                        r#type: "button",
                        "aria-expanded": if is_composing { "true" } else { "false" },
                        "aria-controls": "mem-composer",
                        onclick: move |_| {
                            draft_error.set(None);
                            composing.set(!is_composing);
                        },
                        if is_composing {
                            "Close"
                        } else {
                            Icon { name: "plus", size: 15 }
                            "Add memory"
                        }
                    }
                }
            }
            div { class: "pane-body",
                if let Some(error) = app_error {
                    div { class: "banner err", role: "alert", span { "{error}" } }
                }

                // Header stat tiles (system.css .stat-tile)
                if !memories_loaded && stats_opt.is_none() {
                    div { class: "mem-stats", "aria-busy": "true",
                        for n in 0..4 {
                            div { key: "stat-skel-{n}", class: "stat-tile",
                                div { class: "skeleton line" }
                                div { class: "skeleton title" }
                                div { class: "skeleton line" }
                            }
                        }
                    }
                } else {
                    div { class: "mem-stats",
                        div { class: "stat-tile",
                            span { class: "stat-label", "Active facts" }
                            span { class: "stat-value", "{summary_stats.active_facts}" }
                            span { class: "stat-sub",
                                "{summary_stats.saved_count} saved \u{00b7} {summary_stats.learned_count} learned"
                            }
                        }
                        div { class: "stat-tile",
                            span { class: "stat-label", "Bots using memory" }
                            span { class: "stat-value", "{summary_stats.bot_count}" }
                            span { class: "stat-sub", "{summary_stats.top_bot_hint}" }
                        }
                        div { class: "stat-tile",
                            span { class: "stat-label", "Added (7 days)" }
                            span { class: "stat-value", "{summary_stats.recent_7d}" }
                            span { class: "stat-sub",
                                "of {total_loaded} loaded facts"
                            }
                        }
                        div {
                            class: if summary_stats.conflict_count > 0 || summary_stats.flagged_total > 0 { "stat-tile is-attention" } else { "stat-tile" },
                            span { class: "stat-label", "Flagged / conflicts" }
                            span { class: "stat-value",
                                if review_summary().is_some() {
                                    "{summary_stats.flagged_total}"
                                } else {
                                    "{summary_stats.pending_count}"
                                }
                            }
                            span { class: "stat-sub", "{flagged_sub}" }
                        }
                    }
                }

                // Weekly memory review as a distinct card
                crate::ui_memory_review::MemoryReviewCard {
                    session,
                    review_summary: Some(review_summary),
                }

                if is_composing {
                    div { class: "card mem-composer", id: "mem-composer",
                        div { class: "card-head",
                            h2 { class: "card-title", "Save a new fact" }
                        }
                        div { class: "field",
                            label { class: "field-label", r#for: "mem-content", "What should be remembered?" }
                            textarea {
                                id: "mem-content",
                                class: if draft_error().is_some() || content_chars > MAX_MEMORY_CHARS { "textarea invalid" } else { "textarea" },
                                rows: "2",
                                autofocus: true,
                                value: "{content}",
                                placeholder: "A durable preference, fact, decision, context or note",
                                oninput: move |event| {
                                    draft_error.set(None);
                                    session.write().memory.content = event.value();
                                },
                                onkeydown: move |event: KeyboardEvent| {
                                    if event.key() == Key::Escape {
                                        event.stop_propagation();
                                        composing.set(false);
                                    } else if event.key() == Key::Enter
                                        && (event.modifiers().contains(Modifiers::CONTROL)
                                            || event.modifiers().contains(Modifiers::META))
                                    {
                                        event.prevent_default();
                                        let current = session.read().memory.content.clone();
                                        match validate_memory_content(&current) {
                                            Ok(()) => {
                                                draft_error.set(None);
                                                session.write().add_memory();
                                                composing.set(false);
                                            }
                                            Err(msg) => draft_error.set(Some(msg)),
                                        }
                                    }
                                },
                            }
                            if let Some(msg) = draft_error() {
                                span { class: "field-error", role: "alert", "{msg}" }
                            } else if content_chars > 3_500 {
                                span { class: "tiny", "{content_chars}/{MAX_MEMORY_CHARS}" }
                            }
                        }
                        div { class: "mem-composer-row",
                            div { class: "field",
                                label { class: "field-label", r#for: "mem-kind", "Kind" }
                                select {
                                    id: "mem-kind",
                                    class: "select",
                                    value: "{kind}",
                                    onchange: move |event| session.write().memory.kind = event.value(),
                                    for kind in Session::memory_kinds() {
                                        option { value: "{kind}", "{kind}" }
                                    }
                                }
                            }
                            div { class: "field",
                                label { class: "field-label", r#for: "mem-source", "Source" }
                                input {
                                    id: "mem-source",
                                    class: "input",
                                    value: "{source}",
                                    placeholder: "source",
                                    oninput: move |event| session.write().memory.source = event.value(),
                                }
                            }
                            div { class: "mem-composer-actions",
                                button {
                                    class: "btn ghost",
                                    r#type: "button",
                                    onclick: move |_| composing.set(false),
                                    "Cancel"
                                }
                                button {
                                    class: "btn primary",
                                    r#type: "button",
                                    disabled: !can_add,
                                    onclick: move |_| {
                                        let current = session.read().memory.content.clone();
                                        match validate_memory_content(&current) {
                                            Ok(()) => {
                                                draft_error.set(None);
                                                session.write().add_memory();
                                                composing.set(false);
                                            }
                                            Err(msg) => draft_error.set(Some(msg)),
                                        }
                                    },
                                    "Save memory"
                                }
                            }
                        }
                    }
                }

                // Unified filter card: search + server status seg + client kind/bot/date facets
                div { class: "card mem-filter-panel",
                    div { class: "mem-toolbar",
                        div { class: "search-wrap mem-search",
                            input {
                                class: "input",
                                r#type: "search",
                                value: "{query}",
                                placeholder: "Search all of your memory, including old facts",
                                "aria-label": "Search memories",
                                oninput: move |event| session.write().memory.query = event.value(),
                                onkeydown: move |event: KeyboardEvent| {
                                    if event.key() == Key::Enter {
                                        event.prevent_default();
                                        session.write().load_memories();
                                    } else if event.key() == Key::Escape && !session.read().memory.query.is_empty() {
                                        event.stop_propagation();
                                        session.write().memory.query.clear();
                                        session.write().load_memories();
                                    }
                                },
                            }
                            if !query.trim().is_empty() {
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    onclick: move |_| {
                                        session.write().memory.query.clear();
                                        session.write().load_memories();
                                    },
                                    "Clear"
                                }
                            }
                            button {
                                class: "btn sm",
                                r#type: "button",
                                onclick: move |_| session.write().load_memories(),
                                "Search"
                            }
                        }
                        div { class: "seg", role: "group", "aria-label": "Filter memories by status or origin",
                            for (key, label) in MEMORY_FILTERS.iter().copied() {
                                button {
                                    key: "{key}",
                                    r#type: "button",
                                    class: if filter == key { "seg-btn active" } else { "seg-btn" },
                                    "aria-pressed": if filter == key { "true" } else { "false" },
                                    onclick: move |_| session.write().set_memory_filter(key),
                                    "{label}"
                                }
                            }
                        }
                    }
                    if memories_loaded && (!available_kinds.is_empty() || !available_bots.is_empty() || total_loaded > 0) {
                        div { class: "mem-facets",
                            if !available_kinds.is_empty() {
                                div { class: "mem-facet-group",
                                    span { class: "mem-facet-label", "Kind" }
                                    div { class: "seg mem-seg-wrap", role: "group", "aria-label": "Filter by kind",
                                        button {
                                            r#type: "button",
                                            class: if active_kind_filter == "all" { "seg-btn active" } else { "seg-btn" },
                                            "aria-pressed": if active_kind_filter == "all" { "true" } else { "false" },
                                            onclick: move |_| kind_filter.set("all".to_owned()),
                                            "All"
                                        }
                                        for (kind_key, count) in available_kinds.iter().cloned() {
                                            {
                                                let click_kind = kind_key.clone();
                                                let active = active_kind_filter == kind_key;
                                                rsx! {
                                                    button {
                                                        key: "kind-{kind_key}",
                                                        r#type: "button",
                                                        class: if active { "seg-btn active" } else { "seg-btn" },
                                                        "aria-pressed": if active { "true" } else { "false" },
                                                        onclick: move |_| kind_filter.set(click_kind.clone()),
                                                        "{kind_key} ({count})"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            if !available_bots.is_empty() {
                                div { class: "mem-facet-group",
                                    span { class: "mem-facet-label", "Bot" }
                                    div { class: "seg mem-seg-wrap", role: "group", "aria-label": "Filter by bot",
                                        button {
                                            r#type: "button",
                                            class: if active_bot_filter == "all" { "seg-btn active" } else { "seg-btn" },
                                            "aria-pressed": if active_bot_filter == "all" { "true" } else { "false" },
                                            onclick: move |_| bot_filter.set("all".to_owned()),
                                            "All bots"
                                        }
                                        if general_count > 0 {
                                            button {
                                                r#type: "button",
                                                class: if active_bot_filter == "__general__" { "seg-btn active" } else { "seg-btn" },
                                                "aria-pressed": if active_bot_filter == "__general__" { "true" } else { "false" },
                                                onclick: move |_| bot_filter.set("__general__".to_owned()),
                                                "Owner ({general_count})"
                                            }
                                        }
                                        for (bot_slug, count) in available_bots.iter().cloned() {
                                            {
                                                let click_bot = bot_slug.clone();
                                                let display = bot_names
                                                    .get(&bot_slug)
                                                    .cloned()
                                                    .unwrap_or_else(|| bot_slug.clone());
                                                let active = active_bot_filter == bot_slug;
                                                rsx! {
                                                    button {
                                                        key: "bot-{bot_slug}",
                                                        r#type: "button",
                                                        class: if active { "seg-btn active" } else { "seg-btn" },
                                                        "aria-pressed": if active { "true" } else { "false" },
                                                        onclick: move |_| bot_filter.set(click_bot.clone()),
                                                        "{display} ({count})"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            div { class: "mem-facet-group",
                                span { class: "mem-facet-label", "Date" }
                                div { class: "seg", role: "group", "aria-label": "Filter by creation date",
                                    for (date_key, date_label) in MEMORY_DATE_RANGES.iter().copied() {
                                        button {
                                            key: "date-{date_key}",
                                            r#type: "button",
                                            class: if active_date_filter == date_key { "seg-btn active" } else { "seg-btn" },
                                            "aria-pressed": if active_date_filter == date_key { "true" } else { "false" },
                                            onclick: move |_| date_filter.set(date_key.to_owned()),
                                            "{date_label}"
                                        }
                                    }
                                }
                            }
                            div { class: "mem-facet-status",
                                span { class: "page-count",
                                    if shown_count == total_loaded {
                                        "Showing {shown_count} facts"
                                    } else {
                                        "Showing {shown_count} of {total_loaded} facts"
                                    }
                                }
                                if has_facet_filter {
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        onclick: move |_| {
                                            kind_filter.set("all".to_owned());
                                            bot_filter.set("all".to_owned());
                                            date_filter.set("all".to_owned());
                                        },
                                        "Reset facets"
                                    }
                                }
                            }
                        }
                    }
                }

                div { class: "card-grid mem-grid",
                    if !memories_loaded {
                        for n in 0..6 {
                            div { key: "{n}", class: "card mem-card", "aria-busy": "true",
                                div { class: "skeleton row" }
                                div { class: "skeleton title" }
                                div { class: "skeleton block" }
                            }
                        }
                    }
                    for memory in memories {
                        div {
                            class: if memory.superseded { "card mem-card superseded" } else if memory.pending { "card mem-card pending" } else { "card mem-card" },
                            key: "{memory.id}",
                            div { class: "mem-meta",
                                span { class: if memory.origin == "Learned" { "chip" } else { "chip accent" }, "{memory.origin}" }
                                span { class: "chip", "{memory.kind}" }
                                if let Some(label) = memory.bot_label.clone() {
                                    span {
                                        class: "chip mem-bot-chip",
                                        title: memory.bot.clone().unwrap_or_default(),
                                        "@{label}"
                                    }
                                } else {
                                    span { class: "chip mem-bot-chip", "Owner" }
                                }
                                if memory.superseded {
                                    span { class: "chip", "Superseded" }
                                }
                                if memory.pending {
                                    span { class: "chip warn", "To review" }
                                }
                            }
                            if editing.as_deref() == Some(memory.id.as_str()) {
                                {
                                    let edit_invalid = validate_memory_content(&edit_content).is_err();
                                    rsx! {
                                        textarea {
                                            class: if edit_invalid && !edit_content.trim().is_empty() { "textarea invalid" } else { "textarea" },
                                            autofocus: true,
                                            value: "{edit_content}",
                                            oninput: move |event| session.write().memory.edit_content = event.value(),
                                            onkeydown: move |event: KeyboardEvent| {
                                                if event.key() == Key::Escape {
                                                    event.stop_propagation();
                                                    session.write().cancel_memory_edit();
                                                } else if event.key() == Key::Enter
                                                    && (event.modifiers().contains(Modifiers::CONTROL)
                                                        || event.modifiers().contains(Modifiers::META))
                                                {
                                                    event.prevent_default();
                                                    let cur = session.read().memory.edit_content.clone();
                                                    if validate_memory_content(&cur).is_ok() {
                                                        session.write().save_memory_edit();
                                                    }
                                                }
                                            },
                                        }
                                        div { class: "row-actions",
                                            button {
                                                class: "btn primary sm",
                                                r#type: "button",
                                                disabled: edit_invalid,
                                                onclick: move |_| session.write().save_memory_edit(),
                                                "Save"
                                            }
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                onclick: move |_| session.write().cancel_memory_edit(),
                                                "Cancel"
                                            }
                                        }
                                    }
                                }
                            } else {
                                p { class: "mem-text", "{memory.content}" }
                                div { class: "mem-foot",
                                    div { class: "mem-foot-top",
                                        div { class: "mem-importance", title: "{memory.importance}",
                                            div { class: "meter",
                                                div { class: "meter-fill", style: "width: {memory.importance_pct}%;" }
                                            }
                                            span { class: "tiny", "{memory.importance_pct}%" }
                                        }
                                        if !memory.source.is_empty() {
                                            span { class: "mem-source-tag", title: "Source: {memory.source}", "{memory.source}" }
                                        }
                                    }
                                    p { class: "tiny", title: "{memory.created_raw}", "{memory.created} \u{00b7} {memory.recalled}" }
                                }
                                div { class: "row-actions mem-actions",
                                    if memory.pending {
                                        button {
                                            class: "btn primary sm",
                                            r#type: "button",
                                            onclick: {
                                                let id = memory.id.clone();
                                                move |_| session.write().approve_memory(id.clone())
                                            },
                                            "Approve"
                                        }
                                    }
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        onclick: {
                                            let id = memory.id.clone();
                                            let text = memory.content.clone();
                                            move |_| session.write().begin_memory_edit(id.clone(), &text)
                                        },
                                        "Edit"
                                    }
                                    if confirm.as_deref() == Some(memory.id.as_str()) {
                                        div { class: "vault-confirm", role: "alertdialog", "aria-label": "Confirm delete memory",
                                            button {
                                                class: "btn danger sm",
                                                r#type: "button",
                                                onclick: {
                                                    let id = memory.id.clone();
                                                    move |_| session.write().delete_memory(id.clone())
                                                },
                                                "Confirm delete"
                                            }
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                onclick: move |_| session.write().memory.delete_confirm = None,
                                                "Cancel"
                                            }
                                        }
                                    } else {
                                        button {
                                            class: "btn ghost sm danger",
                                            r#type: "button",
                                            onclick: {
                                                let id = memory.id.clone();
                                                move |_| session.write().memory.delete_confirm = Some(id.clone())
                                            },
                                            "{memory.delete_label}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if memories_loaded && memories_empty {
                    div { class: "empty",
                        div { class: "empty-icon", Icon { name: if empty_filtered { "search" } else { "memory" }, size: 20 } }
                        h3 { class: "empty-title", "{empty_title}" }
                        p { class: "empty-text", "{empty_text}" }
                        if empty_filtered {
                            button {
                                class: "btn sm",
                                r#type: "button",
                                onclick: move |_| {
                                    kind_filter.set("all".to_owned());
                                    bot_filter.set("all".to_owned());
                                    date_filter.set("all".to_owned());
                                    session.write().memory.query.clear();
                                    session.write().set_memory_filter("all");
                                },
                                "Clear filters"
                            }
                        } else {
                            button {
                                class: "btn primary sm",
                                r#type: "button",
                                onclick: move |_| composing.set(true),
                                "Add memory"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(
        id: &str,
        kind: &str,
        origin: &str,
        status: &str,
        bot: Option<&str>,
        created_at: &str,
    ) -> MemoryEntry {
        MemoryEntry {
            id: id.into(),
            kind: kind.into(),
            content: format!("fact {id}"),
            source: "chat".into(),
            created_at: created_at.into(),
            updated_at: created_at.into(),
            origin: origin.into(),
            status: status.into(),
            bot: bot.map(str::to_owned),
            importance: 0.75,
            last_recalled_at: None,
            recall_count: 0,
        }
    }

    #[test]
    fn memory_content_validation_checks_empty_and_length() {
        assert!(validate_memory_content("   \n ").is_err());
        assert!(validate_memory_content("Prefer concise replies").is_ok());
        let too_long = "a".repeat(MAX_MEMORY_CHARS + 1);
        assert!(validate_memory_content(&too_long).is_err());
    }

    #[test]
    fn memory_empty_copy_distinguishes_filtered_from_empty_vault() {
        let (title, _, filtered) = memory_empty_copy("all", "");
        assert_eq!(title, "No memories saved yet");
        assert!(!filtered);

        let (title, _, filtered) = memory_empty_copy("learned", "");
        assert_eq!(title, "No matching memories");
        assert!(filtered);

        let (title, _, filtered) = memory_empty_copy("all", "budget");
        assert_eq!(title, "No matching memories");
        assert!(filtered);

        let (title, _, filtered) =
            memory_empty_copy_with_facets("all", "", "preference", "all", "all");
        assert_eq!(title, "No matching memories");
        assert!(filtered);
    }

    #[test]
    fn summary_stats_and_facet_filters_work() {
        let entries = vec![
            sample_entry(
                "1",
                "preference",
                "explicit",
                "active",
                None,
                "2026-03-10T09:00:00Z",
            ),
            sample_entry(
                "2",
                "fact",
                "extracted",
                "active",
                Some("coder"),
                "2026-03-06T12:00:00Z",
            ),
            sample_entry(
                "3",
                "fact",
                "extracted",
                "pending",
                Some("coder"),
                "2026-02-01T12:00:00Z",
            ),
            sample_entry(
                "4",
                "decision",
                "explicit",
                "superseded",
                Some("writer"),
                "2026-03-09T12:00:00Z",
            ),
        ];

        let summary = compute_memory_summary(None, &entries, "2026-03-10", Some((3, 1)));
        assert_eq!(summary.active_facts, 3);
        assert_eq!(summary.saved_count, 2);
        assert_eq!(summary.learned_count, 2);
        assert_eq!(summary.bot_count, 2);
        assert_eq!(summary.top_bot_hint, "Most from @coder (2)");
        assert_eq!(summary.recent_7d, 3);
        assert_eq!(summary.flagged_total, 3);
        assert_eq!(summary.conflict_count, 1);
        assert_eq!(summary.pending_count, 1);

        let kinds = distinct_memory_kinds(&entries);
        assert_eq!(
            kinds,
            vec![
                ("decision".into(), 1),
                ("fact".into(), 2),
                ("preference".into(), 1)
            ]
        );
        let (bots, general) = distinct_memory_bots(&entries);
        assert_eq!(general, 1);
        assert_eq!(bots, vec![("coder".into(), 2), ("writer".into(), 1)]);

        assert!(matches_memory_facets(
            &entries[0],
            "preference",
            "__general__",
            "today",
            "2026-03-10"
        ));
        assert!(!matches_memory_facets(
            &entries[2],
            "all",
            "coder",
            "7d",
            "2026-03-10"
        ));
        assert!(matches_memory_facets(
            &entries[1],
            "fact",
            "coder",
            "7d",
            "2026-03-10"
        ));
    }
}
