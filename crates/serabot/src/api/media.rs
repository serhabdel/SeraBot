//! Background media jobs (0.24): the acting user's jobs in one conversation,
//! so the chat can show a placeholder while a render is still running.

use super::*;

/// `GET /v1/conversations/{id}/media-jobs`: the owner's jobs in one
/// conversation, oldest first. Another user's jobs are never listed.
pub(super) async fn list_conversation_media_jobs(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    validate_conversation_id(&id)?;
    let jobs = state.store.list_media_jobs(&user.id, &id)?;
    Ok(Json(json!({"media_jobs": jobs})))
}
