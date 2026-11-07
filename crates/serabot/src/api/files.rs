//! Chat file transfer: uploads from the composer and authenticated downloads.
//!
//! Every route here is scoped to the acting user. Uploads land in the user's
//! own inbox area; downloads only serve files the user uploaded or artifacts
//! produced inside the user's own conversations.

use axum::extract::DefaultBodyLimit;

use super::*;
use crate::attachments::{MAX_ATTACHMENT_BYTES, save_upload};

pub(super) fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route(
            "/v1/conversations/{id}/attachments",
            post(upload_attachment),
        )
        // Raw body, read explicitly under the attachment ceiling; the extra
        // 4 KiB lets the handler produce the readable "too large" error.
        .layer(DefaultBodyLimit::max(MAX_ATTACHMENT_BYTES as usize + 4096))
        .layer(RequestBodyLimitLayer::new(
            MAX_ATTACHMENT_BYTES as usize + 4096,
        ))
        .route("/v1/attachments/{id}", get(download_attachment))
}

fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": message, "code": "validation"})),
    )
        .into_response()
}

/// Raw file upload. `X-Filename` carries the file name (percent-encoding is
/// not used; the name must be plain).
pub(super) async fn upload_attachment(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if validate_conversation_id(&id).is_err() {
        return bad_request("invalid conversation id");
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
    // The conversation must be the acting user's (or creatable by them).
    if let Err(error) = state.store.ensure_conversation_accepts_turns(&user.id, &id) {
        return error.into_response();
    }
    // The blocklist applies to filenames.
    if state
        .runtime
        .scan_owner_output(&user.id, &filename)
        .is_err()
    {
        return SeraError::PolicyViolation.into_response();
    }
    let limit = MAX_ATTACHMENT_BYTES as usize;
    let bytes = match axum::body::to_bytes(body, limit).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({
                    "error": format!(
                        "attachment exceeds the {} MB limit",
                        MAX_ATTACHMENT_BYTES / (1024 * 1024)
                    ),
                    "code": "validation",
                })),
            )
                .into_response();
        }
    };
    let result = save_upload(
        &state.config,
        &state.store,
        &user.id,
        &id,
        &filename,
        &bytes,
    );
    match result {
        Ok(record) => {
            let _ = state.store.record_audit(
                &user.id,
                "chat.attachment_upload",
                Some(&record.id),
                None,
                None,
                Some(&record.sha256),
                None,
                Some(&json!({"bytes": record.size_bytes, "mime": record.mime})),
            );
            Json(json!({"attachment": record.view()})).into_response()
        }
        Err(SeraError::Validation(message)) => bad_request(&message),
        Err(error) => error.into_response(),
    }
}

/// Download a file the acting user uploaded.
pub(super) async fn download_attachment(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Response> {
    let record = state
        .store
        .get_chat_attachment(&user.id, &id)?
        .ok_or_else(|| SeraError::NotFound("attachment".into()))?;
    let root = state.config.path(&state.config.documents.watch_dir);
    let prefix = format!("{}/chat/", user.id);
    if !record.rel_path.starts_with(&prefix) || record.rel_path.contains("..") {
        return Err(SeraError::Forbidden("attachment path".into()));
    }
    let bytes = std::fs::read(root.join(&record.rel_path))
        .map_err(|_| SeraError::NotFound("attachment".into()))?;
    Ok(download_response(&record.filename, bytes))
}

pub(super) fn download_response(filename: &str, bytes: Vec<u8>) -> Response {
    let filename = filename.replace(['"', '\\', '\r', '\n'], "-");
    let mime = crate::util::mime_for_filename(&filename);
    (
        [
            ("content-type", mime),
            ("x-content-type-options", "nosniff"),
            (
                "content-disposition",
                &format!("attachment; filename=\"{filename}\""),
            ),
        ],
        Body::from(bytes),
    )
        .into_response()
}
