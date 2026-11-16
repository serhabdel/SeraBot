//! `/v1/handoffs`: the subagent ledger of the session user (v0.50).

use super::*;
use crate::delegate::{ResumeOptions, resume_handoff};

#[derive(Debug, Deserialize)]
pub(super) struct HandoffsQuery {
    #[serde(default)]
    conversation_id: Option<String>,
}

/// `GET /v1/handoffs?conversation_id=`: the session user's subagent handoffs,
/// newest first, with the live status, route, outcome code and resume count of
/// each. Another user's handoffs are never listed.
pub(super) async fn list_handoffs(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<HandoffsQuery>,
) -> Result<Json<Value>> {
    if let Some(conversation) = &query.conversation_id {
        crate::orchestration::validate_conversation_id(conversation)?;
    }
    let handoffs = state
        .store
        .list_handoffs(&user.id, query.conversation_id.as_deref(), 100)?;
    Ok(Json(json!({ "handoffs": handoffs })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RetryHandoffRequest {
    /// Run the retry on this route instead of the bot's own chain. Checked
    /// against the live catalog and the provider's egress approval.
    #[serde(default)]
    route_override: Option<crate::config::BotRoute>,
    /// Retry even though the earlier attempt may have had an effect with an
    /// unknown outcome. The owner takes responsibility for repeating it.
    #[serde(default)]
    confirm_effects_unknown: bool,
}

/// `POST /v1/handoffs/{id}/retry`: the owner's action to run a failed
/// subagent again. A handoff that is not failed, or that is another user's,
/// is refused.
pub(super) async fn retry_handoff(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    body: Option<Json<RetryHandoffRequest>>,
) -> Result<Json<Value>> {
    let request = body.map(|Json(request)| request).unwrap_or_default();
    let view = state.store.handoff_view(&user.id, &id)?;
    if let Some(route) = &request.route_override {
        for text in [&route.provider, &route.model] {
            state.runtime.scan_owner_output(&user.id, text)?;
        }
        state.runtime.ensure_model_catalogs(&user.id).await;
    }
    let bot = state
        .runtime
        .bot(&user.id, &view.to_bot)
        .ok_or_else(|| SeraError::NotFound(format!("bot {}", view.to_bot)))?;
    let resumed = resume_handoff(
        &state.store,
        state.runtime.router(),
        &bot.config,
        &user.id,
        &id,
        &ResumeOptions {
            actor: &user.id,
            tool: None,
            task_id: None,
            route: request.route_override.as_ref(),
            max_resumes: None,
            accept_unknown_effects: request.confirm_effects_unknown,
            daily_limit: state.config.users.child_daily_limit,
        },
    )?;
    Ok(Json(json!({ "handoff": resumed })))
}
