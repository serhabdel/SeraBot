//! Per-user Projects workspace view (0.59, redesigned 0.74.1).
//!
//! Talks to `/v1/projects/*` off the UI thread: browse project cards, create a
//! project, upload/download/delete files, jump into Chat with a file's inbox
//! path pre-filled, inspect diagnostics, and edit text files cleanly in a
//! structured three-pane workspace.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use serde_json::Value;

use crate::ui::Icon;
use serabot_desktop::file_icons;
use serabot_desktop::projects as api;
use serabot_desktop::projects::{
    CheckStatus, DiagRow, DiagSummary, FileRow, ProjectRow, ProjectTreeEntry, build_project_tree,
    format_bytes, format_relative_stamp, is_editable_file, is_editable_name, latest_modified_stamp,
    parse_check_command, parse_diagnostics, parse_files, parse_projects, project_breadcrumbs,
    summarize_diagnostics,
};
use serabot_desktop::session::{Section, Session};
use serabot_desktop::worker::Credentials;
use serabot_desktop::workflows::now_secs;

async fn call<T, F>(session: Signal<Session>, job: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(Credentials) -> Result<T, String> + Send + 'static,
{
    let creds = session.read().credentials.clone();
    let Some(creds) = creds else {
        return Err("Not signed in.".into());
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Err(err) = std::thread::Builder::new()
        .name("serabot-projects".into())
        .spawn(move || {
            let _ = tx.send(job(creds));
        })
    {
        return Err(format!("Could not start request thread: {err}"));
    }
    rx.await
        .map_err(|_| "Request thread exited unexpectedly.".to_owned())?
}

fn fallback_file_icon(name: &str, is_dir: bool) -> &'static str {
    if is_dir {
        return "folder";
    }
    let lower = name.to_ascii_lowercase();
    if is_editable_name(&lower)
        || lower.ends_with(".lock")
        || lower.ends_with(".mdx")
        || lower.ends_with(".yml")
        || lower.ends_with(".yaml")
    {
        "code"
    } else if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
    {
        "image"
    } else {
        "paperclip"
    }
}

#[component]
fn ProjectFileIcon(name: String, is_dir: bool) -> Element {
    let icon_src = (!is_dir)
        .then(|| {
            name.rsplit_once('.')
                .and_then(|(_, ext)| file_icons::file_ext_image_src(ext))
        })
        .flatten();
    if let Some(src) = icon_src {
        rsx! {
            img {
                class: "prj-file-img",
                src: "{src}",
                alt: "",
                "aria-hidden": "true",
            }
        }
    } else {
        let icon = fallback_file_icon(&name, is_dir);
        rsx! {
            span {
                class: if is_dir { "prj-file-fallback dir" } else { "prj-file-fallback" },
                "aria-hidden": "true",
                Icon { name: icon, size: 14 }
            }
        }
    }
}

#[component]
fn CheckStatusPill(status: CheckStatus) -> Element {
    let cls = format!("prj-status-pill {}", status.css_modifier());
    let label = status.label();
    rsx! {
        span { class: "{cls}",
            span { class: "prj-status-dot", "aria-hidden": "true" }
            "{label}"
        }
    }
}

#[component]
pub fn ProjectsView(session: Signal<Session>) -> Element {
    let mut projects = use_signal(Vec::<ProjectRow>::new);
    let mut loaded = use_signal(|| false);
    let mut selected = use_signal(|| Option::<String>::None);
    let mut files = use_signal(Vec::<FileRow>::new);
    let mut new_name = use_signal(String::new);
    let mut subfolder = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);
    let mut notice = use_signal(|| Option::<String>::None);
    let mut confirm_delete = use_signal(|| Option::<String>::None);

    // 0.73+: Problems panel and per-project cached summaries for project cards.
    let mut diag_rows = use_signal(Vec::<DiagRow>::new);
    let mut diag_run = use_signal(|| Option::<Value>::None);
    let mut check_cmd = use_signal(String::new);
    let mut check_busy = use_signal(|| false);
    let mut diag_summaries = use_signal(BTreeMap::<String, DiagSummary>::new);
    let mut last_modified_map = use_signal(BTreeMap::<String, Option<String>>::new);

    // 0.74+: workspace editor state.
    let mut ws_path = use_signal(|| Option::<String>::None);
    let mut ws_text = use_signal(String::new);
    let mut ws_dirty = use_signal(|| false);
    let mut ws_status = use_signal(|| Option::<String>::None);
    let mut ws_jump_line = use_signal(|| Option::<u64>::None);

    let load_diags = move |slug: String| {
        let slug_diag = slug.clone();
        let slug_cmd = slug.clone();
        let slug_key = slug.clone();
        spawn(async move {
            if let Ok(value) =
                call(session, move |creds| api::diagnostics(&creds, &slug_diag)).await
            {
                let (run, rows) = parse_diagnostics(&value);
                let summary = summarize_diagnostics(run.as_ref(), &rows, false);
                diag_summaries
                    .write()
                    .insert(slug_key.clone(), summary.clone());
                if let Some(fin) = summary.finished_at {
                    last_modified_map
                        .write()
                        .entry(slug_key)
                        .or_insert(Some(fin));
                }
                diag_run.set(run);
                diag_rows.set(rows);
            }
            if let Ok(value) = call(session, move |creds| api::check_get(&creds, &slug_cmd)).await {
                let text = value["command"]
                    .as_array()
                    .map(|args| serde_json::to_string(args).unwrap_or_else(|_| "[]".into()))
                    .unwrap_or_default();
                check_cmd.set(text.clone());
                // The command field is uncontrolled (no value binding), so
                // typing never moves the caret; set the fetched text from JS
                // only while the field is not focused.
                if let Ok(js_text) = serde_json::to_string(&text) {
                    document::eval(&format!(
                        "(() => {{ const el = document.querySelector('.check-cmd-input'); if (el && document.activeElement !== el) el.value = {js_text}; }})()"
                    ));
                }
            }
        });
    };

    let run_check = move |_| {
        let Some(slug) = selected.read().clone() else {
            return;
        };
        check_busy.set(true);
        error.set(None);
        notice.set(None);
        let slug2 = slug.clone();
        let slug_sum = slug.clone();
        spawn(async move {
            let result = call(session, move |creds| api::check_run(&creds, &slug)).await;
            check_busy.set(false);
            match result {
                Ok(value) => {
                    if let Some(run) = value.get("run").cloned() {
                        let summary = summarize_diagnostics(Some(&run), &[], false);
                        diag_summaries.write().insert(slug_sum.clone(), summary);
                        diag_run.set(Some(run));
                    }
                    if let Ok(value) =
                        call(session, move |creds| api::diagnostics(&creds, &slug2)).await
                    {
                        let (run, rows) = parse_diagnostics(&value);
                        let summary = summarize_diagnostics(run.as_ref(), &rows, false);
                        diag_summaries.write().insert(slug_sum, summary);
                        diag_run.set(run);
                        diag_rows.set(rows);
                    }
                    notice.set(Some("Check finished.".into()));
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };

    let save_check = move |_| {
        let Some(slug) = selected.read().clone() else {
            return;
        };
        let text = check_cmd.read().clone();
        let command = parse_check_command(&text);
        if command.is_empty() {
            error.set(Some(
                "Enter a check command, e.g. [\"cargo\",\"check\"].".into(),
            ));
            return;
        }
        check_busy.set(true);
        error.set(None);
        spawn(async move {
            let result = call(session, move |creds| {
                api::check_put(&creds, &slug, &command)
            })
            .await;
            check_busy.set(false);
            match result {
                Ok(_) => notice.set(Some("Check command saved.".into())),
                Err(message) => error.set(Some(message)),
            }
        });
    };

    let mut load_files = move |slug: String| {
        let switched = selected.read().as_deref() != Some(slug.as_str());
        if switched {
            ws_path.set(None);
            ws_text.set(String::new());
            ws_dirty.set(false);
            ws_status.set(None);
            ws_jump_line.set(None);
            confirm_delete.set(None);
        }
        busy.set(true);
        error.set(None);
        let slug_req = slug.clone();
        let slug_diag = slug.clone();
        let slug_mod = slug.clone();
        spawn(async move {
            let result = call(session, move |creds| api::list_files(&creds, &slug_req)).await;
            busy.set(false);
            match result {
                Ok(value) => {
                    let parsed = parse_files(&value);
                    let latest = latest_modified_stamp(&parsed);
                    last_modified_map.write().insert(slug_mod, latest);
                    selected.set(Some(slug));
                    files.set(parsed);
                    load_diags(slug_diag);
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };

    let mut refresh = move || {
        busy.set(true);
        error.set(None);
        spawn(async move {
            let result = call(session, |creds| api::list(&creds)).await;
            busy.set(false);
            loaded.set(true);
            match result {
                Ok(value) => {
                    let list = parse_projects(&value);
                    let current = selected.read().clone();
                    let next = current
                        .filter(|s| list.iter().any(|p| &p.slug == s))
                        .or_else(|| list.first().map(|p| p.slug.clone()));
                    let slugs_to_prefetch: Vec<String> = list
                        .iter()
                        .map(|p| p.slug.clone())
                        .filter(|s| next.as_deref() != Some(s.as_str()))
                        .take(8)
                        .collect();
                    projects.set(list);
                    if let Some(slug) = next {
                        load_files(slug);
                    } else {
                        selected.set(None);
                        files.set(Vec::new());
                        diag_run.set(None);
                        diag_rows.set(Vec::new());
                        ws_path.set(None);
                    }
                    // Prefetch last check status pills for other cards without inventing new server endpoints.
                    for slug in slugs_to_prefetch {
                        let slug_req = slug.clone();
                        spawn(async move {
                            if let Ok(val) =
                                call(session, move |creds| api::diagnostics(&creds, &slug_req))
                                    .await
                            {
                                let (run, rows) = parse_diagnostics(&val);
                                let summary = summarize_diagnostics(run.as_ref(), &rows, false);
                                diag_summaries.write().insert(slug.clone(), summary.clone());
                                if let Some(fin) = summary.finished_at {
                                    last_modified_map.write().entry(slug).or_insert(Some(fin));
                                }
                            }
                        });
                    }
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };

    use_hook(refresh);

    let mut create_project_action = move || {
        let name = new_name.read().trim().to_owned();
        if name.is_empty() {
            error.set(Some(
                "Enter a project name before creating a folder.".into(),
            ));
            return;
        }
        busy.set(true);
        error.set(None);
        notice.set(None);
        spawn(async move {
            let result = call(session, move |creds| api::create(&creds, &name)).await;
            busy.set(false);
            match result {
                Ok(value) => {
                    new_name.set(String::new());
                    if let Some(slug) = value["project"].as_str().map(str::to_owned) {
                        notice.set(Some(format!("Project \"{slug}\" ready.")));
                        selected.set(Some(slug));
                    }
                    refresh();
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };

    let create_project = move |_| create_project_action();

    let upload_file = move |_| {
        let Some(slug) = selected.read().clone() else {
            return;
        };
        let sub = subfolder.read().trim().to_owned();
        #[cfg(windows)]
        {
            let picked = rfd::FileDialog::new()
                .set_title("Upload file to project")
                .pick_file();
            let Some(path) = picked else {
                return;
            };
            let Some(filename) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned)
            else {
                error.set(Some("Invalid filename.".into()));
                return;
            };
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    error.set(Some(format!("Could not read file: {e}")));
                    return;
                }
            };
            busy.set(true);
            error.set(None);
            notice.set(None);
            spawn(async move {
                let sub_opt = (!sub.is_empty()).then_some(sub);
                let result = call(session, move |creds| {
                    api::upload(&creds, &slug, &filename, sub_opt.as_deref(), &bytes)
                })
                .await;
                busy.set(false);
                match result {
                    Ok(_) => {
                        notice.set(Some("File uploaded to project.".into()));
                        refresh();
                    }
                    Err(message) => error.set(Some(message)),
                }
            });
        }
        #[cfg(not(windows))]
        {
            let _ = (slug, sub);
            notice.set(Some(
                "File upload dialog is available in the Windows desktop app.".into(),
            ));
        }
    };

    let mut open_file = move |slug: String, path: String, line: Option<u64>| {
        ws_status.set(None);
        let s = slug.clone();
        let p2 = path.clone();
        spawn(async move {
            let result = call(session, move |creds| api::download(&creds, &s, &p2)).await;
            match result {
                Ok(bytes) => {
                    ws_text.set(String::from_utf8_lossy(&bytes).into_owned());
                    ws_path.set(Some(path));
                    ws_dirty.set(false);
                    ws_jump_line.set(line);
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };

    let save_file = move |_| {
        let (Some(slug), Some(path)) = (selected.read().clone(), ws_path.read().clone()) else {
            return;
        };
        let content = ws_text.read().clone();
        ws_status.set(Some("Saving…".into()));
        spawn(async move {
            let result = call(session, move |creds| {
                api::write_file(&creds, &slug, &path, &content)
            })
            .await;
            match result {
                Ok(_) => {
                    ws_dirty.set(false);
                    ws_status.set(Some("Saved.".into()));
                    refresh();
                }
                Err(message) => {
                    ws_status.set(Some(format!("Save failed: {message}")));
                    error.set(Some(message));
                }
            }
        });
    };

    let project_list = projects.read().clone();
    let is_loaded = *loaded.read();
    let file_list = files.read().clone();
    let active_slug = selected.read().clone();
    let is_busy = *busy.read();
    let err = error.read().clone();
    let note = notice.read().clone();
    let del_target = confirm_delete.read().clone();
    let diag_list = diag_rows.read().clone();
    let run_info = diag_run.read().clone();
    let check_text = check_cmd.read().clone();
    let checking = *check_busy.read();
    let summaries_snapshot = diag_summaries.read().clone();
    let modified_snapshot = last_modified_map.read().clone();

    let ws_open = ws_path.read().clone();
    let ws_text_value = ws_text.read().clone();
    let ws_dirty_flag = *ws_dirty.read();
    let ws_status_text = ws_status.read().clone();
    let ws_line_count = ws_text_value.lines().count().max(1);
    let ws_jump = ws_jump_line
        .read()
        .as_ref()
        .map(|l| l.to_string())
        .unwrap_or_default();

    let now = now_secs();
    let tree_entries: Vec<ProjectTreeEntry> = build_project_tree(&file_list);
    let breadcrumbs = project_breadcrumbs(active_slug.as_deref(), ws_open.as_deref());
    let active_summary = summarize_diagnostics(run_info.as_ref(), &diag_list, checking);
    let active_project_meta = active_slug
        .as_deref()
        .and_then(|s| project_list.iter().find(|p| p.slug == s));
    let active_total_bytes = active_project_meta.map(|p| p.total_bytes).unwrap_or(0);
    let active_modified_label = active_slug
        .as_deref()
        .and_then(|s| modified_snapshot.get(s).and_then(|v| v.as_deref()))
        .or_else(|| active_summary.finished_at.as_deref())
        .map(|raw| format_relative_stamp(raw, now));

    rsx! {
        div { class: "pane projects-page",
            div { class: "pane-head prj-hero",
                div { class: "prj-hero-copy",
                    h1 { class: "pane-title", "Projects" }
                    p { class: "muted",
                        "Private server project folders with an integrated file tree, text editor and check diagnostics."
                    }
                }
                div { class: "pane-actions prj-head-actions",
                    if active_slug.is_some() {
                        button {
                            r#type: "button",
                            class: "btn primary sm",
                            disabled: is_busy || checking,
                            onclick: run_check,
                            Icon { name: "refresh", size: 13 }
                            if checking { "Checking…" } else { "Run checks" }
                        }
                    }
                    button {
                        r#type: "button",
                        class: "btn ghost sm",
                        disabled: is_busy,
                        "aria-label": "Refresh projects",
                        onclick: move |_| refresh(),
                        Icon { name: "refresh", size: 14 }
                        "Refresh"
                    }
                }
            }

            div { class: "pane-body",
                if let Some(msg) = err {
                    div { class: "banner err prj-banner", role: "alert",
                        span { "{msg}" }
                        button {
                            r#type: "button",
                            class: "btn ghost sm",
                            onclick: move |_| error.set(None),
                            "Dismiss"
                        }
                    }
                }
                if let Some(msg) = note {
                    div { class: "banner info prj-banner", role: "status",
                        span { "{msg}" }
                        button {
                            r#type: "button",
                            class: "btn ghost sm",
                            onclick: move |_| notice.set(None),
                            "Dismiss"
                        }
                    }
                }

                // Create project bar
                div { class: "card prj-create-card",
                    div { class: "prj-create-form",
                        label { class: "prj-create-field",
                            span { class: "field-label", "New project folder" }
                            input {
                                r#type: "text",
                                class: "input",
                                placeholder: "e.g. q4-invoices or backend-service",
                                "aria-label": "New project name",
                                value: "{new_name}",
                                onkeydown: move |e| {
                                    if e.key() == Key::Enter {
                                        create_project_action();
                                    }
                                },
                                oninput: move |e| {
                                    error.set(None);
                                    new_name.set(e.value());
                                },
                            }
                        }
                        div { class: "prj-create-action",
                            button {
                                r#type: "button",
                                class: "btn primary sm",
                                disabled: is_busy || new_name.read().trim().is_empty(),
                                onclick: create_project,
                                Icon { name: "plus", size: 14 }
                                "Create project"
                            }
                        }
                    }
                }

                // Project cards section
                if !is_loaded && project_list.is_empty() {
                    div { class: "prj-cards-grid", role: "status", "aria-label": "Loading projects",
                        for n in 0..3 {
                            div { key: "{n}", class: "skeleton prj-skeleton-card" }
                        }
                    }
                } else if project_list.is_empty() {
                    div { class: "empty compact prj-empty", role: "status",
                        span { class: "empty-icon", Icon { name: "folder", size: 22 } }
                        h3 { class: "empty-title", "No projects yet" }
                        p { class: "empty-text",
                            "Create a project folder above or ask any bot in Chat to file attachments into a project."
                        }
                    }
                } else {
                    section { class: "prj-cards-section", "aria-label": "Projects",
                        div { class: "prj-section-head",
                            h2 { class: "prj-section-title", "Project folders" }
                            span { class: "prj-count-pill", "{project_list.len()}" }
                        }
                        div { class: "prj-cards-grid", role: "list",
                            for proj in project_list.clone() {
                                {
                                    let slug = proj.slug.clone();
                                    let is_active = active_slug.as_deref() == Some(slug.as_str());
                                    let size_str = format_bytes(proj.total_bytes);
                                    let click_slug = slug.clone();
                                    let card_summary = if is_active {
                                        active_summary.clone()
                                    } else {
                                        summaries_snapshot.get(&slug).cloned().unwrap_or(DiagSummary {
                                            status: CheckStatus::Unchecked,
                                            errors: 0,
                                            warnings: 0,
                                            notes: 0,
                                            finished_at: None,
                                            error_message: None,
                                        })
                                    };
                                    let mod_label = modified_snapshot
                                        .get(&slug)
                                        .and_then(|v| v.as_deref())
                                        .or(card_summary.finished_at.as_deref())
                                        .map(|raw| format_relative_stamp(raw, now))
                                        .unwrap_or_else(|| "Open to inspect".to_owned());
                                    let folder_hint = format!("projects/{slug}");
                                    let file_word = if proj.file_count == 1 { "file" } else { "files" };
                                    rsx! {
                                        button {
                                            key: "{slug}",
                                            r#type: "button",
                                            role: "listitem",
                                            class: if is_active { "card prj-card active" } else { "card prj-card" },
                                            "aria-pressed": if is_active { "true" } else { "false" },
                                            onclick: move |_| load_files(click_slug.clone()),
                                            div { class: "prj-card-head",
                                                div { class: "prj-card-title-wrap",
                                                    span { class: "prj-card-folder-icon", Icon { name: "folder", size: 15 } }
                                                    div { class: "prj-card-titles",
                                                        span { class: "prj-card-name", "{slug}" }
                                                        span { class: "prj-card-folder mono", "{folder_hint}" }
                                                    }
                                                }
                                                CheckStatusPill { status: card_summary.status }
                                            }
                                            div { class: "prj-card-meta",
                                                span { "{proj.file_count} {file_word}" }
                                                span { "·" }
                                                span { "{size_str}" }
                                                span { "·" }
                                                span { "{mod_label}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Active project workspace
                if let Some(ws_slug) = active_slug.clone() {
                    div { class: "prj-workspace-shell",
                        // Breadcrumbs & quick project summary bar
                        nav { class: "prj-breadcrumbs-bar", "aria-label": "Project path",
                            ol { class: "prj-breadcrumbs",
                                for (idx, crumb) in breadcrumbs.iter().enumerate() {
                                    {
                                        let is_last = idx + 1 == breadcrumbs.len();
                                        let is_project_crumb = idx == 1;
                                        let slug_jump = ws_slug.clone();
                                        rsx! {
                                            li { key: "{idx}-{crumb}", class: "prj-crumb-item",
                                                if idx > 0 {
                                                    span { class: "prj-crumb-sep", "aria-hidden": "true",
                                                        Icon { name: "chevron-right", size: 12 }
                                                    }
                                                }
                                                if is_project_crumb && ws_open.is_some() {
                                                    button {
                                                        r#type: "button",
                                                        class: "prj-crumb-btn",
                                                        onclick: move |_| {
                                                            let _ = &slug_jump;
                                                            ws_path.set(None);
                                                            ws_dirty.set(false);
                                                            ws_status.set(None);
                                                            ws_jump_line.set(None);
                                                        },
                                                        "{crumb}"
                                                    }
                                                } else {
                                                    span {
                                                        class: if is_last { "prj-crumb current" } else { "prj-crumb" },
                                                        "{crumb}"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            div { class: "prj-ws-summary",
                                span { class: "chip mono", "{format_bytes(active_total_bytes)}" }
                                if let Some(mod_str) = active_modified_label.as_ref() {
                                    span { class: "prj-ws-mod", "Updated {mod_str}" }
                                }
                                CheckStatusPill { status: active_summary.status }
                            }
                        }

                        // Three-pane workspace grid: File Tree (left) | Editor (center) | Problems (right)
                        div { class: "prj-workspace",
                            // Left: File tree panel (sticky header + resizable width on desktop)
                            aside { class: "card prj-tree-panel", "aria-label": "Project files",
                                div { class: "prj-panel-head prj-tree-head",
                                    div { class: "prj-panel-title-row",
                                        span { class: "prj-panel-title", "Files" }
                                        span { class: "prj-count-pill", "{file_list.len()}" }
                                    }
                                    div { class: "prj-upload-bar",
                                        input {
                                            r#type: "text",
                                            class: "input prj-subfolder-input",
                                            placeholder: "Subfolder (optional)",
                                            "aria-label": "Upload subfolder",
                                            value: "{subfolder}",
                                            oninput: move |e| subfolder.set(e.value()),
                                        }
                                        button {
                                            r#type: "button",
                                            class: "btn sm",
                                            disabled: is_busy,
                                            title: "Upload file into this project",
                                            onclick: upload_file,
                                            Icon { name: "paperclip", size: 13 }
                                            "Upload"
                                        }
                                    }
                                }
                                div { class: "prj-tree-scroll", role: "tree", "aria-label": "Project file tree",
                                    if tree_entries.is_empty() {
                                        div { class: "empty compact prj-tree-empty",
                                            span { class: "empty-icon", Icon { name: "folder", size: 18 } }
                                            h3 { class: "empty-title", "Folder is empty" }
                                            p { class: "empty-text", "Upload a file or ask a bot to file an attachment here." }
                                        }
                                    } else {
                                        for entry in tree_entries {
                                            if entry.is_dir {
                                                {
                                                    let pad = 10 + entry.depth * 14;
                                                    rsx! {
                                                        div {
                                                            key: "{entry.key}",
                                                            class: "prj-tree-dir",
                                                            style: "padding-left:{pad}px;",
                                                            role: "treeitem",
                                                            ProjectFileIcon { name: entry.label.clone(), is_dir: true }
                                                            span { class: "prj-tree-dir-name", "{entry.label}" }
                                                        }
                                                    }
                                                }
                                            } else if let Some(file) = entry.file {
                                                {
                                                    let pad = 10 + entry.depth * 14;
                                                    let editable = is_editable_file(&file.mime, &file.name);
                                                    let is_open = ws_open.as_deref() == Some(file.path.as_str());
                                                    let confirming = del_target.as_deref() == Some(file.path.as_str());
                                                    let size_str = format_bytes(file.size_bytes);
                                                    let p_open = file.path.clone();
                                                    let s_open = ws_slug.clone();
                                                    let inbox_for_chat = file.inbox_path.clone();
                                                    let path_for_dl = file.path.clone();
                                                    let name_for_dl = file.name.clone();
                                                    let slug_for_dl = ws_slug.clone();
                                                    let path_for_del = file.path.clone();
                                                    let slug_for_del = ws_slug.clone();
                                                    rsx! {
                                                        div {
                                                            key: "{entry.key}",
                                                            class: if is_open { "prj-tree-row active" } else { "prj-tree-row" },
                                                            role: "treeitem",
                                                            "aria-selected": if is_open { "true" } else { "false" },
                                                            button {
                                                                r#type: "button",
                                                                class: if is_open {
                                                                    "ws-tree-item prj-tree-btn active"
                                                                } else if editable {
                                                                    "ws-tree-item prj-tree-btn"
                                                                } else {
                                                                    "ws-tree-item prj-tree-btn dim"
                                                                },
                                                                style: "padding-left:{pad}px;",
                                                                disabled: !editable,
                                                                title: if editable {
                                                                    format!("{} ({size_str}) — click to edit", file.path)
                                                                } else {
                                                                    format!("{} ({size_str}) — binary file", file.path)
                                                                },
                                                                onclick: move |_| {
                                                                    if editable {
                                                                        open_file(s_open.clone(), p_open.clone(), None);
                                                                    }
                                                                },
                                                                ProjectFileIcon { name: file.name.clone(), is_dir: false }
                                                                span { class: "prj-tree-label", "{entry.label}" }
                                                                span { class: "prj-tree-size", "{size_str}" }
                                                            }
                                                            div { class: "prj-tree-actions",
                                                                button {
                                                                    r#type: "button",
                                                                    class: "btn ghost sm prj-mini-btn",
                                                                    title: "Ask in chat ({file.inbox_path})",
                                                                    "aria-label": "Ask about {file.name} in chat",
                                                                    onclick: move |_| {
                                                                        let mut s = session.write();
                                                                        s.composer = format!("Please review `{}`: ", inbox_for_chat);
                                                                        s.open_section(Section::Chat);
                                                                    },
                                                                    Icon { name: "chat", size: 12 }
                                                                }
                                                                button {
                                                                    r#type: "button",
                                                                    class: "btn ghost sm prj-mini-btn",
                                                                    disabled: is_busy,
                                                                    title: "Save copy to computer",
                                                                    "aria-label": "Save {file.name}",
                                                                    onclick: move |_| {
                                                                        let slug_c = slug_for_dl.clone();
                                                                        let rel_c = path_for_dl.clone();
                                                                        let fname = name_for_dl.clone();
                                                                        #[cfg(windows)]
                                                                        {
                                                                            let save = rfd::FileDialog::new()
                                                                                .set_file_name(&fname)
                                                                                .save_file();
                                                                            let Some(dest) = save else { return; };
                                                                            busy.set(true);
                                                                            spawn(async move {
                                                                                let res = call(session, move |creds| api::download(creds, &slug_c, &rel_c)).await;
                                                                                busy.set(false);
                                                                                match res {
                                                                                    Ok(bytes) => {
                                                                                        if let Err(e) = std::fs::write(&dest, bytes) {
                                                                                            error.set(Some(format!("Save failed: {e}")));
                                                                                        } else {
                                                                                            notice.set(Some(format!("Saved {fname}.")));
                                                                                        }
                                                                                    }
                                                                                    Err(e) => error.set(Some(e)),
                                                                                }
                                                                            });
                                                                        }
                                                                        #[cfg(not(windows))]
                                                                        {
                                                                            let _ = (slug_c, rel_c, fname);
                                                                            notice.set(Some("File save dialog is available in the Windows desktop app.".into()));
                                                                        }
                                                                    },
                                                                    Icon { name: "download", size: 12 }
                                                                }
                                                                if confirming {
                                                                    button {
                                                                        r#type: "button",
                                                                        class: "btn danger sm prj-del-confirm",
                                                                        disabled: is_busy,
                                                                        onclick: move |_| {
                                                                            confirm_delete.set(None);
                                                                            let s_del = slug_for_del.clone();
                                                                            let p_del = path_for_del.clone();
                                                                            busy.set(true);
                                                                            spawn(async move {
                                                                                let res = call(session, move |creds| api::delete_file(creds, &s_del, &p_del)).await;
                                                                                busy.set(false);
                                                                                match res {
                                                                                    Ok(_) => {
                                                                                        if ws_path.read().as_deref() == Some(p_del.as_str()) {
                                                                                            ws_path.set(None);
                                                                                            ws_text.set(String::new());
                                                                                            ws_dirty.set(false);
                                                                                        }
                                                                                        notice.set(Some("File deleted.".into()));
                                                                                        refresh();
                                                                                    }
                                                                                    Err(e) => error.set(Some(e)),
                                                                                }
                                                                            });
                                                                        },
                                                                        "Confirm"
                                                                    }
                                                                    button {
                                                                        r#type: "button",
                                                                        class: "btn ghost sm prj-mini-btn",
                                                                        "aria-label": "Cancel delete",
                                                                        onclick: move |_| confirm_delete.set(None),
                                                                        Icon { name: "x", size: 12 }
                                                                    }
                                                                } else {
                                                                    button {
                                                                        r#type: "button",
                                                                        class: "btn ghost sm prj-mini-btn",
                                                                        title: "Delete file",
                                                                        "aria-label": "Delete {file.name}",
                                                                        onclick: move |_| confirm_delete.set(Some(path_for_del.clone())),
                                                                        Icon { name: "trash", size: 12 }
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
                            }

                            // Center: Tab-like editor panel
                            section { class: "card ws-editor prj-editor-panel", "aria-label": "File editor",
                                if let Some(path) = ws_open.clone() {
                                    {
                                        let tab_name = path.rsplit('/').next().unwrap_or(&path).to_owned();
                                        let open_file_row = file_list.iter().find(|f| f.path == path);
                                        let open_inbox = open_file_row.map(|f| f.inbox_path.clone());
                                        rsx! {
                                            div { class: "ws-editor-head prj-editor-head",
                                                div { class: "prj-editor-tab-wrap",
                                                    div { class: "prj-editor-tab",
                                                        ProjectFileIcon { name: tab_name, is_dir: false }
                                                        span { class: "prj-editor-path mono", title: "{path}", "{path}" }
                                                        span {
                                                            class: if ws_dirty_flag { "prj-dirty-badge dirty" } else { "prj-dirty-badge clean" },
                                                            if ws_dirty_flag {
                                                                span { class: "prj-dirty-dot", "aria-hidden": "true" }
                                                                "Unsaved"
                                                            } else {
                                                                "Saved"
                                                            }
                                                        }
                                                    }
                                                }
                                                div { class: "prj-editor-actions",
                                                    if let Some(status) = ws_status_text.as_ref() {
                                                        span { class: "prj-save-status", "{status}" }
                                                    }
                                                    if let Some(inbox) = open_inbox {
                                                        button {
                                                            r#type: "button",
                                                            class: "btn ghost sm",
                                                            title: "Ask about this file in Chat",
                                                            onclick: move |_| {
                                                                let mut s = session.write();
                                                                s.composer = format!("Please review `{}`: ", inbox);
                                                                s.open_section(Section::Chat);
                                                            },
                                                            Icon { name: "chat", size: 13 }
                                                            "Ask in chat"
                                                        }
                                                    }
                                                    button {
                                                        r#type: "button",
                                                        class: "btn primary sm",
                                                        disabled: !ws_dirty_flag || is_busy,
                                                        onclick: save_file,
                                                        Icon { name: "check", size: 13 }
                                                        "Save"
                                                    }
                                                }
                                            }
                                            div { class: "ws-editor-body prj-editor-body",
                                                div { class: "ws-gutter", "aria-hidden": "true",
                                                    for n in 1..=ws_line_count {
                                                        div { key: "{n}", "{n}" }
                                                    }
                                                }
                                                textarea {
                                                    key: "{path}",
                                                    class: "ws-textarea",
                                                    spellcheck: false,
                                                    "aria-label": "Edit {path}",
                                                    "data-ws-init": "{ws_text_value}",
                                                    "data-ws-line": "{ws_jump}",
                                                    onmounted: move |_| {
                                                        document::eval(
                                                            "(() => { const ta = document.querySelector('.ws-textarea[data-ws-init]'); if (!ta) return; ta.value = ta.getAttribute('data-ws-init') || ''; const line = Number(ta.getAttribute('data-ws-line') || 0); const g = document.querySelector('.ws-gutter'); if (line > 0) { const lines = ta.value.split('\\n'); let off = 0; for (let i = 0; i < line - 1 && i < lines.length; i++) off += lines[i].length + 1; ta.focus(); ta.selectionStart = ta.selectionEnd = off; const lh = parseFloat(getComputedStyle(ta).lineHeight) || 21; ta.scrollTop = Math.max(0, (line - 1) * lh - ta.clientHeight / 3); if (g) g.scrollTop = ta.scrollTop; } })()",
                                                        );
                                                    },
                                                    onscroll: move |_| {
                                                        document::eval(
                                                            "(() => { const ta = document.querySelector('.ws-textarea'); const g = document.querySelector('.ws-gutter'); if (ta && g) g.scrollTop = ta.scrollTop; })()",
                                                        );
                                                    },
                                                    oninput: move |e| {
                                                        ws_text.set(e.value());
                                                        ws_dirty.set(true);
                                                        ws_status.set(None);
                                                    },
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    div { class: "empty prj-editor-empty",
                                        span { class: "empty-icon", Icon { name: "code", size: 22 } }
                                        h3 { class: "empty-title", "Select a file to inspect or edit" }
                                        p { class: "empty-text",
                                            "Pick any text or code file from the tree on the left, or click a diagnostic on the right to jump straight to its line."
                                        }
                                    }
                                }
                            }

                            // Right/bottom: Problems & Check Runner panel
                            aside { class: "card prj-problems-panel", "aria-label": "Problems and checks",
                                div { class: "prj-panel-head prj-problems-head",
                                    div { class: "prj-panel-title-row",
                                        h2 { class: "prj-panel-title", "Problems" }
                                        div { class: "prj-sev-badges",
                                            span {
                                                class: if active_summary.errors > 0 { "prj-sev-badge err" } else { "prj-sev-badge" },
                                                title: "Errors",
                                                Icon { name: "alert", size: 12 }
                                                "{active_summary.errors}"
                                            }
                                            span {
                                                class: if active_summary.warnings > 0 { "prj-sev-badge warn" } else { "prj-sev-badge" },
                                                title: "Warnings",
                                                Icon { name: "alert-circle", size: 12 }
                                                "{active_summary.warnings}"
                                            }
                                            if active_summary.notes > 0 {
                                                span {
                                                    class: "prj-sev-badge note",
                                                    title: "Notes",
                                                    Icon { name: "code", size: 12 }
                                                    "{active_summary.notes}"
                                                }
                                            }
                                        }
                                    }
                                    div { class: "prj-problems-actions",
                                        CheckStatusPill { status: active_summary.status }
                                        button {
                                            r#type: "button",
                                            class: "btn primary sm",
                                            disabled: is_busy || checking,
                                            onclick: run_check,
                                            if checking {
                                                span { class: "spinner", "aria-hidden": "true" }
                                                "Running…"
                                            } else {
                                                Icon { name: "refresh", size: 13 }
                                                "Run checks"
                                            }
                                        }
                                    }
                                }

                                div { class: "prj-check-config",
                                    label { class: "prj-check-field",
                                        span { class: "prj-check-label", "Check command (argv JSON or words)" }
                                        div { class: "prj-check-row",
                                            input {
                                                r#type: "text",
                                                class: "input check-cmd-input",
                                                placeholder: "[\"cargo\",\"check\",\"--message-format=json\"]",
                                                "aria-label": "Check command",
                                                "data-check-init": "{check_text}",
                                                onmounted: move |_| {
                                                    document::eval(
                                                        "(() => { const el = document.querySelector('.check-cmd-input[data-check-init]'); if (!el) return; el.value = el.getAttribute('data-check-init') || ''; })()",
                                                    );
                                                },
                                                oninput: move |e| check_cmd.set(e.value()),
                                            }
                                            button {
                                                r#type: "button",
                                                class: "btn sm",
                                                disabled: checking,
                                                onclick: save_check,
                                                "Save"
                                            }
                                        }
                                    }
                                    if let Some(err_msg) = active_summary.error_message.as_ref() {
                                        p { class: "prj-check-err", role: "alert", "{err_msg}" }
                                    }
                                }

                                div { class: "prj-diag-scroll",
                                    if checking {
                                        div { class: "prj-diag-progress", role: "status",
                                            span { class: "spinner", "aria-hidden": "true" }
                                            span { "Running project check…" }
                                        }
                                    } else if diag_list.is_empty() {
                                        div { class: "empty compact prj-diag-empty",
                                            span { class: "empty-icon",
                                                Icon {
                                                    name: if run_info.is_some() { "check" } else { "shield" },
                                                    size: 18,
                                                }
                                            }
                                            h3 { class: "empty-title",
                                                if run_info.is_some() {
                                                    "No problems found"
                                                } else {
                                                    "Not checked yet"
                                                }
                                            }
                                            p { class: "empty-text",
                                                if run_info.is_some() {
                                                    "The last check completed with zero errors or warnings."
                                                } else {
                                                    "Configure a check command above and run checks to inspect diagnostics."
                                                }
                                            }
                                        }
                                    } else {
                                        div { class: "diag-list prj-diag-list", role: "list",
                                            for d in diag_list {
                                                {
                                                    let key = format!(
                                                        "{}:{}:{}:{}",
                                                        d.file,
                                                        d.line.unwrap_or(0),
                                                        d.column.unwrap_or(0),
                                                        d.message.chars().take(48).collect::<String>()
                                                    );
                                                    let loc = match (d.line, d.column) {
                                                        (Some(l), Some(c)) => format!("{}:{}:{}", d.file, l, c),
                                                        (Some(l), None) => format!("{}:{}", d.file, l),
                                                        _ => d.file.clone(),
                                                    };
                                                    let (icon, sev_class) = match d.severity.as_str() {
                                                        "error" => ("alert", "diag-sev error"),
                                                        "warning" => ("alert-circle", "diag-sev warning"),
                                                        _ => ("code", "diag-sev note"),
                                                    };
                                                    let d_file = d.file.clone();
                                                    let d_line = d.line;
                                                    let d_editable = is_editable_name(&d_file);
                                                    let d_slug = ws_slug.clone();
                                                    rsx! {
                                                        button {
                                                            key: "{key}",
                                                            r#type: "button",
                                                            role: "listitem",
                                                            class: if d_editable { "diag-row prj-diag-btn clickable" } else { "diag-row prj-diag-btn" },
                                                            disabled: !d_editable,
                                                            title: if d_editable { "Open in workspace editor" } else { "Not an editable text file" },
                                                            onclick: move |_| {
                                                                if d_editable {
                                                                    open_file(d_slug.clone(), d_file.clone(), d_line);
                                                                }
                                                            },
                                                            span { class: "{sev_class}", Icon { name: icon, size: 14 } }
                                                            div { class: "diag-copy",
                                                                div { class: "diag-loc mono", "{loc}" }
                                                                div { class: "diag-msg", "{d.message}" }
                                                            }
                                                            if let Some(code) = d.code.clone() {
                                                                span { class: "chip mono", "{code}" }
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
                    }
                }
            }
        }
    }
}
