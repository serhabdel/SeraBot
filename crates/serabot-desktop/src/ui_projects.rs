//! Per-user Projects workspace view (0.59).
//!
//! Talks to `/v1/projects/*` off the UI thread: browse project folders, create
//! a project, upload files directly into a project, download or delete files,
//! and jump straight into Chat with a file's inbox path pre-filled.

use dioxus::prelude::*;
use serde_json::Value;

use crate::ui::Icon;
use serabot_desktop::projects as api;
use serabot_desktop::session::{Section, Session};
use serabot_desktop::worker::Credentials;

async fn call<T: Send + 'static>(
    session: Signal<Session>,
    job: impl FnOnce(&Credentials) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let Some(credentials) = session.read().credentials.clone() else {
        return Err("Not connected to the server.".into());
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("serabot-projects".into())
        .spawn(move || {
            let _ = tx.send(job(&credentials));
        });
    if spawned.is_err() {
        return Err("Could not start the request.".into());
    }
    rx.await
        .unwrap_or_else(|_| Err("The request was interrupted.".into()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectRow {
    slug: String,
    file_count: usize,
    total_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileRow {
    name: String,
    path: String,
    inbox_path: String,
    size_bytes: u64,
    mime: String,
}

fn parse_projects(value: &Value) -> Vec<ProjectRow> {
    value["projects"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(ProjectRow {
                        slug: item["slug"].as_str()?.to_owned(),
                        file_count: item["file_count"].as_u64().unwrap_or(0) as usize,
                        total_bytes: item["total_bytes"].as_u64().unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_files(value: &Value) -> Vec<FileRow> {
    value["files"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(FileRow {
                        name: item["name"].as_str()?.to_owned(),
                        path: item["path"].as_str()?.to_owned(),
                        inbox_path: item["inbox_path"].as_str()?.to_owned(),
                        size_bytes: item["size_bytes"].as_u64().unwrap_or(0),
                        mime: item["mime"].as_str().unwrap_or("").to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiagRow {
    file: String,
    line: Option<u64>,
    column: Option<u64>,
    severity: String,
    code: Option<String>,
    message: String,
}

/// Split `GET /v1/projects/{slug}/diagnostics` into the run summary and rows.
fn parse_diagnostics(value: &Value) -> (Option<Value>, Vec<DiagRow>) {
    let run = value.get("run").cloned();
    let rows = value["diagnostics"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| DiagRow {
                    file: item["file"].as_str().unwrap_or("").to_owned(),
                    line: item["line"].as_u64(),
                    column: item["column"].as_u64(),
                    severity: item["severity"].as_str().unwrap_or("note").to_owned(),
                    code: item["code"].as_str().map(str::to_owned),
                    message: item["message"].as_str().unwrap_or("").to_owned(),
                })
                .collect()
        })
        .unwrap_or_default();
    (run, rows)
}

/// Whether a project file opens in the workspace editor (0.74).
fn is_editable_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        ".rs", ".toml", ".md", ".txt", ".json", ".csv", ".yaml", ".yml", ".py", ".js", ".ts",
        ".tsx", ".jsx", ".html", ".css", ".xml", ".svg", ".log", ".sh", ".bat", ".sql", ".env",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

fn is_editable_file(mime: &str, name: &str) -> bool {
    mime.starts_with("text/")
        || mime.contains("json")
        || mime.contains("xml")
        || is_editable_name(name)
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[component]
pub fn ProjectsView(session: Signal<Session>) -> Element {
    let mut projects = use_signal(Vec::<ProjectRow>::new);
    let mut selected = use_signal(|| Option::<String>::None);
    let mut files = use_signal(Vec::<FileRow>::new);
    let mut new_name = use_signal(String::new);
    let mut subfolder = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);
    let mut notice = use_signal(|| Option::<String>::None);
    let mut confirm_delete = use_signal(|| Option::<String>::None);
    // 0.73: project check harness — Problems panel state.
    let mut diag_rows = use_signal(Vec::<DiagRow>::new);
    let mut diag_run = use_signal(|| Option::<Value>::None);
    let mut check_cmd = use_signal(String::new);
    let mut check_busy = use_signal(|| false);

    let load_diags = move |slug: String| {
        let slug_diag = slug.clone();
        let slug_cmd = slug.clone();
        spawn(async move {
            if let Ok(value) = call(session, move |creds| api::diagnostics(creds, &slug_diag)).await
            {
                let (run, rows) = parse_diagnostics(&value);
                diag_run.set(run);
                diag_rows.set(rows);
            }
            if let Ok(value) = call(session, move |creds| api::check_get(creds, &slug_cmd)).await {
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
        let slug2 = slug.clone();
        spawn(async move {
            let result = call(session, move |creds| api::check_run(creds, &slug)).await;
            check_busy.set(false);
            match result {
                Ok(value) => {
                    if let Some(run) = value.get("run").cloned() {
                        diag_run.set(Some(run));
                    }
                    if let Ok(value) =
                        call(session, move |creds| api::diagnostics(creds, &slug2)).await
                    {
                        let (run, rows) = parse_diagnostics(&value);
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
        let text = check_cmd.read().trim().to_owned();
        let command: Vec<String> = serde_json::from_str(&text)
            .ok()
            .filter(|parts: &Vec<String>| !parts.is_empty())
            .unwrap_or_else(|| text.split_whitespace().map(str::to_owned).collect());
        if command.is_empty() {
            error.set(Some(
                "Enter a check command, e.g. [\"cargo\",\"check\"].".into(),
            ));
            return;
        }
        check_busy.set(true);
        error.set(None);
        spawn(async move {
            let result = call(session, move |creds| api::check_put(creds, &slug, &command)).await;
            check_busy.set(false);
            match result {
                Ok(_) => notice.set(Some("Check command saved.".into())),
                Err(message) => error.set(Some(message)),
            }
        });
    };

    let mut load_files = move |slug: String| {
        busy.set(true);
        error.set(None);
        let slug_req = slug.clone();
        let slug_diag = slug.clone();
        spawn(async move {
            let result = call(session, move |creds| api::list_files(creds, &slug_req)).await;
            busy.set(false);
            match result {
                Ok(value) => {
                    selected.set(Some(slug));
                    files.set(parse_files(&value));
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
            let result = call(session, api::list).await;
            busy.set(false);
            match result {
                Ok(value) => {
                    let list = parse_projects(&value);
                    let current = selected.read().clone();
                    let next = current
                        .filter(|s| list.iter().any(|p| &p.slug == s))
                        .or_else(|| list.first().map(|p| p.slug.clone()));
                    projects.set(list);
                    if let Some(slug) = next {
                        load_files(slug);
                    } else {
                        selected.set(None);
                        files.set(Vec::new());
                    }
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };

    use_hook(refresh);

    let create_project = move |_| {
        let name = new_name.read().trim().to_owned();
        if name.is_empty() {
            return;
        }
        busy.set(true);
        error.set(None);
        notice.set(None);
        spawn(async move {
            let result = call(session, move |creds| api::create(creds, &name)).await;
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
                    api::upload(creds, &slug, &filename, sub_opt.as_deref(), &bytes)
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

    // 0.74: workspace editor state and actions.
    let mut ws_path = use_signal(|| Option::<String>::None);
    let mut ws_text = use_signal(String::new);
    let mut ws_dirty = use_signal(|| false);
    let mut ws_status = use_signal(|| Option::<String>::None);
    let mut ws_jump_line = use_signal(|| Option::<u64>::None);

    let mut open_file = move |slug: String, path: String, line: Option<u64>| {
        ws_status.set(None);
        let s = slug.clone();
        let p2 = path.clone();
        spawn(async move {
            let result = call(session, move |creds| api::download(creds, &s, &p2)).await;
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
                api::write_file(creds, &slug, &path, &content)
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
    let ws_open = ws_path.read().clone();
    let ws_text_value = ws_text.read().clone();
    let ws_dirty_flag = *ws_dirty.read();
    let ws_status_text = ws_status.read().clone();
    let ws_files = files.read().clone();
    let ws_line_count = ws_text_value.lines().count().max(1);
    let ws_jump = ws_jump_line
        .read()
        .as_ref()
        .map(|l| l.to_string())
        .unwrap_or_default();

    rsx! {
        div { class: "page-shell projects-page",
            div { class: "page-header",
                div {
                    h2 { "Projects" }
                    p { class: "muted", "Your private server project folders. Bots can file chat attachments here and read them by inbox path." }
                }
                div { class: "header-actions",
                    button {
                        r#type: "button",
                        class: "btn",
                        disabled: is_busy,
                        onclick: move |_| refresh(),
                        Icon { name: "refresh", size: 15 }
                        " Refresh"
                    }
                }
            }

            if let Some(msg) = err {
                div { class: "banner error", "{msg}" }
            }
            if let Some(msg) = note {
                div { class: "banner info", "{msg}" }
            }

            div { class: "surface-card",
                div { class: "field-row",
                    input {
                        r#type: "text",
                        class: "input",
                        placeholder: "New project name (e.g. Q4 Invoices)",
                        value: "{new_name}",
                        oninput: move |e| new_name.set(e.value()),
                    }
                    button {
                        r#type: "button",
                        class: "btn btn-primary",
                        disabled: is_busy || new_name.read().trim().is_empty(),
                        onclick: create_project,
                        Icon { name: "plus", size: 15 }
                        " Create project"
                    }
                }
            }

            div { class: "projects-grid", style: "display:grid;grid-template-columns:260px 1fr;gap:16px;align-items:start;",
                div { class: "surface-card",
                    h3 { "Folders ({project_list.len()})" }
                    if project_list.is_empty() {
                        p { class: "muted", "No projects yet. Create one above or ask any bot to file a document into a project." }
                    } else {
                        ul { class: "list-plain",
                            for proj in project_list {
                                {
                                    let slug = proj.slug.clone();
                                    let is_active = active_slug.as_deref() == Some(slug.as_str());
                                    let size_str = format_bytes(proj.total_bytes);
                                    let click_slug = slug.clone();
                                    rsx! {
                                        li { key: "{slug}",
                                            button {
                                                r#type: "button",
                                                class: if is_active { "nav-item active" } else { "nav-item" },
                                                style: "width:100%;display:flex;justify-content:space-between;align-items:center;",
                                                onclick: move |_| load_files(click_slug.clone()),
                                                span { "{slug}" }
                                                span { class: "muted", "{proj.file_count} · {size_str}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                div { class: "surface-card",
                    if let Some(slug) = active_slug.clone() {
                        div { class: "section-head", style: "display:flex;justify-content:space-between;align-items:center;gap:12px;margin-bottom:12px;",
                            h3 { "{slug}" }
                            div { class: "field-row",
                                input {
                                    r#type: "text",
                                    class: "input",
                                    placeholder: "Optional subfolder (e.g. 2026)",
                                    value: "{subfolder}",
                                    oninput: move |e| subfolder.set(e.value()),
                                }
                                button {
                                    r#type: "button",
                                    class: "btn btn-primary",
                                    disabled: is_busy,
                                    onclick: upload_file,
                                    Icon { name: "paperclip", size: 15 }
                                    " Upload file"
                                }
                            }
                        }

                        if file_list.is_empty() {
                            p { class: "muted", "This project folder is empty. Upload a file or ask a bot to file an attachment here." }
                        } else {
                            div { class: "project-files-list",
                                for file in file_list {
                                    {
                                        let key = file.path.clone();
                                        let path_for_del = file.path.clone();
                                        let path_for_dl = file.path.clone();
                                        let name_for_dl = file.name.clone();
                                        let slug_for_dl = slug.clone();
                                        let slug_for_del = slug.clone();
                                        let inbox_for_chat = file.inbox_path.clone();
                                        let size_str = format_bytes(file.size_bytes);
                                        let confirming = del_target.as_deref() == Some(file.path.as_str());
                                        rsx! {
                                            div { key: "{key}", class: "list-row", style: "display:flex;justify-content:space-between;align-items:center;gap:12px;padding:8px 0;border-bottom:1px solid var(--border-subtle);",
                                                div {
                                                    div { style: "font-weight:600;", "{file.path}" }
                                                    div { class: "muted", "{file.mime} · {size_str}" }
                                                }
                                                div { class: "row-actions", style: "display:flex;gap:6px;",
                                                    button {
                                                        r#type: "button",
                                                        class: "btn",
                                                        onclick: move |_| {
                                                            let mut s = session.write();
                                                            s.composer = format!("Please review `{}`: ", inbox_for_chat);
                                                            s.open_section(Section::Chat);
                                                        },
                                                        "Ask in chat"
                                                    }
                                                    button {
                                                        r#type: "button",
                                                        class: "btn",
                                                        disabled: is_busy,
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
                                                            }
                                                        },
                                                        "Save"
                                                    }
                                                    if confirming {
                                                        button {
                                                            r#type: "button",
                                                            class: "btn btn-danger",
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
                                                                            notice.set(Some("File deleted.".into()));
                                                                            refresh();
                                                                        }
                                                                        Err(e) => error.set(Some(e)),
                                                                    }
                                                                });
                                                            },
                                                            "Confirm delete"
                                                        }
                                                    } else {
                                                        button {
                                                            r#type: "button",
                                                            class: "btn",
                                                            onclick: move |_| confirm_delete.set(Some(path_for_del.clone())),
                                                            "Delete"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        div { style: "margin-top:16px;border-top:1px solid var(--border-subtle);padding-top:12px;",
                            div { style: "display:flex;justify-content:space-between;align-items:center;gap:12px;margin-bottom:8px;",
                                h4 { style: "margin:0;display:flex;align-items:center;gap:8px;",
                                    "Problems"
                                    if let Some(run) = run_info.as_ref() {
                                        {
                                            let errors = run["errors"].as_u64().unwrap_or(0);
                                            let warnings = run["warnings"].as_u64().unwrap_or(0);
                                            rsx! {
                                                span { class: "chip", "{errors} errors \u{b7} {warnings} warnings" }
                                            }
                                        }
                                    }
                                }
                                button {
                                    r#type: "button",
                                    class: "btn",
                                    disabled: is_busy || checking,
                                    onclick: run_check,
                                    Icon { name: "refresh", size: 14 }
                                    " Run checks"
                                }
                            }
                            div { class: "field-row", style: "margin-bottom:8px;",
                                input {
                                    r#type: "text",
                                    class: "input check-cmd-input",
                                    placeholder: "Check command as JSON argv, e.g. [\"cargo\",\"check\",\"--message-format=json\"]",
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
                                    class: "btn",
                                    disabled: checking,
                                    onclick: save_check,
                                    "Save command"
                                }
                            }
                            if diag_list.is_empty() {
                                p { class: "muted",
                                    if run_info.is_some() {
                                        "No problems found in the last check."
                                    } else {
                                        "No check has run yet for this project."
                                    }
                                }
                            } else {
                                div { class: "diag-list",
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
                                            let d_slug = slug.clone();
                                            rsx! {
                                                div {
                                                    key: "{key}",
                                                    class: "diag-row",
                                                    style: if d_editable { "cursor:pointer;" } else { "" },
                                                    title: if d_editable { "Open in workspace editor" } else { "" },
                                                    onclick: move |_| {
                                                        if d_editable {
                                                            open_file(d_slug.clone(), d_file.clone(), d_line);
                                                        }
                                                    },
                                                    span { class: "{sev_class}", Icon { name: icon, size: 14 } }
                                                    div { class: "diag-copy",
                                                        div { class: "diag-loc", "{loc}" }
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
                    } else {
                        p { class: "muted", "Select or create a project to browse its files." }
                    }
                }
            }
            if let Some(ws_slug) = active_slug.clone() {
                div { class: "surface-card", style: "margin-top:16px;",
                    div { class: "section-head", style: "display:flex;justify-content:space-between;align-items:center;gap:12px;margin-bottom:12px;",
                        h3 { "Workspace" }
                        div { class: "field-row",
                            if let Some(status) = ws_status_text.as_ref() {
                                span { class: "muted", "{status}" }
                            }
                            button {
                                r#type: "button",
                                class: "btn btn-primary",
                                disabled: !ws_dirty_flag || is_busy,
                                onclick: save_file,
                                Icon { name: "check", size: 14 }
                                " Save"
                            }
                        }
                    }
                    div { class: "ws-grid",
                        div { class: "ws-tree", role: "tree", "aria-label": "Project files",
                            if ws_files.is_empty() {
                                p { class: "muted", style: "padding:8px;", "No files yet." }
                            }
                            for file in ws_files.clone() {
                                {
                                    let depth = file.path.matches('/').count();
                                    let pad = 8 + depth * 14;
                                    let editable = is_editable_file(&file.mime, &file.name);
                                    let active = ws_open.as_deref() == Some(file.path.as_str());
                                    let p_click = file.path.clone();
                                    let s_click = ws_slug.clone();
                                    rsx! {
                                        button {
                                            key: "{file.path}",
                                            r#type: "button",
                                            class: if active { "ws-tree-item active" } else if editable { "ws-tree-item" } else { "ws-tree-item dim" },
                                            style: "padding-left:{pad}px;",
                                            disabled: !editable,
                                            title: "{file.path}",
                                            onclick: move |_| {
                                                if editable {
                                                    open_file(s_click.clone(), p_click.clone(), None);
                                                }
                                            },
                                            "{file.path}"
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "ws-editor",
                            if let Some(path) = ws_open.clone() {
                                div { class: "ws-editor-head",
                                    span { "{path}" }
                                    span { class: "muted", if ws_dirty_flag { "unsaved changes" } else { "saved" } }
                                }
                                div { class: "ws-editor-body",
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
                            } else {
                                p { class: "muted", style: "padding:12px;", "Select a text file on the left to edit it here. A problem below jumps straight to its line." }
                            }
                        }
                    }
                }
            }
        }
    }
}
