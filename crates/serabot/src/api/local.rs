//! Desktop side of the local folder relay (see `local_relay`).
//!
//! The desktop long-polls for work and answers it. All routes act for the
//! authenticated user only.

use super::*;
use crate::local_relay::{LocalRoot, MAX_RESULT_BYTES, RelayResult};

pub(super) fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/v1/local/poll", post(local_poll))
        .route("/v1/local/result/{id}", post(local_result))
        .route("/v1/local/status", get(local_status))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PollRequest {
    #[serde(default)]
    roots: Vec<LocalRoot>,
    #[serde(default)]
    wait_secs: u64,
}

/// Register the desktop's allowlisted roots (id, label, mode) and wait up to
/// `wait_secs` for one request. An empty `roots` list switches local folders
/// off for this user.
pub(super) async fn local_poll(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<PollRequest>,
) -> Result<Json<Value>> {
    let wait = Duration::from_secs(request.wait_secs.min(25));
    let next = state.local.poll(&user.id, request.roots, wait).await?;
    Ok(Json(json!({"request": next})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ResultRequest {
    ok: bool,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

pub(super) async fn local_result(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<ResultRequest>,
) -> Result<Json<Value>> {
    if id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(SeraError::Validation("invalid request id".into()));
    }
    if request.text.as_deref().map_or(0, str::len) > MAX_RESULT_BYTES {
        return Err(SeraError::Validation("local result is too large".into()));
    }
    let accepted = state.local.complete(
        &user.id,
        &id,
        RelayResult {
            ok: request.ok,
            text: request.text,
            error: request.error,
        },
    );
    if !accepted {
        return Err(SeraError::NotFound("local request".into()));
    }
    Ok(Json(json!({"accepted": true})))
}

/// Whether the user's desktop is online and which roots it registered.
pub(super) async fn local_status(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Json<Value> {
    Json(json!({
        "online": state.local.online(&user.id),
        "roots": state.local.roots(&user.id),
    }))
}
