use super::*;

pub(super) type AdminResult<T> = std::result::Result<T, (StatusCode, Json<Value>)>;

/// Admin rejections carry our own safe message so the console can explain the
/// fix (for example a missing secret name) without exposing provider bodies.
pub(super) fn admin_error(error: SeraError) -> (StatusCode, Json<Value>) {
    let (status, message) = match &error {
        SeraError::NotFound(message) => (StatusCode::NOT_FOUND, message.clone()),
        SeraError::InvalidState(message) => (StatusCode::CONFLICT, message.clone()),
        SeraError::Validation(message) | SeraError::Config(message) => {
            (StatusCode::UNPROCESSABLE_ENTITY, message.clone())
        }
        SeraError::Forbidden(message) => (StatusCode::FORBIDDEN, message.clone()),
        SeraError::PolicyViolation => (
            StatusCode::FORBIDDEN,
            "blocked by the content policy".to_owned(),
        ),
        _ => {
            warn!(%error, "admin request failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "administration failed".to_owned(),
            )
        }
    };
    (status, Json(json!({"error": message, "code": "admin"})))
}

pub(super) fn admin_store(state: &ApiState) -> AdminResult<&AdminStore> {
    state
        .admin
        .as_ref()
        .ok_or_else(|| admin_error(SeraError::Internal("administration is unavailable".into())))
}

pub(super) fn audit_config(
    state: &ApiState,
    actor: &str,
    event: &str,
    details: Value,
) -> AdminResult<()> {
    state
        .store
        .record_audit(
            actor,
            event,
            None,
            None,
            None,
            None,
            Some(state.runtime.policy_version()),
            Some(&details),
        )
        .map_err(admin_error)?;
    Ok(())
}

#[derive(Deserialize)]
pub(super) struct AddProviderRequest {
    id: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    api_key_secret: String,
    #[serde(default)]
    catalog_fetch: bool,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    auth: String,
    #[serde(default)]
    refresh_token_secret: String,
    #[serde(default)]
    oauth_client_id_secret: String,
    #[serde(default)]
    oauth_client_secret_secret: String,
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    oauth_client_id: Option<String>,
    #[serde(default)]
    oauth_client_secret: Option<String>,
}

pub(super) async fn admin_add_provider(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<AddProviderRequest>,
) -> AdminResult<(StatusCode, Json<Value>)> {
    require_admin(&user).map_err(admin_error)?;
    let admin = admin_store(&state)?;
    let profile = ProviderConfig {
        base_url: request.base_url,
        api_key_secret: request.api_key_secret,
        models: request.models,
        data_egress_approved: true,
        kind: request.kind,
        catalog_fetch: request.catalog_fetch,
        auth: request.auth,
        refresh_token_secret: request.refresh_token_secret,
        oauth_client_id_secret: request.oauth_client_id_secret,
        oauth_client_secret_secret: request.oauth_client_secret_secret,
        project_id: request.project_id,
        ..ProviderConfig::default()
    };
    let kind = profile.kind.clone();
    let catalog_fetch = profile.catalog_fetch;
    admin
        .add_provider_with_secrets(
            &state.config,
            &state.secrets,
            &request.id,
            profile,
            ProviderSecrets {
                api_key: request.api_key.as_deref(),
                refresh_token: request.refresh_token.as_deref(),
                oauth_client_id: request.oauth_client_id.as_deref(),
                oauth_client_secret: request.oauth_client_secret.as_deref(),
            },
        )
        .map_err(admin_error)?;
    let applied = apply_provider_change(&state);
    audit_config(
        &state,
        &user.id,
        "config.provider_saved",
        json!({"provider": request.id, "kind": kind, "catalog_fetch": catalog_fetch, "applied": applied}),
    )?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"saved": true, "restart_required": !applied})),
    ))
}

/// Put a console provider change in front of the bots at once: rebuild the
/// model router from the live provider list and swap it in. Returns whether
/// it was applied; when the new list does not build (it is checked the same
/// way as at startup), the running router stays and a restart applies the
/// change, so a bad save never takes the working providers down.
pub(super) fn apply_provider_change(state: &ApiState) -> bool {
    let Some(admin) = state.admin.as_ref() else {
        return false;
    };
    let llm = admin.live_llm(&state.config.llm);
    match state.runtime.router().rebuilt(&llm) {
        Ok(router) => {
            state.runtime.replace_router(router);
            true
        }
        Err(error) => {
            tracing::warn!(%error, "provider change waits for a restart");
            false
        }
    }
}

pub(super) async fn admin_remove_provider(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
    let admin = admin_store(&state)?;
    admin.remove_provider(&id).map_err(admin_error)?;
    let applied = apply_provider_change(&state);
    audit_config(
        &state,
        &user.id,
        "config.provider_removed",
        json!({"provider": id, "applied": applied}),
    )?;
    Ok(Json(json!({"removed": id, "restart_required": !applied})))
}

#[derive(Deserialize, Default)]
pub(super) struct OAuthStartRequest {
    #[serde(default)]
    challenge: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    redirect_uri: String,
}

#[derive(Deserialize)]
pub(super) struct OAuthCompleteRequest {
    code: String,
    verifier: String,
    redirect_uri: String,
}

pub(super) fn oauth_profile(
    state: &ApiState,
    admin: &AdminStore,
    id: &str,
) -> Result<ProviderConfig> {
    admin
        .snapshot()
        .providers
        .get(id)
        .or_else(|| state.config.llm.providers.get(id))
        .cloned()
        .ok_or_else(|| SeraError::NotFound(format!("provider {id}")))
}

/// A sign-in failure as the admin console reads it: our own words for a
/// refusal, and a plain sentence for anything the provider did.
fn signin_admin_error(error: SeraError) -> (StatusCode, Json<Value>) {
    match error {
        SeraError::Rejected { message, .. } => admin_error(SeraError::Validation(message)),
        SeraError::Validation(_) | SeraError::Config(_) | SeraError::NotFound(_) => {
            admin_error(error)
        }
        _ => admin_error(SeraError::Validation(
            "Sign-in could not be completed. Start it again.".into(),
        )),
    }
}

/// Begin a provider sign-in for the acting admin. The result is the admin's own
/// account in the per-user store, exactly as a member's would be; the pre-0.49
/// age-store path is gone. PKCE providers get their authorization URL (the
/// browser leg and loopback listener run on the owner's machine); device
/// providers start a bounded background poll and return the code to enter.
pub(super) async fn admin_provider_oauth_start(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    body: Option<Json<OAuthStartRequest>>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
    let admin = admin_store(&state)?;
    let profile = oauth_profile(&state, admin, &id).map_err(admin_error)?;
    let kind = crate::providers::canonical_kind(&profile.kind).map_err(admin_error)?;
    super::provider_accounts::plan_signin_gate(&user, &profile, &kind).map_err(admin_error)?;
    let flows = state.signin.registry().flows_for(&kind);
    let Some(first) = flows.first().copied() else {
        return Err(admin_error(SeraError::Validation(
            "this provider does not use OAuth sign-in".into(),
        )));
    };
    let flow = crate::auth_flows::FlowKind::parse(first)
        .ok_or_else(|| admin_error(SeraError::Internal("unknown sign-in flow".into())))?;
    let request = body.map(|Json(body)| body).unwrap_or_default();
    let some = |value: String| Some(value).filter(|value| !value.is_empty());
    let started = state
        .signin
        .start(
            &user.id,
            &id,
            &kind,
            crate::auth_flows::StartRequest {
                flow: Some(flow),
                redirect_uri: some(request.redirect_uri),
                challenge: some(request.challenge),
                state: some(request.state),
            },
        )
        .await
        .map_err(signin_admin_error)?;
    Ok(Json(match flow {
        crate::auth_flows::FlowKind::Pkce => json!({
            "flow": "pkce",
            "url": started.url,
            "flow_id": started.flow_id,
        }),
        crate::auth_flows::FlowKind::Device => json!({
            "flow": "device",
            "url": started.url,
            "code": started.user_code,
            "flow_id": started.flow_id,
            "expires_in": started.expires_in,
            "interval": started.interval,
        }),
    }))
}

/// Finish a PKCE sign-in with the code the console caught on its loopback
/// listener. The account is stored sealed under the acting admin; nothing
/// secret is returned and no restart is needed.
pub(super) async fn admin_provider_oauth_complete(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<OAuthCompleteRequest>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
    let admin = admin_store(&state)?;
    let profile = oauth_profile(&state, admin, &id).map_err(admin_error)?;
    let kind = crate::providers::canonical_kind(&profile.kind).map_err(admin_error)?;
    super::provider_accounts::plan_signin_gate(&user, &profile, &kind).map_err(admin_error)?;
    if !state.signin.registry().flows_for(&kind).contains(&"pkce") {
        return Err(admin_error(SeraError::Validation(
            "this provider does not use a browser code sign-in".into(),
        )));
    }
    let outcome = state
        .signin
        .complete(
            &user.id,
            &id,
            &kind,
            crate::auth_flows::CompleteRequest {
                flow_id: None,
                code: request.code,
                redirect_uri: Some(request.redirect_uri).filter(|uri| !uri.is_empty()),
                verifier: Some(request.verifier),
            },
        )
        .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            audit_config(
                &state,
                &user.id,
                "config.provider_oauth_failed",
                json!({"provider": id, "kind": kind}),
            )?;
            return Err(signin_admin_error(error));
        }
    };
    state.runtime.invalidate_model_credentials(Some(&user.id));
    let project_discovered = state
        .accounts
        .get(&user.id, &id, &outcome.account_id)
        .ok()
        .flatten()
        .is_some_and(|account| account.identity.get("project_id").is_some());
    audit_config(
        &state,
        &user.id,
        "config.provider_oauth_signed_in",
        json!({"provider": id, "kind": kind, "account": outcome.account_id,
               "project_discovered": project_discovered}),
    )?;
    Ok(Json(json!({
        "signed_in": true,
        "account_id": outcome.account_id,
        "created": outcome.created,
        "project_discovered": project_discovered,
    })))
}

/// Begin an ElevenLabs OAuth sign-in for the acting user. PKCE only: the
/// browser leg and loopback listener run on the user's machine (the desktop
/// console); the server only builds the pinned consent URL. No pending state
/// is stored server-side — the desktop holds verifier and state.
pub(super) async fn admin_connector_oauth_start(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<OAuthStartRequest>,
) -> AdminResult<Json<Value>> {
    if id != "elevenlabs" {
        return Err(admin_error(SeraError::NotFound(format!(
            "connector {id} does not support OAuth sign-in"
        ))));
    }
    let url = crate::elevenlabs_oauth::authorize_url(
        &state.config.elevenlabs.oauth_client_id,
        &request.redirect_uri,
        &state.config.elevenlabs.oauth_scope,
        &request.challenge,
        &request.state,
    )
    .map_err(admin_error)?;
    audit_config(
        &state,
        &user.id,
        "config.connector_oauth_started",
        json!({"connector": id}),
    )?;
    Ok(Json(json!({"flow": "pkce", "url": url})))
}

/// Complete the ElevenLabs sign-in: the server exchanges the code at the
/// pinned token endpoint and seals both tokens to the acting account's own
/// credential. Tokens are never returned to the caller.
pub(super) async fn admin_connector_oauth_complete(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<OAuthCompleteRequest>,
) -> AdminResult<Json<Value>> {
    if id != "elevenlabs" {
        return Err(admin_error(SeraError::NotFound(format!(
            "connector {id} does not support OAuth sign-in"
        ))));
    }
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| admin_error(SeraError::External(error.to_string())))?;
    let pair = crate::elevenlabs_oauth::exchange_code(
        &http,
        &state.config.elevenlabs.oauth_client_id,
        &request.code,
        &request.redirect_uri,
        &request.verifier,
    )
    .await
    .map_err(admin_error)?;
    // A refresh the exchange did not rotate keeps the one this account has.
    let refresh = match &pair.refresh_token {
        Some(refresh) => refresh.clone(),
        None => state
            .users
            .get(&user.id, crate::elevenlabs_mcp::CREDENTIAL_PROVIDER)
            .map_err(admin_error)?
            .and_then(|stored| crate::user_secrets::ProviderCredential::parse(&stored).ok())
            .map(|credential| credential.refresh_token)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                admin_error(SeraError::External(
                    "ElevenLabs did not return a refresh token".into(),
                ))
            })?,
    };
    crate::elevenlabs_mcp::store_sign_in(
        &state.users,
        &user.id,
        &pair.access_token,
        &refresh,
        pair.expires_in,
    )
    .map_err(admin_error)?;
    if let Some(elevenlabs) = &state.elevenlabs {
        elevenlabs.forget(&user.id).await;
    }
    audit_config(
        &state,
        &user.id,
        "config.connector_oauth_signed_in",
        json!({"connector": id, "ok": true}),
    )?;
    // Tools are registered at startup; the sign-in itself takes effect at once.
    Ok(Json(json!({"signed_in": true, "restart_required": false})))
}

/// Forget the acting account's ElevenLabs sign-in. Calls fail from then on
/// until the account signs in again.
pub(super) async fn admin_connector_oauth_disconnect(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> AdminResult<Json<Value>> {
    if id != "elevenlabs" {
        return Err(admin_error(SeraError::NotFound(format!(
            "connector {id} does not support sign-in"
        ))));
    }
    let removed =
        crate::elevenlabs_mcp::remove_sign_in(&state.users, &user.id).map_err(admin_error)?;
    if let Some(elevenlabs) = &state.elevenlabs {
        elevenlabs.forget(&user.id).await;
    }
    audit_config(
        &state,
        &user.id,
        "config.connector_oauth_signed_out",
        json!({"connector": id, "removed": removed}),
    )?;
    Ok(Json(json!({"signed_in": false, "removed": removed})))
}

#[derive(Deserialize)]
pub(super) struct ConnectorRequest {
    enabled: bool,
    #[serde(default)]
    values: BTreeMap<String, String>,
}

pub(super) async fn admin_set_connector(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<ConnectorRequest>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
    let admin = admin_store(&state)?;
    admin
        .set_connector(
            &state.config,
            &state.secrets,
            &id,
            request.enabled,
            &request.values,
        )
        .map_err(admin_error)?;
    audit_config(
        &state,
        &user.id,
        "config.connector_saved",
        json!({"connector": id, "enabled": request.enabled, "credentials": request.values.keys().collect::<Vec<_>>()}),
    )?;
    Ok(Json(json!({"saved": true, "restart_required": true})))
}

#[derive(Deserialize)]
pub(super) struct AudioRequest {
    enabled: bool,
    #[serde(default)]
    transcription_model: Option<String>,
    #[serde(default)]
    understanding_model: Option<String>,
}

pub(super) async fn admin_set_audio(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<AudioRequest>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
    let admin = admin_store(&state)?;
    admin
        .set_audio(
            &state.config,
            &state.secrets,
            request.enabled,
            request.transcription_model,
            request.understanding_model,
        )
        .map_err(admin_error)?;
    audit_config(
        &state,
        &user.id,
        "config.audio_saved",
        json!({"enabled": request.enabled}),
    )?;
    Ok(Json(json!({"saved": true, "restart_required": true})))
}

/// Voice, speech-to-text, text-to-speech, and OCR settings for the console.
///
/// Readable by any signed-in user (they need to know whether voice replies
/// work), writable by the admin only. Every value is a model id, a voice id, or
/// a mode name — never a secret.
#[derive(Serialize)]
pub(super) struct VoiceSettings {
    stt: ModelSetting,
    tts: SpeechSetting,
    ocr: ModelSetting,
    reply_mode: String,
    max_speech_chars: usize,
}

#[derive(Serialize)]
pub(super) struct ModelSetting {
    model: String,
    ready: bool,
    /// Why it is not ready. Names the missing setting, never a secret.
    detail: Option<String>,
}

#[derive(Serialize)]
pub(super) struct SpeechSetting {
    model: String,
    voice_id: String,
    ready: bool,
    detail: Option<String>,
}

pub(super) fn voice_settings_payload(config: &Config) -> VoiceSettings {
    let (stt_ready, stt_detail) = match crate::audio::audio_provider_state(config) {
        crate::audio::AudioProviderState::Ready => (true, None),
        crate::audio::AudioProviderState::Unavailable(reason) => (false, Some(reason)),
    };
    let (tts_ready, tts_detail) = match crate::audio::speech_provider_state(config) {
        crate::audio::AudioProviderState::Ready => (true, None),
        crate::audio::AudioProviderState::Unavailable(reason) => (false, Some(reason)),
    };
    let (ocr_ready, ocr_detail) = match crate::ocr::ocr_provider_state(config) {
        crate::ocr::OcrProviderState::Ready => (true, None),
        crate::ocr::OcrProviderState::Unavailable(reason) => (false, Some(reason)),
    };
    VoiceSettings {
        stt: ModelSetting {
            model: config.audio.transcription_model.clone(),
            ready: stt_ready,
            detail: stt_detail,
        },
        tts: SpeechSetting {
            model: config.audio.speech_model.clone(),
            voice_id: config.audio.voice_id.clone(),
            ready: tts_ready,
            detail: tts_detail,
        },
        ocr: ModelSetting {
            model: config.ocr.model.clone(),
            ready: ocr_ready,
            detail: ocr_detail,
        },
        reply_mode: config.audio.reply_mode.clone(),
        max_speech_chars: config.audio.max_speech_chars,
    }
}

pub(super) async fn voice_settings(
    State(state): State<Arc<ApiState>>,
) -> AdminResult<Json<VoiceSettings>> {
    Ok(Json(voice_settings_payload(&state.config)))
}

#[derive(Deserialize)]
pub(super) struct VoiceSettingsRequest {
    #[serde(default)]
    speech_model: Option<String>,
    #[serde(default)]
    voice_id: Option<String>,
    #[serde(default)]
    reply_mode: Option<String>,
    #[serde(default)]
    ocr_enabled: Option<bool>,
    #[serde(default)]
    ocr_model: Option<String>,
}

pub(super) async fn admin_set_voice(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<VoiceSettingsRequest>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
    let admin = admin_store(&state)?;
    let reply_mode = request.reply_mode.clone();
    let ocr_enabled = request.ocr_enabled;
    admin
        .set_voice_settings(
            &state.config,
            &state.secrets,
            request.speech_model,
            request.voice_id,
            request.reply_mode,
            request.ocr_enabled,
            request.ocr_model,
        )
        .map_err(admin_error)?;
    audit_config(
        &state,
        &user.id,
        "config.voice_saved",
        json!({
            "reply_mode": reply_mode,
            "ocr_enabled": ocr_enabled,
        }),
    )?;
    Ok(Json(json!({"saved": true, "restart_required": true})))
}

#[derive(Serialize)]
pub(super) struct ReplyModeResponse {
    conversation_id: String,
    mode: String,
    /// Whether the server can actually produce voice right now.
    voice_ready: bool,
    detail: Option<String>,
}

pub(super) async fn get_reply_mode(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> AdminResult<Json<ReplyModeResponse>> {
    let mode = state.store.reply_mode(&user.id, &id).map_err(admin_error)?;
    let (voice_ready, detail) = match crate::audio::speech_provider_state(&state.config) {
        crate::audio::AudioProviderState::Ready => (true, None),
        crate::audio::AudioProviderState::Unavailable(reason) => (false, Some(reason)),
    };
    Ok(Json(ReplyModeResponse {
        conversation_id: id,
        mode,
        voice_ready,
        detail,
    }))
}

#[derive(Deserialize)]
pub(super) struct ReplyModeRequest {
    mode: String,
}

pub(super) async fn set_reply_mode(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<ReplyModeRequest>,
) -> AdminResult<Json<ReplyModeResponse>> {
    state
        .store
        .set_reply_mode(&user.id, &id, request.mode.trim())
        .map_err(admin_error)?;
    let (voice_ready, detail) = match crate::audio::speech_provider_state(&state.config) {
        crate::audio::AudioProviderState::Ready => (true, None),
        crate::audio::AudioProviderState::Unavailable(reason) => (false, Some(reason)),
    };
    Ok(Json(ReplyModeResponse {
        conversation_id: id,
        mode: request.mode.trim().to_owned(),
        voice_ready,
        detail,
    }))
}

#[derive(Deserialize)]
pub(super) struct BotProfileRequest {
    display_name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    avatar: String,
}

pub(super) async fn admin_set_bot_profile(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<BotProfileRequest>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
    let admin = admin_store(&state)?;
    if !state.runtime.bot_loaded(&user.id, &id) {
        let configured = state
            .runtime
            .fleet()
            .configured(&user.id)
            .map_err(admin_error)?;
        if !configured.iter().any(|bot| bot.id == id) {
            return Err(admin_error(SeraError::NotFound(format!("bot {id}"))));
        }
    }
    for value in [&request.display_name, &request.description, &request.avatar] {
        state
            .runtime
            .scan_owner_output(&user.id, value)
            .map_err(admin_error)?;
    }
    admin
        .set_bot_profile(
            &id,
            BotProfile {
                display_name: request.display_name,
                description: request.description,
                avatar: request.avatar,
            },
        )
        .map_err(admin_error)?;
    audit_config(
        &state,
        &user.id,
        "config.bot_profile_saved",
        json!({"bot": id}),
    )?;
    Ok(Json(json!({"saved": true, "restart_required": false})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BotConfigRequest {
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    skills: Option<Vec<String>>,
    #[serde(default)]
    require_approval: Option<Vec<String>>,
}

/// Tool list rules; see `fleet_mutation::validate_tool_list`.
pub(super) fn validate_bot_tool_list(
    state: &ApiState,
    tools: &[String],
    role: BotRole,
    kept: &[String],
) -> Result<Vec<String>> {
    crate::fleet_mutation::validate_tool_list(tools, role, kept, &|tool| {
        state.runtime.gateway().registry().is_registered(tool)
    })
}

/// Skill list rules; see `fleet_mutation::validate_skill_list`.
pub(super) fn validate_bot_skill_list(
    state: &ApiState,
    user_id: &str,
    skills: &[String],
) -> Result<Vec<String>> {
    crate::fleet_mutation::validate_skill_list(&state.runtime.fleet(), user_id, skills)
}

/// `require_approval` entries; see `fleet_mutation::validate_require_approval_list`.
pub(super) fn validate_require_approval_list(
    list: &[String],
    tools: &[String],
) -> Result<Vec<String>> {
    crate::fleet_mutation::validate_require_approval_list(list, tools)
}

/// `PUT /v1/admin/bots/{id}/config` — validate and persist a partial bot
/// config update (tools/skills/require_approval) into the admin overlay, like
/// `admin_set_bot_profile`. Takes effect after restart.
pub(super) async fn admin_update_bot_config(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(mut request): Json<BotConfigRequest>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
    let admin = admin_store(&state)?;
    // A console from before a tool rename still sends the old names.
    if let Some(tools) = request.tools.as_mut() {
        crate::config::rename_tool_list(tools);
    }
    if let Some(approval) = request.require_approval.as_mut() {
        crate::config::rename_tool_list(approval);
    }
    let configured = state
        .runtime
        .fleet()
        .configured(&user.id)
        .map_err(admin_error)?;
    let Some(entry) = configured.iter().find(|bot| bot.id == id) else {
        return Err(admin_error(SeraError::NotFound(format!("bot {id}"))));
    };
    if request.tools.is_none() && request.skills.is_none() && request.require_approval.is_none() {
        return Err(admin_error(SeraError::Validation(
            "at least one of tools, skills, require_approval is required".into(),
        )));
    }
    // Untrusted input is scanned before it is validated or stored.
    for list in [&request.tools, &request.skills, &request.require_approval]
        .into_iter()
        .flatten()
    {
        for item in list {
            state
                .runtime
                .scan_owner_output(&user.id, item)
                .map_err(admin_error)?;
        }
    }
    let role = state
        .runtime
        .bot(&user.id, &id)
        .map(|bot| bot.config.role)
        .unwrap_or(if entry.role == BotRole::Orchestrator.as_str() {
            BotRole::Orchestrator
        } else {
            BotRole::Specialist
        });
    let (current_tools, _, _) = bot_lists_with_override(&state, entry);
    let tools = match &request.tools {
        Some(tools) => {
            Some(validate_bot_tool_list(&state, tools, role, &current_tools).map_err(admin_error)?)
        }
        None => None,
    };
    let skills = match &request.skills {
        Some(skills) => {
            Some(validate_bot_skill_list(&state, &user.id, skills).map_err(admin_error)?)
        }
        None => None,
    };
    let effective_tools = tools.clone().unwrap_or_else(|| current_tools.clone());
    let require_approval = match &request.require_approval {
        Some(list) => {
            Some(validate_require_approval_list(list, &effective_tools).map_err(admin_error)?)
        }
        None => None,
    };
    admin
        .set_bot_config(
            &id,
            BotConfigOverlay {
                tools,
                skills,
                require_approval,
            },
        )
        .map_err(admin_error)?;
    audit_config(
        &state,
        &user.id,
        "config.bot_config_saved",
        json!({"bot": id}),
    )?;
    Ok(Json(json!({"saved": true, "restart_required": true})))
}

#[derive(Deserialize)]
pub(super) struct RestartRequest {
    #[serde(default)]
    confirm: bool,
}

pub(super) async fn admin_restart(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<RestartRequest>,
) -> AdminResult<(StatusCode, Json<Value>)> {
    require_admin(&user).map_err(admin_error)?;
    if !request.confirm {
        return Err(admin_error(SeraError::Validation(
            "restart requires explicit confirmation".into(),
        )));
    }
    let pending = state.store.pending_task_count_all().map_err(admin_error)?;
    if pending > 0 {
        return Err(admin_error(SeraError::InvalidState(format!(
            "{pending} task(s) are still in flight; wait for them to finish"
        ))));
    }
    audit_config(&state, &user.id, "config.restart_requested", Value::Null)?;
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(400)).await;
        tracing::warn!("restart requested by the owner console; exiting for systemd");
        std::process::exit(1);
    });
    Ok((StatusCode::ACCEPTED, Json(json!({"restarting": true}))))
}
