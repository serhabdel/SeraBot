use super::*;

#[derive(Debug, Deserialize)]
pub(super) struct MessageRequest {
    conversation_id: String,
    text: String,
    #[serde(default)]
    bot: Option<String>,
}

pub(super) async fn send_message(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<MessageRequest>,
) -> Result<Json<Value>> {
    validate_conversation_id(&request.conversation_id)?;
    let task = state.runtime.submit_text(
        &user.id,
        &request.conversation_id,
        &request.text,
        request.bot.as_deref(),
    )?;
    Ok(Json(json!({"task": task_view(&task)})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ModelQuery {
    conversation_id: String,
    bot: Option<String>,
}

pub(super) async fn models(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<ModelQuery>,
) -> Result<Json<Value>> {
    validate_conversation_id(&query.conversation_id)?;
    state.runtime.refresh_model_catalogs(&user.id).await?;
    let bot = query.bot.as_deref().unwrap_or("main");
    let selection = state
        .runtime
        .model_selection(&user.id, &query.conversation_id, bot)?;
    let effective_selection =
        state
            .runtime
            .effective_model_selection(&user.id, &query.conversation_id, bot)?;
    Ok(Json(json!({
        "providers": state.runtime.model_catalog(&user.id)?,
        "entries": state.runtime.model_catalog_entries(&user.id)?,
        "selection": selection,
        "effective_selection": effective_selection,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SelectModelRequest {
    provider: String,
    #[serde(default)]
    model: String,
}

pub(super) async fn select_model(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<SelectModelRequest>,
) -> Result<Json<Value>> {
    validate_conversation_id(&id)?;
    state.runtime.refresh_model_catalogs(&user.id).await?;
    let selection = if request.provider == crate::model_router::AUTO_PROVIDER {
        ModelSelection::auto_mode()
    } else {
        ModelSelection {
            provider: request.provider,
            model: request.model,
        }
    };
    let selection = state.runtime.select_model(&user.id, &id, selection)?;
    Ok(Json(json!({"selection": selection})))
}

/// Raw owner audio, transcribed with the configured audio provider.
pub(super) async fn transcribe_audio(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let limit = crate::audio::DEFAULT_TELEGRAM_AUDIO_MAX_BYTES;
    let filename = headers
        .get("x-audio-filename")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let Some(filename) = filename else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "X-Audio-Filename header is required",
                "code": "validation",
            })),
        )
            .into_response();
    };
    if filename.len() > 128
        || filename
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '"' | '\''))
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid audio filename", "code": "validation"})),
        )
            .into_response();
    }
    // Read the raw payload under the Telegram audio ceiling. The `Bytes`
    // extractor would apply axum's server-wide 2 MiB default limit instead,
    // so the body is read explicitly and an oversized payload is reported
    // with a message that names the limit.
    let body = match axum::body::to_bytes(body, limit as usize).await {
        Ok(body) => body,
        Err(_) => return audio_payload_too_large(limit),
    };
    if body.len() as u64 > limit {
        return audio_payload_too_large(limit);
    }
    if let crate::audio::AudioProviderState::Unavailable(message) =
        crate::audio::audio_provider_state(&state.config)
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": message, "code": "audio_unavailable"})),
        )
            .into_response();
    }
    match crate::audio::transcribe_audio_bytes(&state.config, &body, &filename).await {
        Ok(text) => {
            if let Err(error) = state.runtime.scan_owner_output(&user.id, &text) {
                return error.into_response();
            }
            Json(json!({
                "text": text,
                "filename": filename,
                "bytes": body.len(),
            }))
            .into_response()
        }
        Err(SeraError::Validation(message)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": message, "code": "validation"})),
        )
            .into_response(),
        Err(SeraError::Config(message)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": message, "code": "audio_unavailable"})),
        )
            .into_response(),
        Err(error) => {
            warn!(%error, "audio transcription failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "transcription failed", "code": "audio_failed"})),
            )
                .into_response()
        }
    }
}

/// Clear, safe rejection for an audio payload above the documented ceiling.
/// The message names the limit and never echoes the payload.
pub(super) fn audio_payload_too_large(limit: u64) -> Response {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        Json(json!({
            "error": format!("audio payload exceeds the {limit} byte limit"),
            "code": "validation",
        })),
    )
        .into_response()
}
