//! Per-user project folders HTTP API (`/v1/projects/*`, v0.59).
//!
//! Every route is strictly scoped to the acting user (`Extension(user)`).
//! Files live under `<watch_dir>/<user_id>/projects/<slug>/` and share the
//! path-safety, slugification, and validation logic in `crate::projects`.

use axum::extract::DefaultBodyLimit;

use super::*;
use crate::attachments::MAX_ATTACHMENT_BYTES;
use crate::projects::{
    create_project, delete_project_file, list_project_files, list_projects_with_stats,
    read_project_file, save_project_upload,
};

pub(super) fn routes() -> Router<Arc<ApiState>> {
    let upload = Router::new()
        .route("/v1/projects/{slug}/upload", post(upload_project_file))
        .layer(DefaultBodyLimit::max(MAX_ATTACHMENT_BYTES as usize + 4096))
        .layer(RequestBodyLimitLayer::new(
            MAX_ATTACHMENT_BYTES as usize + 4096,
        ));
    Router::new()
        .route(
            "/v1/projects",
            get(list_projects).post(create_project_route),
        )
        .route("/v1/projects/{slug}/files", get(list_files))
        .route(
            "/v1/projects/{slug}/files/{*path}",
            get(download_project_file).delete(delete_file),
        )
        .merge(upload)
}

#[derive(Deserialize)]
struct CreateProjectRequest {
    name: String,
}

async fn list_projects(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let projects = list_projects_with_stats(&state.config, &user.id)?;
    Ok(Json(json!({ "projects": projects })))
}

async fn create_project_route(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(body): Json<CreateProjectRequest>,
) -> Result<Json<Value>> {
    if state
        .runtime
        .scan_owner_output(&user.id, &body.name)
        .is_err()
    {
        return Err(SeraError::PolicyViolation);
    }
    let (slug, created) = create_project(&state.config, &user.id, &body.name)?;
    let _ = state.store.record_audit(
        &user.id,
        "project.create",
        Some(&slug),
        None,
        None,
        None,
        None,
        Some(&json!({ "created": created })),
    );
    Ok(Json(json!({ "project": slug, "created": created })))
}

async fn list_files(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(slug): Path<String>,
) -> Result<Json<Value>> {
    let (slug, files) = list_project_files(&state.config, &user.id, &slug)?;
    Ok(Json(json!({ "project": slug, "files": files })))
}

async fn upload_project_file(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(filename) = headers
        .get("x-filename")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "X-Filename header is required", "code": "validation"})),
        )
            .into_response();
    };
    let subfolder = headers
        .get("x-subfolder")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned);
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
                        "file exceeds the {} MB limit",
                        MAX_ATTACHMENT_BYTES / (1024 * 1024)
                    ),
                    "code": "validation",
                })),
            )
                .into_response();
        }
    };
    match save_project_upload(
        &state.config,
        &user.id,
        &slug,
        subfolder.as_deref(),
        &filename,
        &bytes,
    ) {
        Ok(entry) => {
            let _ = state.store.record_audit(
                &user.id,
                "project.upload",
                Some(&entry.inbox_path),
                None,
                None,
                None,
                None,
                Some(&json!({"bytes": entry.size_bytes, "mime": entry.mime})),
            );
            Json(json!({ "file": entry })).into_response()
        }
        Err(SeraError::Validation(message)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": message, "code": "validation"})),
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}

async fn download_project_file(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path((slug, path)): Path<(String, String)>,
) -> Result<Response> {
    let (filename, bytes) = read_project_file(&state.config, &user.id, &slug, &path)?;
    Ok(files::download_response(&filename, bytes))
}

async fn delete_file(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path((slug, path)): Path<(String, String)>,
) -> Result<Json<Value>> {
    let filename = delete_project_file(&state.config, &user.id, &slug, &path)?;
    let _ = state.store.record_audit(
        &user.id,
        "project.delete_file",
        Some(&format!("{slug}/{path}")),
        None,
        None,
        None,
        None,
        None,
    );
    Ok(Json(json!({ "deleted": filename })))
}
