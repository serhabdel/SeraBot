use super::*;
use crate::browser_profiles::{MAX_BOT_DESCRIPTION_CHARS, MAX_BOT_DISPLAY_NAME_CHARS};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateBotRequest {
    /// Explicit slug (older clients). New clients send `name` and the server
    /// derives the id.
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: String,
    /// Model id from the catalog; the provider follows from it.
    #[serde(default)]
    model: Option<String>,
    /// Room the bot joins once the owner confirms its onboarding.
    #[serde(default)]
    group: Option<String>,
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
    let bot_id = match (request.id.as_deref(), request.name.as_deref()) {
        (Some(id), _) => id.to_owned(),
        (None, Some(name)) => {
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 60 {
                return Err(SeraError::Validation(
                    "the name must be 1 to 60 characters".into(),
                ));
            }
            state.runtime.scan_owner_output(&user.id, name)?;
            let base = super::profiles::slugify(name, 32, "bot");
            super::profiles::unique_slug(&base, 32, |candidate| {
                Ok(candidate == crate::orchestration::ORCHESTRATOR_BOT
                    || fleet.row(&user.id, candidate)?.is_some())
            })?
        }
        (None, None) => return Err(SeraError::Validation("a name is required".into())),
    };
    let mut scaffolded = build_specialist_bot(
        &bot_id,
        &request.description,
        request.tools.as_deref(),
        request.skills.as_deref(),
        &known,
        state.runtime.policy(),
    )?;
    if let Some(model) = request.model.as_deref().filter(|m| !m.is_empty()) {
        state.runtime.scan_owner_output(&user.id, model)?;
        if state.runtime.router().provider_for_model(model).is_none() {
            return Err(SeraError::Validation("unknown model".into()));
        }
        scaffolded.config.model = Some(model.to_owned());
    }
    if let Some(group) = request.group.as_deref() {
        team_conversation(&state, &user, group)?;
    }
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
    if let Err(error) = state.store.put_onboarding(
        &user.id,
        &scaffolded.id,
        &super::profiles::new_interview_row(&scaffolded.id, request.group.clone())?,
    ) {
        let _ = state.store.delete_bot(&user.id, &scaffolded.id);
        state.runtime.fleet().invalidate(&user.id);
        return Err(error);
    }
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
        let _ = state
            .store
            .delete_bot_profile_rows(&user.id, &scaffolded.id);
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
    let avatar_versions = state.store.bot_avatar_versions(&user.id)?;
    let mut settings_by_bot = HashMap::new();
    let mut profile_by_bot = HashMap::new();
    for bot in &configured {
        settings_by_bot.insert(bot.id.clone(), state.store.bot_settings(&user.id, &bot.id)?);
        profile_by_bot.insert(
            bot.id.clone(),
            state.store.bot_browser_profile(&user.id, &bot.id)?,
        );
    }
    let bots = configured
        .into_iter()
        .map(|bot| {
            let id = bot.id.clone();
            let settings = settings_by_bot.remove(&id).unwrap_or_default();
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
                "display_name": settings.display_name.clone().or_else(|| profile.as_ref().map(|p| p.display_name.clone()).filter(|name| !name.is_empty())),
                "description": settings.description.clone().or_else(|| profile.as_ref().map(|p| p.description.clone()).filter(|text| !text.is_empty())),
                "browser_profile": profile_by_bot.get(&id).cloned().unwrap_or_else(|| "default".into()),
                "avatar": profile.as_ref().map(|p| p.avatar.clone()).filter(|avatar| !avatar.is_empty()),
                "avatar_version": avatar_versions.get(&id),
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
    let settings = state.store.bot_settings(&user.id, &id)?;
    let display_name = settings
        .display_name
        .clone()
        .or_else(|| {
            profile
                .as_ref()
                .map(|p| p.display_name.clone())
                .filter(|name| !name.is_empty())
        })
        .unwrap_or_else(|| id.clone());
    let description = settings
        .description
        .clone()
        .or_else(|| {
            profile
                .as_ref()
                .map(|p| p.description.clone())
                .filter(|text| !text.is_empty())
        })
        .unwrap_or_default();
    let browser_profile = state.store.bot_browser_profile(&user.id, &id)?;
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
        "browser_profile": browser_profile,
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
    pub(super) enabled: Option<bool>,
    #[serde(default)]
    pub(super) prompt: Option<String>,
    #[serde(default)]
    pub(super) tools: Option<Vec<String>>,
    #[serde(default)]
    pub(super) skills: Option<Vec<String>>,
    #[serde(default)]
    pub(super) require_approval: Option<Vec<String>>,
    /// 1-48 characters.
    #[serde(default)]
    pub(super) display_name: Option<String>,
    /// 0-280 characters; empty clears it.
    #[serde(default)]
    pub(super) description: Option<String>,
    /// `{"provider","model"}`, or `null` for the default model.
    #[serde(default, deserialize_with = "double_option")]
    pub(super) model: Option<Option<ModelChoice>>,
    /// A browser profile id, or `null` for the default profile.
    #[serde(default, deserialize_with = "double_option")]
    pub(super) browser_profile: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ModelChoice {
    provider: String,
    model: String,
}

/// Distinguishes an absent field (`None`) from an explicit `null` (`Some(None)`).
fn double_option<'de, T, D>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
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
        && request.display_name.is_none()
        && request.description.is_none()
        && request.model.is_none()
        && request.browser_profile.is_none()
    {
        return Err(SeraError::Validation(
            "at least one of enabled, prompt, tools, skills, require_approval, display_name, description, model, browser_profile is required".into(),
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

    // Owner-editable profile fields (0.16): validated and scanned before any write.
    let mut settings = state.store.bot_settings(&user.id, &id)?;
    let settings_changed = request.display_name.is_some()
        || request.description.is_some()
        || request.browser_profile.is_some();
    if let Some(name) = &request.display_name {
        let name = name.trim();
        let count = name.chars().count();
        if count == 0 || count > MAX_BOT_DISPLAY_NAME_CHARS || name.chars().any(char::is_control) {
            return Err(SeraError::Validation(format!(
                "display_name must be 1 to {MAX_BOT_DISPLAY_NAME_CHARS} characters"
            )));
        }
        state.runtime.scan_owner_output(&user.id, name)?;
        settings.display_name = Some(name.to_owned());
    }
    if let Some(description) = &request.description {
        let description = description.trim();
        if description.chars().count() > MAX_BOT_DESCRIPTION_CHARS
            || description
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return Err(SeraError::Validation(format!(
                "description must be at most {MAX_BOT_DESCRIPTION_CHARS} characters"
            )));
        }
        state.runtime.scan_owner_output(&user.id, description)?;
        settings.description = (!description.is_empty()).then(|| description.to_owned());
    }
    if let Some(choice) = &request.browser_profile {
        settings.browser_profile = match choice.as_deref() {
            None | Some(crate::browser_profiles::DEFAULT_PROFILE) => None,
            Some(profile) => {
                if !crate::browser_profiles::valid_profile_id(profile)
                    || state
                        .store
                        .get_browser_profile(&user.id, profile)?
                        .is_none()
                {
                    return Err(SeraError::Validation("unknown browser profile".into()));
                }
                Some(profile.to_owned())
            }
        };
    }
    let model_changed = request.model.is_some();
    if let Some(choice) = &request.model {
        config.model = match choice {
            None => None,
            Some(choice) => {
                state
                    .runtime
                    .scan_owner_output(&user.id, &choice.provider)?;
                state.runtime.scan_owner_output(&user.id, &choice.model)?;
                if !state
                    .runtime
                    .router()
                    .provider_serves_model(&choice.provider, &choice.model)
                {
                    return Err(SeraError::Validation(
                        "unknown model for that provider".into(),
                    ));
                }
                Some(choice.model.clone())
            }
        };
    }

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
    bot.config.model = config.model.clone();
    bot.prompt = prompt.clone();
    crate::fleet::FleetRegistry::validate_fleet(&candidate)?;

    config.enabled = enabled;
    config.tools = tools.clone();
    config.skills = skills.clone();
    config.require_approval = require_approval.clone();
    let role = config.role.as_str().to_owned();
    let config_model = config.model.clone();
    let config = serde_json::to_value(&config)?;
    if !state
        .store
        .update_bot(&user.id, &id, &config, &prompt, enabled)?
    {
        return Err(SeraError::NotFound(format!("bot {id}")));
    }
    if settings_changed {
        state.store.put_bot_settings(&user.id, &id, &settings)?;
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
            "profile_changed": settings_changed,
            "model_changed": model_changed,
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
            "display_name": settings.display_name,
            "description": settings.description,
            "model": config_model,
            "browser_profile": settings
                .browser_profile
                .clone()
                .unwrap_or_else(|| crate::browser_profiles::DEFAULT_PROFILE.to_owned()),
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
    state.store.delete_bot_profile_rows(&user.id, &id)?;
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
