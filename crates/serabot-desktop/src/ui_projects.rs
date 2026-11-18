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

    let mut load_files = move |slug: String| {
        busy.set(true);
        error.set(None);
        let slug_req = slug.clone();
        spawn(async move {
            let result = call(session, move |creds| api::list_files(creds, &slug_req)).await;
            busy.set(false);
            match result {
                Ok(value) => {
                    selected.set(Some(slug));
                    files.set(parse_files(&value));
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

    let project_list = projects.read().clone();
    let file_list = files.read().clone();
    let active_slug = selected.read().clone();
    let is_busy = *busy.read();
    let err = error.read().clone();
    let note = notice.read().clone();
    let del_target = confirm_delete.read().clone();

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
                    } else {
                        p { class: "muted", "Select or create a project to browse its files." }
                    }
                }
            }
        }
    }
}
