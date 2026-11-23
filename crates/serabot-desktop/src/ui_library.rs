//! The Library page (0.74.8): one cross-chat list of every file a bot made
//! (`generated`) plus the member's own uploads (`uploaded`), with tabs, search,
//! date and sort filters, an upload button, and a per-item dialog. The server
//! mirrors each file to the member's own OneDrive when it is connected.

use dioxus::prelude::*;
use serabot_desktop::LibraryOnedriveStatus;
use serabot_desktop::bot_library::{self as lib, DateFilter, KindFilter, SortOrder};
use serabot_desktop::session::{Session, humanize_label};

use crate::ui::{Icon, stamp, today_date};

/// Run one API job on a dedicated thread, like the mail window's jobs.
async fn run_library_job<T, F>(session: Signal<Session>, job: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&serabot_desktop::ApiClient) -> Result<T, serabot_desktop::ApiError> + Send + 'static,
{
    let Some(creds) = session.read().credentials.clone() else {
        return Err("Not signed in.".to_owned());
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Err(err) = std::thread::Builder::new()
        .name("serabot-library".into())
        .spawn(move || {
            let res = serabot_desktop::ApiClient::new(creds.endpoint.clone(), creds.token.clone())
                .and_then(|client| job(&client))
                .map_err(|e| e.to_string());
            let _ = tx.send(res);
        })
    {
        return Err(format!("Could not start request thread: {err}"));
    }
    rx.await
        .map_err(|_| "Request thread exited unexpectedly.".to_owned())?
}

/// Native file picker for library uploads. The library takes any file type up
/// to the server's artifact ceiling; names and sizes are checked before any
/// bytes are read.
#[cfg(windows)]
fn pick_library_files() -> Vec<Result<(String, Vec<u8>), String>> {
    let Some(paths) = rfd::FileDialog::new()
        .set_title("Add to Library")
        .pick_files()
    else {
        return Vec::new();
    };
    paths
        .into_iter()
        .take(8)
        .map(|path| {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_owned();
            if name.trim().is_empty()
                || name.len() > 255
                || name
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '/' | '\\'))
            {
                return Err(format!("{name}: this file name cannot be used."));
            }
            let size = std::fs::metadata(&path)
                .map_err(|_| format!("{name}: could not read the file."))?
                .len();
            if size == 0 || size > serabot_desktop::MAX_LIBRARY_FILE_BYTES as u64 {
                return Err(format!(
                    "{name}: library files must be between 1 byte and 100 MB."
                ));
            }
            let bytes =
                std::fs::read(&path).map_err(|_| format!("{name}: could not read the file."))?;
            Ok((name, bytes))
        })
        .collect()
}

#[cfg(not(windows))]
fn pick_library_files() -> Vec<Result<(String, Vec<u8>), String>> {
    vec![Err("Uploads need the Windows app.".into())]
}

/// The Library page: tabs, search, date/sort filters, upload, and the items.
#[component]
pub fn LibraryView(session: Signal<Session>) -> Element {
    let session = session;
    let mut items = use_signal(Vec::<lib::LibraryItem>::new);
    let mut onedrive = use_signal(|| Option::<LibraryOnedriveStatus>::None);
    let mut loading = use_signal(|| true);
    let mut uploading = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);
    let mut notice = use_signal(|| Option::<String>::None);
    let mut tab = use_signal(KindFilter::default);
    let mut date = use_signal(DateFilter::default);
    let mut sort = use_signal(SortOrder::default);
    let mut query = use_signal(String::new);
    let mut selected = use_signal(|| Option::<lib::LibraryItem>::None);

    let mut reload = move || {
        loading.set(true);
        error.set(None);
        spawn(async move {
            match run_library_job(session, |c| c.global_library()).await {
                Ok(status) => {
                    items.set(status.items);
                    onedrive.set(Some(status.onedrive));
                    loading.set(false);
                }
                Err(msg) => {
                    loading.set(false);
                    error.set(Some(msg));
                }
            }
        });
    };
    use_hook(reload);

    let upload = move |_| {
        let picked = pick_library_files();
        if picked.is_empty() {
            return;
        }
        uploading.set(true);
        notice.set(None);
        error.set(None);
        spawn(async move {
            let mut ok = 0usize;
            let mut failed: Vec<String> = Vec::new();
            for file in picked {
                match file {
                    Ok((name, bytes)) => {
                        let upload_name = name.clone();
                        match run_library_job(session, move |c| {
                            c.library_upload(&upload_name, &bytes)
                        })
                        .await
                        {
                            Ok(reply) => {
                                ok += 1;
                                if !reply.onedrive.is_empty()
                                    && reply.onedrive != "uploaded"
                                    && reply.onedrive != "off"
                                {
                                    failed.push(format!(
                                        "{name}: OneDrive mirror — {}",
                                        reply.onedrive
                                    ));
                                }
                            }
                            Err(msg) => failed.push(format!("{name}: {msg}")),
                        }
                    }
                    Err(msg) => failed.push(msg),
                }
            }
            uploading.set(false);
            let mut summary = format!("Uploaded {ok} file{}", if ok == 1 { "" } else { "s" });
            if !failed.is_empty() {
                summary.push_str(&format!(" ({} failed)", failed.len()));
                error.set(Some(failed.join("\n")));
            }
            notice.set(Some(summary));
            reload();
        });
    };

    let today = today_date();
    let shown: Vec<lib::LibraryItem> =
        lib::view(&items.read(), tab(), date(), &query.read(), sort(), &today)
            .into_iter()
            .cloned()
            .collect();
    let grouped = sort() != SortOrder::Name;
    let mut rows: Vec<(Option<String>, lib::LibraryItem)> = Vec::new();
    let mut last = String::new();
    for item in shown {
        let group = lib::date_group(item.day(), &today);
        let heading = (grouped && group != last).then(|| group.clone());
        last = group;
        rows.push((heading, item));
    }
    let downloading = session.read().downloading;
    let od = onedrive();
    let mirror_label = match od.as_ref() {
        Some(o) if o.mirror && o.connected => format!("OneDrive: {}", o.folder),
        Some(o) if o.mirror => "OneDrive: not connected".to_owned(),
        _ => "OneDrive mirror off".to_owned(),
    };

    rsx! {
        div { class: "pane library-page",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Library" }
                    p { class: "muted",
                        "Every file your bots made in any chat, plus files you upload here."
                    }
                }
                div { class: "pane-head-actions",
                    span { class: "pill", title: "Library files are mirrored to your OneDrive when it is connected", "{mirror_label}" }
                    button {
                        class: "btn icon",
                        r#type: "button",
                        title: "Refresh",
                        "aria-label": "Refresh the library",
                        onclick: move |_| reload(),
                        Icon { name: "refresh", size: 14 }
                    }
                    button {
                        class: "btn primary",
                        r#type: "button",
                        disabled: uploading(),
                        onclick: upload,
                        Icon { name: "upload", size: 14 }
                        if uploading() { "Uploading\u{2026}" } else { "Upload" }
                    }
                }
            }
            div { class: "library-toolbar",
                div { class: "search-wrap",
                    input {
                        class: "input",
                        r#type: "search",
                        placeholder: "Search files\u{2026}",
                        "aria-label": "Search the library",
                        value: "{query}",
                        oninput: move |event| query.set(event.value()),
                    }
                }
                select {
                    class: "input library-sort",
                    "aria-label": "Sort files",
                    onchange: move |event| {
                        let value = event.value();
                        if let Some(order) = SortOrder::ALL.into_iter().find(|o| o.label() == value) {
                            sort.set(order);
                        }
                    },
                    for order in SortOrder::ALL {
                        option { key: "{order.label()}", value: "{order.label()}", selected: sort() == order, "{order.label()}" }
                    }
                }
            }
            div { class: "seg", role: "tablist", "aria-label": "Library tabs",
                for option in KindFilter::ALL {
                    button {
                        key: "{option.label()}",
                        r#type: "button",
                        role: "tab",
                        class: if tab() == option { "seg-btn active" } else { "seg-btn" },
                        "aria-selected": if tab() == option { "true" } else { "false" },
                        onclick: move |_| tab.set(option),
                        "{option.label()}"
                    }
                }
            }
            div { class: "library-chips", role: "group", "aria-label": "Date",
                for option in DateFilter::ALL {
                    button {
                        key: "{option.label()}",
                        r#type: "button",
                        class: if date() == option { "chip-toggle on" } else { "chip-toggle" },
                        "aria-pressed": if date() == option { "true" } else { "false" },
                        onclick: move |_| date.set(option),
                        "{option.label()}"
                    }
                }
            }
            if let Some(msg) = error() {
                div { class: "banner err", role: "alert", span { class: "grow", "{msg}" } }
            }
            if let Some(msg) = notice() {
                div { class: "banner info", role: "status", span { class: "grow", "{msg}" } }
            }
            if loading() && items().is_empty() {
                p { class: "hint", role: "status", "Loading the library\u{2026}" }
            } else if items().is_empty() {
                p { class: "hint",
                    "Nothing here yet. Files your bots make in any chat and files you upload will show up here."
                }
            } else if rows.is_empty() {
                p { class: "hint", "No files match these filters." }
            } else {
                div { class: "library-list", role: "list",
                    for (heading, item) in rows {
                        div { key: "{item.kind}-{item.id}", class: "library-item",
                        if let Some(heading) = heading {
                            div { class: "library-day", "{heading}" }
                        }
                        button {
                            class: "library-row",
                            r#type: "button",
                            role: "listitem",
                            onclick: {
                                let item = item.clone();
                                move |_| selected.set(Some(item.clone()))
                            },
                            span { class: "library-ext", "{item.extension()}" }
                            span { class: "library-copy",
                                span { class: "library-name", "{item.label()}" }
                                span { class: "library-meta",
                                    if item.generated() { "Made by bots" } else { "Uploaded" }
                                    " \u{b7} {item.day()}"
                                }
                            }
                        }
                        }
                    }
                }
            }
            if let Some(item) = selected() {
                LibraryItemDialog { session, item, downloading, selected }
            }
        }
    }
}

/// One library item: what it is, where it came from, and what to do with it.
#[component]
fn LibraryItemDialog(
    session: Signal<Session>,
    item: lib::LibraryItem,
    downloading: bool,
    selected: Signal<Option<lib::LibraryItem>>,
) -> Element {
    let mut session = session;
    let mut selected = selected;
    let size = lib::human_size(item.size_bytes);
    let source = if item.generated() {
        format!("Made by bots ({})", humanize_label(&item.tool))
    } else {
        "Uploaded by you".to_owned()
    };
    let chat = item
        .conversation_title
        .clone()
        .filter(|title| !title.trim().is_empty());
    let generated = item.generated();
    let id = item.id.clone();
    let conversation = item.conversation_id.clone();
    rsx! {
        div { class: "library-scrim", onclick: move |_| selected.set(None) }
        div {
            class: "library-dialog",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "{item.label()}",
            tabindex: "0",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    event.prevent_default();
                    selected.set(None);
                }
            },
            div { class: "library-dialog-head",
                h3 { "{item.label()}" }
                button {
                    class: "btn icon",
                    r#type: "button",
                    "aria-label": "Close",
                    onclick: move |_| selected.set(None),
                    Icon { name: "x", size: 15 }
                }
            }
            dl { class: "library-facts",
                dt { "Date" }
                dd { "{stamp(&item.created_at)}" }
                dt { "Source" }
                dd { "{source}" }
                if let Some(chat) = chat.clone() {
                    dt { "Chat" }
                    dd { "{chat}" }
                }
                if let Some(name) = item.filename.clone() {
                    dt { "File" }
                    dd { "{name}" }
                }
                if let Some(size) = size {
                    dt { "Size" }
                    dd { "{size}" }
                }
            }
            div { class: "library-actions",
                button {
                    class: "btn primary sm",
                    r#type: "button",
                    disabled: downloading,
                    onclick: {
                        let id = id.clone();
                        move |_| session.write().open_artifact(id.clone())
                    },
                    Icon { name: "external", size: 14 }
                    "Open"
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    disabled: downloading,
                    onclick: {
                        let id = id.clone();
                        move |_| session.write().reveal_artifact(id.clone())
                    },
                    Icon { name: "folder", size: 14 }
                    "Show in Explorer"
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    disabled: downloading,
                    onclick: {
                        let id = id.clone();
                        move |_| session.write().download_artifact(id.clone())
                    },
                    Icon { name: "download", size: 14 }
                    "Save to Downloads"
                }
                if generated {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| {
                            selected.set(None);
                            session.write().open_conversation(conversation.clone());
                        },
                        Icon { name: "chat", size: 14 }
                        "Go to chat"
                    }
                }
            }
        }
    }
}
