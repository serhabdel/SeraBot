//! The member's Library API (`/v1/library/*`, 0.74.8): one cross-chat list of
//! every file a bot made in the member's conversations plus the member's own
//! uploads, and a raw-bytes upload that lands in the per-user artifact store
//! (so the existing per-user artifact download/preview endpoints serve it) and
//! is mirrored, best-effort, to the member's own OneDrive (`[library]`).
//!
//! Every route is scoped to the acting user; another member's library items
//! are never listed, and an upload is never visible to anyone else.
//!
//! `GET /v1/library/preview-doc` (0.74.9) is a bounded, read-only text
//! preview of one of the acting user's own documents (`kind=pptx|xlsx`),
//! and `GET /v1/library/thumbnail` (0.74.9) serves a downscaled image for
//! one of the acting user's own artifacts, both gated exactly like the
//! artifact download endpoint: another member's hash is a 404.

use std::io::Cursor;

use axum::body::Body;
use axum::extract::DefaultBodyLimit;

use super::*;
use crate::library::{LIBRARY_SCOPE, LIBRARY_UPLOAD_TOOL, LibraryMirror};
use crate::maker::{ArtifactMeta, MAX_ARTIFACT_BYTES, StoredArtifact};
use crate::store::LibraryItemRow;

/// Preview-doc bounds (0.74.9): at most 50 slides, 2000 characters of text
/// per slide, 100 000 characters in total; at most 5 sheets, 20 rows and
/// 20 columns per sheet, cell strings capped at 200 characters.
const PREVIEW_DOC_MAX_SLIDES: usize = 50;
const PREVIEW_DOC_MAX_SLIDE_CHARS: usize = 2_000;
const PREVIEW_DOC_MAX_TOTAL_CHARS: usize = 100_000;
const PREVIEW_DOC_MAX_SHEETS: usize = 5;
const PREVIEW_DOC_MAX_ROWS: usize = 20;
const PREVIEW_DOC_MAX_COLS: usize = 20;
const PREVIEW_DOC_MAX_CELL_CHARS: usize = 200;

/// Thumbnail bounds (0.74.9): `w` defaults to 320 px, clamps to 16–1024 px,
/// and never upscales. JPEG quality for opaque thumbnails.
const THUMBNAIL_DEFAULT_W: u32 = 320;
const THUMBNAIL_MIN_W: u32 = 16;
const THUMBNAIL_MAX_W: u32 = 1024;
const THUMBNAIL_JPEG_QUALITY: u8 = 82;

pub(super) fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/v1/library/upload", post(upload))
        // Raw body, read explicitly under the artifact ceiling; the extra
        // 4 KiB lets the handler produce the readable "too large" error.
        .layer(DefaultBodyLimit::max(MAX_ARTIFACT_BYTES + 4096))
        .layer(RequestBodyLimitLayer::new(MAX_ARTIFACT_BYTES + 4096))
        .route("/v1/library", get(list))
        .route("/v1/library/preview-doc", get(preview_doc))
        .route("/v1/library/thumbnail", get(thumbnail))
}

fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": message, "code": "validation"})),
    )
        .into_response()
}

fn ensure_enabled(state: &ApiState) -> Result<()> {
    if !state.config.library.enabled {
        return Err(SeraError::NotFound("library".into()));
    }
    Ok(())
}

fn mirror_of(state: &ApiState) -> LibraryMirror {
    LibraryMirror::new((*state.config).clone(), state.onedrive.clone())
}

fn item_json(state: &ApiState, row: &LibraryItemRow) -> Value {
    let meta = state.artifacts.meta(&row.hash).ok();
    let filename = meta
        .as_ref()
        .map(|meta| meta.filename.clone())
        .unwrap_or_else(|| row.filename.clone());
    json!({
        "kind": row.kind,
        "id": row.hash,
        "tool": row.tool,
        "name": row
            .title
            .clone()
            .or_else(|| meta.as_ref().map(|meta| meta.title.clone()))
            .or_else(|| Some(filename.clone())),
        "filename": filename,
        "mime": crate::util::mime_for_filename(&filename),
        "size_bytes": state.artifacts.size(&row.hash).ok(),
        "conversation_id": row.conversation_id,
        "conversation_title": row.conversation_title,
        "created_at": row.created_at,
    })
}

/// `GET /v1/library`: the acting user's library items, newest first, plus the
/// OneDrive mirror status for this account (read-only).
async fn list(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    ensure_enabled(&state)?;
    let rows = state.store.list_library_items(&user.id, 500)?;
    let items: Vec<Value> = rows.iter().map(|row| item_json(&state, row)).collect();
    let mirror = mirror_of(&state);
    Ok(Json(json!({
        "enabled": true,
        "onedrive": {
            "mirror": mirror.enabled(),
            "folder": mirror.folder(),
            "connected": mirror.connected(&user.id),
        },
        "items": items,
    })))
}

/// `POST /v1/library/upload`: raw file body with an `X-Filename` header. The
/// file is saved into the acting user's artifact store, recorded under the
/// library scope, audited, and mirrored to their OneDrive when connected. The
/// mirror is best-effort: its status is returned but never fails the upload.
async fn upload(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if !state.config.library.enabled {
        return SeraError::NotFound("library".into()).into_response();
    }
    let Some(filename) = headers
        .get("x-filename")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
    else {
        return bad_request("X-Filename header is required");
    };
    // The blocklist applies to filenames.
    if state
        .runtime
        .scan_owner_output(&user.id, &filename)
        .is_err()
    {
        return SeraError::PolicyViolation.into_response();
    }
    let bytes = match axum::body::to_bytes(body, MAX_ARTIFACT_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({
                    "error": format!(
                        "file exceeds the {} MB library limit",
                        MAX_ARTIFACT_BYTES / (1024 * 1024)
                    ),
                    "code": "validation",
                })),
            )
                .into_response();
        }
    };
    if bytes.is_empty() {
        return bad_request("the file is empty");
    }
    let meta = ArtifactMeta {
        title: filename.clone(),
        filename: filename.clone(),
        language: crate::domain::Language::En,
        source_artifact: None,
        created_at: Utc::now(),
    };
    let stored = match state.artifacts.save(&bytes, &filename, meta) {
        Ok(stored) => stored,
        Err(SeraError::Validation(message)) => return bad_request(&message),
        Err(error) => return error.into_response(),
    };
    if let Err(error) = state.store.record_conversation_artifact(
        &user.id,
        LIBRARY_SCOPE,
        None,
        LIBRARY_UPLOAD_TOOL,
        &stored.hash,
        &stored.filename,
        Some(&stored.meta.title),
    ) {
        return error.into_response();
    }
    let mime = crate::util::mime_for_filename(&stored.filename);
    let onedrive = mirror_of(&state)
        .mirror(&user.id, &stored.filename, &stored.bytes, mime)
        .await;
    let _ = state.store.record_audit(
        &user.id,
        "library.upload",
        Some(LIBRARY_UPLOAD_TOOL),
        None,
        None,
        Some(&stored.hash),
        None,
        Some(&json!({
            "bytes": stored.bytes.len(),
            "filename": stored.filename,
            "mime": mime,
            "onedrive": onedrive,
        })),
    );
    let row = LibraryItemRow {
        kind: "uploaded".into(),
        hash: stored.hash.clone(),
        tool: LIBRARY_UPLOAD_TOOL.into(),
        filename: stored.filename.clone(),
        title: Some(stored.meta.title.clone()),
        conversation_id: LIBRARY_SCOPE.into(),
        conversation_title: None,
        created_at: stored.meta.created_at,
    };
    Json(json!({
        "item": item_json(&state, &row),
        "onedrive": onedrive,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewDocQuery {
    hash: Option<String>,
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThumbnailQuery {
    hash: Option<String>,
    w: Option<String>,
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn artifact_extension(filename: &str) -> String {
    std::path::Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Resolve the acting user's own artifact for a hash-addressed library route.
/// Same gate as the download endpoint: another member's hash is a 404, and a
/// malformed hash is a 400 before anything touches the store.
fn own_artifact(
    state: &ApiState,
    user: &ActingUser,
    hash: &str,
) -> std::result::Result<StoredArtifact, Box<Response>> {
    if !is_sha256_hex(hash) {
        return Err(Box::new(bad_request(
            "hash must be a 64-character sha256 hex string",
        )));
    }
    let owned = match state.store.user_owns_artifact(&user.id, hash) {
        Ok(owned) => owned,
        Err(error) => return Err(Box::new(error.into_response())),
    };
    if !owned {
        return Err(Box::new(
            SeraError::NotFound("artifact".into()).into_response(),
        ));
    }
    state
        .artifacts
        .load(hash)
        .map_err(|error| Box::new(error.into_response()))
}

/// `GET /v1/library/preview-doc?hash=…&kind=pptx|xlsx`: a bounded, read-only
/// text preview of one of the acting user's own documents.
///
/// PPTX returns `slide_count` (complete) plus up to 50 slides, each with its
/// title and up to 2000 characters of text, under a 100 000-character total.
/// XLSX returns `sheet_count` (complete) plus up to 5 sheets, each with its
/// name, the range read, and up to 20 rows × 20 columns of first-row values,
/// cell strings capped at 200 characters. `truncated` is true when any cap
/// cut something off. The file extension must match `kind`; a foreign hash
/// is a 404, an unknown kind or a malformed hash is a 400, and an unreadable
/// package is a 400 from the parser.
///
/// The extracted text is the owner's own file content, treated as data: it is
/// returned only as JSON string values, never executed or rendered as HTML,
/// and control characters are stripped by the parsers. No blocklist scan is
/// applied here — the blocklist covers outgoing model/filename text, and this
/// read-only preview of the owner's own artifact follows the same rule as
/// `GET /v1/artifacts/{hash}/preview`.
async fn preview_doc(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<PreviewDocQuery>,
) -> Response {
    if let Err(error) = ensure_enabled(&state) {
        return error.into_response();
    }
    let Some(hash) = query
        .hash
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return bad_request("hash is required");
    };
    let Some(kind) = query
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return bad_request("kind is required");
    };
    if !matches!(kind, "pptx" | "xlsx") {
        return bad_request("kind must be pptx or xlsx");
    }
    let artifact = match own_artifact(&state, &user, hash) {
        Ok(artifact) => artifact,
        Err(response) => return *response,
    };
    let extension = artifact_extension(&artifact.filename);
    let matches_kind = match kind {
        "pptx" => extension == "pptx",
        "xlsx" => matches!(extension.as_str(), "xlsx" | "xlsm"),
        _ => false,
    };
    if !matches_kind {
        return bad_request(&format!("artifact is not a .{kind} file"));
    }
    let result = match kind {
        "pptx" => pptx_preview_doc(&artifact),
        _ => xlsx_preview_doc(&artifact),
    };
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

fn pptx_preview_doc(artifact: &StoredArtifact) -> Result<Value> {
    let preview = crate::pptx_ops::slide_preview(
        &artifact.bytes,
        PREVIEW_DOC_MAX_SLIDES,
        PREVIEW_DOC_MAX_SLIDE_CHARS,
    )?;
    let mut truncated = preview.truncated;
    let mut total_chars = 0usize;
    let mut slides = Vec::with_capacity(preview.slides.len());
    for slide in preview.slides {
        if total_chars >= PREVIEW_DOC_MAX_TOTAL_CHARS {
            truncated = true;
            break;
        }
        let remaining = PREVIEW_DOC_MAX_TOTAL_CHARS - total_chars;
        let mut text = slide.text;
        let mut slide_truncated = slide.truncated;
        if text.chars().count() > remaining {
            text = truncate_chars(&text, remaining);
            slide_truncated = true;
        }
        total_chars += text.chars().count();
        truncated |= slide_truncated;
        slides.push(json!({
            "slide": slide.slide,
            "title": slide.title,
            "text": text,
            "truncated": slide_truncated,
        }));
    }
    Ok(json!({
        "hash": artifact.hash,
        "filename": artifact.filename,
        "title": artifact.meta.title,
        "kind": "pptx",
        "slide_count": preview.slide_count,
        "slides": slides,
        "truncated": truncated,
    }))
}

fn xlsx_preview_doc(artifact: &StoredArtifact) -> Result<Value> {
    let workbook = crate::workbooks::import(&artifact.bytes)?;
    let names = workbook.sheet_names();
    let sheet_count = names.len();
    let mut truncated = sheet_count > PREVIEW_DOC_MAX_SHEETS;
    let mut sheets = Vec::with_capacity(names.len().min(PREVIEW_DOC_MAX_SHEETS));
    for name in names.iter().take(PREVIEW_DOC_MAX_SHEETS) {
        let used = workbook
            .sheets
            .iter()
            .find(|sheet| sheet.name == *name)
            .and_then(|sheet| sheet.used_range.as_deref());
        let sheet_json = xlsx_sheet_preview(&artifact.bytes, name, used);
        if sheet_json["truncated"].as_bool().unwrap_or(false) {
            truncated = true;
        }
        sheets.push(sheet_json);
    }
    Ok(json!({
        "hash": artifact.hash,
        "filename": artifact.filename,
        "title": artifact.meta.title,
        "kind": "xlsx",
        "sheet_count": sheet_count,
        "sheets": sheets,
        "truncated": truncated,
    }))
}

/// Parse an `A1`-style address into 1-based (column, row).
fn parse_a1(address: &str) -> Option<(usize, usize)> {
    let mut col = 0usize;
    let mut row = 0usize;
    let mut seen_col = false;
    for c in address.chars() {
        if c.is_ascii_alphabetic() {
            col = col * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
            seen_col = true;
        } else if c.is_ascii_digit() {
            row = row * 10 + (c as usize - '0' as usize);
        } else {
            return None;
        }
    }
    (seen_col && row > 0).then_some((col, row))
}

/// 1-based column number to letters (`1` → `A`, `27` → `AA`).
fn col_letters(mut col: usize) -> String {
    let mut letters = Vec::new();
    while col > 0 {
        col -= 1;
        letters.push(b'A' + (col % 26) as u8);
        col /= 26;
    }
    letters.reverse();
    String::from_utf8_lossy(&letters).into_owned()
}

fn a1_address(col: usize, row: usize) -> String {
    format!("{}{}", col_letters(col), row)
}

/// Intersect a sheet's used range (e.g. `B2:D10`, or a single cell `A1`)
/// with the preview window (`A1:T20`). Returns the window to read as
/// 1-based (start column, start row, end column, end row), and whether any
/// content sits outside it. `None` means the sheet is empty, its used range
/// is unreadable, or its content is entirely outside the window.
fn preview_window(used: &str) -> (Option<(usize, usize, usize, usize)>, bool) {
    let (sc, sr, ec, er) = match used.split_once(':') {
        Some((from, to)) => match (parse_a1(from), parse_a1(to)) {
            (Some(from), Some(to)) => (from.0, from.1, to.0, to.1),
            _ => return (None, false),
        },
        None => match parse_a1(used) {
            Some((col, row)) => (col, row, col, row),
            None => return (None, false),
        },
    };
    let (wsc, wsr) = (sc.max(1), sr.max(1));
    let (wec, wer) = (ec.min(PREVIEW_DOC_MAX_COLS), er.min(PREVIEW_DOC_MAX_ROWS));
    if wsc > wec || wsr > wer {
        // Content exists, but entirely outside the first 20 rows/columns.
        return (None, true);
    }
    let cut = ec > PREVIEW_DOC_MAX_COLS || er > PREVIEW_DOC_MAX_ROWS;
    (Some((wsc, wsr, wec, wer)), cut)
}

/// One sheet's first rows: the used range intersected with `A1:T20`, read
/// through the bounded `workbooks::read_range` (formulas are never
/// evaluated; only the cached value is reported). A sheet that cannot be
/// read degrades to empty rows with `truncated: true`, like the artifact
/// preview does.
fn xlsx_sheet_preview(bytes: &[u8], name: &str, used_range: Option<&str>) -> Value {
    let empty = |truncated: bool| {
        json!({
            "name": name,
            "range": Value::Null,
            "rows": [],
            "truncated": truncated,
        })
    };
    let (window, cut) = used_range.map_or((None, false), preview_window);
    let Some((sc, sr, ec, er)) = window else {
        return empty(cut);
    };
    let escaped = name.replace('\'', "''");
    let spec = format!("'{escaped}'!{}:{}", a1_address(sc, sr), a1_address(ec, er));
    let Ok(read) = crate::workbooks::read_range(bytes, &spec) else {
        return empty(true);
    };
    let mut rows: Vec<Vec<Value>> = vec![vec![Value::Null; ec - sc + 1]; er - sr + 1];
    let mut clipped = read.truncated_values > 0;
    for cell in &read.cells {
        let Some((col, row)) = parse_a1(&cell.address) else {
            continue;
        };
        if !(sc..=ec).contains(&col) || !(sr..=er).contains(&row) {
            continue;
        }
        let value = cell.value.clone().or_else(|| cell.cached.clone());
        let value = match value {
            Some(Value::String(text)) => {
                if text.chars().count() > PREVIEW_DOC_MAX_CELL_CHARS {
                    clipped = true;
                }
                Value::String(truncate_chars(&text, PREVIEW_DOC_MAX_CELL_CHARS))
            }
            Some(other) => other,
            None => Value::Null,
        };
        rows[row - sr][col - sc] = value;
    }
    json!({
        "name": read.sheet,
        "range": read.range,
        "rows": rows,
        "truncated": clipped || cut,
    })
}

/// `GET /v1/library/thumbnail?hash=…&w=320`: a downscaled image for one of
/// the acting user's own artifacts, for the Library grid.
///
/// PNG/JPEG/WebP are decoded with the `image` crate (already a dependency)
/// and re-encoded to fit within `w`×`w` px — JPEG for opaque pictures, PNG
/// when the picture has an alpha channel — re-encoding also drops EXIF/GPS
/// metadata. A picture that already fits is served untouched. Formats the
/// decoder cannot read (GIF, BMP, SVG) are served as-is with the same cache
/// headers, so the desktop can scale them with CSS. Anything else is a 400;
/// a foreign hash is a 404, a malformed hash is a 400.
///
/// The bytes are per-user and served only behind the session token, so the
/// cache is `private`, never shared.
async fn thumbnail(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<ThumbnailQuery>,
) -> Response {
    if let Err(error) = ensure_enabled(&state) {
        return error.into_response();
    }
    let Some(hash) = query
        .hash
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return bad_request("hash is required");
    };
    let width = match query.w.as_deref().map(str::trim) {
        None | Some("") => THUMBNAIL_DEFAULT_W,
        Some(raw) => match raw.parse::<u32>() {
            Ok(width) => width.clamp(THUMBNAIL_MIN_W, THUMBNAIL_MAX_W),
            Err(_) => return bad_request("w must be a number of pixels"),
        },
    };
    let artifact = match own_artifact(&state, &user, hash) {
        Ok(artifact) => artifact,
        Err(response) => return *response,
    };
    let extension = artifact_extension(&artifact.filename);
    if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp") {
        if !matches!(extension.as_str(), "gif" | "bmp" | "svg") {
            return bad_request("no thumbnail for this artifact type");
        }
        let mime = crate::util::mime_for_filename(&artifact.filename);
        return thumbnail_response(mime, artifact.bytes);
    }
    if artifact.bytes.len() > crate::image_ops::MAX_IMAGE_BYTES {
        return bad_request("image exceeds the 50 MiB thumbnail limit");
    }
    let mime = crate::util::mime_for_filename(&artifact.filename);
    match make_thumbnail(&artifact.bytes, mime, width) {
        Ok((bytes, content_type)) => thumbnail_response(content_type, bytes),
        Err(error) => error.into_response(),
    }
}

/// Downscale an image to fit within `width`×`width`, keeping the aspect
/// ratio and never upscaling. Returns the thumbnail bytes and content type.
/// A picture that already fits is returned untouched, so no quality is lost.
fn make_thumbnail(bytes: &[u8], mime: &'static str, width: u32) -> Result<(Vec<u8>, &'static str)> {
    let image = crate::image_ops::decode_oriented(bytes)?;
    if image.width() <= width && image.height() <= width {
        return Ok((bytes.to_vec(), mime));
    }
    let small = image.thumbnail(width, width);
    if small.has_alpha() {
        let mut out = Vec::new();
        small
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .map_err(|_| SeraError::Validation("could not encode the thumbnail".into()))?;
        Ok((out, "image/png"))
    } else {
        let rgb = image::DynamicImage::ImageRgb8(small.to_rgb8());
        let mut out = Vec::new();
        let encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, THUMBNAIL_JPEG_QUALITY);
        rgb.write_with_encoder(encoder)
            .map_err(|_| SeraError::Validation("could not encode the thumbnail".into()))?;
        Ok((out, "image/jpeg"))
    }
}

fn thumbnail_response(content_type: &str, bytes: Vec<u8>) -> Response {
    (
        [
            ("content-type", content_type),
            ("cache-control", "private, max-age=86400"),
            ("x-content-type-options", "nosniff"),
            ("content-disposition", "inline"),
        ],
        Body::from(bytes),
    )
        .into_response()
}
