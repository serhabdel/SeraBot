//! Conversation tool timelines (0.29.0, Wave A5): redacted `tool.*` and
//! `approval.*` audit rows for one chat.

use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TracesQuery {
    conversation_id: String,
  #[serde(default)]
  limit: Option<usize>,
}

/// `GET /v1/traces`: hashes, destinations and elapsed times only.
pub(super) async fn list_traces(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<TracesQuery>,
) -> Result<Json<Value>> {
    validate_conversation_id(&query.conversation_id)?;
    if state
        .store
        .get_conversation(&user.id, &query.conversation_id)?
        .is_none()
    {
        return Err(SeraError::NotFound("conversation".into()));
    }
    let limit = query.limit.unwrap_or(100);
    let traces = state.store.list_conversation_traces(
        &user.id,
        &query.conversation_id,
        limit,
    )?;
    Ok(Json(json!({
        "conversation_id": query.conversation_id,
        "traces": traces,
    })))
}
