//! The Library page (0.74.9): one cross-chat list of every file a bot made
//! (`generated`) plus the member's own uploads (`uploaded`), with tabs, search,
//! date and sort filters, an upload button, and a per-item dialog. The server
//! mirrors each file to the member's own OneDrive when it is connected.
//!
//! 0.74.9 adds a Grid/List toggle with real image thumbnails, a full image
//! viewer (zoom, prev/next), in-app previews for decks and workbooks via
//! `GET /v1/library/preview-doc` (falling back to the artifact preview
//! endpoint when that route is not shipped yet), PDF preview through the
//! existing PreviewDock, and "Open with default app" on every item.

use std::collections::{BTreeSet, HashMap};

use dioxus::prelude::*;
use serabot_desktop::LibraryDocPreview;
use serabot_desktop::LibraryOnedriveStatus;
use serabot_desktop::bot_library::{self as lib, DateFilter, KindFilter, SortOrder};
use serabot_desktop::preview::{ArtifactPreview, PreviewContent, PreviewVisual};
use serabot_desktop::session::{Session, humanize_label};

use crate::ui::{Icon, stamp, today_date};

/// Most thumbnails fetched for one grid render. Past this, cards show the
/// type badge until the member narrows the filters.
const MAX_GRID_THUMBS: usize = 48;
/// Images above this size do not get an automatic grid thumbnail.
const MAX_THUMBNAIL_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
/// Most slides shown in a deck preview; the rest are counted, not rendered.
const MAX_PREVIEW_SLIDES_SHOWN: usize = 50;
/// Most rows / columns shown in a sheet preview.
const MAX_PREVIEW_ROWS_SHOWN: usize = 50;
const MAX_PREVIEW_COLS_SHOWN: usize = 20;
/// Most plain-text preview characters shown in the dialog.
const MAX_PREVIEW_TEXT_SHOWN: usize = 8_000;

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

/// Whether the item is an image (MIME first, extension as a fallback).
fn is_image(item: &lib::LibraryItem) -> bool {
    if let Some(mime) = item.mime.as_deref() {
        let mime = mime.trim().to_ascii_lowercase();
        if mime.starts_with("image/") {
            return true;
        }
        if !mime.is_empty() {
            return false;
        }
    }
    matches!(
        item.extension().as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "svg"
    )
}

/// Whether the item is a PDF.
fn is_pdf(item: &lib::LibraryItem) -> bool {
    item.extension() == "pdf"
        || item
            .mime
            .as_deref()
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/pdf"))
}

/// Whether the item's id is an artifact hash the preview/thumbnail routes
/// accept. Library ids are artifact hashes for generated and uploaded items
/// alike (the server sends `row.hash` as `id`).
fn valid_hash(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// A small thumbnail for one image, as a data URL for an `<img>`.
///
/// The thumbnail route needs the bearer token, which a bare `<img src>`
/// cannot carry, so the bytes are fetched through the authenticated client
/// and rendered as a data URL. `GET /v1/library/thumbnail?hash=...&w=320`
/// is tried first; when the server has not shipped it yet (404) the
/// artifact preview endpoint's image is used instead.
async fn fetch_thumb(session: Signal<Session>, hash: String) -> Option<String> {
    let thumb_hash = hash.clone();
    if let Ok(Some((mime, bytes))) =
        run_library_job(session, move |c| c.library_thumbnail(&thumb_hash, 320)).await
    {
        let mime = mime.trim().to_ascii_lowercase();
        if mime.starts_with("image/") {
            return Some(serabot_desktop::avatars::preview_data_url(&mime, &bytes));
        }
    }
    let preview_hash = hash.clone();
    match run_library_job(session, move |c| c.artifact_preview(&preview_hash)).await {
        Ok(Some(preview)) => match preview.render() {
            Some(PreviewContent::Image(url)) => Some(url),
            _ => None,
        },
        _ => None,
    }
}

/// The full-size image for the viewer, as a data URL: the artifact preview
/// first, the (larger) server thumbnail as a fallback.
async fn fetch_full_image(session: Signal<Session>, hash: String) -> Option<String> {
    let preview_hash = hash.clone();
    if let Ok(Some(preview)) =
        run_library_job(session, move |c| c.artifact_preview(&preview_hash)).await
        && let Some(PreviewContent::Image(url)) = preview.render()
    {
        return Some(url);
    }
    let thumb_hash = hash.clone();
    if let Ok(Some((mime, bytes))) =
        run_library_job(session, move |c| c.library_thumbnail(&thumb_hash, 1024)).await
    {
        let mime = mime.trim().to_ascii_lowercase();
        if mime.starts_with("image/") {
            return Some(serabot_desktop::avatars::preview_data_url(&mime, &bytes));
        }
    }
    None
}

/// A deck/workbook preview converted from the artifact preview endpoint, so
/// slides and sheets still render when `preview-doc` is not shipped yet.
fn doc_preview_from_artifact(preview: &ArtifactPreview) -> Option<LibraryDocPreview> {
    match &preview.visual {
        PreviewVisual::Pptx {
            slide_count,
            slides,
        } => Some(LibraryDocPreview {
            kind: "pptx".into(),
            slide_count: Some(*slide_count),
            sheet_count: None,
            truncated: false,
            slides: slides
                .iter()
                .map(|slide| serabot_desktop::LibraryDocSlide {
                    index: slide.slide,
                    title: slide.title.clone(),
                    text: slide.text.clone(),
                })
                .collect(),
            sheets: Vec::new(),
        }),
        PreviewVisual::Xlsx { sheets } => Some(LibraryDocPreview {
            kind: "xlsx".into(),
            slide_count: None,
            sheet_count: Some(sheets.len()),
            truncated: false,
            slides: Vec::new(),
            sheets: sheets
                .iter()
                .map(|sheet| serabot_desktop::LibraryDocSheet {
                    name: sheet.name.clone(),
                    rows: sheet.rows.clone(),
                })
                .collect(),
        }),
        _ => None,
    }
}

/// List or grid layout for the items.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ViewMode {
    #[default]
    List,
    Grid,
}

impl ViewMode {
    const ALL: [Self; 2] = [Self::List, Self::Grid];

    fn label(self) -> &'static str {
        match self {
            Self::List => "List",
            Self::Grid => "Grid",
        }
    }
}

/// The image viewer overlay: which filtered images it steps through, where
/// it is, and the zoom percent.
#[derive(Clone, Debug)]
struct ViewerState {
    items: Vec<lib::LibraryItem>,
    index: usize,
    zoom: u32,
}

fn viewer_step(mut viewer: Signal<Option<ViewerState>>, delta: isize) {
    let mut guard = viewer.write();
    let Some(state) = guard.as_mut() else {
        return;
    };
    let len = state.items.len();
    if len == 0 {
        return;
    }
    let next = (state.index as isize + delta).rem_euclid(len as isize);
    state.index = next as usize;
}

fn viewer_zoom(mut viewer: Signal<Option<ViewerState>>, delta: i32) {
    if let Some(state) = viewer.write().as_mut() {
        state.zoom = (state.zoom as i32 + delta).clamp(25, 400) as u32;
    }
}

fn viewer_zoom_reset(mut viewer: Signal<Option<ViewerState>>) {
    if let Some(state) = viewer.write().as_mut() {
        state.zoom = 100;
    }
}

fn open_viewer(
    mut viewer: Signal<Option<ViewerState>>,
    images: &[lib::LibraryItem],
    item: &lib::LibraryItem,
) {
    let index = images
        .iter()
        .position(|candidate| candidate.id == item.id)
        .unwrap_or(0);
    viewer.set(Some(ViewerState {
        items: images.to_vec(),
        index,
        zoom: 100,
    }));
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
    let mut view_mode = use_signal(ViewMode::default);
    let thumbs = use_signal(HashMap::<String, String>::new);
    let thumb_failed = use_signal(BTreeSet::<String>::new);
    let viewer = use_signal(|| Option::<ViewerState>::None);

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
    let images: Vec<lib::LibraryItem> = shown
        .iter()
        .filter(|item| is_image(item))
        .cloned()
        .collect();
    // Only the first thumbnails are fetched automatically; the rest show
    // the type badge until the member narrows the filters.
    let thumb_ids: BTreeSet<String> = images
        .iter()
        .take(MAX_GRID_THUMBS)
        .map(|item| item.id.clone())
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
    // Grid groups keep the same date headings, one grid per group.
    let mut grid_groups: Vec<(String, Vec<lib::LibraryItem>)> = Vec::new();
    for (heading, item) in &rows {
        let name = heading.clone().unwrap_or_else(|| {
            grid_groups
                .last()
                .map(|(name, _)| name.clone())
                .unwrap_or_else(|| "All files".to_owned())
        });
        if grid_groups
            .last()
            .is_none_or(|(last_name, _)| last_name != &name)
        {
            grid_groups.push((name, Vec::new()));
        }
        if let Some((_, group)) = grid_groups.last_mut() {
            group.push(item.clone());
        }
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
                div { class: "seg library-view-toggle", role: "group", "aria-label": "View",
                    for mode in ViewMode::ALL {
                        button {
                            key: "{mode.label()}",
                            r#type: "button",
                            class: if view_mode() == mode { "seg-btn active" } else { "seg-btn" },
                            "aria-pressed": if view_mode() == mode { "true" } else { "false" },
                            onclick: move |_| view_mode.set(mode),
                            "{mode.label()}"
                        }
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
                    "Nothing here yet. Files your bots made in any chat and files you upload will show up here."
                }
            } else if rows.is_empty() {
                p { class: "hint", "No files match these filters." }
            } else if view_mode() == ViewMode::Grid {
                div { class: "library-grid-wrap", role: "list",
                    for (group_name, group_items) in grid_groups {
                        div { key: "{group_name}", class: "library-grid-group",
                            div { class: "library-day", "{group_name}" }
                            div { class: "library-grid",
                                for item in group_items.iter() {
                                    LibraryCard {
                                        key: "{item.kind}-{item.id}",
                                        session,
                                        item: item.clone(),
                                        images: images.clone(),
                                        viewer,
                                        selected,
                                        thumbs,
                                        thumb_failed,
                                        downloading,
                                        fetch_thumb: thumb_ids.contains(&item.id)
                                            && item.size_bytes.is_none_or(|size| size <= MAX_THUMBNAIL_IMAGE_BYTES),
                                    }
                                }
                            }
                        }
                    }
                }
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
                LibraryItemDialog { session, item, downloading, selected, images: images.clone(), viewer }
            }
            if viewer().is_some() {
                ImageViewer { session, viewer, downloading }
            }
        }
    }
}

/// One grid card: a real thumbnail for images, a type badge otherwise, plus
/// filename, size, date, bot/chat, and a clear Open action.
#[component]
fn LibraryCard(
    session: Signal<Session>,
    item: lib::LibraryItem,
    images: Vec<lib::LibraryItem>,
    viewer: Signal<Option<ViewerState>>,
    selected: Signal<Option<lib::LibraryItem>>,
    thumbs: Signal<HashMap<String, String>>,
    thumb_failed: Signal<BTreeSet<String>>,
    downloading: bool,
    fetch_thumb: bool,
) -> Element {
    let item_image = is_image(&item);
    let thumb_url = thumbs.read().get(&item.id).cloned();
    let size = lib::human_size(item.size_bytes);
    let ext = item.extension();
    let badge = if ext.is_empty() {
        "FILE".into()
    } else {
        ext.to_uppercase()
    };
    let origin = if item.generated() {
        "Made by bots"
    } else {
        "Uploaded"
    };
    let chat = item
        .conversation_title
        .clone()
        .filter(|title| !title.trim().is_empty());
    let open_id = item.id.clone();
    let details_item = item.clone();
    let media_item = item.clone();
    let images_for_media = images.clone();
    rsx! {
        div { class: "library-card", role: "listitem",
            button {
                class: "library-card-media",
                r#type: "button",
                "aria-label": if item_image { format!("View {}", item.label()) } else { format!("Open {}", item.label()) },
                onclick: move |_| {
                    if is_image(&media_item) {
                        open_viewer(viewer, &images_for_media, &media_item);
                    } else {
                        selected.set(Some(media_item.clone()));
                    }
                },
                if item_image {
                    if let Some(url) = thumb_url {
                        img {
                            class: "library-card-thumb",
                            src: "{url}",
                            alt: "{item.label()}",
                            loading: "lazy",
                        }
                    } else {
                        LibraryThumb { session, hash: item.id.clone(), enabled: fetch_thumb, thumbs, thumb_failed }
                        span { class: "library-ext library-card-ext", "{badge}" }
                    }
                } else {
                    span { class: "library-ext library-card-ext", "{badge}" }
                }
            }
            div { class: "library-card-copy",
                span { class: "library-name library-card-name", title: "{item.label()}", "{item.label()}" }
                span { class: "library-meta",
                    "{origin} \u{b7} {item.day()}"
                    if let Some(size) = size { " \u{b7} {size}" }
                }
                if let Some(chat) = chat {
                    span { class: "library-meta", "{chat}" }
                } else {
                    span { class: "library-meta", "{humanize_label(&item.tool)}" }
                }
            }
            div { class: "library-card-actions",
                button {
                    class: "btn primary sm",
                    r#type: "button",
                    title: "Open with the default app",
                    disabled: downloading,
                    onclick: move |_| session.write().open_artifact(open_id.clone()),
                    "Open"
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| selected.set(Some(details_item.clone())),
                    "Details"
                }
            }
        }
    }
}

/// Fetches one grid thumbnail on mount (the card remounts when its item
/// changes, so a mount fetch is the right trigger). Failures are remembered
/// so a card never retries in a loop.
#[component]
fn LibraryThumb(
    session: Signal<Session>,
    hash: String,
    enabled: bool,
    thumbs: Signal<HashMap<String, String>>,
    thumb_failed: Signal<BTreeSet<String>>,
) -> Element {
    let hash_for_effect = hash.clone();
    use_effect(move || {
        if !enabled
            || thumbs.peek().contains_key(&hash_for_effect)
            || thumb_failed.peek().contains(&hash_for_effect)
        {
            return;
        }
        let hash = hash_for_effect.clone();
        spawn(async move {
            match fetch_thumb(session, hash.clone()).await {
                Some(url) => {
                    thumbs.write().insert(hash, url);
                }
                None => {
                    thumb_failed.write().insert(hash);
                }
            }
        });
    });
    rsx! {}
}

/// The full image viewer: large image, zoom, prev/next through the filtered
/// images, Download, Open with default app, and Go to chat.
#[component]
fn ImageViewer(
    session: Signal<Session>,
    viewer: Signal<Option<ViewerState>>,
    downloading: bool,
) -> Element {
    let mut viewer = viewer;
    // Hooks first: they must run on every render, before any early return.
    let mut image = use_signal(|| Option::<(String, String)>::None);
    use_effect(move || {
        let Some(current) = viewer
            .read()
            .as_ref()
            .and_then(|current| current.items.get(current.index).cloned())
        else {
            return;
        };
        if image
            .peek()
            .as_ref()
            .is_some_and(|(hash, _)| hash == &current.id)
        {
            return;
        }
        image.set(None);
        let hash = current.id.clone();
        spawn(async move {
            if let Some(url) = fetch_full_image(session, hash.clone()).await {
                let still_showing = viewer
                    .peek()
                    .as_ref()
                    .and_then(|current| current.items.get(current.index))
                    .is_some_and(|shown| shown.id == hash);
                if still_showing {
                    image.set(Some((hash, url)));
                }
            }
        });
    });
    let Some(state) = viewer.read().clone() else {
        return rsx! {};
    };
    let total = state.items.len();
    if total == 0 {
        return rsx! {};
    }
    let index = state.index.min(total - 1);
    let Some(item) = state.items.get(index).cloned() else {
        return rsx! {};
    };
    let zoom = state.zoom;

    let scale = zoom as f32 / 100.0;
    let style = format!("transform: scale({scale:.2});");
    let size = lib::human_size(item.size_bytes);
    let generated = item.generated();
    let conversation = item.conversation_id.clone();
    let download_id = item.id.clone();
    let open_id = item.id.clone();
    rsx! {
        div { class: "library-scrim library-viewer-scrim", onclick: move |_| viewer.set(None) }
        div {
            class: "library-viewer",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "{item.label()}",
            tabindex: "0",
            onkeydown: move |event: KeyboardEvent| {
                match event.key() {
                    Key::Escape => {
                        event.prevent_default();
                        viewer.set(None);
                    }
                    Key::ArrowLeft => viewer_step(viewer, -1),
                    Key::ArrowRight => viewer_step(viewer, 1),
                    Key::Character(ref pressed) if pressed == "+" || pressed == "=" => {
                        viewer_zoom(viewer, 25);
                    }
                    Key::Character(ref pressed) if pressed == "-" => {
                        viewer_zoom(viewer, -25);
                    }
                    Key::Character(ref pressed) if pressed == "0" => {
                        viewer_zoom_reset(viewer);
                    }
                    _ => {}
                }
            },
            div { class: "library-viewer-head",
                div { class: "library-viewer-title",
                    h3 { "{item.label()}" }
                    span { class: "library-meta",
                        "Image {index + 1} / {total}"
                        if let Some(size) = size { " \u{b7} {size}" }
                        " \u{b7} {item.day()}"
                    }
                }
                button {
                    class: "btn icon",
                    r#type: "button",
                    "aria-label": "Close viewer",
                    onclick: move |_| viewer.set(None),
                    Icon { name: "x", size: 15 }
                }
            }
            div { class: "library-viewer-stage",
                if let Some((_, url)) = image() {
                    img {
                        class: "library-viewer-img",
                        src: "{url}",
                        alt: "{item.label()}",
                        style: "{style}",
                    }
                } else {
                    p { class: "hint", role: "status", "Loading the image\u{2026}" }
                }
            }
            div { class: "library-viewer-bar",
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    disabled: total <= 1,
                    onclick: move |_| viewer_step(viewer, -1),
                    Icon { name: "chevron-left", size: 14 }
                    "Previous"
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    disabled: total <= 1,
                    onclick: move |_| viewer_step(viewer, 1),
                    "Next"
                    Icon { name: "chevron-right", size: 14 }
                }
                span { class: "library-viewer-zoom",
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        "aria-label": "Zoom out",
                        onclick: move |_| viewer_zoom(viewer, -25),
                        "\u{2212}"
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        title: "Reset zoom",
                        onclick: move |_| viewer_zoom_reset(viewer),
                        "{zoom}%"
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        "aria-label": "Zoom in",
                        onclick: move |_| viewer_zoom(viewer, 25),
                        "+"
                    }
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    disabled: downloading,
                    onclick: move |_| session.write().download_artifact(download_id.clone()),
                    Icon { name: "download", size: 14 }
                    "Save to Downloads"
                }
                button {
                    class: "btn primary sm",
                    r#type: "button",
                    disabled: downloading,
                    onclick: move |_| session.write().open_artifact(open_id.clone()),
                    Icon { name: "external", size: 14 }
                    "Open with default app"
                }
                if generated {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| {
                            viewer.set(None);
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

/// What the item dialog's in-app preview holds. Extracted text is untrusted
/// data and is only ever rendered as plain text nodes, never as HTML.
#[derive(Clone, Debug)]
enum DialogPreview {
    Idle,
    Loading,
    Image(String),
    Doc(LibraryDocPreview),
    Text(String),
    Unavailable(String),
}

/// Load the in-app preview for one item: decks and workbooks from
/// `preview-doc` (artifact preview as fallback), images full-size, and
/// Word/Markdown/text from the artifact preview. PDFs are not loaded here —
/// they open in the existing PreviewDock instead.
async fn load_dialog_preview(session: Signal<Session>, item: lib::LibraryItem) -> DialogPreview {
    if !valid_hash(&item.id) {
        return DialogPreview::Unavailable(
            "No in-app preview for this file. Open it with its app instead.".into(),
        );
    }
    if is_image(&item) {
        return match fetch_full_image(session, item.id.clone()).await {
            Some(url) => DialogPreview::Image(url),
            None => DialogPreview::Unavailable(
                "Could not load an image preview. Open the file to view it.".into(),
            ),
        };
    }
    let ext = item.extension();
    match ext.as_str() {
        "pptx" | "xlsx" | "xlsm" => {
            // The server takes kind=xlsx for .xlsm files too.
            let kind = if ext == "xlsm" {
                "xlsx".to_owned()
            } else {
                ext.clone()
            };
            let hash = item.id.clone();
            match run_library_job(session, move |c| c.library_preview_doc(&hash, &kind)).await {
                Ok(Some(doc)) => DialogPreview::Doc(doc),
                Ok(None) | Err(_) => {
                    let hash = item.id.clone();
                    match run_library_job(session, move |c| c.artifact_preview(&hash)).await {
                        Ok(Some(preview)) => match doc_preview_from_artifact(&preview) {
                            Some(doc) => DialogPreview::Doc(doc),
                            None => DialogPreview::Unavailable(
                                "No preview for this file yet. Open it to see the full file."
                                    .into(),
                            ),
                        },
                        Ok(None) => DialogPreview::Unavailable(
                            "No preview for this file yet. Open it to see the full file.".into(),
                        ),
                        Err(msg) => DialogPreview::Unavailable(format!("Preview failed: {msg}")),
                    }
                }
            }
        }
        "docx" | "md" | "markdown" | "txt" | "csv" => {
            let hash = item.id.clone();
            match run_library_job(session, move |c| c.artifact_preview(&hash)).await {
                Ok(Some(preview)) => match preview.render() {
                    Some(PreviewContent::Text(text)) => {
                        let mut shown: String = text.chars().take(MAX_PREVIEW_TEXT_SHOWN).collect();
                        if text.chars().count() > MAX_PREVIEW_TEXT_SHOWN {
                            shown.push_str(
                                "\n\n[Preview truncated \u{2014} open the file for the full text]",
                            );
                        }
                        DialogPreview::Text(shown)
                    }
                    _ => DialogPreview::Unavailable(
                        "No preview for this file yet. Open it to see the full file.".into(),
                    ),
                },
                Ok(None) => DialogPreview::Unavailable(
                    "No preview for this file yet. Open it to see the full file.".into(),
                ),
                Err(msg) => DialogPreview::Unavailable(format!("Preview failed: {msg}")),
            }
        }
        _ => DialogPreview::Unavailable(
            "No in-app preview for this file type. Open it with its app instead.".into(),
        ),
    }
}

/// One library item: what it is, where it came from, and what to do with it.
#[component]
fn LibraryItemDialog(
    session: Signal<Session>,
    item: lib::LibraryItem,
    downloading: bool,
    selected: Signal<Option<lib::LibraryItem>>,
    images: Vec<lib::LibraryItem>,
    viewer: Signal<Option<ViewerState>>,
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
    let ext = item.extension();
    let previewable = is_image(&item)
        || matches!(
            ext.as_str(),
            "pptx" | "xlsx" | "xlsm" | "docx" | "md" | "markdown" | "txt" | "csv"
        );
    let mut preview = use_signal(|| DialogPreview::Idle);
    let mut sheet_idx = use_signal(|| 0usize);
    use_effect(move || {
        // Re-run when the selected item changes (or the dialog closes).
        let Some(current) = selected.read().clone() else {
            return;
        };
        if !is_image(&current)
            && !matches!(
                current.extension().as_str(),
                "pptx" | "xlsx" | "xlsm" | "docx" | "md" | "markdown" | "txt" | "csv"
            )
        {
            preview.set(DialogPreview::Idle);
            return;
        }
        preview.set(DialogPreview::Loading);
        sheet_idx.set(0);
        spawn(async move {
            let next = load_dialog_preview(session, current).await;
            preview.set(next);
        });
    });

    let preview_view = match preview() {
        DialogPreview::Idle => rsx! {},
        DialogPreview::Loading => rsx! {
            p { class: "hint", role: "status", "Loading preview\u{2026}" }
        },
        DialogPreview::Image(url) => rsx! {
            div { class: "library-preview",
                img {
                    class: "library-dialog-image",
                    src: "{url}",
                    alt: "{item.label()}",
                }
            }
        },
        DialogPreview::Doc(doc) => {
            let kind_label = if doc.kind == "pptx" {
                "Slides"
            } else {
                "Sheets"
            };
            let total_slides = doc.slide_count.unwrap_or(doc.slides.len());
            let shown_slides: Vec<_> = doc.slides.iter().take(MAX_PREVIEW_SLIDES_SHOWN).collect();
            let sheet_count = doc.sheets.len();
            let total_sheets = doc.sheet_count.unwrap_or(sheet_count);
            let active_sheet = sheet_idx().min(sheet_count.saturating_sub(1));
            let sheet = doc.sheets.get(active_sheet);
            let shown_rows: Vec<_> = sheet
                .map(|sheet| sheet.rows.iter().take(MAX_PREVIEW_ROWS_SHOWN).collect())
                .unwrap_or_default();
            let total_rows = sheet.map(|sheet| sheet.rows.len()).unwrap_or(0);
            let slides_label = if total_slides == 1 {
                "1 slide in this deck".to_owned()
            } else {
                format!("{total_slides} slides in this deck")
            };
            let sheets_label = if total_sheets == 1 {
                "1 sheet in this workbook".to_owned()
            } else {
                format!("{total_sheets} sheets in this workbook")
            };
            let more_slides = total_slides.saturating_sub(shown_slides.len());
            let more_slides_label = if more_slides == 1 {
                "+ 1 more slide \u{2014} open the file for the full deck.".to_owned()
            } else {
                format!("+ {more_slides} more slides \u{2014} open the file for the full deck.")
            };
            rsx! {
                div { class: "library-preview",
                    p { class: "library-preview-head",
                        "{kind_label} \u{b7} "
                        if doc.kind == "pptx" {
                            "{slides_label}"
                        } else {
                            "{sheets_label}"
                        }
                    }
                    if doc.kind == "pptx" {
                        for (slide_pos, slide) in shown_slides.iter().enumerate() {
                            div { key: "slide-{slide_pos}", class: "library-slide",
                                p { class: "library-slide-head",
                                    "Slide {slide.index}"
                                    if let Some(title) = &slide.title {
                                        " \u{b7} {title}"
                                    }
                                }
                                if !slide.text.trim().is_empty() {
                                    p { class: "library-slide-text", "{slide.text}" }
                                }
                            }
                        }
                        if more_slides > 0 {
                            p { class: "hint", "{more_slides_label}" }
                        }
                        if shown_slides.is_empty() {
                            p { class: "hint", "No slide text in this deck." }
                        }
                    } else {
                        div { class: "library-sheet-tabs", role: "tablist", "aria-label": "Sheets",
                            for (tab_pos, tab_sheet) in doc.sheets.iter().enumerate() {
                                button {
                                    key: "sheet-{tab_pos}",
                                    r#type: "button",
                                    role: "tab",
                                    class: if tab_pos == active_sheet { "chip-toggle on" } else { "chip-toggle" },
                                    "aria-selected": if tab_pos == active_sheet { "true" } else { "false" },
                                    onclick: move |_| sheet_idx.set(tab_pos),
                                    "{tab_sheet.name}"
                                }
                            }
                        }
                        if sheet.is_some() {
                            div { class: "library-preview-table-wrap",
                                table { class: "library-sheet-table",
                                    tbody {
                                        for (row_pos, row) in shown_rows.iter().enumerate() {
                                            tr { key: "row-{row_pos}",
                                                for (cell_pos, cell) in row.iter().take(MAX_PREVIEW_COLS_SHOWN).enumerate() {
                                                    td { key: "cell-{cell_pos}", "{cell}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            if total_rows > shown_rows.len() {
                                p { class: "hint", "Showing the first {shown_rows.len()} of {total_rows} rows." }
                            }
                            if shown_rows.is_empty() {
                                p { class: "hint", "This sheet has no rows to show." }
                            }
                        }
                    }
                    if doc.truncated {
                        p { class: "hint", "Preview truncated \u{2014} open the file for the full document." }
                    }
                }
            }
        }
        DialogPreview::Text(text) => rsx! {
            div { class: "library-preview",
                div { class: "library-preview-text", "{text}" }
            }
        },
        DialogPreview::Unavailable(message) => rsx! {
            p { class: "hint", "{message}" }
        },
    };

    let pdf_hash = id.clone();
    let view_item = item.clone();
    let images_for_view = images.clone();
    rsx! {
        div { class: "library-scrim", onclick: move |_| selected.set(None) }
        div {
            class: if previewable || is_pdf(&item) { "library-dialog wide" } else { "library-dialog" },
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
            {preview_view}
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
                    "Open with default app"
                }
                if is_pdf(&item) {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| {
                            session.write().open_preview_dock(&pdf_hash);
                            selected.set(None);
                        },
                        Icon { name: "external", size: 14 }
                        "Preview PDF"
                    }
                }
                if is_image(&item) {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| open_viewer(viewer, &images_for_view, &view_item),
                        Icon { name: "external", size: 14 }
                        "View full size"
                    }
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
