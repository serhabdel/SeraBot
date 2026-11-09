//! Active multi-step plans (0.29.0, Wave A3): conversations with incomplete
//! checklists the orchestrator can resume.

use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActiveWorkflowsQuery {
    #[serde(default)]
    limit: Option<usize>,
}

/// `GET /v1/workflows/active`: incomplete stored plans for the acting user.
pub(super) async fn list_active_workflows(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<ActiveWorkflowsQuery>,
) -> Result<Json<Value>> {
    let limit = query.limit.unwrap_or(100);
    let plans = state.store.list_active_workflow_plans(&user.id, limit)?;
    let workflows = plans
        .into_iter()
        .map(|plan| {
            json!({
                "conversation_id": plan.conversation_id,
                "goal": plan.goal,
                "done": plan.done_count(),
                "total": plan.items.len(),
                "updated_at": plan.updated_at,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"workflows": workflows})))
}
