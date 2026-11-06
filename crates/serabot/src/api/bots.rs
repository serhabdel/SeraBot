use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateBotRequest {
    id: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    skills: Option<Vec<String>>,
}

/// Legacy path labels returned to the desktop client, which validates their
/// shape. The database row is authoritative; these are not files.
pub(super) fn bot_file_label(id: &str) -> String {
    format!("db/bots/{id}.toml")
}

pub(super) fn prompt_file_label(id: &str) -> String {
    format!("db/prompts/{id}.txt")
}

/// `POST /v1/bots` — scaffold a disabled specialist as a row owned by the
/// session user. The fixed template is shared; the description, tools, and
/// skills are validated and policy-scanned before the insert.
pub(super) async fn create_bot(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<CreateBotRequest>,
) -> Result<Json<Value>> {
    state
        .runtime
        .scan_owner_output(&user.id, &request.description)?;
    let fleet = state.runtime.fleet();
    let known = fleet
        .catalog()
        .effective(&state.store, &user.id)?
        .skill_ids();
    let scaffolded = build_specialist_bot(
        &request.id,
        &request.description,
        request.tools.as_deref(),
        request.skills.as_deref(),
        &known,
        state.runtime.policy(),
    )?;
    if state
        .runtime
        .fleet()
        .row(&user.id, &scaffolded.id)?
        .is_some()
    {
        return Err(SeraError::InvalidState("bot already exists".into()));
    }
    let config = serde_json::to_value(&scaffolded.config)?;
    if !state
        .store
        .insert_bot(&user.id, &scaffolded.id, &config, &scaffolded.prompt, false)?
    {
        return Err(SeraError::InvalidState("bot already exists".into()));
    }
    state.runtime.fleet().invalidate(&user.id);
    if let Err(error) = state.store.record_audit(
        &user.id,
        "bot.scaffolded",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({
            "id": scaffolded.id,
            "enabled": false,
            "role": "specialist",
            "tools": scaffolded.tools,
            "skills": scaffolded.skills,
            "scope": "user",
        })),
    ) {
        let _ = state.store.delete_bot(&user.id, &scaffolded.id);
        state.runtime.fleet().invalidate(&user.id);
        return Err(error);
    }
    Ok(Json(json!({
        "bot": {
            "id": scaffolded.id,
            "enabled": false,
            "role": "specialist",
            "live": false,
            "tools": scaffolded.tools,
            "skills": scaffolded.skills,
            "require_approval": scaffolded.require_approval,
            "bot_file": bot_file_label(&scaffolded.id),
            "prompt_file": prompt_file_label(&scaffolded.id),
        }
    })))
}

pub(super) async fn list_bots(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let configured = state.runtime.fleet().configured(&user.id)?;
    let bots = configured
        .into_iter()
        .map(|bot| {
            let id = bot.id.clone();
            let profile = if user.is_admin() {
                state
                    .admin
                    .as_ref()
                    .and_then(|admin| admin.bot_profile(&id))
            } else {
                None
            };
            let (tools, skills, require_approval) = if user.is_admin() {
                bot_lists_with_override(&state, &bot)
            } else {
                (
                    bot.tools.clone(),
                    bot.skills.clone(),
                    bot.require_approval.clone(),
                )
            };
            json!({
                "id": id,
                "role": bot.role,
                "enabled": bot.enabled,
                "live": state.runtime.bot_loaded(&user.id, &bot.id),
                "model": bot.model,
                "display_name": profile.as_ref().map(|p| p.display_name.clone()).filter(|name| !name.is_empty()),
                "description": profile.as_ref().map(|p| p.description.clone()).filter(|text| !text.is_empty()),
                "avatar": profile.as_ref().map(|p| p.avatar.clone()).filter(|avatar| !avatar.is_empty()),
                "tools": tools,
                "skills": skills,
                "require_approval": require_approval,
            })
        })
        .collect::<Vec<_>>();
    let body = json!({ "bots": bots });
    state
        .runtime
        .scan_owner_output(&user.id, &body.to_string())?;
    Ok(Json(body))
}

/// Bot config lists with the admin-console overlay merged over the admin's
/// row, so the console sees values it just saved even before the restart.
/// Only called for the admin; members never see the overlay.
pub(super) fn bot_lists_with_override(
    state: &ApiState,
    entry: &crate::config::ConfiguredBot,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut tools = entry.tools.clone();
    let mut skills = entry.skills.clone();
    let mut require_approval = entry.require_approval.clone();
    if let Some(admin) = &state.admin
        && let Some(overlay) = admin.bot_config(&entry.id)
    {
        if let Some(value) = overlay.tools {
            tools = value;
        }
        if let Some(value) = overlay.skills {
            skills = value;
        }
        if let Some(value) = overlay.require_approval {
            require_approval = value;
        }
    }
    (tools, skills, require_approval)
}

/// Cap for the `instructions` field of `GET /v1/bots/{id}`.
pub(super) const MAX_INSTRUCTIONS_BYTES: usize = 32 * 1024;
/// Spec cap for a bot system prompt.
pub(super) const MAX_BOT_PROMPT_BYTES: usize = 100_000;

pub(super) fn parse_bot_config(row: &crate::store::StoredBot) -> Result<BotConfig> {
    let mut config: BotConfig = serde_json::from_value(row.config.clone())
        .map_err(|_| SeraError::InvalidState(format!("bot '{}' config is invalid", row.name)))?;
    config.name = row.name.clone();
    config.enabled = row.enabled;
    Ok(config)
}

/// `GET /v1/bots/{id}` — one bot's full owner-facing detail. No secrets and no
/// secret values; the response passes the egress scan. Another user's bot is a
/// 404.
pub(super) async fn bot_detail(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    if !safe_bot_label(&id) {
        return Err(SeraError::NotFound(format!("bot {id}")));
    }
    let row = state
        .runtime
        .fleet()
        .row(&user.id, &id)?
        .ok_or_else(|| SeraError::NotFound(format!("bot {id}")))?;
    let configured = state.runtime.fleet().configured(&user.id)?;
    let Some(entry) = configured.iter().find(|bot| bot.id == id) else {
        return Err(SeraError::NotFound(format!("bot {id}")));
    };
    let profile = if user.is_admin() {
        state
            .admin
            .as_ref()
            .and_then(|admin| admin.bot_profile(&id))
    } else {
        None
    };
    // The desktop detail record expects strings, so an absent profile falls
    // back to the bot id and empty text instead of null.
    let display_name = profile
        .as_ref()
        .map(|p| p.display_name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| id.clone());
    let description = profile
        .as_ref()
        .map(|p| p.description.clone())
        .filter(|text| !text.is_empty())
        .unwrap_or_default();
    let avatar = profile
        .as_ref()
        .map(|p| p.avatar.clone())
        .filter(|avatar| !avatar.is_empty())
        .unwrap_or_default();
    let loaded = state.runtime.bot(&user.id, &id);
    let model_hint = loaded
        .as_ref()
        .and_then(|bot| bot.config.model.clone())
        .or_else(|| state.config.llm.bot_models.get(&id).cloned())
        .or_else(|| entry.model.clone());
    let model = model_hint.unwrap_or_else(|| state.config.llm.model.clone());
    let provider = state
        .runtime
        .router()
        .provider_for_model(&model)
        .unwrap_or_else(|| "default".into());
    let fallback = state.runtime.router().fallback_for_model(&model);
    let routing_classes = state.runtime.router().classes_for_model(&model);
    let (tools, skills, require_approval) = if user.is_admin() {
        bot_lists_with_override(&state, entry)
    } else {
        (
            entry.tools.clone(),
            entry.skills.clone(),
            entry.require_approval.clone(),
        )
    };
    let prompt = row.prompt;
    let (instructions, instructions_bytes) = if prompt.len() <= MAX_INSTRUCTIONS_BYTES {
        (Some(prompt.clone()), prompt.len())
    } else {
        // Over the cap the content is withheld entirely; the byte count is kept.
        (None, prompt.len())
    };
    let memory_count = state.store.count_memories(&user.id)?;
    let artifact_count = state.store.count_bot_artifacts(&user.id, &id)?;
    let body = json!({
        "id": entry.id,
        "display_name": display_name,
        "role": entry.role,
        "description": description,
        "avatar": avatar,
        "enabled": entry.enabled,
        "provider": provider,
        "model": model,
        "fallback_provider": fallback.as_ref().map(|route| route.provider.clone()),
        "fallback_model": fallback.as_ref().map(|route| route.model.clone()),
        "routing_classes": routing_classes,
        "instructions": instructions,
        "instructions_bytes": instructions_bytes,
        "skills": skills,
        "tools": tools,
        "require_approval": require_approval,
        "memory_count": memory_count,
        "artifact_count": artifact_count,
    });
    state
        .runtime
        .scan_owner_output(&user.id, &body.to_string())?;
    Ok(Json(body))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateBotRequest {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    skills: Option<Vec<String>>,
    #[serde(default)]
    require_approval: Option<Vec<String>>,
}

/// `PUT /v1/bots/{id}` — update the session user's bot row. Enable/disable and
/// prompt edits are ordinary scoped DB writes; the fleet cache is invalidated
/// so the next turn sees them. A change that would leave the fleet invalid
/// (for example disabling `main` or a delegate) is rejected before the write.
pub(super) async fn update_bot(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<UpdateBotRequest>,
) -> Result<Json<Value>> {
    if !safe_bot_label(&id) {
        return Err(SeraError::NotFound(format!("bot {id}")));
    }
    if request.enabled.is_none()
        && request.prompt.is_none()
        && request.tools.is_none()
        && request.skills.is_none()
        && request.require_approval.is_none()
    {
        return Err(SeraError::Validation(
            "at least one of enabled, prompt, tools, skills, require_approval is required".into(),
        ));
    }
    // Untrusted input is scanned before it is validated or stored.
    for list in [&request.tools, &request.skills, &request.require_approval]
        .into_iter()
        .flatten()
    {
        for item in list {
            state.runtime.scan_owner_output(&user.id, item)?;
        }
    }
    let row = state
        .runtime
        .fleet()
        .row(&user.id, &id)?
        .ok_or_else(|| SeraError::NotFound(format!("bot {id}")))?;
    let mut config = parse_bot_config(&row)?;

    let prompt_changed = request.prompt.is_some();
    let prompt = match request.prompt {
        Some(prompt) => {
            if prompt.trim().is_empty() {
                return Err(SeraError::Validation("bot prompt must not be empty".into()));
            }
            if prompt.len() > MAX_BOT_PROMPT_BYTES {
                return Err(SeraError::Validation(format!(
                    "bot prompt exceeds {MAX_BOT_PROMPT_BYTES} bytes"
                )));
            }
            if prompt
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
            {
                return Err(SeraError::Validation(
                    "bot prompt contains unsupported control characters".into(),
                ));
            }
            state.runtime.scan_owner_output(&user.id, &prompt)?;
            prompt
        }
        None => row.prompt.clone(),
    };
    let tools = match &request.tools {
        Some(tools) => validate_bot_tool_list(&state, tools, config.role, &config.tools)?,
        None => config.tools.clone(),
    };
    let skills = match &request.skills {
        Some(skills) => validate_bot_skill_list(&state, &user.id, skills)?,
        None => config.skills.clone(),
    };
    let require_approval = match &request.require_approval {
        Some(list) => validate_require_approval_list(list, &tools)?,
        None => validate_require_approval_list(&config.require_approval, &tools)?,
    };
    let enabled = request.enabled.unwrap_or(row.enabled);
    if !enabled && config.role == BotRole::Orchestrator {
        return Err(SeraError::Validation(
            "the orchestrator cannot be disabled".into(),
        ));
    }

    // Validate the fleet the write would produce, not just the row.
    let mut candidate = state.runtime.fleet().candidates(&user.id)?;
    let Some(bot) = candidate.iter_mut().find(|bot| bot.config.name == id) else {
        return Err(SeraError::NotFound(format!("bot {id}")));
    };
    bot.config.enabled = enabled;
    bot.config.tools = tools.clone();
    bot.config.skills = skills.clone();
    bot.config.require_approval = require_approval.clone();
    bot.prompt = prompt.clone();
    crate::fleet::FleetRegistry::validate_fleet(&candidate)?;

    config.enabled = enabled;
    config.tools = tools.clone();
    config.skills = skills.clone();
    config.require_approval = require_approval.clone();
    let role = config.role.as_str().to_owned();
    let config = serde_json::to_value(&config)?;
    if !state
        .store
        .update_bot(&user.id, &id, &config, &prompt, enabled)?
    {
        return Err(SeraError::NotFound(format!("bot {id}")));
    }
    state.runtime.fleet().invalidate(&user.id);
    state.store.record_audit(
        &user.id,
        "bot.updated",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({
            "id": id,
            "enabled": enabled,
            "role": role,
            "prompt_changed": prompt_changed,
            "tools_changed": request.tools.is_some(),
            "skills_changed": request.skills.is_some(),
            "require_approval_changed": request.require_approval.is_some(),
        })),
    )?;
    Ok(Json(json!({
        "bot": {
            "id": id,
            "enabled": enabled,
            "role": role,
            "live": state.runtime.bot_loaded(&user.id, &id),
            "tools": tools,
            "skills": skills,
            "require_approval": require_approval,
        }
    })))
}

/// `DELETE /v1/bots/{id}` — remove one specialist owned by the session user.
/// The orchestrator cannot be deleted, and a delete that would leave a
/// delegate dangling is rejected before the write.
pub(super) async fn delete_bot(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    if !safe_bot_label(&id) {
        return Err(SeraError::NotFound(format!("bot {id}")));
    }
    let Some(row) = state.runtime.fleet().row(&user.id, &id)? else {
        return Err(SeraError::NotFound(format!("bot {id}")));
    };
    if row.name == crate::orchestration::ORCHESTRATOR_BOT {
        return Err(SeraError::Validation(
            "the orchestrator cannot be deleted".into(),
        ));
    }
    let mut candidate = state.runtime.fleet().candidates(&user.id)?;
    candidate.retain(|bot| bot.config.name != id);
    crate::fleet::FleetRegistry::validate_fleet(&candidate)?;
    if !state.store.delete_bot(&user.id, &id)? {
        return Err(SeraError::NotFound(format!("bot {id}")));
    }
    state.runtime.fleet().invalidate(&user.id);
    state.store.record_audit(
        &user.id,
        "bot.deleted",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": id, "scope": "user"})),
    )?;
    Ok(Json(json!({"deleted": true, "id": id})))
}
