//! The member's Library API (`/v1/library/*`, 0.74.8): one cross-chat list of
//! every file a bot made in the member's conversations plus the member's own
//! uploads, and a raw-bytes upload that lands in the per-user artifact store
//! (so the existing per-user artifact download/preview endpoints serve it) and
//! is mirrored, best-effort, to the member's own OneDrive (`[library]`).
//!
//! Every route is scoped to the acting user; another member's library items
//! are never listed, and an upload is never visible to anyone else.

use axum::body::Body;
use axum::extract::DefaultBodyLimit;

use super::*;
use crate::library::{LIBRARY_SCOPE, LIBRARY_UPLOAD_TOOL, LibraryMirror};
use crate::maker::{ArtifactMeta, MAX_ARTIFACT_BYTES};
use crate::store::LibraryItemRow;

pub(super) fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/v1/library/upload", post(upload))
        // Raw body, read explicitly under the artifact ceiling; the extra
        // 4 KiB lets the handler produce the readable "too large" error.
        .layer(DefaultBodyLimit::max(MAX_ARTIFACT_BYTES + 4096))
        .layer(RequestBodyLimitLayer::new(MAX_ARTIFACT_BYTES + 4096))
        .route("/v1/library", get(list))
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
