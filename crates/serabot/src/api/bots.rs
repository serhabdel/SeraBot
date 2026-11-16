use super::*;
use crate::default_avatars::default_avatar_version;
use crate::fleet_mutation::{self, BotPatch, ModelChoice, MutationEnv, Origin};

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
                // The owner's own name and text win; the shipped title and
                // description are the fallback before the console uses the id.
                "display_name": settings.display_name.clone().or_else(|| profile.as_ref().map(|p| p.display_name.clone()).filter(|name| !name.is_empty())).or_else(|| bot.title.clone()),
                "title": bot.title,
                "description": settings.description.clone().or_else(|| profile.as_ref().map(|p| p.description.clone()).filter(|text| !text.is_empty())).or_else(|| bot.description.clone()),
                "browser_profile": profile_by_bot.get(&id).cloned().unwrap_or_else(|| "default".into()),
                "avatar": profile.as_ref().map(|p| p.avatar.clone()).filter(|avatar| !avatar.is_empty()),
                // A bundled default counts as a picture, so the console fetches it.
                "avatar_version": avatar_versions.get(&id).cloned().or_else(|| {
                    default_avatar_version(&id)
                }),
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

/// `GET /v1/bots/catalog` — the shipped templates, for "Meet your team". Each
/// entry says whether the session user still has that bot; the installed flag
/// is read from the user's own rows only.
pub(super) async fn bot_catalog(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let entries = state.runtime.fleet().template_catalog(&user.id)?;
    let bots = entries
        .into_iter()
        .map(|entry| {
            json!({
                "id": entry.name,
                "title": entry.title,
                "description": entry.description,
                "role": entry.role,
                "installed": entry.installed,
                "enabled": entry.enabled,
                "avatar_version": default_avatar_version(&entry.name),
            })
        })
        .collect::<Vec<_>>();
    let body = json!({
        "bots": bots,
        "templates_total": bots.len(),
        "installed_total": bots.iter().filter(|bot| bot["installed"] == true).count(),
    });
    state
        .runtime
        .scan_owner_output(&user.id, &body.to_string())?;
    Ok(Json(body))
}

/// `POST /v1/bots/catalog/{name}/install` — "Add to my team": restore one
/// shipped template as the session user's own row, disabled or enabled as the
/// template says. Only that user's rows are written. A name that is not a
/// shipped template is a 404; a bot the user already has is a conflict.
pub(super) async fn install_catalog_bot(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    let fleet = state.runtime.fleet();
    if !safe_bot_label(&name) || !fleet.has_template(&name) {
        return Err(SeraError::NotFound(format!("bot {name}")));
    }
    if !fleet.install_template(&user.id, &name)? {
        return Err(SeraError::InvalidState("bot already exists".into()));
    }
    fleet.invalidate(&user.id);
    state.store.record_audit(
        &user.id,
        "bot.installed",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": name, "scope": "user"})),
    )?;
    let row = fleet
        .row(&user.id, &name)?
        .ok_or_else(|| SeraError::NotFound(format!("bot {name}")))?;
    let template = fleet
        .templates()
        .iter()
        .find(|template| template.config.name == name);
    let body = json!({
        "bot": {
            "id": name,
            "title": template.and_then(|t| t.config.title.clone()),
            "description": template.and_then(|t| t.config.description.clone()),
            "role": template.map(|t| t.config.role.as_str()),
            "enabled": row.enabled,
            "installed": true,
            "avatar_version": default_avatar_version(&name),
        }
    });
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
        .or_else(|| entry.title.clone())
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
        .or_else(|| entry.description.clone())
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
    let thinking = loaded.as_ref().and_then(|bot| bot.config.thinking.clone());
    let thinking_levels = state.runtime.router().thinking_levels(
        &user.id,
        &ModelSelection {
            provider: provider.clone(),
            model: model.clone(),
            account: None,
        },
    );
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
        "title": entry.title,
        "role": entry.role,
        "browser_profile": browser_profile,
        "description": description,
        "avatar": avatar,
        "enabled": entry.enabled,
        "provider": provider,
        "model": model,
        "thinking": thinking,
        "thinking_levels": thinking_levels,
        "fallback_provider": fallback.as_ref().map(|route| route.provider.clone()),
        "fallback_model": fallback.as_ref().map(|route| route.model.clone()),
        "route": route_view(&state, &user.id, &id, loaded.as_ref().map(|bot| &bot.config)),
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
    /// A thinking level (`off`, `low`, ...), or `null` to use the model's
    /// default. Applied only when the chosen model supports it.
    #[serde(default, deserialize_with = "double_option")]
    pub(super) thinking: Option<Option<String>>,
    /// A browser profile id, or `null` for the default profile.
    #[serde(default, deserialize_with = "double_option")]
    pub(super) browser_profile: Option<Option<String>>,
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
/// The change runs through `fleet_mutation::apply`, the same path the main
/// bot's management tools use.
pub(super) async fn update_bot(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<UpdateBotRequest>,
) -> Result<Json<Value>> {
    if !safe_bot_label(&id) {
        return Err(SeraError::NotFound(format!("bot {id}")));
    }
    let patch = BotPatch {
        enabled: request.enabled,
        prompt: request.prompt,
        tools: request.tools,
        skills: request.skills,
        require_approval: request.require_approval,
        display_name: request.display_name,
        description: request.description,
        model: request.model,
        thinking: request.thinking,
        browser_profile: request.browser_profile,
        ..BotPatch::default()
    };
    let mutation = run_mutation(&state, &user, &id, patch).await?;
    Ok(Json(json!({
        "bot": {
            "id": id,
            "enabled": mutation.enabled,
            "role": mutation.role,
            "live": state.runtime.bot_loaded(&user.id, &id),
            "tools": mutation.config.tools,
            "skills": mutation.config.skills,
            "require_approval": mutation.config.require_approval,
            "display_name": mutation.settings.display_name,
            "description": mutation.settings.description,
            "model": mutation.config.model,
            "thinking": mutation.config.thinking,
            "browser_profile": mutation
                .settings
                .browser_profile
                .clone()
                .unwrap_or_else(|| crate::browser_profiles::DEFAULT_PROFILE.to_owned()),
        }
    })))
}

/// Run one change through the shared fleet mutation path as the owner.
async fn run_mutation(
    state: &ApiState,
    user: &ActingUser,
    id: &str,
    patch: BotPatch,
) -> Result<crate::fleet_mutation::Mutation> {
    let fleet = state.runtime.fleet();
    let registered = |tool: &str| state.runtime.gateway().registry().is_registered(tool);
    let env = MutationEnv {
        store: &state.store,
        fleet: &fleet,
        router: state.runtime.router(),
        policy: state.runtime.policy(),
        tool_registered: &registered,
    };
    fleet_mutation::apply(&env, &user.id, id, patch, Origin::owner(&user.id)).await
}

/// The bot's route as the owner sees it: what is configured, where a call
/// would start now, and the last time a turn switched away from it.
fn route_view(state: &ApiState, user_id: &str, id: &str, config: Option<&BotConfig>) -> Value {
    let Some(config) = config else {
        return Value::Null;
    };
    let chain = state.runtime.router().bot_chain(user_id, config);
    let last_switch = state
        .store
        .route_events(user_id, None, 100)
        .ok()
        .and_then(|events| {
            events
                .into_iter()
                .find(|event| event.bot.as_deref() == Some(id))
        });
    json!({
        "route": config.route,
        "fallbacks": config.fallbacks,
        "failover": config.failover,
        "effective": chain.head.map(|head| json!({
            "provider": head.provider,
            "model": head.model,
            "account": head.account,
            "thinking": chain.head_thinking,
        })),
        "last_switch": last_switch,
    })
}

#[derive(Debug, Deserialize)]
pub(super) struct RouteEventsQuery {
    #[serde(default)]
    conversation: Option<String>,
    #[serde(default)]
    bot: Option<String>,
}

/// `GET /v1/route-events?conversation=&bot=`: the session user's recorded
/// route switches, newest first, for the chat's "Switched to X" rows. With
/// `bot`, the reply also carries that bot's chain so a failed turn can offer
/// "Retry on <fallback>". Another user's conversation yields no rows.
pub(super) async fn list_route_events(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<RouteEventsQuery>,
) -> Result<Json<Value>> {
    if let Some(conversation) = &query.conversation {
        validate_conversation_id(conversation)?;
    }
    let events = state
        .store
        .route_events(&user.id, query.conversation.as_deref(), 50)?;
    let chain = match query.bot.as_deref() {
        Some(bot) if safe_bot_label(bot) => state
            .runtime
            .bot(&user.id, bot)
            .map(|loaded| route_view(&state, &user.id, bot, Some(&loaded.config)))
            .unwrap_or(Value::Null),
        _ => Value::Null,
    };
    Ok(Json(json!({ "events": events, "chain": chain })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateRouteRequest {
    /// The primary route, or `null` to follow the default route.
    #[serde(default, deserialize_with = "double_option")]
    route: Option<Option<crate::config::BotRoute>>,
    /// At most two routes tried after the primary.
    #[serde(default)]
    fallbacks: Option<Vec<crate::config::BotRoute>>,
    #[serde(default)]
    failover: Option<crate::config::FailoverPolicy>,
}

/// `PUT /v1/bots/{id}/route`: set the session user's bot route, fallbacks
/// and failover policy. Every entry is checked against the live catalog, the
/// model's thinking levels and the provider's egress approval before the
/// write. Turns queued under the old chain are refused and resubmitted.
pub(super) async fn update_bot_route(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<UpdateRouteRequest>,
) -> Result<Json<Value>> {
    if !safe_bot_label(&id) {
        return Err(SeraError::NotFound(format!("bot {id}")));
    }
    if request.route.is_none() && request.fallbacks.is_none() && request.failover.is_none() {
        return Err(SeraError::Validation(
            "at least one of route, fallbacks, failover is required".into(),
        ));
    }
    let patch = BotPatch {
        route: request.route,
        fallbacks: request.fallbacks,
        failover: request.failover,
        ..BotPatch::default()
    };
    let mutation = run_mutation(&state, &user, &id, patch).await?;
    Ok(Json(
        json!({ "bot": id, "route": route_view(&state, &user.id, &id, Some(&mutation.config)) }),
    ))
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
    // A shipped bot stays removed (not re-seeded) until the owner re-adds it.
    // Mark first so a failure here leaves the row intact instead of restoring it.
    state.runtime.fleet().mark_removed(&user.id, &id)?;
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
