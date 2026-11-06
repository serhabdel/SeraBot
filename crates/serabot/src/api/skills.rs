use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateSkillRequest {
    id: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    body: Option<String>,
}

pub(super) async fn list_skills(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let fleet = state.runtime.fleet();
    let skills = fleet
        .catalog()
        .effective(&state.store, &user.id)?
        .summaries();
    let body = json!({ "skills": skills });
    state
        .runtime
        .scan_owner_output(&user.id, &body.to_string())?;
    Ok(Json(body))
}

pub(super) async fn create_skill_pack(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<CreateSkillRequest>,
) -> Result<Json<Value>> {
    state.runtime.scan_owner_output(&user.id, &request.id)?;
    state
        .runtime
        .scan_owner_output(&user.id, &request.description)?;
    if let Some(body) = &request.body {
        state.runtime.scan_owner_output(&user.id, body)?;
    }
    let fleet = state.runtime.fleet();
    let created = create_user_skill(
        &state.store,
        &user.id,
        &request.id,
        &request.description,
        request.body.as_deref(),
        state.runtime.policy(),
        fleet.catalog(),
    )?;
    if let Err(error) = state.store.record_audit(
        &user.id,
        "skill.created",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({
            "id": created.id,
            "name": created.name,
            "scope": "user",
        })),
    ) {
        let _ = state.store.delete_user_skill(&user.id, &created.id);
        return Err(error);
    }
    Ok(Json(json!({
        "skill": {
            "id": created.id,
            "name": created.name,
            "description": created.description,
            "file": format!("user_skills/{}/SKILL.md", created.id),
            "live": false,
        }
    })))
}
