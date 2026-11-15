//! Active multi-step plans (0.29.0, Wave A3): conversations with incomplete
//! checklists the orchestrator can resume.

use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActiveWorkflowsQuery {
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PutWorkflowPlanRequest {
    #[serde(default)]
    goal: Option<String>,
    #[serde(default)]
    items: Option<Vec<crate::plans::PlanItem>>,
}

fn workflow_json(plan: &crate::plans::Plan) -> Value {
    json!({
        "conversation_id": plan.conversation_id,
        "goal": plan.goal,
        "items": plan.items,
        "done": plan.done_count(),
        "total": plan.items.len(),
        "updated_at": plan.updated_at,
    })
}

fn validate_workflow_text(field: &str, raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 240 || trimmed.chars().any(char::is_control)
    {
        return Err(SeraError::Validation(format!(
            "{field} must be 1..=240 characters with no control characters"
        )));
    }
    Ok(trimmed.to_owned())
}

/// `GET /v1/workflows/active`: incomplete stored plans for the acting user.
pub(super) async fn list_active_workflows(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<ActiveWorkflowsQuery>,
) -> Result<Json<Value>> {
    let limit = query.limit.unwrap_or(100);
    let plans = state.store.list_active_workflow_plans(&user.id, limit)?;
    let workflows = plans.iter().map(workflow_json).collect::<Vec<_>>();
    Ok(Json(json!({"workflows": workflows})))
}

/// `PUT /v1/workflows/active/{conversation_id}`: edit an active workflow's goal or checklist items.
pub(super) async fn put_active_workflow(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(conversation_id): Path<String>,
    Json(body): Json<PutWorkflowPlanRequest>,
) -> Result<Json<Value>> {
    if body.goal.is_none() && body.items.is_none() {
        return Err(SeraError::Validation("no workflow fields to update".into()));
    }
    let existing = state.store.get_plan(&user.id, &conversation_id)?;
    let goal = match body.goal {
        Some(raw) => {
            let clean = validate_workflow_text("goal", &raw)?;
            state.runtime.scan_owner_output(&user.id, &clean)?;
            clean
        }
        None => existing
            .as_ref()
            .ok_or_else(|| SeraError::NotFound(format!("workflow {conversation_id}")))?
            .goal
            .clone(),
    };
    let items = match body.items {
        Some(raw_items) => {
            if raw_items.len() > 32 {
                return Err(SeraError::Validation(
                    "a workflow holds at most 32 items".into(),
                ));
            }
            let mut validated = Vec::with_capacity(raw_items.len());
            for item in raw_items {
                let text = validate_workflow_text("item text", &item.text)?;
                state.runtime.scan_owner_output(&user.id, &text)?;
                validated.push(crate::plans::PlanItem {
                    text,
                    status: item.status,
                });
            }
            validated
        }
        None => existing
            .as_ref()
            .ok_or_else(|| SeraError::NotFound(format!("workflow {conversation_id}")))?
            .items
            .clone(),
    };
    let plan = state
        .store
        .set_plan(&user.id, &conversation_id, &goal, &items)?;
    state.store.record_audit(
        &user.id,
        "workflow.updated",
        Some(&conversation_id),
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"done": plan.done_count(), "total": plan.items.len()})),
    )?;
    Ok(Json(json!({"workflow": workflow_json(&plan)})))
}
