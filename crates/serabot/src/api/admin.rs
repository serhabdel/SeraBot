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
    audit_config(
        &state,
        &user.id,
        "config.provider_saved",
        json!({"provider": request.id, "kind": kind, "catalog_fetch": catalog_fetch}),
    )?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"saved": true, "restart_required": true})),
    ))
}

pub(super) async fn admin_remove_provider(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
    let admin = admin_store(&state)?;
    admin.remove_provider(&id).map_err(admin_error)?;
    audit_config(
        &state,
        &user.id,
        "config.provider_removed",
        json!({"provider": id}),
    )?;
    Ok(Json(json!({"removed": id, "restart_required": true})))
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

pub(super) fn oauth_secret_name(configured: &str, default: &str) -> String {
    if configured.trim().is_empty() {
        default.to_owned()
    } else {
        configured.to_owned()
    }
}

pub(super) fn release_oauth(state: &ApiState, id: &str) {
    state
        .oauth_inflight
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(id);
}

/// Begin a provider sign-in. PKCE providers get their authorization URL; the
/// browser leg and loopback listener run on the owner's machine. Device
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
    let request = body.map(|Json(body)| body).unwrap_or_default();
    match crate::providers::canonical_kind(&profile.kind)
        .map_err(admin_error)?
        .as_str()
    {
        "google-antigravity" => {
            let url = crate::cloud_code::authorize_url(
                &request.redirect_uri,
                &request.challenge,
                &request.state,
            )
            .map_err(|_| admin_error(SeraError::Validation("invalid sign-in request".into())))?;
            Ok(Json(json!({"flow": "pkce", "url": url})))
        }
        "muse-code" => {
            let api_name = oauth_secret_name(&profile.api_key_secret, "muse_code_api_key");
            let identity_name =
                oauth_secret_name(&profile.refresh_token_secret, "muse_code_identity");
            let first = state
                .oauth_inflight
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(id.clone());
            if !first {
                return Err(admin_error(SeraError::InvalidState(
                    "a sign-in for this provider is already running".into(),
                )));
            }
            let (tx, rx) = tokio::sync::oneshot::channel::<(String, String)>();
            let task_state = state.clone();
            let task_id = id.clone();
            let task_user_id = user.id.clone();
            tokio::spawn(async move {
                use secrecy::ExposeSecret;
                let outcome = async {
                    let http = reqwest::Client::builder()
                        .timeout(Duration::from_secs(30))
                        .redirect(reqwest::redirect::Policy::none())
                        .build()
                        .map_err(|_| SeraError::External("sign-in could not start".into()))?;
                    let mut tx = Some(tx);
                    tokio::time::timeout(
                        Duration::from_secs(900),
                        crate::providers::login_muse_code(&http, |url, code| {
                            if let Some(tx) = tx.take() {
                                let _ = tx.send((url.to_owned(), code.to_owned()));
                            }
                        }),
                    )
                    .await
                    .map_err(|_| SeraError::External("sign-in timed out".into()))?
                }
                .await;
                let event = match outcome {
                    Ok(credentials) => match task_state.secrets.persist(&[
                        (api_name.as_str(), credentials.api_key.expose_secret()),
                        (
                            identity_name.as_str(),
                            credentials.identity_token.expose_secret(),
                        ),
                    ]) {
                        Ok(()) => "config.provider_oauth_signed_in",
                        Err(_) => "config.provider_oauth_failed",
                    },
                    Err(_) => "config.provider_oauth_failed",
                };
                let _ = audit_config(
                    &task_state,
                    &task_user_id,
                    event,
                    json!({"provider": task_id, "kind": "muse-code"}),
                );
                release_oauth(&task_state, &task_id);
            });
            match tokio::time::timeout(Duration::from_secs(15), rx).await {
                Ok(Ok((url, code))) => {
                    Ok(Json(json!({"flow": "device", "url": url, "code": code})))
                }
                _ => {
                    release_oauth(&state, &id);
                    Err(admin_error(SeraError::Validation(
                        "sign-in could not be started".into(),
                    )))
                }
            }
        }
        _ => Err(admin_error(SeraError::Validation(
            "this provider does not use OAuth sign-in".into(),
        ))),
    }
}

/// Finish a PKCE sign-in with the code the console caught on its loopback
/// listener. Tokens are stored server-side and never returned.
pub(super) async fn admin_provider_oauth_complete(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<OAuthCompleteRequest>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
    use secrecy::ExposeSecret;
    let admin = admin_store(&state)?;
    let profile = oauth_profile(&state, admin, &id).map_err(admin_error)?;
    if crate::providers::canonical_kind(&profile.kind).map_err(admin_error)? != "google-antigravity"
    {
        return Err(admin_error(SeraError::Validation(
            "this provider does not use a browser code sign-in".into(),
        )));
    }
    let access_name = oauth_secret_name(&profile.api_key_secret, "antigravity_access_token");
    let refresh_name =
        oauth_secret_name(&profile.refresh_token_secret, "antigravity_refresh_token");
    let client_name = oauth_secret_name(
        &profile.oauth_client_secret_secret,
        "antigravity_oauth_client_secret",
    );
    let client_secret = state.secrets.get(&client_name).ok_or_else(|| {
        admin_error(SeraError::Validation(format!(
            "store the OAuth client secret as {client_name} first"
        )))
    })?;
    let login = crate::cloud_code::complete_antigravity_login(
        &client_secret,
        &request.code,
        &request.redirect_uri,
        &request.verifier,
    )
    .await
    .map_err(|error| match error {
        SeraError::Validation(_) | SeraError::Config(_) => admin_error(error),
        _ => admin_error(SeraError::Validation(
            "Google sign-in could not be completed. Start it again.".into(),
        )),
    })?;
    state
        .secrets
        .persist(&[
            (access_name.as_str(), login.access_token.expose_secret()),
            (refresh_name.as_str(), login.refresh_token.expose_secret()),
        ])
        .map_err(admin_error)?;
    let project_saved = match login.project_id.as_deref() {
        Some(project) => admin
            .set_provider_project_if_empty(&id, project)
            .unwrap_or(false),
        None => false,
    };
    audit_config(
        &state,
        &user.id,
        "config.provider_oauth_signed_in",
        json!({"provider": id, "kind": "google-antigravity", "project_discovered": login.project_id.is_some()}),
    )?;
    Ok(Json(json!({
        "signed_in": true,
        "project_saved": project_saved,
        "restart_required": true
    })))
}

/// Begin an ElevenLabs OAuth sign-in for the connector. PKCE only: the
/// browser leg and loopback listener run on the owner's machine (the desktop
/// console); the server only builds the pinned consent URL. No pending state
/// is stored server-side — the desktop holds verifier and state.
pub(super) async fn admin_connector_oauth_start(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<OAuthStartRequest>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
    if id != "elevenlabs" {
        return Err(admin_error(SeraError::NotFound(format!(
            "connector {id} does not support OAuth sign-in"
        ))));
    }
    let url = crate::elevenlabs::authorize_url(
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
/// pinned token endpoint and persists both tokens to the age store. Tokens
/// are never returned to the caller.
pub(super) async fn admin_connector_oauth_complete(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<OAuthCompleteRequest>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
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
    let pair = crate::elevenlabs::exchange_code(
        &http,
        &state.config.elevenlabs.oauth_client_id,
        &request.code,
        &request.redirect_uri,
        &request.verifier,
    )
    .await
    .map_err(admin_error)?;
    let access_name = state.config.elevenlabs.oauth_access_token_secret.clone();
    let refresh_name = state.config.elevenlabs.oauth_refresh_token_secret.clone();
    let refresh_value = match &pair.refresh_token {
        Some(refresh) => refresh.clone(),
        None => state.secrets.get(&refresh_name).ok_or_else(|| {
            admin_error(SeraError::External(
                "ElevenLabs did not return a refresh token".into(),
            ))
        })?,
    };
    state
        .secrets
        .persist(&[
            (access_name.as_str(), pair.access_token.as_str()),
            (refresh_name.as_str(), refresh_value.as_str()),
        ])
        .map_err(admin_error)?;
    audit_config(
        &state,
        &user.id,
        "config.connector_oauth_signed_in",
        json!({"connector": id, "ok": true}),
    )?;
    Ok(Json(json!({"signed_in": true, "restart_required": true})))
}

#[derive(Deserialize)]
pub(super) struct ConnectorRequest {
    enabled: bool,
    #[serde(default)]
    values: BTreeMap<String, String>,
    /// ElevenLabs only: switch auth mode ("api_key" or "oauth").
    #[serde(default)]
    auth: Option<String>,
}

pub(super) async fn admin_set_connector(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<ConnectorRequest>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
    let admin = admin_store(&state)?;
    if let Some(auth) = &request.auth {
        if id != "elevenlabs" {
            return Err(admin_error(SeraError::Validation(
                "auth mode is only supported for the elevenlabs connector".into(),
            )));
        }
        let mode = match auth.as_str() {
            "api_key" => crate::config::ElevenLabsAuth::ApiKey,
            "oauth" => crate::config::ElevenLabsAuth::Oauth,
            other => {
                return Err(admin_error(SeraError::Validation(format!(
                    "unknown elevenlabs auth mode '{other}'"
                ))));
            }
        };
        admin.set_elevenlabs_auth(mode).map_err(admin_error)?;
    }
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
        json!({"connector": id, "enabled": request.enabled, "credentials": request.values.keys().collect::<Vec<_>>(), "auth": request.auth}),
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

/// Every console ability on at once is ~95 tools plus `delegate`.
pub(super) const MAX_BOT_CONFIG_TOOLS: usize = 128;
pub(super) const MAX_BOT_CONFIG_SKILLS: usize = 16;

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

/// Tool list rules matching the desktop `validate_tool_list` semantics on top
/// of the server's tool registry: unique safe ids, `delegate` only for the
/// orchestrator, and every other tool must be a registered tool -- or one the
/// bot already has (`kept`). A tool whose service is switched off (for example
/// `tavily_search` while Tavily is disabled) is unregistered, and without the
/// `kept` exception every later edit of that bot's tools was refused (0.14.0).
pub(super) fn validate_bot_tool_list(
    state: &ApiState,
    tools: &[String],
    role: BotRole,
    kept: &[String],
) -> Result<Vec<String>> {
    if tools.len() > MAX_BOT_CONFIG_TOOLS {
        return Err(SeraError::Validation(format!(
            "at most {MAX_BOT_CONFIG_TOOLS} tools are allowed"
        )));
    }
    let mut seen = HashSet::with_capacity(tools.len());
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        if !safe_bot_label(tool) {
            return Err(SeraError::Validation(format!("invalid tool id '{tool}'")));
        }
        if tool == crate::delegate::DELEGATE_TOOL {
            if role != BotRole::Orchestrator {
                return Err(SeraError::Validation(
                    "specialists cannot be granted delegate".into(),
                ));
            }
        } else if !state.runtime.gateway().registry().is_registered(tool)
            && !kept.iter().any(|existing| existing == tool)
            && !crate::tools::known_tool_names().contains(&tool.as_str())
        {
            return Err(SeraError::Validation(format!("unknown tool '{tool}'")));
        }
        if !seen.insert(tool.clone()) {
            return Err(SeraError::Validation(format!("duplicate tool '{tool}'")));
        }
        out.push(tool.clone());
    }
    Ok(out)
}

/// Skill list rules matching the desktop `parse_skill_list` semantics plus the
/// acting user's effective catalog check: at most 16 unique, valid skill ids
/// that exist in the shipped catalog or the user's own rows.
pub(super) fn validate_bot_skill_list(
    state: &ApiState,
    user_id: &str,
    skills: &[String],
) -> Result<Vec<String>> {
    if skills.len() > MAX_BOT_CONFIG_SKILLS {
        return Err(SeraError::Validation(format!(
            "at most {MAX_BOT_CONFIG_SKILLS} skills are allowed"
        )));
    }
    let known = state.runtime.fleet().effective_skill_ids(user_id)?;
    let mut seen = HashSet::with_capacity(skills.len());
    let mut out = Vec::with_capacity(skills.len());
    for skill in skills {
        crate::skills::validate_skill_id(skill)?;
        if !known.iter().any(|id| id == skill) {
            return Err(SeraError::Validation(format!("unknown skill '{skill}'")));
        }
        if !seen.insert(skill.clone()) {
            return Err(SeraError::Validation(format!("duplicate skill '{skill}'")));
        }
        out.push(skill.clone());
    }
    Ok(out)
}

/// `require_approval` entries must be safe ids contained in the bot's
/// effective tool list (the new tools when the request sets them).
pub(super) fn validate_require_approval_list(
    list: &[String],
    tools: &[String],
) -> Result<Vec<String>> {
    if list.len() > MAX_BOT_CONFIG_TOOLS {
        return Err(SeraError::Validation(format!(
            "at most {MAX_BOT_CONFIG_TOOLS} require_approval entries are allowed"
        )));
    }
    let mut seen = HashSet::with_capacity(list.len());
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        if !safe_bot_label(item) {
            return Err(SeraError::Validation(format!(
                "invalid require_approval entry '{item}'"
            )));
        }
        if !tools.iter().any(|tool| tool == item) {
            return Err(SeraError::Validation(format!(
                "require_approval entry '{item}' is not in the bot's tools"
            )));
        }
        if !seen.insert(item.clone()) {
            return Err(SeraError::Validation(format!(
                "duplicate require_approval entry '{item}'"
            )));
        }
        out.push(item.clone());
    }
    Ok(out)
}

/// `PUT /v1/admin/bots/{id}/config` — validate and persist a partial bot
/// config update (tools/skills/require_approval) into the admin overlay, like
/// `admin_set_bot_profile`. Takes effect after restart.
pub(super) async fn admin_update_bot_config(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<BotConfigRequest>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
    let admin = admin_store(&state)?;
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
