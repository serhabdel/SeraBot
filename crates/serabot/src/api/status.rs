use super::*;

pub(super) async fn status(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Response {
    let pending = state.store.pending_task_count(&user.id).unwrap_or(-1);
    let approvals = state
        .store
        .list_pending_approvals(&user.id, 50)
        .map(|items| items.len())
        .unwrap_or(0);
    let approval_items = state
        .store
        .list_pending_approvals(&user.id, 50)
        .unwrap_or_default()
        .into_iter()
        .map(|approval| {
            json!({
                "id": approval.id,
                "tool": approval.tool,
                "summary": approval.summary,
                "expires_at": approval.expires_at,
                "action_id": approval.action_id,
            })
        })
        .collect::<Vec<_>>();
    Json(json!({
        "ok": true,
        "pending_tasks": pending,
        "pending_approvals": approvals,
        "approvals": approval_items,
        "policy_version": "managed-by-server",
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub(super) struct UsageQuery {
    #[serde(default)]
    days: Option<i64>,
    #[serde(default)]
    bot: Option<String>,
    #[serde(default)]
    conversation_id: Option<String>,
}

/// `GET /v1/usage` rolls up `model.used` audit rows. `days` defaults to 7 and
/// is clamped to 1..=90; `bot` and `conversation_id` are optional filters.
pub(super) async fn usage(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<UsageQuery>,
) -> Result<Json<Value>> {
    let days = query.days.unwrap_or(7).clamp(1, 90);
    if let Some(bot) = &query.bot
        && !safe_bot_label(bot)
    {
        return Err(SeraError::Validation("invalid bot id".into()));
    }
    if let Some(conversation_id) = &query.conversation_id {
        validate_conversation_id(conversation_id)?;
    }
    let since = Utc::now() - chrono::Duration::days(days);
    let rollup = state.store.usage_rollup(
        &user.id,
        since,
        query.bot.as_deref(),
        query.conversation_id.as_deref(),
    )?;
    let body = json!({
        "totals": rollup.totals,
        "per_day": rollup.per_day,
        "per_bot": rollup.per_bot,
    });
    // Same egress scan as the model catalog endpoints.
    state
        .runtime
        .scan_owner_output(&user.id, &body.to_string())?;
    Ok(Json(body))
}
