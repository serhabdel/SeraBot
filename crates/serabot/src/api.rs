use std::collections::{BTreeMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Extension, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use chrono::{DateTime, SecondsFormat, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::warn;

use crate::admin::{
    AdminStore, BotConfigOverlay, BotProfile, ProviderSecrets, connector_secret_roles,
};
use crate::auth::{self, AuthService};
use crate::config::{Config, OAuthConnectorSettings, ProviderConfig, scaffold_specialist_bot};
use crate::connectors::{ApiKeyProbe, probe_api_key};
use crate::domain::Task;
use crate::error::{Result, SeraError};
use crate::gateway::Decision;
use crate::maker::ArtifactStore;
use crate::memory;
use crate::model_router::ModelSelection;
use crate::orchestration::{
    BotRole, ConversationKind, safe_bot_label, scoped_conversation_id, validate_conversation_id,
};
use crate::runtime::{ApprovalNotice, Notifier, Runtime};
use crate::secrets::SharedSecrets;
use crate::skills::{SkillCatalog, create_skill, discard_created_skill};
use crate::store::{Store, StoredConversation, User, UserRole, UserStatus};
use crate::util::{constant_time_eq, sha256_hex, truncate_chars};

#[derive(Clone)]
pub struct ApiNotifier {
    store: Store,
}

impl ApiNotifier {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl Notifier for ApiNotifier {
    async fn send_text(&self, conversation_id: &str, text: &str) -> Result<()> {
        self.store.append_event(
            "message",
            &json!({"conversation_id": conversation_id, "text": text}),
        )?;
        Ok(())
    }

    async fn approval_requested(
        &self,
        conversation_id: &str,
        notice: &ApprovalNotice,
    ) -> Result<()> {
        self.store.append_event(
            "approval",
            &json!({
                "conversation_id": conversation_id,
                "approval_id": notice.approval.id,
                "action_id": notice.action.id,
                "tool": notice.approval.tool,
                "summary": notice.approval.summary,
                "destination": notice.action.destination,
                "expires_at": notice.approval.expires_at,
            }),
        )?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct ApiState {
    runtime: Runtime,
    store: Store,
    config: Arc<Config>,
    artifacts: Arc<ArtifactStore>,
    secrets: SharedSecrets,
    token_hash: String,
    rate: Arc<Mutex<VecDeque<Instant>>>,
    rate_limit: usize,
    admin: Option<AdminStore>,
    /// The OIDC sign-in service; `None` while `[auth] enabled = false`, so
    /// every sign-in endpoint fails closed.
    auth: Option<Arc<AuthService>>,
    /// Providers with a device sign-in still running in the background.
    oauth_inflight: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

impl ApiState {
    /// Fails fast when sign-in is enabled but misconfigured: a missing auth
    /// client id, a bad redirect, or an unusable provider must stop startup
    /// rather than surface at sign-in time.
    pub fn new(
        runtime: Runtime,
        store: Store,
        config: Arc<Config>,
        artifacts: Arc<ArtifactStore>,
        secrets: SharedSecrets,
    ) -> Result<Self> {
        let auth = if config.auth.enabled {
            Some(Arc::new(AuthService::new(
                &config,
                store.clone(),
                &secrets,
            )?))
        } else {
            None
        };
        Ok(Self {
            runtime,
            store,
            artifacts,
            secrets,
            token_hash: config.api.token_hash.to_ascii_lowercase(),
            rate: Arc::new(Mutex::new(VecDeque::new())),
            rate_limit: config.server.rate_limit_per_minute.max(1) as usize,
            admin: AdminStore::open(&config).ok(),
            auth,
            oauth_inflight: Arc::default(),
            config,
        })
    }
}

pub fn router(state: ApiState) -> Router {
    let shared = Arc::new(state);
    let protected = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/status", get(status))
        .route("/v1/usage", get(usage))
        .route("/v1/messages", post(send_message))
        .route("/v1/bots", get(list_bots).post(create_bot))
        .route("/v1/bots/{id}", get(bot_detail))
        .route("/v1/skills", get(list_skills).post(create_skill_pack))
        .route(
            "/v1/conversations",
            get(list_conversations).post(create_conversation),
        )
        .route(
            "/v1/conversations/{id}",
            patch(rename_conversation).delete(archive_conversation),
        )
        .route(
            "/v1/conversations/{id}/unarchive",
            post(unarchive_conversation),
        )
        .route("/v1/memories", get(list_memories).post(add_memory))
        .route("/v1/memories/{id}", delete(delete_memory))
        .route("/v1/providers", get(list_providers))
        .route("/v1/admin/providers", post(admin_add_provider))
        .route("/v1/admin/providers/{id}", delete(admin_remove_provider))
        .route(
            "/v1/admin/providers/{id}/oauth/start",
            post(admin_provider_oauth_start),
        )
        .route(
            "/v1/admin/providers/{id}/oauth/complete",
            post(admin_provider_oauth_complete),
        )
        .route("/v1/admin/connectors/{id}", put(admin_set_connector))
        .route("/v1/admin/connectors/{id}/test", post(admin_test_connector))
        .route(
            "/v1/admin/connectors/{id}/oauth/start",
            post(admin_connector_oauth_start),
        )
        .route(
            "/v1/admin/connectors/{id}/oauth/complete",
            post(admin_connector_oauth_complete),
        )
        .route("/v1/admin/audio", put(admin_set_audio))
        .route("/v1/admin/bots/{id}", put(admin_set_bot_profile))
        .route("/v1/admin/bots/{id}/config", put(admin_update_bot_config))
        .route("/v1/admin/restart", post(admin_restart))
        .route("/v1/connectors", get(list_connectors))
        .route("/v1/models", get(models))
        .route("/v1/conversations/{id}/model", put(select_model))
        .route(
            "/v1/conversations/{id}/messages",
            get(conversation_messages),
        )
        .route(
            "/v1/conversations/{id}/team",
            get(conversation_team).put(set_conversation_team),
        )
        .route("/v1/conversations/{id}/library", get(conversation_library))
        .route("/v1/tasks/{id}", get(task))
        .route("/v1/approvals", get(list_approvals))
        .route("/v1/approvals/{id}/decide", post(decide_approval))
        .route("/v1/events", get(events))
        .route("/v1/audit", get(audit))
        .route("/v1/artifacts/{hash}", get(artifact))
        .route("/v1/artifacts/{hash}/approve", post(approve_artifact))
        .route("/v1/auth/logout", post(auth_logout))
        .route("/v1/auth/invites", post(auth_invites))
        .route("/v1/users", get(list_users))
        .route("/v1/users/{id}/disable", post(disable_user))
        .route("/v1/users/{id}/enable", post(enable_user))
        .layer(RequestBodyLimitLayer::new(
            shared.config.server.body_limit_bytes,
        ));
    // Voice capture posts raw bytes up to the Telegram voice ceiling; the JSON
    // endpoints keep the smaller server body limit.
    //
    // The route layer alone is not enough: axum's own `DefaultBodyLimit`
    // (2 MiB) is what a `Bytes` extraction actually enforces, so the audio
    // route raises that limit too and the handler reads the body through
    // `to_bytes` with an explicit ceiling of its own.
    let audio = Router::new()
        .route("/v1/audio/transcribe", post(transcribe_audio))
        .layer(axum::extract::DefaultBodyLimit::max(
            crate::audio::DEFAULT_TELEGRAM_AUDIO_MAX_BYTES as usize,
        ));
    // Sign-in entry points are public: the caller by definition has no
    // session token yet. They share the JSON body limit and the per-minute
    // rate limit, but not the bearer-token middleware.
    let public_auth = Router::new()
        .route("/v1/auth/providers", get(auth_providers))
        .route("/v1/auth/login/start", post(auth_login_start))
        .route("/v1/auth/callback", get(auth_callback))
        .route("/login/microsoft", get(auth_login_microsoft))
        .layer(RequestBodyLimitLayer::new(
            shared.config.server.body_limit_bytes,
        ))
        .layer(middleware::from_fn_with_state(shared.clone(), rate_limit));
    let protected = protected
        .merge(audio)
        .layer(middleware::from_fn_with_state(shared.clone(), authorize))
        .layer(middleware::from_fn_with_state(shared.clone(), rate_limit));
    crate::web::router()
        .merge(public_auth)
        .merge(protected)
        .layer(TraceLayer::new_for_http())
        .with_state(shared)
}

async fn health(State(state): State<Arc<ApiState>>) -> Response {
    match state.store.health() {
        Ok(()) => Json(json!({
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
            "build": crate::web::build_sha(),
            "audio_enabled": state.config.audio.enabled,
            "audio_transcription_model": state.config.audio.transcription_model,
            "audio_understanding_model": state.config.audio.understanding_model,
            "firecrawl_enabled": state.config.firecrawl.enabled,
            "tavily_enabled": state.config.tavily.enabled,
            "telegram_enabled": state.config.telegram.enabled,
            "catalog_fetch": state.config.llm.catalog_fetch,
            "bot_chat_binding": true,
            "bot_profiles": true,
            "conversation_titles": true,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "error": "database unavailable"})),
        )
            .into_response()
            .tap(|_| warn!(%error, "health check failed")),
    }
}

trait Tap: Sized {
    fn tap(self, f: impl FnOnce(&Self)) -> Self {
        f(&self);
        self
    }
}
impl<T> Tap for T {}

async fn status(State(state): State<Arc<ApiState>>) -> Response {
    let pending = state.store.pending_task_count().unwrap_or(-1);
    let approvals = state
        .store
        .list_pending_approvals(50)
        .map(|items| items.len())
        .unwrap_or(0);
    let approval_items = state
        .store
        .list_pending_approvals(50)
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
struct UsageQuery {
    #[serde(default)]
    days: Option<i64>,
    #[serde(default)]
    bot: Option<String>,
    #[serde(default)]
    conversation_id: Option<String>,
}

/// `GET /v1/usage` rolls up `model.used` audit rows. `days` defaults to 7 and
/// is clamped to 1..=90; `bot` and `conversation_id` are optional filters.
async fn usage(
    State(state): State<Arc<ApiState>>,
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
    state.runtime.scan_owner_output(&body.to_string())?;
    Ok(Json(body))
}

#[derive(Debug, Deserialize)]
struct MessageRequest {
    conversation_id: String,
    text: String,
    #[serde(default)]
    bot: Option<String>,
}

async fn send_message(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<MessageRequest>,
) -> Result<Json<Value>> {
    validate_conversation_id(&request.conversation_id)?;
    let task = state.runtime.submit_text(
        &request.conversation_id,
        &request.text,
        request.bot.as_deref(),
    )?;
    Ok(Json(json!({"task": task_view(&task)})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelQuery {
    conversation_id: String,
    bot: Option<String>,
}

async fn models(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ModelQuery>,
) -> Result<Json<Value>> {
    validate_conversation_id(&query.conversation_id)?;
    state.runtime.refresh_model_catalogs().await?;
    let bot = query.bot.as_deref().unwrap_or("main");
    let selection = state.runtime.model_selection(&query.conversation_id, bot)?;
    let effective_selection = state
        .runtime
        .effective_model_selection(&query.conversation_id, bot)?;
    Ok(Json(json!({
        "providers": state.runtime.model_catalog()?,
        "entries": state.runtime.model_catalog_entries()?,
        "selection": selection,
        "effective_selection": effective_selection,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectModelRequest {
    provider: String,
    #[serde(default)]
    model: String,
}

async fn select_model(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<SelectModelRequest>,
) -> Result<Json<Value>> {
    validate_conversation_id(&id)?;
    state.runtime.refresh_model_catalogs().await?;
    let selection = if request.provider == crate::model_router::AUTO_PROVIDER {
        ModelSelection::auto_mode()
    } else {
        ModelSelection {
            provider: request.provider,
            model: request.model,
        }
    };
    let selection = state.runtime.select_model(&id, selection)?;
    Ok(Json(json!({"selection": selection})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryQuery {
    limit: Option<usize>,
}

async fn conversation_messages(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<Value>> {
    validate_conversation_id(&id)?;
    let messages = state
        .store
        .list_visible_messages(&id, query.limit.unwrap_or(100).clamp(1, 200))?
        .into_iter()
        .map(|message| {
            let content = if state.runtime.scan_owner_output(&message.content).is_ok() {
                message.content
            } else {
                "[Content withheld by policy]".to_owned()
            };
            let mut body = json!({
                "id": message.id,
                "role": message.role,
                "content": content,
                "created_at": message.created_at,
            });
            if let Some(name) = message
                .name
                .as_deref()
                .filter(|name| crate::orchestration::safe_bot_label(name))
                && state.runtime.scan_owner_output(name).is_ok()
            {
                body["name"] = json!(name);
            }
            if let Some(bot) = message
                .bot
                .as_deref()
                .filter(|bot| crate::orchestration::safe_bot_label(bot))
                && state.runtime.scan_owner_output(bot).is_ok()
            {
                body["bot"] = json!(bot);
            }
            body
        })
        .collect::<Vec<_>>();
    // Opening the thread is the owner reading it.
    state.store.mark_conversation_read(&id)?;
    Ok(Json(json!({"messages": messages})))
}

/// Team rejections must be HTTP 400 with the offending value named, so they do
/// not use the generic `SeraError::Validation` mapping (422, message dropped).
fn team_error(error: SeraError) -> Response {
    match error {
        SeraError::Validation(message) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": message, "code": "invalid_team"})),
        )
            .into_response(),
        other => other.into_response(),
    }
}

/// A conversation that may carry a team: it must exist and be a `room:`.
fn team_conversation(state: &ApiState, id: &str) -> Result<StoredConversation> {
    validate_conversation_id(id)?;
    let record = state
        .store
        .get_conversation(id)?
        .ok_or_else(|| SeraError::NotFound(format!("conversation {id}")))?;
    if crate::store::parse_stored_conversation_kind(&record.kind)? != ConversationKind::Room {
        return Err(SeraError::Validation(
            "a team is only available on a room conversation".into(),
        ));
    }
    Ok(record)
}

fn team_member_view(state: &ApiState, bots: &[crate::config::ConfiguredBot], bot: &str) -> Value {
    let entry = bots.iter().find(|candidate| candidate.id == bot);
    json!({
        "bot": bot,
        "role": entry.map(|entry| entry.role.clone()).unwrap_or_else(|| "unknown".into()),
        "enabled": entry.is_some_and(|entry| entry.enabled),
        "live": state.runtime.bot_loaded(bot),
    })
}

fn team_view(
    state: &ApiState,
    conversation_id: &str,
    members: &[String],
    mode: &str,
    rules: &str,
) -> Result<Value> {
    let bots = crate::config::list_configured_bots(&state.config)?;
    let members = members
        .iter()
        .map(|bot| team_member_view(state, &bots, bot))
        .collect::<Vec<_>>();
    Ok(json!({
        "conversation_id": conversation_id,
        "mode": mode,
        "members": members,
        "rules": rules,
        "max_members": crate::teams::MAX_TEAM_MEMBERS,
    }))
}

async fn conversation_team(State(state): State<Arc<ApiState>>, Path(id): Path<String>) -> Response {
    let body = (|| -> Result<Value> {
        team_conversation(&state, &id)?;
        let (members, mode, rules) = match state.store.get_conversation_team(&id)? {
            Some(team) => (team.members, team.mode, team.rules),
            // No team yet is an empty shape, never a 404.
            None => (
                Vec::new(),
                crate::teams::TeamMode::Selected.as_str().to_owned(),
                String::new(),
            ),
        };
        let body = team_view(&state, &id, &members, &mode, &rules)?;
        state.runtime.scan_owner_output(&body.to_string())?;
        Ok(body)
    })();
    match body {
        Ok(body) => Json(body).into_response(),
        Err(error) => team_error(error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetTeamRequest {
    members: Vec<String>,
    #[serde(default = "default_team_mode")]
    mode: String,
    #[serde(default)]
    rules: String,
}

fn default_team_mode() -> String {
    crate::teams::TeamMode::Selected.as_str().to_owned()
}

async fn set_conversation_team(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<SetTeamRequest>,
) -> Response {
    let body = (|| -> Result<Value> {
        team_conversation(&state, &id)?;
        let mode = crate::teams::TeamMode::from_name(&request.mode)?;
        crate::teams::validate_members(&request.members)?;
        if request.rules.chars().count() > 4000 {
            return Err(SeraError::Validation(
                "team rules must be at most 4000 characters".into(),
            ));
        }
        if request
            .rules
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err(SeraError::Validation(
                "team rules must not contain control characters".into(),
            ));
        }
        // Rules reach every later model request in the room, so they are
        // scanned before they are stored, not only on the way out.
        state.runtime.scan_owner_output(&request.rules)?;
        let bots = crate::config::list_configured_bots(&state.config)?;
        for member in &request.members {
            let entry = bots.iter().find(|candidate| candidate.id == *member);
            let Some(entry) = entry else {
                return Err(SeraError::Validation(format!("unknown bot {member}")));
            };
            if member == crate::orchestration::ORCHESTRATOR_BOT || entry.role == "orchestrator" {
                return Err(SeraError::Validation(format!(
                    "bot {member} cannot be a team member"
                )));
            }
            if !entry.enabled {
                return Err(SeraError::Validation(format!("bot {member} is disabled")));
            }
            if !state.runtime.bot_loaded(member) {
                return Err(SeraError::Validation(format!("bot {member} is not loaded")));
            }
        }
        state
            .store
            .set_conversation_team(&id, &request.members, mode.as_str(), &request.rules)?;
        let details = json!({
            "conversation_id": id,
            "members": request.members,
            "mode": mode.as_str(),
            "rules": request.rules,
        });
        state.store.record_audit(
            "owner",
            crate::teams::AUDIT_MEMBER_SET,
            None,
            None,
            None,
            None,
            Some(state.runtime.policy_version()),
            Some(&details),
        )?;
        state
            .store
            .append_event(crate::teams::AUDIT_MEMBER_SET, &details)?;
        let body = team_view(&state, &id, &request.members, mode.as_str(), &request.rules)?;
        state.runtime.scan_owner_output(&body.to_string())?;
        Ok(body)
    })();
    match body {
        Ok(body) => Json(body).into_response(),
        Err(error) => team_error(error),
    }
}

async fn conversation_library(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    validate_conversation_id(&id)?;
    state
        .store
        .get_conversation(&id)?
        .ok_or_else(|| SeraError::NotFound(format!("conversation {id}")))?;
    let entries = state
        .store
        .list_conversation_artifacts(&id)?
        .into_iter()
        .map(|entry| {
            let meta = state.artifacts.meta(&entry.hash).ok();
            json!({
                "action_id": entry.action_id,
                "tool": entry.tool,
                "hash": entry.hash,
                "filename": meta.as_ref().map(|meta| meta.filename.clone()),
                "title": meta.as_ref().map(|meta| meta.title.clone()),
                "created_at": entry.created_at,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"artifacts": entries})))
}

/// Raw owner audio, transcribed with the configured audio provider.
async fn transcribe_audio(
    State(state): State<Arc<ApiState>>,
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
            if let Err(error) = state.runtime.scan_owner_output(&text) {
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
fn audio_payload_too_large(limit: u64) -> Response {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        Json(json!({
            "error": format!("audio payload exceeds the {limit} byte limit"),
            "code": "validation",
        })),
    )
        .into_response()
}

fn task_view(task: &Task) -> Value {
    json!({
        "id": task.id,
        "kind": task.kind,
        "bot": task.bot,
        "status": task.status,
        "attempts": task.attempts,
        "created_at": task.created_at,
        "run_after": task.run_after,
    })
}

async fn task(State(state): State<Arc<ApiState>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let task = state.store.get_task(&id)?;
    Ok(Json(json!({"task": task_view(&task)})))
}

async fn list_approvals(State(state): State<Arc<ApiState>>) -> Result<Json<Value>> {
    let approvals = state
        .store
        .list_pending_approvals(100)?
        .into_iter()
        .map(|approval| {
            json!({
                "id": approval.id,
                "action_id": approval.action_id,
                "tool": approval.tool,
                "summary": approval.summary,
                "expires_at": approval.expires_at,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"approvals": approvals})))
}

#[derive(Debug, Deserialize)]
struct DecideRequest {
    decision: String,
    #[serde(default)]
    note: Option<String>,
}

async fn decide_approval(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<DecideRequest>,
) -> Result<Json<Value>> {
    if request
        .note
        .as_ref()
        .is_some_and(|note| note.chars().count() > 1000)
    {
        return Err(SeraError::Validation("note is too long".into()));
    }
    let decision = match request.decision.as_str() {
        "once" => Decision::Once,
        "always" => Decision::Always,
        "deny" => Decision::Deny,
        _ => {
            return Err(SeraError::Validation(
                "decision must be once, always, or deny".into(),
            ));
        }
    };
    let result = state.runtime.handle_approval(&id, decision).await?;
    Ok(Json(json!({
        "approval_id": result.approval.id,
        "approval_state": result.approval.state,
        "action_id": result.action.id,
        "action_status": result.action.status,
        "resume_task": result.resume_task,
    })))
}

#[derive(Debug, Deserialize)]
struct EventQuery {
    #[serde(default)]
    after: Option<i64>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn events(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Query(query): Query<EventQuery>,
) -> Sse<impl futures::Stream<Item = std::result::Result<SseEvent, Infallible>>> {
    let last = resume_cursor(&headers, query.after);
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let store = state.store.clone();
    let stream = futures::stream::unfold((last, store), move |(last, store)| async move {
        loop {
            tokio::time::sleep(Duration::from_millis(180)).await;
            match store.events_after(last, limit) {
                Ok(events) if !events.is_empty() => {
                    let next = events.last().map(|event| event.id).unwrap_or(last);
                    let items = events
                        .into_iter()
                        .map(|event| {
                            Ok(SseEvent::default()
                                .id(event.id.to_string())
                                .event(event.kind)
                                .data(event.payload.to_string()))
                        })
                        .collect::<Vec<_>>();
                    return Some((futures::stream::iter(items), (next, store)));
                }
                Ok(_) => continue,
                Err(error) => {
                    warn!(%error, "event stream query failed");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    })
    .flatten();
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

/// `?after=` wins; otherwise honor the standard SSE reconnect header.
fn resume_cursor(headers: &HeaderMap, after: Option<i64>) -> i64 {
    after
        .or_else(|| {
            headers
                .get("last-event-id")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<i64>().ok())
        })
        .unwrap_or(0)
        .max(0)
}

#[derive(Debug, Deserialize)]
struct AuditQuery {
    #[serde(default)]
    limit: Option<usize>,
}

async fn audit(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Value>> {
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let entries = state.store.list_audit(limit)?;
    Ok(Json(json!({"audit": entries})))
}

async fn artifact(
    State(state): State<Arc<ApiState>>,
    Path(hash): Path<String>,
) -> Result<Response> {
    let artifact = state.artifacts.load(&hash)?;
    let filename = artifact.filename.replace(['"', '\\', '\r', '\n'], "-");
    Ok((
        [
            ("content-type", "application/pdf"),
            ("x-content-type-options", "nosniff"),
            (
                "content-disposition",
                &format!("attachment; filename=\"{filename}\""),
            ),
        ],
        Body::from(artifact.bytes),
    )
        .into_response())
}

async fn approve_artifact(
    State(state): State<Arc<ApiState>>,
    Path(hash): Path<String>,
) -> Result<Json<Value>> {
    state.artifacts.approve(&state.store, &hash)?;
    Ok(Json(json!({
        "artifact_hash": hash,
        "content_approved": true,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBotRequest {
    id: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    skills: Option<Vec<String>>,
}

async fn create_bot(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateBotRequest>,
) -> Result<Json<Value>> {
    if state.runtime.bot_loaded(&request.id) {
        return Err(SeraError::InvalidState("bot already exists".into()));
    }
    state.runtime.scan_owner_output(&request.description)?;
    let scaffolded = scaffold_specialist_bot(
        &state.config,
        &request.id,
        &request.description,
        request.tools.as_deref(),
        request.skills.as_deref(),
        state.runtime.policy(),
    )?;
    if let Err(error) = state.store.record_audit(
        "owner",
        "bot.scaffolded",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({
            "id": scaffolded.id,
            "bot_file": scaffolded.bot_file,
            "prompt_file": scaffolded.prompt_file,
            "enabled": false,
            "role": "specialist",
            "tools": scaffolded.tools,
            "skills": scaffolded.skills,
        })),
    ) {
        let _ = std::fs::remove_file(&scaffolded.bot_path);
        let _ = std::fs::remove_file(&scaffolded.prompt_path);
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
            "bot_file": scaffolded.bot_file,
            "prompt_file": scaffolded.prompt_file,
        }
    })))
}

async fn list_bots(State(state): State<Arc<ApiState>>) -> Result<Json<Value>> {
    let configured = crate::config::list_configured_bots(&state.config)?;
    let bots = configured
        .into_iter()
        .map(|bot| {
            let profile = state
                .admin
                .as_ref()
                .and_then(|admin| admin.bot_profile(&bot.id));
            json!({
                "id": bot.id,
                "role": bot.role,
                "enabled": bot.enabled,
                "live": state.runtime.bot_loaded(&bot.id),
                "model": bot.model,
                "display_name": profile.as_ref().map(|p| p.display_name.clone()).filter(|name| !name.is_empty()),
                "description": profile.as_ref().map(|p| p.description.clone()).filter(|text| !text.is_empty()),
                "avatar": profile.as_ref().map(|p| p.avatar.clone()).filter(|avatar| !avatar.is_empty()),
                "tools": bot.tools,
                "skills": bot.skills,
                "require_approval": bot.require_approval,
            })
        })
        .collect::<Vec<_>>();
    let body = json!({ "bots": bots });
    state.runtime.scan_owner_output(&body.to_string())?;
    Ok(Json(body))
}

/// Bot config lists with the admin-console overlay merged over the bot file, so
/// the console sees values it just saved even before the restart.
fn bot_lists_with_override(
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
const MAX_INSTRUCTIONS_BYTES: usize = 32 * 1024;

/// `GET /v1/bots/{id}` — one bot's full owner-facing detail. No secrets and no
/// secret values; the response passes the egress scan.
async fn bot_detail(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    if !safe_bot_label(&id) {
        return Err(SeraError::NotFound(format!("bot {id}")));
    }
    let configured = crate::config::list_configured_bots(&state.config)?;
    let Some(entry) = configured.iter().find(|bot| bot.id == id) else {
        return Err(SeraError::NotFound(format!("bot {id}")));
    };
    let profile = state
        .admin
        .as_ref()
        .and_then(|admin| admin.bot_profile(&id));
    let loaded = state.runtime.bot(&id).cloned();
    let prompt = loaded.as_ref().map(|bot| bot.prompt.clone()).or_else(|| {
        crate::config::load_bot_prompt(&state.config, &id)
            .ok()
            .flatten()
    });
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
    let (tools, skills, require_approval) = bot_lists_with_override(&state, entry);
    let (instructions, instructions_bytes) = match prompt {
        Some(text) if text.len() <= MAX_INSTRUCTIONS_BYTES => (Some(text.clone()), text.len()),
        // Over the cap the content is withheld entirely; the byte count is kept.
        Some(text) => (None, text.len()),
        None => (None, 0),
    };
    let memory_count = state.store.count_memories()?;
    let artifact_count = state.store.count_bot_artifacts(&id)?;
    let body = json!({
        "id": entry.id,
        "display_name": profile.as_ref().map(|p| p.display_name.clone()).filter(|name| !name.is_empty()),
        "role": entry.role,
        "description": profile.as_ref().map(|p| p.description.clone()).filter(|text| !text.is_empty()),
        "avatar": profile.as_ref().map(|p| p.avatar.clone()).filter(|avatar| !avatar.is_empty()),
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
    state.runtime.scan_owner_output(&body.to_string())?;
    Ok(Json(body))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateSkillRequest {
    id: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    body: Option<String>,
}

async fn list_skills(State(state): State<Arc<ApiState>>) -> Result<Json<Value>> {
    let catalog = SkillCatalog::load(&state.config)?;
    let skills = catalog.summaries();
    Ok(Json(json!({ "skills": skills })))
}

async fn create_skill_pack(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateSkillRequest>,
) -> Result<Json<Value>> {
    state.runtime.scan_owner_output(&request.id)?;
    state.runtime.scan_owner_output(&request.description)?;
    if let Some(body) = &request.body {
        state.runtime.scan_owner_output(body)?;
    }
    let created = create_skill(
        &state.config,
        &request.id,
        &request.description,
        request.body.as_deref(),
        state.runtime.policy(),
    )?;
    if let Err(error) = state.store.record_audit(
        "owner",
        "skill.created",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({
            "id": created.id,
            "file": created.file,
            "live": false,
        })),
    ) {
        let _ = discard_created_skill(&state.config, &created);
        return Err(error);
    }
    Ok(Json(json!({
        "skill": {
            "id": created.id,
            "name": created.name,
            "description": created.description,
            "file": created.file,
            "live": false,
        }
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateConversationRequest {
    kind: String,
    slug: String,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

async fn create_conversation(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateConversationRequest>,
) -> Result<Json<Value>> {
    let kind = ConversationKind::from_name(&request.kind)?;
    let id = scoped_conversation_id(kind, &request.slug)?;
    let parent_id = match request.parent_id.as_deref() {
        None => None,
        Some("") => {
            return Err(SeraError::Validation(
                "conversation parent id is empty".into(),
            ));
        }
        Some(parent) => Some(parent),
    };
    if matches!(kind, ConversationKind::Section) && parent_id.is_some() {
        return Err(SeraError::Validation(
            "section conversations cannot have a parent".into(),
        ));
    }
    if matches!(kind, ConversationKind::Group) && parent_id.is_none() {
        return Err(SeraError::Validation(
            "group conversations require a section parent".into(),
        ));
    }
    let title = match request.title.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => {
            let title = normalize_conversation_title(raw)?;
            state.runtime.scan_owner_output(&title)?;
            Some(title)
        }
    };
    state
        .store
        .create_conversation_titled(&id, kind.as_str(), parent_id, title.as_deref())?;
    let record = state
        .store
        .get_conversation(&id)?
        .ok_or_else(|| SeraError::Internal("conversation missing after insert".into()))?;
    state.store.record_audit(
        "owner",
        "conversation.created",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({
            "id": record.id,
            "kind": record.kind,
            "parent_id": record.parent_id,
            "title": record.title,
            "accepts_turns": kind.accepts_turns(),
        })),
    )?;
    Ok(Json(json!({"conversation": conversation_view(&record)?})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationListQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    include_archived: Option<bool>,
}

async fn list_conversations(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ConversationListQuery>,
) -> Result<Json<Value>> {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let cursor = match query.cursor.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(value) => Some(parse_conversation_cursor(value)?),
    };
    let records = state.store.list_conversations(
        limit,
        cursor.as_ref().map(|(at, id)| (*at, id.as_str())),
        query.include_archived.unwrap_or(false),
    )?;
    let next_cursor = if records.len() == limit {
        records.last().map(conversation_cursor)
    } else {
        None
    };
    let conversations = records
        .iter()
        .map(conversation_view)
        .collect::<Result<Vec<_>>>()?;
    Ok(Json(json!({
        "conversations": conversations,
        "next_cursor": next_cursor,
    })))
}

fn conversation_view(record: &StoredConversation) -> Result<Value> {
    // Legacy rows predate the kind column; they are turn-capable direct chats.
    let kind = crate::store::parse_stored_conversation_kind(&record.kind)?;
    Ok(json!({
        "id": record.id,
        "kind": record.kind,
        "parent_id": record.parent_id,
        "title": record.title,
        "created_at": record.created_at,
        "updated_at": record.updated_at,
        "last_message_at": record.last_message_at,
        "archived_at": record.archived_at,
        "unread_count": record.unread_count,
        "accepts_turns": kind.accepts_turns(),
    }))
}

fn conversation_cursor(record: &StoredConversation) -> String {
    format!(
        "{}|{}",
        record
            .updated_at
            .to_rfc3339_opts(SecondsFormat::Millis, true),
        record.id
    )
}

fn parse_conversation_cursor(cursor: &str) -> Result<(DateTime<Utc>, String)> {
    let invalid = || SeraError::Validation("invalid conversation cursor".into());
    let (at, id) = cursor.rsplit_once('|').ok_or_else(invalid)?;
    let at = DateTime::parse_from_rfc3339(at)
        .map_err(|_| invalid())?
        .with_timezone(&Utc);
    validate_conversation_id(id)?;
    Ok((at, id.to_owned()))
}

fn normalize_conversation_title(raw: &str) -> Result<String> {
    let title = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return Err(SeraError::Validation(
            "conversation title must not be empty".into(),
        ));
    }
    let length = title.chars().count();
    if length > 120 {
        return Err(SeraError::Validation(format!(
            "conversation title has {length} characters; maximum is 120"
        )));
    }
    Ok(title)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameConversationRequest {
    title: String,
}

async fn rename_conversation(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<RenameConversationRequest>,
) -> Result<Json<Value>> {
    validate_conversation_id(&id)?;
    let title = normalize_conversation_title(&request.title)?;
    state.runtime.scan_owner_output(&title)?;
    if !state.store.rename_conversation(&id, &title)? {
        return Err(SeraError::NotFound(format!("conversation {id}")));
    }
    state.store.record_audit(
        "owner",
        "conversation.renamed",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": id, "title": title})),
    )?;
    let record = state
        .store
        .get_conversation(&id)?
        .ok_or_else(|| SeraError::NotFound(format!("conversation {id}")))?;
    Ok(Json(json!({"conversation": conversation_view(&record)?})))
}

async fn archive_conversation(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    validate_conversation_id(&id)?;
    if !state.store.archive_conversation(&id)? {
        return Err(SeraError::NotFound(format!("conversation {id}")));
    }
    state.store.record_audit(
        "owner",
        "conversation.archived",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": id})),
    )?;
    Ok(Json(json!({"id": id, "archived": true})))
}

async fn unarchive_conversation(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    validate_conversation_id(&id)?;
    if !state.store.unarchive_conversation(&id)? {
        return Err(SeraError::NotFound(format!("conversation {id}")));
    }
    state.store.record_audit(
        "owner",
        "conversation.unarchived",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": id})),
    )?;
    Ok(Json(json!({"id": id, "archived": false})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryListQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    query: Option<String>,
}

async fn list_memories(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MemoryListQuery>,
) -> Result<Json<Value>> {
    let limit = query.limit.unwrap_or(100).clamp(1, memory::MAX_LIST_LIMIT);
    let memories = match query
        .query
        .as_deref()
        .map(str::trim)
        .filter(|needle| !needle.is_empty())
    {
        Some(needle) => {
            let needle = needle.to_lowercase();
            let mut matches = memory::list(&state.store, memory::MAX_LIST_LIMIT)?
                .into_iter()
                .filter(|memory| {
                    memory.content.to_lowercase().contains(&needle)
                        || memory.kind.to_lowercase().contains(&needle)
                        || memory.source.to_lowercase().contains(&needle)
                })
                .collect::<Vec<_>>();
            matches.truncate(limit);
            matches
        }
        None => memory::list(&state.store, limit)?,
    };
    let body = json!({"memories": memories});
    state.runtime.scan_owner_output(&body.to_string())?;
    Ok(Json(body))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AddMemoryRequest {
    kind: String,
    content: String,
    #[serde(default)]
    source: Option<String>,
}

async fn add_memory(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<AddMemoryRequest>,
) -> Result<Json<Value>> {
    state.runtime.scan_owner_output(&request.content)?;
    let source = request
        .source
        .as_deref()
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .unwrap_or("operator:desktop");
    let memory = memory::add(&state.store, &request.kind, &request.content, source)?;
    state.store.record_audit(
        "owner",
        "memory.created",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({
            "id": memory.id,
            "kind": memory.kind,
            "source": memory.source,
        })),
    )?;
    Ok(Json(json!({"memory": memory})))
}

async fn delete_memory(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SeraError::Validation("invalid memory id".into()));
    }
    if !memory::delete(&state.store, &id)? {
        return Err(SeraError::NotFound(format!("memory {id}")));
    }
    state.store.record_audit(
        "owner",
        "memory.deleted",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": id})),
    )?;
    Ok(Json(json!({"deleted": id})))
}

async fn list_providers(State(state): State<Arc<ApiState>>) -> Result<Json<Value>> {
    let catalog = state.runtime.model_catalog()?;
    let llm = &state.config.llm;
    let mut settings: BTreeMap<String, ProviderConfig> = llm.providers.clone();
    settings.insert("default".to_owned(), llm.default_provider());
    let mut providers = catalog
        .into_iter()
        .map(|provider| {
            let managed = state
                .admin
                .as_ref()
                .is_some_and(|admin| admin.manages_provider(&provider.id));
            let config = settings.get(&provider.id);
            let routing_classes = llm
                .classes
                .iter()
                .filter(|(_, models)| {
                    models
                        .iter()
                        .any(|model| provider.models.iter().any(|advertised| advertised == model))
                })
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            let is_oauth =
                config.is_some_and(|c| {
                    c.auth.eq_ignore_ascii_case("oauth")
                        || matches!(c.kind.as_str(), "google-antigravity" | "muse-code")
                }) || matches!(provider.kind.as_str(), "google-antigravity" | "muse-code");
            let mut secrets = json!({
                "api_key": secret_status(&state.secrets, config.map(|c| c.api_key_secret.as_str())),
            });
            if is_oauth || config.is_some_and(|c| !c.refresh_token_secret.is_empty()) {
                secrets["refresh_token"] = secret_status(
                    &state.secrets,
                    config.map(|c| c.refresh_token_secret.as_str()),
                );
            }
            if is_oauth || config.is_some_and(|c| !c.oauth_client_id_secret.is_empty()) {
                secrets["oauth_client_id"] = secret_status(
                    &state.secrets,
                    config.map(|c| c.oauth_client_id_secret.as_str()),
                );
            }
            if is_oauth || config.is_some_and(|c| !c.oauth_client_secret_secret.is_empty()) {
                secrets["oauth_client_secret"] = secret_status(
                    &state.secrets,
                    config.map(|c| c.oauth_client_secret_secret.as_str()),
                );
            }
            json!({
                "id": provider.id,
                "kind": provider.kind,
                "auth": if is_oauth { "oauth" } else { "api_key" },
                "base_url": config.map(|c| c.base_url.as_str()).unwrap_or(""),
                "models": provider.models,
                "routing_classes": routing_classes,
                "data_egress_approved": provider.data_egress_approved,
                "catalog_fetch": provider.catalog_fetch,
                "catalog_verified": provider.catalog_verified,
                "managed": managed,
                "secrets": secrets,
            })
        })
        .collect::<Vec<_>>();

    if let Some(admin) = &state.admin {
        for (id, profile) in admin.snapshot().providers {
            if !providers
                .iter()
                .any(|p| p.get("id").and_then(Value::as_str) == Some(&id))
            {
                let is_oauth = profile.auth.eq_ignore_ascii_case("oauth")
                    || matches!(profile.kind.as_str(), "google-antigravity" | "muse-code");
                let mut secrets = json!({
                    "api_key": secret_status(&state.secrets, Some(profile.api_key_secret.as_str())),
                });
                if is_oauth || !profile.refresh_token_secret.is_empty() {
                    secrets["refresh_token"] =
                        secret_status(&state.secrets, Some(profile.refresh_token_secret.as_str()));
                }
                if is_oauth || !profile.oauth_client_id_secret.is_empty() {
                    secrets["oauth_client_id"] = secret_status(
                        &state.secrets,
                        Some(profile.oauth_client_id_secret.as_str()),
                    );
                }
                if is_oauth || !profile.oauth_client_secret_secret.is_empty() {
                    secrets["oauth_client_secret"] = secret_status(
                        &state.secrets,
                        Some(profile.oauth_client_secret_secret.as_str()),
                    );
                }
                let routing_classes = llm
                    .classes
                    .iter()
                    .filter(|(_, models)| {
                        models.iter().any(|model| {
                            profile.models.iter().any(|advertised| advertised == model)
                        })
                    })
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>();
                providers.push(json!({
                    "id": id,
                    "kind": profile.kind,
                    "auth": if is_oauth { "oauth" } else { "api_key" },
                    "base_url": profile.base_url,
                    "models": profile.models,
                    "routing_classes": routing_classes,
                    "data_egress_approved": profile.data_egress_approved,
                    "catalog_fetch": profile.catalog_fetch,
                    "catalog_verified": false,
                    "managed": true,
                    "secrets": secrets,
                }));
            }
        }
    }

    Ok(Json(json!({"providers": providers})))
}

async fn list_connectors(State(state): State<Arc<ApiState>>) -> Result<Json<Value>> {
    let connectors = &state.config.connectors;
    let oauth: [(&str, &dyn OAuthConnectorSettings); 7] = [
        ("google_drive", &connectors.google_drive),
        ("google_sheets", &connectors.google_sheets),
        ("google_calendar", &connectors.google_calendar),
        ("gmail", &connectors.gmail),
        ("outlook", &connectors.outlook),
        ("outlook_calendar", &connectors.outlook_calendar),
        ("notion", &connectors.notion),
    ];
    let mut views = oauth
        .into_iter()
        .map(|(id, connector)| oauth_connector_view(&state, id, connector))
        .collect::<Vec<_>>();
    let agentmail = &connectors.agentmail;
    let api_base = agentmail.api_base.trim();
    let inbox_id = agentmail.default_inbox_id.trim();
    views.push(json!({
        "id": "agentmail",
        "kind": "api_key",
        "enabled": agentmail.enabled,
        "redirect_uri": Value::Null,
        "api_base": if api_base.is_empty() { Value::Null } else { json!(api_base) },
        "default_inbox_id": if inbox_id.is_empty() { Value::Null } else { json!(inbox_id) },
        "secrets": {
            "api_key": secret_status(&state.secrets, Some(agentmail.api_key_secret.as_str())),
        },
    }));
    for (id, enabled, api_base, key_secret) in [
        (
            "tavily",
            state.config.tavily.enabled,
            state.config.tavily.api_base.as_str(),
            state.config.tavily.api_key_secret.as_str(),
        ),
        (
            "firecrawl",
            state.config.firecrawl.enabled,
            state.config.firecrawl.api_base.as_str(),
            state.config.firecrawl.api_key_secret.as_str(),
        ),
        (
            "elevenlabs",
            state.config.elevenlabs.enabled,
            state.config.elevenlabs.api_base.as_str(),
            state.config.elevenlabs.api_key_secret.as_str(),
        ),
    ] {
        views.push(json!({
            "id": id,
            "kind": "api_key",
            "enabled": enabled,
            "redirect_uri": Value::Null,
            "api_base": api_base,
            "secrets": {
                "api_key": secret_status(&state.secrets, Some(key_secret)),
            },
        }));
        if id == "elevenlabs" {
            let view = views.last_mut().expect("just pushed");
            view["auth"] = json!(match state.config.elevenlabs.auth {
                crate::config::ElevenLabsAuth::ApiKey => "api_key",
                crate::config::ElevenLabsAuth::Oauth => "oauth",
            });
            view["auth_modes"] = json!(["api_key", "oauth"]);
            view["secrets"]["access_token"] = secret_status(
                &state.secrets,
                Some(state.config.elevenlabs.oauth_access_token_secret.as_str()),
            );
            view["secrets"]["refresh_token"] = secret_status(
                &state.secrets,
                Some(state.config.elevenlabs.oauth_refresh_token_secret.as_str()),
            );
            view["oauth"] = json!({
                "client_id": state.config.elevenlabs.oauth_client_id,
                "authorize_url": crate::elevenlabs::ELEVENLABS_AUTHORIZE_URL,
                "cimd_path": crate::elevenlabs::CIMD_PATH,
                "callback_uri": crate::elevenlabs::oauth_callback_uri(),
                "scope": state.config.elevenlabs.oauth_scope,
            });
        }
    }
    for view in &mut views {
        let group = view["id"].as_str().map(connector_group).unwrap_or("Other");
        view["group"] = json!(group);
    }
    Ok(Json(json!({"connectors": views})))
}

/// Console grouping only; it carries no security meaning.
fn connector_group(id: &str) -> &'static str {
    match id {
        "elevenlabs" => "Voice & Media",
        "tavily" | "firecrawl" => "Web Research",
        "agentmail" => "Email",
        _ => "Productivity",
    }
}

fn api_key_probe<'a>(config: &'a Config, id: &str) -> Option<ApiKeyProbe<'a>> {
    match id {
        "agentmail" => Some(ApiKeyProbe::AgentMail(
            &config.connectors.agentmail.api_base,
        )),
        "tavily" => Some(ApiKeyProbe::Tavily(&config.tavily.api_base)),
        "firecrawl" => Some(ApiKeyProbe::Firecrawl(&config.firecrawl.api_base)),
        "elevenlabs" => Some(match config.elevenlabs.auth {
            crate::config::ElevenLabsAuth::ApiKey => {
                ApiKeyProbe::ElevenLabs(&config.elevenlabs.api_base)
            }
            crate::config::ElevenLabsAuth::Oauth => {
                ApiKeyProbe::ElevenLabsOauth(&config.elevenlabs.api_base)
            }
        }),
        _ => None,
    }
}

/// Owner "test connection". Makes at most one read-only, time-bounded request
/// for API-key connectors. OAuth connectors only report whether their secrets
/// are stored. Messages are fixed strings; no secret or provider body leaves.
async fn admin_test_connector(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> AdminResult<Json<Value>> {
    let roles = connector_secret_roles(&state.config, &id).map_err(admin_error)?;
    let checked_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let missing = roles.iter().find(|(_, name)| !state.secrets.has(name));
    let (ok, message) = match (missing, api_key_probe(&state.config, &id)) {
        (Some((_, name)), _) => (false, format!("Missing secret: {name}")),
        (None, Some(probe)) => {
            let key = roles
                .iter()
                .find(|(role, _)| *role == "api_key" || *role == "access_token")
                .and_then(|(_, name)| state.secrets.get(name))
                .unwrap_or_default();
            match probe_api_key(probe, &key).await {
                Ok(()) => (true, "Key accepted".to_owned()),
                Err(message) => (false, message),
            }
        }
        (None, None) => (
            true,
            "Client ID, client secret and refresh token are stored. The OAuth token exchange was not run.".to_owned(),
        ),
    };
    audit_config(
        &state,
        "config.connector_tested",
        json!({"connector": id, "ok": ok}),
    )?;
    Ok(Json(json!({
        "id": id,
        "ok": ok,
        "message": message,
        "checked_at": checked_at,
    })))
}

fn oauth_connector_view(
    state: &ApiState,
    id: &str,
    connector: &dyn OAuthConnectorSettings,
) -> Value {
    let redirect = connector.redirect_uri().trim();
    json!({
        "id": id,
        "kind": "oauth",
        "enabled": connector.oauth_enabled(),
        "redirect_uri": if redirect.is_empty() { Value::Null } else { json!(redirect) },
        "secrets": {
            "client_id": secret_status(&state.secrets, Some(connector.client_id_secret_name())),
            "client_secret": secret_status(&state.secrets, Some(connector.client_secret_secret_name())),
            "refresh_token": secret_status(&state.secrets, Some(connector.refresh_token_secret_name())),
        },
    })
}

type AdminResult<T> = std::result::Result<T, (StatusCode, Json<Value>)>;

/// Admin rejections carry our own safe message so the console can explain the
/// fix (for example a missing secret name) without exposing provider bodies.
fn admin_error(error: SeraError) -> (StatusCode, Json<Value>) {
    let (status, message) = match &error {
        SeraError::NotFound(message) => (StatusCode::NOT_FOUND, message.clone()),
        SeraError::InvalidState(message) => (StatusCode::CONFLICT, message.clone()),
        SeraError::Validation(message) | SeraError::Config(message) => {
            (StatusCode::UNPROCESSABLE_ENTITY, message.clone())
        }
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

fn admin_store(state: &ApiState) -> AdminResult<&AdminStore> {
    state
        .admin
        .as_ref()
        .ok_or_else(|| admin_error(SeraError::Internal("administration is unavailable".into())))
}

fn audit_config(state: &ApiState, event: &str, details: Value) -> AdminResult<()> {
    state
        .store
        .record_audit(
            "owner",
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
struct AddProviderRequest {
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

async fn admin_add_provider(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<AddProviderRequest>,
) -> AdminResult<(StatusCode, Json<Value>)> {
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
        "config.provider_saved",
        json!({"provider": request.id, "kind": kind, "catalog_fetch": catalog_fetch}),
    )?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"saved": true, "restart_required": true})),
    ))
}

async fn admin_remove_provider(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> AdminResult<Json<Value>> {
    let admin = admin_store(&state)?;
    admin.remove_provider(&id).map_err(admin_error)?;
    audit_config(&state, "config.provider_removed", json!({"provider": id}))?;
    Ok(Json(json!({"removed": id, "restart_required": true})))
}

#[derive(Deserialize, Default)]
struct OAuthStartRequest {
    #[serde(default)]
    challenge: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    redirect_uri: String,
}

#[derive(Deserialize)]
struct OAuthCompleteRequest {
    code: String,
    verifier: String,
    redirect_uri: String,
}

fn oauth_profile(state: &ApiState, admin: &AdminStore, id: &str) -> Result<ProviderConfig> {
    admin
        .snapshot()
        .providers
        .get(id)
        .or_else(|| state.config.llm.providers.get(id))
        .cloned()
        .ok_or_else(|| SeraError::NotFound(format!("provider {id}")))
}

fn oauth_secret_name(configured: &str, default: &str) -> String {
    if configured.trim().is_empty() {
        default.to_owned()
    } else {
        configured.to_owned()
    }
}

fn release_oauth(state: &ApiState, id: &str) {
    state
        .oauth_inflight
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(id);
}

/// Begin a provider sign-in. PKCE providers get their authorization URL; the
/// browser leg and loopback listener run on the owner's machine. Device
/// providers start a bounded background poll and return the code to enter.
async fn admin_provider_oauth_start(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: Option<Json<OAuthStartRequest>>,
) -> AdminResult<Json<Value>> {
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
async fn admin_provider_oauth_complete(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<OAuthCompleteRequest>,
) -> AdminResult<Json<Value>> {
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
async fn admin_connector_oauth_start(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<OAuthStartRequest>,
) -> AdminResult<Json<Value>> {
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
        "config.connector_oauth_started",
        json!({"connector": id}),
    )?;
    Ok(Json(json!({"flow": "pkce", "url": url})))
}

/// Complete the ElevenLabs sign-in: the server exchanges the code at the
/// pinned token endpoint and persists both tokens to the age store. Tokens
/// are never returned to the caller.
async fn admin_connector_oauth_complete(
    State(state): State<Arc<ApiState>>,
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
        "config.connector_oauth_signed_in",
        json!({"connector": id, "ok": true}),
    )?;
    Ok(Json(json!({"signed_in": true, "restart_required": true})))
}

#[derive(Deserialize)]
struct ConnectorRequest {
    enabled: bool,
    #[serde(default)]
    values: BTreeMap<String, String>,
    /// ElevenLabs only: switch auth mode ("api_key" or "oauth").
    #[serde(default)]
    auth: Option<String>,
}

async fn admin_set_connector(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<ConnectorRequest>,
) -> AdminResult<Json<Value>> {
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
        "config.connector_saved",
        json!({"connector": id, "enabled": request.enabled, "credentials": request.values.keys().collect::<Vec<_>>(), "auth": request.auth}),
    )?;
    Ok(Json(json!({"saved": true, "restart_required": true})))
}

#[derive(Deserialize)]
struct AudioRequest {
    enabled: bool,
    #[serde(default)]
    transcription_model: Option<String>,
    #[serde(default)]
    understanding_model: Option<String>,
}

async fn admin_set_audio(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<AudioRequest>,
) -> AdminResult<Json<Value>> {
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
        "config.audio_saved",
        json!({"enabled": request.enabled}),
    )?;
    Ok(Json(json!({"saved": true, "restart_required": true})))
}

#[derive(Deserialize)]
struct BotProfileRequest {
    display_name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    avatar: String,
}

async fn admin_set_bot_profile(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<BotProfileRequest>,
) -> AdminResult<Json<Value>> {
    let admin = admin_store(&state)?;
    if !state.runtime.bot_loaded(&id) {
        let configured = crate::config::list_configured_bots(&state.config).map_err(admin_error)?;
        if !configured.iter().any(|bot| bot.id == id) {
            return Err(admin_error(SeraError::NotFound(format!("bot {id}"))));
        }
    }
    for value in [&request.display_name, &request.description, &request.avatar] {
        state
            .runtime
            .scan_owner_output(value)
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
    audit_config(&state, "config.bot_profile_saved", json!({"bot": id}))?;
    Ok(Json(json!({"saved": true, "restart_required": false})))
}

const MAX_BOT_CONFIG_TOOLS: usize = 32;
const MAX_BOT_CONFIG_SKILLS: usize = 16;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BotConfigRequest {
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    skills: Option<Vec<String>>,
    #[serde(default)]
    require_approval: Option<Vec<String>>,
}

/// Tool list rules matching the desktop `validate_tool_list` semantics on top
/// of the server's tool registry: at most 32 unique safe ids, `delegate` only
/// for the orchestrator, and every other tool must be a registered tool.
fn validate_bot_tool_list(
    state: &ApiState,
    tools: &[String],
    role: BotRole,
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
        } else if !state.runtime.gateway().registry().is_registered(tool) {
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
/// server's catalog check: at most 16 unique, valid skill ids that exist.
fn validate_bot_skill_list(state: &ApiState, skills: &[String]) -> Result<Vec<String>> {
    if skills.len() > MAX_BOT_CONFIG_SKILLS {
        return Err(SeraError::Validation(format!(
            "at most {MAX_BOT_CONFIG_SKILLS} skills are allowed"
        )));
    }
    let catalog = SkillCatalog::load(&state.config)?;
    let known = catalog.skill_ids();
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
fn validate_require_approval_list(list: &[String], tools: &[String]) -> Result<Vec<String>> {
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
async fn admin_update_bot_config(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<BotConfigRequest>,
) -> AdminResult<Json<Value>> {
    let admin = admin_store(&state)?;
    let configured = crate::config::list_configured_bots(&state.config).map_err(admin_error)?;
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
            state.runtime.scan_owner_output(item).map_err(admin_error)?;
        }
    }
    let role = state.runtime.bot(&id).map(|bot| bot.config.role).unwrap_or(
        if entry.role == BotRole::Orchestrator.as_str() {
            BotRole::Orchestrator
        } else {
            BotRole::Specialist
        },
    );
    let (current_tools, _, _) = bot_lists_with_override(&state, entry);
    let tools = match &request.tools {
        Some(tools) => Some(validate_bot_tool_list(&state, tools, role).map_err(admin_error)?),
        None => None,
    };
    let skills = match &request.skills {
        Some(skills) => Some(validate_bot_skill_list(&state, skills).map_err(admin_error)?),
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
    audit_config(&state, "config.bot_config_saved", json!({"bot": id}))?;
    Ok(Json(json!({"saved": true, "restart_required": true})))
}

#[derive(Deserialize)]
struct RestartRequest {
    #[serde(default)]
    confirm: bool,
}

async fn admin_restart(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RestartRequest>,
) -> AdminResult<(StatusCode, Json<Value>)> {
    if !request.confirm {
        return Err(admin_error(SeraError::Validation(
            "restart requires explicit confirmation".into(),
        )));
    }
    let pending = state.store.pending_task_count().map_err(admin_error)?;
    if pending > 0 {
        return Err(admin_error(SeraError::InvalidState(format!(
            "{pending} task(s) are still in flight; wait for them to finish"
        ))));
    }
    audit_config(&state, "config.restart_requested", Value::Null)?;
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(400)).await;
        tracing::warn!("restart requested by the owner console; exiting for systemd");
        std::process::exit(1);
    });
    Ok((StatusCode::ACCEPTED, Json(json!({"restarting": true}))))
}

/// Names only. Secret values never leave the age store.
fn secret_status(secrets: &SharedSecrets, name: Option<&str>) -> Value {
    match name.filter(|name| !name.is_empty()) {
        Some(name) => json!({"name": name, "configured": secrets.has(name)}),
        None => json!({"name": Value::Null, "configured": false}),
    }
}

/// The identity behind an authenticated request, threaded into handlers
/// through request extensions. Carries only ids and the role — never a raw
/// token (the `token_hash` field is the sha256 the server stores).
#[derive(Clone, Debug)]
pub struct ActingUser {
    pub id: String,
    pub role: UserRole,
    /// The device-token row id when the bearer token is a session; `None`
    /// for the static config token, which has no revocable session row.
    pub session_id: Option<String>,
    pub token_hash: String,
}

impl ActingUser {
    pub fn is_admin(&self) -> bool {
        self.role == UserRole::Admin
    }
}

/// Gate a handler on the admin role. Members receive an explicit 403.
fn require_admin(user: &ActingUser) -> Result<()> {
    if user.role != UserRole::Admin {
        return Err(SeraError::Forbidden(
            "this endpoint requires the admin role".into(),
        ));
    }
    Ok(())
}

/// Resolve the user behind a bearer token, or fail closed.
///
/// The static config token is the operator's own credential and maps to the
/// bootstrap admin. Device tokens map to the owning user recorded on the
/// row (existing operator tokens were backfilled to the admin by migration
/// 0008). Unknown owners, disabled accounts, and revoked or expired tokens
/// all reject identically, so a rejected caller learns nothing about which
/// check failed.
async fn authorize(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Response {
    if !state.config.api.enabled {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "api disabled"})),
        )
            .into_response();
    }
    let Some(header) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return unauthorized();
    };
    let Some(token) = header.strip_prefix("Bearer ") else {
        return unauthorized();
    };
    if token.len() < 32 || token.len() > 512 {
        return unauthorized();
    }
    let candidate = sha256_hex(token);
    if constant_time_eq(&candidate, &state.token_hash) {
        let Some(admin) = active_user(&state, None) else {
            return unauthorized();
        };
        request.extensions_mut().insert(ActingUser {
            id: admin.id,
            role: admin.role,
            session_id: None,
            token_hash: candidate,
        });
        return next.run(request).await;
    }
    let now = chrono::Utc::now();
    match state.store.device_token_by_hash(&candidate) {
        Ok(Some(record)) if record.active_at(now) => {
            let Some(user) = active_user(&state, record.user_id.as_deref()) else {
                return unauthorized();
            };
            if let Err(error) = state.store.touch_device_token(&record.id, now) {
                warn!(%error, "device token last-used update failed");
            }
            request.extensions_mut().insert(ActingUser {
                id: user.id,
                role: user.role,
                session_id: Some(record.id),
                token_hash: candidate,
            });
            next.run(request).await
        }
        Ok(_) => unauthorized(),
        Err(error) => {
            warn!(%error, "device token lookup failed");
            unauthorized()
        }
    }
}

/// The active user behind a token, or `None` when the owner is missing or
/// disabled. A `None` owner id maps to the bootstrap admin (pre-migration
/// rows were backfilled, but the lookup stays explicit either way).
fn active_user(state: &ApiState, user_id: Option<&str>) -> Option<User> {
    let user = match user_id {
        Some(id) => state.store.get_user(id).ok()??,
        None => state.store.admin_user().ok()??,
    };
    (user.status == UserStatus::Active).then_some(user)
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "unauthorized"})),
    )
        .into_response()
}

// ---- Auth endpoints (public entry, protected sessions) ----

/// The providers list rendered by web, desktop, and mobile clients. Public
/// and derived from config; `google`/`github` always report disabled in v1.
async fn auth_providers(State(state): State<Arc<ApiState>>) -> Json<Value> {
    let providers: Vec<Value> = auth::provider_report(&state.config.auth)
        .into_iter()
        .map(|provider| json!({"id": provider.id, "enabled": provider.enabled}))
        .collect();
    Json(json!({"providers": providers}))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginStartRequest {
    provider: String,
    #[serde(default)]
    invite_code: Option<String>,
}

/// Start a browser sign-in. Public by definition (no session yet); the
/// invite code, when presented, is validated but consumed only at the
/// callback — inside the transaction that creates the account.
async fn auth_login_start(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<LoginStartRequest>,
) -> Result<Json<Value>> {
    let service = auth_service(&state)?;
    if request.provider.len() > 64 {
        return Err(SeraError::Validation(
            "provider must be a known provider id".into(),
        ));
    }
    let invite = request
        .invite_code
        .as_deref()
        .map(str::trim)
        .filter(|code| !code.is_empty());
    let url = service.login_start(&request.provider, invite).await?;
    Ok(Json(json!({"url": url})))
}

/// The `/login` page's Microsoft button target: start the flow and hand the
/// browser to the provider. Failures render the same minimal error page as
/// the callback (fail closed, no provider detail).
async fn auth_login_microsoft(State(state): State<Arc<ApiState>>) -> Response {
    let Some(service) = state.auth.as_ref() else {
        return auth_error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Sign-in is not enabled on this deployment.",
        );
    };
    match service.login_start("microsoft", None).await {
        Ok(url) => (
            StatusCode::FOUND,
            [
                (header::LOCATION, url.as_str()),
                (header::CACHE_CONTROL, "no-store"),
            ],
        )
            .into_response(),
        Err(error) => {
            warn!(%error, "web login start failed");
            auth_error_page(
                StatusCode::BAD_REQUEST,
                "Sign-in could not be started. Check that sign-in is configured, then try again.",
            )
        }
    }
}

#[derive(Debug, Deserialize)]
struct CallbackParams {
    state: String,
    code: String,
}

/// The OIDC redirect target. Completes the sign-in and shows the new
/// session token exactly once, with a paste-into-your-client hint; the
/// token is never stored in the clear and never shown again.
async fn auth_callback(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<CallbackParams>,
) -> Response {
    let Some(service) = state.auth.as_ref() else {
        return auth_error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Sign-in is not enabled on this deployment.",
        );
    };
    match service.callback(&params.state, &params.code).await {
        Ok((_user, token)) => html_response(StatusCode::OK, session_token_page(&token)),
        Err(error) => {
            warn!(%error, "auth callback failed");
            auth_error_page(
                StatusCode::BAD_REQUEST,
                "Sign-in could not be completed. The link may have expired or been used; start again from the client.",
            )
        }
    }
}

/// Revoke the caller's own session. The static config token has no session
/// row, so it cannot be revoked here — rotating it is a config change.
async fn auth_logout(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    if user.session_id.is_none() {
        return Err(SeraError::InvalidState(
            "this token is the server configuration token and cannot be revoked here".into(),
        ));
    }
    state.store.revoke_session(&user.token_hash)?;
    audit_auth(&state, &user.id, "auth.logout", json!({}))?;
    Ok(Json(json!({"revoked": true})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MintInviteRequest {
    uses: u32,
}

/// Mint an invitation (admin only). The raw code is returned exactly once;
/// the server keeps only its SHA-256 hash.
async fn auth_invites(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<MintInviteRequest>,
) -> Result<Json<Value>> {
    require_admin(&user)?;
    let service = auth_service(&state)?;
    if request.uses == 0 || request.uses > 100 {
        return Err(SeraError::Validation(
            "uses must be between 1 and 100".into(),
        ));
    }
    let code = service.mint_invite(request.uses, &user.id)?;
    audit_auth(
        &state,
        &user.id,
        "auth.invite_minted",
        json!({"uses": request.uses}),
    )?;
    Ok(Json(json!({"code": code})))
}

/// List accounts (admin only). Member sessions get 403 and never see the
/// user directory.
async fn list_users(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    require_admin(&user)?;
    let users = state.store.list_users()?;
    let users: Vec<Value> = users.iter().map(user_view).collect();
    Ok(Json(json!({"users": users})))
}

fn user_view(user: &User) -> Value {
    json!({
        "id": user.id,
        "provider_subject": user.provider_subject,
        "email": user.email,
        "display_name": user.display_name,
        "role": user.role.as_str(),
        "status": user.status.as_str(),
        "created_at": user.created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
    })
}

async fn disable_user(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    change_user_status(&state, &user, &id, UserStatus::Disabled)
}

async fn enable_user(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    change_user_status(&state, &user, &id, UserStatus::Active)
}

/// Enable or disable an account (admin only). A disabled user's sessions
/// stop authorizing immediately. The admin account itself cannot be
/// disabled: it owns recovery, and locking it out would strand the
/// deployment with no admin path at all.
fn change_user_status(
    state: &ApiState,
    acting: &ActingUser,
    id: &str,
    status: UserStatus,
) -> Result<Json<Value>> {
    require_admin(acting)?;
    crate::util::validate_id(id)?;
    let target = state
        .store
        .get_user(id)?
        .ok_or_else(|| SeraError::NotFound(format!("user {id}")))?;
    if status == UserStatus::Disabled && target.role == UserRole::Admin {
        return Err(SeraError::Validation(
            "the admin account cannot be disabled".into(),
        ));
    }
    state.store.set_user_status(id, status)?;
    audit_auth(
        state,
        &acting.id,
        if status == UserStatus::Disabled {
            "auth.user_disabled"
        } else {
            "auth.user_enabled"
        },
        json!({"user_id": id, "status": status.as_str()}),
    )?;
    Ok(Json(json!({"id": id, "status": status.as_str()})))
}

/// The auth service, or an explicit fail-closed error while sign-in is off.
fn auth_service(state: &ApiState) -> Result<&AuthService> {
    state
        .auth
        .as_deref()
        .ok_or_else(|| SeraError::Forbidden("sign-in is disabled".into()))
}

/// Record an auth audit event under the acting user's id.
fn audit_auth(state: &ApiState, actor: &str, event: &str, details: Value) -> Result<()> {
    state
        .store
        .record_audit(actor, event, None, None, None, None, None, Some(&details))?;
    Ok(())
}

/// Minimal page shell matching the landing aesthetic: inline CSS only, no
/// scripts, no external origins. `body` is server-controlled text only —
/// never provider- or user-supplied content.
fn auth_page(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="dark light">
<title>{title}</title>
<style>
  :root {{ color-scheme: dark light; --bg: #101418; --panel: #171d23; --border: #2b343d; --text: #e6edf3; --muted: #9aa7b4; --accent: #4d9fff; }}
  * {{ box-sizing: border-box; }}
  body {{ margin: 0; min-height: 100vh; background: var(--bg); color: var(--text); font: 16px/1.55 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; display: flex; justify-content: center; padding: 40px 20px; }}
  main {{ width: 100%; max-width: 640px; }}
  .eyebrow {{ margin: 0 0 8px; color: var(--muted); font-size: 13px; letter-spacing: 0.08em; text-transform: uppercase; }}
  h1 {{ margin: 0 0 12px; font-size: 40px; line-height: 1.1; letter-spacing: -0.02em; }}
  p {{ margin: 0 0 12px; }}
  .lede {{ color: var(--muted); }}
  section {{ margin-top: 20px; padding: 20px; background: var(--panel); border: 1px solid var(--border); border-radius: 12px; }}
  code {{ display: block; margin: 12px 0; padding: 12px; border: 1px solid var(--border); border-radius: 8px; background: #0d1116; font-size: 13px; word-break: break-all; user-select: all; }}
  .meta {{ color: var(--muted); font-size: 14px; }}
</style>
</head>
<body>
<main>
<header>
<p class="eyebrow">SeraBot</p>
<h1>{title}</h1>
</header>
<section>
{body}
</section>
</main>
</body>
</html>
"#
    )
}

/// The one-time session token page shown after the OIDC callback.
fn session_token_page(token: &str) -> String {
    auth_page(
        "Signed in",
        &format!(
            "<p class=\"lede\">Your session token is below. It is shown <strong>once</strong> and cannot be requested again.</p>\n<code>{token}</code>\n<p class=\"meta\">Paste it into your desktop or mobile client's sign-in field. Treat it like a password; the server stores only its hash.</p>"
        ),
    )
}

/// A plain, link-free error page for failed sign-ins.
fn auth_error_page(status: StatusCode, message: &str) -> Response {
    html_response(
        status,
        auth_page("Sign-in", &format!("<p class=\"lede\">{message}</p>")),
    )
}

fn html_response(status: StatusCode, page: String) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CONTENT_SECURITY_POLICY, crate::web::CSP),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        page,
    )
        .into_response()
}

async fn rate_limit(State(state): State<Arc<ApiState>>, request: Request, next: Next) -> Response {
    {
        let mut entries = state.rate.lock().await;
        let now = Instant::now();
        while entries
            .front()
            .is_some_and(|entry| now.duration_since(*entry) > Duration::from_secs(60))
        {
            entries.pop_front();
        }
        if entries.len() >= state.rate_limit {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "60")],
                Json(json!({"error": "rate limit exceeded"})),
            )
                .into_response();
        }
        entries.push_back(now);
    }
    next.run(request).await
}

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: String,
    code: &'static str,
}

impl IntoResponse for SeraError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            SeraError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found", "not found"),
            SeraError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden", "forbidden"),
            SeraError::PolicyViolation => (
                StatusCode::FORBIDDEN,
                "policy_violation",
                "blocked by content policy",
            ),
            SeraError::Validation(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation",
                "invalid request",
            ),
            SeraError::InvalidState(_) => (
                StatusCode::CONFLICT,
                "invalid_state",
                "the requested operation is not valid in the current state",
            ),
            _ => {
                warn!(%self, "api request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal server error",
                )
            }
        };
        (
            status,
            Json(ApiErrorBody {
                error: message.to_string(),
                code,
            }),
        )
            .into_response()
    }
}

pub fn sanitize_for_event(value: &str) -> String {
    truncate_chars(value, 2000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BotConfig, LoadedBot};
    use crate::gateway::{Gateway, ToolRegistry};
    use crate::llm::LlmClient;
    use crate::policy::Policy;
    use crate::runtime::{NullNotifier, Runtime};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn app() -> (Router, Arc<Config>, Store) {
        app_with_policy(true, Policy::empty())
    }

    fn app_with_policy(egress_approved: bool, policy: Policy) -> (Router, Arc<Config>, Store) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::env::current_dir().unwrap());
        app_with_root(egress_approved, policy, root)
    }

    fn app_with_root(
        egress_approved: bool,
        policy: Policy,
        base_dir: std::path::PathBuf,
    ) -> (Router, Arc<Config>, Store) {
        app_with_fleet(
            egress_approved,
            policy,
            base_dir,
            vec![test_bot(
                "main",
                crate::orchestration::BotRole::Orchestrator,
            )],
        )
    }

    fn test_bot(name: &str, role: crate::orchestration::BotRole) -> LoadedBot {
        LoadedBot {
            config: BotConfig {
                name: name.into(),
                trigger: "command".into(),
                model: None,
                system_prompt: format!("prompts/{name}.txt"),
                skills: vec![],
                tools: vec![],
                require_approval: vec![],
                enabled: true,
                temperature: None,
                role,
                delegates: vec![],
            },
            prompt: "test prompt".into(),
        }
    }

    fn app_with_fleet(
        egress_approved: bool,
        policy: Policy,
        base_dir: std::path::PathBuf,
        bots: Vec<LoadedBot>,
    ) -> (Router, Arc<Config>, Store) {
        app_with_auth_stack(egress_approved, policy, base_dir, bots, None)
    }

    /// Full app stack, optionally with sign-in enabled. The auth service is
    /// built against the same store as the API and pointed at `auth_issuer`
    /// (a loopback mock identity provider in tests).
    fn app_with_auth_stack(
        egress_approved: bool,
        policy: Policy,
        base_dir: std::path::PathBuf,
        bots: Vec<LoadedBot>,
        auth_issuer: Option<&str>,
    ) -> (Router, Arc<Config>, Store) {
        let mut config = Config::default();
        config.api.enabled = true;
        config.api.token_hash = sha256_hex("test-token-that-is-long-enough-123");
        config.llm.data_egress_approved = egress_approved;
        config.llm.model = "test".into();
        config.llm.base_url = "https://example.com/v1".into();
        config.llm.models = vec!["test-alternative".into()];
        if auth_issuer.is_some() {
            config.auth.enabled = true;
            config.auth.providers = vec!["microsoft".into()];
            config.auth.base_url = "https://bot.example.example".into();
            config.auth.max_users = 10;
        }
        config.base_dir = base_dir;
        let directory = std::env::temp_dir().join(format!("serabot-api-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let database = directory.join("serabot.db");
        config.database.path = database.to_string_lossy().into_owned();
        let config = Arc::new(config);
        let store = Store::open(&database, 384, 5000).unwrap();
        let gateway = Gateway::new(
            store.clone(),
            Arc::new(policy),
            ToolRegistry::default(),
            1,
            false,
        );
        let llm = LlmClient::new("https://example.com/v1", "test", 5, 50)
            .unwrap()
            .with_model("test");
        let runtime = Runtime::new(
            config.clone(),
            store.clone(),
            gateway,
            crate::model_router::ModelRouter::new(
                &config.llm,
                std::collections::BTreeMap::from([("default".into(), llm)]),
            )
            .unwrap(),
            bots,
            Arc::new(NullNotifier),
            tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        let artifacts = Arc::new(
            ArtifactStore::new(
                std::env::temp_dir().join(format!("serabot-api-test-{}", uuid::Uuid::new_v4())),
            )
            .unwrap(),
        );
        let mut secrets = std::collections::BTreeMap::from([(
            config.llm.api_key_secret.clone(),
            "synthetic-test-key".to_string(),
        )]);
        if auth_issuer.is_some() {
            secrets.insert(
                config.auth.client_id_secret.clone(),
                crate::auth::test_idp::TEST_CLIENT_ID.to_string(),
            );
        }
        let secrets = SharedSecrets::new(crate::secrets::SecretStore::from_map(secrets));
        let auth = match auth_issuer {
            Some(issuer) => {
                let mut service = AuthService::new(&config, store.clone(), &secrets).unwrap();
                service.set_issuer_url(issuer).unwrap();
                Some(service)
            }
            None => None,
        };
        let mut state =
            ApiState::new(runtime, store.clone(), config.clone(), artifacts, secrets).unwrap();
        if let Some(service) = auth {
            state.auth = Some(Arc::new(service));
        }
        (router(state), config, store)
    }

    /// An app with sign-in enabled and the auth service pointed at a
    /// synthetic mock identity provider, for exercising the auth endpoints.
    async fn auth_app() -> (Router, Arc<Config>, Store, crate::auth::test_idp::MockIdp) {
        let idp = crate::auth::test_idp::mock_idp().await;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::env::current_dir().unwrap());
        let (app, config, store) = app_with_auth_stack(
            true,
            Policy::empty(),
            root,
            vec![test_bot(
                "main",
                crate::orchestration::BotRole::Orchestrator,
            )],
            Some(&idp.issuer),
        );
        (app, config, store, idp)
    }

    fn request(
        _app: &Router,
        method: &str,
        uri: &str,
        auth: bool,
        body: Option<Value>,
    ) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if auth {
            builder = builder.header(
                "authorization",
                format!("Bearer {}", "test-token-that-is-long-enough-123"),
            );
        }
        builder = builder.header("content-type", "application/json");
        builder
            .body(
                body.map(|value| Body::from(value.to_string()))
                    .unwrap_or_else(Body::empty),
            )
            .unwrap()
    }

    fn request_with_token(
        method: &str,
        uri: &str,
        token: &str,
        body: Option<Value>,
    ) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(
                body.map(|value| Body::from(value.to_string()))
                    .unwrap_or_else(Body::empty),
            )
            .unwrap()
    }

    #[tokio::test]
    async fn requires_owner_token() {
        let (app, _, _) = app();
        let router = app.clone();
        let response = router
            .oneshot(request(&app, "GET", "/v1/health", false, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn accepts_owner_token_and_reports_health() {
        let (app, _, store) = app();
        let response = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/health", true, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("database"));
        assert!(!text.contains(&store.path().display().to_string()));
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value["build"], crate::web::build_sha());
    }

    #[tokio::test]
    async fn accepts_minted_device_tokens_and_records_use() {
        let (app, _, store) = app();
        let token = crate::token_cli::mint(&store, "api-test", None).unwrap();
        let response = app
            .clone()
            .oneshot(request_with_token("GET", "/v1/health", &token, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let listed = store.list_device_tokens().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].label, "api-test");
        assert!(listed[0].last_used_at.is_some());
    }

    #[tokio::test]
    async fn rejects_revoked_expired_and_unknown_device_tokens() {
        let (app, _, store) = app();
        let revoked = crate::token_cli::mint(&store, "revoked", None).unwrap();
        let record = store
            .device_token_by_hash(&sha256_hex(&revoked))
            .unwrap()
            .unwrap();
        assert!(store.revoke_device_token(&record.id).unwrap());
        let expired = crate::token_cli::mint(&store, "expired", Some(0)).unwrap();
        for token in [
            revoked,
            expired,
            "unknown-token-that-is-long-enough".to_string(),
        ] {
            let response = app
                .clone()
                .oneshot(request_with_token("GET", "/v1/health", &token, None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{token}");
        }
    }

    #[tokio::test]
    async fn serves_public_landing_page_without_authentication() {
        let (app, _, _) = app();
        for path in ["/", "/app"] {
            let response = app
                .clone()
                .oneshot(request(&app, "GET", path, false, None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response
                    .headers()
                    .get("content-security-policy")
                    .and_then(|value| value.to_str().ok()),
                Some(crate::web::CSP)
            );
            assert_eq!(
                response
                    .headers()
                    .get("content-type")
                    .and_then(|value| value.to_str().ok()),
                Some("text/html; charset=utf-8")
            );
            assert_eq!(
                response
                    .headers()
                    .get("x-content-type-options")
                    .and_then(|value| value.to_str().ok()),
                Some("nosniff")
            );
            let bytes = http_body_util::BodyExt::collect(response.into_body())
                .await
                .unwrap()
                .to_bytes();
            let body = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(!body.contains("<script"));
            assert!(body.contains("SeraBot"));
            assert!(body.contains("private single-operator deployment"));
        }
    }

    #[tokio::test]
    async fn public_downloads_redirect_or_report_unconfigured() {
        let (app, _, _) = app();
        for (path, asset) in [
            ("/app/latest.apk", crate::web::APK_ASSET),
            ("/app/SHA256SUMS", crate::web::SHA256SUMS_ASSET),
        ] {
            let response = app
                .clone()
                .oneshot(request(&app, "GET", path, false, None))
                .await
                .unwrap();
            match crate::web::releases_repo() {
                Some(repo) => {
                    assert_eq!(response.status(), StatusCode::FOUND);
                    let expected =
                        format!("https://github.com/{repo}/releases/latest/download/{asset}");
                    assert_eq!(
                        response
                            .headers()
                            .get("location")
                            .and_then(|value| value.to_str().ok()),
                        Some(expected.as_str())
                    );
                    assert_eq!(
                        response
                            .headers()
                            .get("cache-control")
                            .and_then(|value| value.to_str().ok()),
                        Some("no-store")
                    );
                }
                None => assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE),
            }
        }
    }

    #[tokio::test]
    async fn public_health_reports_build_without_database_details() {
        let (app, _, store) = app();
        let response = app
            .clone()
            .oneshot(request(&app, "GET", "/healthz", false, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("database"));
        assert!(!text.contains(&store.path().display().to_string()));
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value["build"], crate::web::build_sha());
        assert!(value.get("database").is_none());
    }

    #[tokio::test]
    async fn enqueues_owner_message() {
        let (app, _, store) = app();
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/messages",
                true,
                Some(json!({"conversation_id": "api-owner", "text": "hello"})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(store.pending_task_count().unwrap(), 1);
    }

    #[tokio::test]
    async fn rejects_wrong_token() {
        let (app, _, _) = app();
        let request = Request::builder()
            .method("GET")
            .uri("/v1/health")
            .header(
                "authorization",
                "Bearer wrong-token-that-is-also-long-enough",
            )
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_invalid_conversation_identifier() {
        let (app, _, _) = app();
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/messages",
                true,
                Some(json!({"conversation_id": "../etc", "text": "hello"})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn decides_unknown_approval_as_not_found() {
        let (app, _, _) = app();
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/approvals/missing/decide",
                true,
                Some(json!({"decision": "deny"})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn selection_is_persistent_and_applies_only_to_future_tasks() {
        let (app, _, store) = app();
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/messages",
                true,
                Some(json!({"conversation_id":"owner", "text":"first"})),
            ))
            .await
            .unwrap();
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let original = store
            .get_task(value["task"]["id"].as_str().unwrap())
            .unwrap();
        let selection = json!({"provider":"default", "model":"test-alternative"});
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/conversations/owner/model",
                true,
                Some(selection.clone()),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            store.get_task(&original.id).unwrap().payload,
            original.payload
        );
        assert!(store.kv_get("model_selection:owner").unwrap().is_some());
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/models?conversation_id=owner",
                true,
                None,
            ))
            .await
            .unwrap();
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["selection"], selection);
    }

    #[tokio::test]
    async fn selection_does_not_grant_egress_consent() {
        let (app, _, store) = app_with_policy(false, Policy::empty());
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/conversations/owner/model",
                true,
                Some(json!({"provider":"default", "model":"test-alternative"})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(store.kv_get("model_selection:owner").unwrap().is_none());
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/models?conversation_id=owner",
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn history_redacts_content_blocked_by_current_policy() {
        let (app, _, store) = app_with_policy(true, Policy::from_text("synthetic blocked phrase"));
        store
            .append_message(
                "owner",
                None,
                crate::domain::Role::User,
                "synthetic blocked phrase",
            )
            .unwrap();
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/conversations/owner/messages",
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value["messages"][0]["content"],
            "[Content withheld by policy]"
        );
    }

    #[tokio::test]
    async fn selection_rejects_client_supplied_endpoint() {
        let (app, _, store) = app();
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/conversations/owner/model",
                true,
                Some(
                    json!({"provider":"default", "model":"test", "base_url":"https://example.com"}),
                ),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(store.kv_get("model_selection:owner").unwrap().is_none());
    }

    #[tokio::test]
    async fn model_catalog_requires_authentication() {
        let (app, _, _) = app();
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/models?conversation_id=windows-owner",
                false,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn lists_configured_models_without_endpoint_or_credentials() {
        let (app, config, _) = app();
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/models?conversation_id=windows-owner",
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["selection"]["provider"], "default");
        assert_eq!(value["selection"]["model"], config.llm.model);
        assert_eq!(value["effective_selection"], value["selection"]);
        assert_eq!(value["providers"][0]["id"], "default");
        assert_eq!(value["entries"][0]["provider"], "default");
        assert_eq!(value["entries"][0]["model"], config.llm.model);
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(!text.contains("base_url"));
        assert!(!text.contains("api_key"));
        assert!(!text.contains(&config.llm.base_url));
    }

    #[tokio::test]
    async fn model_selection_rejects_unknown_provider_without_changing_state() {
        let (app, _, store) = app();
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/conversations/owner/model",
                true,
                Some(json!({"provider":"missing", "model":"invented"})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(store.kv_get("model_selection:owner").unwrap().is_none());
    }

    #[tokio::test]
    async fn history_returns_only_visible_messages_from_requested_conversation() {
        let (app, _, store) = app();
        store
            .append_message("owner", None, crate::domain::Role::User, "hello")
            .unwrap();
        store
            .append_message("owner", None, crate::domain::Role::Assistant, "bonjour")
            .unwrap();
        store
            .append_message(
                "owner",
                None,
                crate::domain::Role::System,
                "private system prompt",
            )
            .unwrap();
        store
            .append_message(
                "other",
                None,
                crate::domain::Role::User,
                "other conversation",
            )
            .unwrap();
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/conversations/owner/messages?limit=100",
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let messages = value["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["content"], "hello");
        assert_eq!(messages[1]["content"], "bonjour");
        assert!(messages.iter().all(|message| message.get("name").is_none()));
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(!text.contains("private system prompt"));
        assert!(!text.contains("other conversation"));
    }

    #[tokio::test]
    async fn room_history_returns_handoff_speaker_and_drops_unsafe_names() {
        let (app, _, store) = app();
        store
            .append_message_named(
                "room:review",
                None,
                crate::domain::Role::Assistant,
                "Synthetic analyst summary.",
                Some("analyst"),
            )
            .unwrap();
        store
            .append_message_named(
                "room:review",
                None,
                crate::domain::Role::Assistant,
                "orchestrator note",
                Some("bot999:secretvalue"),
            )
            .unwrap();
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/conversations/room:review/messages",
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value = read_json(response).await;
        let messages = value["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["name"], "analyst");
        assert!(messages[1].get("name").is_none());
        let text = value.to_string();
        assert!(!text.contains("secretvalue"));
        assert!(!text.contains("bot999"));
    }

    #[tokio::test]
    async fn lists_configured_bots_without_prompts_or_secrets() {
        let root = temp_base();
        let (app, _, _) = app_with_root(true, Policy::empty(), root.clone());
        let denied = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/bots", false, None))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let empty = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/bots", true, None))
            .await
            .unwrap();
        assert_eq!(empty.status(), StatusCode::OK);
        assert_eq!(read_json(empty).await["bots"].as_array().unwrap().len(), 0);

        let secret = "super-secret-prompt-body-should-not-leak";
        std::fs::create_dir_all(root.join("prompts")).unwrap();
        std::fs::write(root.join("prompts").join("notes.txt"), secret).unwrap();
        std::fs::create_dir_all(root.join("bots")).unwrap();
        std::fs::write(
            root.join("bots").join("notes.toml"),
            "name = \"notes\"\nrole = \"specialist\"\ntrigger = \"command\"\nsystem_prompt = \"prompts/notes.txt\"\nskills = [\"ask\"]\ntools = [\"skill_list\"]\nenabled = false\n",
        )
        .unwrap();

        let listed = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/bots", true, None))
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        let listed = read_json(listed).await;
        let bots = listed["bots"].as_array().unwrap();
        assert_eq!(bots.len(), 1);
        assert_eq!(bots[0]["id"], "notes");
        assert_eq!(bots[0]["role"], "specialist");
        assert_eq!(bots[0]["enabled"], false);
        assert_eq!(bots[0]["live"], false);
        assert_eq!(bots[0]["tools"][0], "skill_list");
        assert_eq!(bots[0]["skills"][0], "ask");
        let text = listed.to_string();
        assert!(!text.contains(secret));
        assert!(!text.contains("system_prompt"));
        assert!(!text.contains("prompts/notes.txt"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn new_conversation_routes_validate_identity_and_require_authentication() {
        let (app, _, _) = app();
        for uri in [
            "/v1/models?conversation_id=bad%20id",
            "/v1/conversations/bad%20id/messages",
        ] {
            let response = app
                .clone()
                .oneshot(request(&app, "GET", uri, true, None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        }
        for (method, uri, body) in [
            ("GET", "/v1/conversations/owner/messages", None),
            (
                "PUT",
                "/v1/conversations/owner/model",
                Some(json!({"provider":"default", "model":"test"})),
            ),
        ] {
            let response = app
                .clone()
                .oneshot(request(&app, method, uri, false, body))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn connector_catalog_lists_elevenlabs_in_voice_group() {
        let (app, _, _) = app();
        let response = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/connectors", true, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        let entry = body["connectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|connector| connector["id"] == "elevenlabs")
            .expect("elevenlabs listed");
        assert_eq!(entry["kind"], "api_key");
        assert_eq!(entry["enabled"], false);
        assert_eq!(entry["group"], "Voice & Media");
        assert_eq!(entry["secrets"]["api_key"]["name"], "elevenlabs_api_key");
        assert_eq!(entry["secrets"]["api_key"]["configured"], false);
    }

    #[tokio::test]
    async fn connector_test_is_owner_only_and_reports_missing_secrets_without_network() {
        let (app, _, _) = app();
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/admin/connectors/elevenlabs/test",
                false,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/admin/connectors/no-such-connector/test",
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        for id in ["elevenlabs", "tavily", "firecrawl", "agentmail", "gmail"] {
            let response = app
                .clone()
                .oneshot(request(
                    &app,
                    "POST",
                    &format!("/v1/admin/connectors/{id}/test"),
                    true,
                    None,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{id}");
            let body = read_json(response).await;
            assert_eq!(body["id"], id);
            assert_eq!(body["ok"], false, "{id} has no stored secrets");
            assert!(
                body["message"]
                    .as_str()
                    .unwrap()
                    .starts_with("Missing secret: "),
                "{id}: {body}"
            );
            assert!(body["checked_at"].as_str().is_some());
        }
    }

    async fn read_json(response: Response) -> Value {
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn read_body(response: Response) -> String {
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn temp_base() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("serabot-api-fleet-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("skills").join("ask")).unwrap();
        std::fs::write(
            root.join("skills").join("ask").join("SKILL.md"),
            "---\nname: \"Ask\"\ndescription: \"Synthetic ask skill\"\n---\n\n# Ask\n",
        )
        .unwrap();
        root.canonicalize().unwrap()
    }

    #[tokio::test]
    async fn scaffolds_bot_from_template_without_owner_paths() {
        let root = temp_base();
        let (app, _, store) = app_with_root(true, Policy::empty(), root.clone());
        let denied = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/bots",
                false,
                Some(json!({"id": "notes"})),
            ))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/bots",
                true,
                Some(json!({
                    "id": "notes",
                    "description": "Synthetic notes specialist.",
                    "tools": ["rag_search", "skill_list"],
                    "skills": ["ask"]
                })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value = read_json(response).await;
        assert_eq!(value["bot"]["id"], "notes");
        assert_eq!(value["bot"]["enabled"], false);
        assert_eq!(value["bot"]["role"], "specialist");
        assert_eq!(value["bot"]["live"], false);
        assert_eq!(value["bot"]["bot_file"], "bots/notes.toml");
        assert_eq!(value["bot"]["prompt_file"], "prompts/notes.txt");
        assert!(!value.to_string().contains("delegate"));
        let toml = std::fs::read_to_string(root.join("bots").join("notes.toml")).unwrap();
        assert!(toml.contains("enabled = false"));
        assert!(toml.contains("role = \"specialist\""));
        assert!(!toml.contains("delegates = [\""));
        assert!(root.join("prompts").join("notes.txt").is_file());
        assert!(
            store
                .list_audit(10)
                .unwrap()
                .iter()
                .any(|entry| entry.event == "bot.scaffolded")
        );

        let again = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/bots",
                true,
                Some(json!({"id": "notes"})),
            ))
            .await
            .unwrap();
        assert_eq!(again.status(), StatusCode::CONFLICT);

        let escaped = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/bots",
                true,
                Some(json!({"id": "../outside", "path": "C:/secret"})),
            ))
            .await
            .unwrap();
        assert_eq!(escaped.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!root.join("outside.toml").exists());

        let delegate = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/bots",
                true,
                Some(json!({"id": "other", "tools": ["delegate"]})),
            ))
            .await
            .unwrap();
        assert_eq!(delegate.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!root.join("bots").join("other.toml").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn bot_scaffold_rejects_policy_violations() {
        let root = temp_base();
        let (app, _, _) = app_with_root(
            true,
            Policy::from_text("synthetic blocked phrase"),
            root.clone(),
        );
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/bots",
                true,
                Some(json!({
                    "id": "notes",
                    "description": "mentions synthetic blocked phrase"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!root.join("bots").join("notes.toml").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn registers_skill_pack_without_returning_or_logging_the_body() {
        let root = temp_base();
        let (app, _, store) = app_with_root(true, Policy::empty(), root.clone());
        let denied = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/skills",
                false,
                Some(json!({"id": "notes-pack"})),
            ))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let marker = "Unique workflow sentence alpha-bridge-7.";
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/skills",
                true,
                Some(json!({
                    "id": "notes-pack",
                    "description": "Public summary",
                    "body": marker
                })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value = read_json(response).await;
        assert_eq!(value["skill"]["id"], "notes-pack");
        assert_eq!(value["skill"]["file"], "skills/notes-pack/SKILL.md");
        assert_eq!(value["skill"]["live"], false);
        assert!(!value.to_string().contains(marker));
        let raw = std::fs::read_to_string(root.join("skills").join("notes-pack").join("SKILL.md"))
            .unwrap();
        assert!(raw.contains(marker));
        assert!(
            store
                .list_audit(10)
                .unwrap()
                .iter()
                .any(|entry| entry.event == "skill.created")
        );
        let audit = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/audit?limit=20", true, None))
            .await
            .unwrap();
        let audit = read_json(audit).await;
        assert!(!audit.to_string().contains(marker));

        let listed = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/skills", true, None))
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        let listed = read_json(listed).await;
        let skills = listed["skills"].as_array().unwrap();
        assert!(skills.iter().any(|skill| skill["id"] == "notes-pack"));
        assert!(skills.iter().any(|skill| skill["id"] == "ask"));
        assert!(!listed.to_string().contains(marker));

        let again = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/skills",
                true,
                Some(json!({"id": "notes-pack"})),
            ))
            .await
            .unwrap();
        assert_eq!(again.status(), StatusCode::CONFLICT);

        let escaped = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/skills",
                true,
                Some(json!({"id": "../outside", "path": "C:/secret"})),
            ))
            .await
            .unwrap();
        assert_eq!(escaped.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!root.join("outside").exists());
        assert!(!root.join("skills").join("outside").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn skill_registration_rejects_policy_violations() {
        let root = temp_base();
        let (app, _, _) = app_with_root(
            true,
            Policy::from_text("synthetic blocked phrase"),
            root.clone(),
        );
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/skills",
                true,
                Some(json!({
                    "id": "notes-pack",
                    "description": "Public summary",
                    "body": "mentions synthetic blocked phrase"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!root.join("skills").join("notes-pack").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn creates_conversation_tree_and_keeps_containers_turnless() {
        let (app, _, store) = app();
        let denied = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/conversations",
                false,
                Some(json!({"kind": "section", "slug": "finance"})),
            ))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let section = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/conversations",
                true,
                Some(json!({"kind": "section", "slug": "finance"})),
            ))
            .await
            .unwrap();
        assert_eq!(section.status(), StatusCode::OK);
        let section = read_json(section).await;
        assert_eq!(section["conversation"]["id"], "sec:finance");
        assert_eq!(section["conversation"]["accepts_turns"], false);
        assert!(section["conversation"]["parent_id"].is_null());

        let group = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/conversations",
                true,
                Some(json!({
                    "kind": "group",
                    "slug": "ops",
                    "parent_id": "sec:finance"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(group.status(), StatusCode::OK);
        let group = read_json(group).await;
        assert_eq!(group["conversation"]["id"], "grp:ops");
        assert_eq!(group["conversation"]["parent_id"], "sec:finance");
        assert_eq!(group["conversation"]["accepts_turns"], false);

        let direct = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/conversations",
                true,
                Some(json!({
                    "kind": "direct",
                    "slug": "analyst",
                    "parent_id": "grp:ops"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(direct.status(), StatusCode::OK);
        let direct = read_json(direct).await;
        assert_eq!(direct["conversation"]["id"], "dir:analyst");
        assert_eq!(direct["conversation"]["accepts_turns"], true);

        let room = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/conversations",
                true,
                Some(json!({
                    "kind": "room",
                    "slug": "review",
                    "parent_id": "sec:finance"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(room.status(), StatusCode::OK);
        assert_eq!(read_json(room).await["conversation"]["id"], "room:review");

        let orphan_group = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/conversations",
                true,
                Some(json!({"kind": "group", "slug": "loose"})),
            ))
            .await
            .unwrap();
        assert_eq!(orphan_group.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let bad_parent = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/conversations",
                true,
                Some(json!({
                    "kind": "direct",
                    "slug": "bad",
                    "parent_id": "dir:analyst"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(bad_parent.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let duplicate = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/conversations",
                true,
                Some(json!({"kind": "section", "slug": "finance"})),
            ))
            .await
            .unwrap();
        assert_eq!(duplicate.status(), StatusCode::CONFLICT);

        let section_turn = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/messages",
                true,
                Some(json!({"conversation_id": "sec:finance", "text": "hello"})),
            ))
            .await
            .unwrap();
        assert_eq!(section_turn.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let group_turn = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/messages",
                true,
                Some(json!({"conversation_id": "grp:ops", "text": "hello"})),
            ))
            .await
            .unwrap();
        assert_eq!(group_turn.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let direct_turn = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/messages",
                true,
                Some(json!({"conversation_id": "dir:analyst", "text": "hello"})),
            ))
            .await
            .unwrap();
        assert_eq!(direct_turn.status(), StatusCode::OK);
        assert_eq!(store.pending_task_count().unwrap(), 1);
        assert_eq!(
            store
                .get_conversation("dir:analyst")
                .unwrap()
                .unwrap()
                .parent_id
                .as_deref(),
            Some("grp:ops")
        );
    }

    #[tokio::test]
    async fn lists_renames_archives_and_paginates_conversations() {
        let (app, _, _) = app();
        for (kind, slug, parent) in [
            ("section", "alpha", None),
            ("section", "beta", None),
            ("direct", "analyst", Some("sec:beta")),
        ] {
            let mut body = json!({"kind": kind, "slug": slug});
            if let Some(parent) = parent {
                body["parent_id"] = json!(parent);
            }
            let response = app
                .clone()
                .oneshot(request(&app, "POST", "/v1/conversations", true, Some(body)))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{kind} {slug}");
        }

        let first = read_json(
            app.clone()
                .oneshot(request(
                    &app,
                    "GET",
                    "/v1/conversations?limit=2",
                    true,
                    None,
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(first["conversations"].as_array().unwrap().len(), 2);
        let cursor = first["next_cursor"]
            .as_str()
            .expect("next cursor")
            .to_owned();
        let encoded = url::form_urlencoded::byte_serialize(cursor.as_bytes()).collect::<String>();
        let second = read_json(
            app.clone()
                .oneshot(request(
                    &app,
                    "GET",
                    &format!("/v1/conversations?limit=2&cursor={encoded}"),
                    true,
                    None,
                ))
                .await
                .unwrap(),
        )
        .await;
        let mut ids = first["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .chain(second["conversations"].as_array().unwrap().iter())
            .map(|conversation| conversation["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(ids, ["dir:analyst", "sec:alpha", "sec:beta"]);

        let renamed = read_json(
            app.clone()
                .oneshot(request(
                    &app,
                    "PATCH",
                    "/v1/conversations/sec:beta",
                    true,
                    Some(json!({"title": "  Finance   reviews  "})),
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(renamed["conversation"]["title"], "Finance reviews");

        let empty_title = app
            .clone()
            .oneshot(request(
                &app,
                "PATCH",
                "/v1/conversations/sec:beta",
                true,
                Some(json!({"title": "   "})),
            ))
            .await
            .unwrap();
        assert_eq!(empty_title.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let missing = app
            .clone()
            .oneshot(request(
                &app,
                "PATCH",
                "/v1/conversations/sec:missing",
                true,
                Some(json!({"title": "Nope"})),
            ))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let archived = read_json(
            app.clone()
                .oneshot(request(
                    &app,
                    "DELETE",
                    "/v1/conversations/dir:analyst",
                    true,
                    None,
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(archived["archived"], true);

        let visible = read_json(
            app.clone()
                .oneshot(request(&app, "GET", "/v1/conversations", true, None))
                .await
                .unwrap(),
        )
        .await;
        assert!(!visible.to_string().contains("dir:analyst"));

        let with_archived = read_json(
            app.clone()
                .oneshot(request(
                    &app,
                    "GET",
                    "/v1/conversations?include_archived=true",
                    true,
                    None,
                ))
                .await
                .unwrap(),
        )
        .await;
        assert!(with_archived.to_string().contains("dir:analyst"));

        let restored = read_json(
            app.clone()
                .oneshot(request(
                    &app,
                    "POST",
                    "/v1/conversations/dir:analyst/unarchive",
                    true,
                    None,
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(restored["archived"], false);

        let visible = read_json(
            app.clone()
                .oneshot(request(&app, "GET", "/v1/conversations", true, None))
                .await
                .unwrap(),
        )
        .await;
        assert!(visible.to_string().contains("dir:analyst"));

        let missing_restore = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/conversations/dir:missing/unarchive",
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(missing_restore.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn library_lists_no_rows_for_a_fresh_conversation_and_404s_unknown_ones() {
        let (app, _, _) = app();
        let created = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/conversations",
                true,
                Some(json!({"kind": "room", "slug": "review"})),
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);

        let empty = read_json(
            app.clone()
                .oneshot(request(
                    &app,
                    "GET",
                    "/v1/conversations/room:review/library",
                    true,
                    None,
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(empty["artifacts"], json!([]));

        let missing = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/conversations/room:ghost/library",
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let unauthenticated = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/conversations/room:review/library",
                false,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn memory_crud_over_http() {
        let (app, _, _) = app();
        let denied = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/memories", false, None))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let created = read_json(
            app.clone()
                .oneshot(request(
                    &app,
                    "POST",
                    "/v1/memories",
                    true,
                    Some(json!({
                        "kind": "preference",
                        "content": "Prefers concise summaries"
                    })),
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(created["memory"]["kind"], "preference");
        assert_eq!(created["memory"]["source"], "operator:desktop");
        let id = created["memory"]["id"].as_str().unwrap().to_owned();

        let matched = read_json(
            app.clone()
                .oneshot(request(
                    &app,
                    "GET",
                    "/v1/memories?query=concise",
                    true,
                    None,
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(matched["memories"].as_array().unwrap().len(), 1);

        let unmatched = read_json(
            app.clone()
                .oneshot(request(
                    &app,
                    "GET",
                    "/v1/memories?query=absent-term",
                    true,
                    None,
                ))
                .await
                .unwrap(),
        )
        .await;
        assert!(unmatched["memories"].as_array().unwrap().is_empty());

        let bad_kind = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/memories",
                true,
                Some(json!({"kind": "guess", "content": "x"})),
            ))
            .await
            .unwrap();
        assert_eq!(bad_kind.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let deleted = app
            .clone()
            .oneshot(request(
                &app,
                "DELETE",
                &format!("/v1/memories/{id}"),
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::OK);
        let missing = app
            .clone()
            .oneshot(request(
                &app,
                "DELETE",
                &format!("/v1/memories/{id}"),
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn memory_writes_respect_the_content_policy() {
        let (app, _, _) = app_with_policy(true, Policy::from_text("synthetic blocked phrase"));
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/memories",
                true,
                Some(json!({
                    "kind": "fact",
                    "content": "mentions synthetic blocked phrase"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let listed = read_json(
            app.clone()
                .oneshot(request(&app, "GET", "/v1/memories", true, None))
                .await
                .unwrap(),
        )
        .await;
        assert!(listed["memories"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn admin_provider_writes_are_owner_gated_and_persist_an_overlay() {
        let (app, config, _) = app();
        let payload = json!({
            "id": "aux",
            "kind": "openai-compatible",
            "base_url": "https://api.example.com/v1",
            "models": ["aux-model"],
            "api_key_secret": "llm_api_key"
        });
        let unauthorized = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/admin/providers",
                false,
                Some(payload.clone()),
            ))
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let missing = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/admin/providers",
                true,
                Some(json!({
                    "id": "aux",
                    "kind": "openai-compatible",
                    "base_url": "https://api.example.com/v1",
                    "models": ["aux-model"],
                    "api_key_secret": "NOT_CONFIGURED"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = read_json(missing).await;
        assert!(body["error"].as_str().unwrap().contains("NOT_CONFIGURED"));

        let saved = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/admin/providers",
                true,
                Some(payload),
            ))
            .await
            .unwrap();
        let status = saved.status();
        let saved_body = read_json(saved).await;
        assert_eq!(status, StatusCode::CREATED, "{saved_body}");
        let overlay =
            crate::admin::AdminOverlay::load(&crate::admin::AdminOverlay::path_for(&config))
                .unwrap();
        assert!(overlay.providers.contains_key("aux"));
        assert!(overlay.providers["aux"].data_egress_approved);

        let removed = app
            .clone()
            .oneshot(request(
                &app,
                "DELETE",
                "/v1/admin/providers/aux",
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(removed.status(), StatusCode::OK);

        let unconfirmed = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/admin/restart",
                true,
                Some(json!({"confirm": false})),
            ))
            .await
            .unwrap();
        assert_eq!(unconfirmed.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn provider_oauth_endpoints_are_owner_gated_and_validate_before_any_network_call() {
        let (app, _config, _) = app();
        let unauthorized = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/admin/providers/aux/oauth/start",
                false,
                Some(json!({})),
            ))
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let unknown = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/admin/providers/nope/oauth/start",
                true,
                Some(json!({})),
            ))
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

        let saved = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/admin/providers",
                true,
                Some(json!({
                    "id": "aux",
                    "kind": "openai-compatible",
                    "base_url": "https://api.example.com/v1",
                    "models": ["aux-model"],
                    "api_key_secret": "llm_api_key"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(saved.status(), StatusCode::CREATED);
        for path in ["start", "complete"] {
            let wrong_kind = app
                .clone()
                .oneshot(request(
                    &app,
                    "POST",
                    &format!("/v1/admin/providers/aux/oauth/{path}"),
                    true,
                    Some(json!({
                        "code": "synthetic-auth-code",
                        "verifier": "synthetic-verifier-synthetic-verifier-0123456789",
                        "redirect_uri": "http://127.0.0.1:51121/oauth-callback"
                    })),
                ))
                .await
                .unwrap();
            assert_eq!(wrong_kind.status(), StatusCode::UNPROCESSABLE_ENTITY);
            let body = read_json(wrong_kind).await;
            assert!(body["error"].as_str().unwrap().contains("does not use"));
        }
    }

    #[tokio::test]
    async fn bot_profiles_and_titled_conversations_are_owner_editable() {
        let (app, _, _) = app();
        let saved = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/admin/bots/main",
                true,
                Some(json!({
                    "display_name": "Chief of Staff",
                    "description": "Synthetic orchestrator profile.",
                    "avatar": "CS"
                })),
            ))
            .await
            .unwrap();
        let status = saved.status();
        let saved_body = read_json(saved).await;
        assert_eq!(status, StatusCode::OK, "{saved_body}");
        let bots = read_json(
            app.clone()
                .oneshot(request(&app, "GET", "/v1/bots", true, None))
                .await
                .unwrap(),
        )
        .await;
        let main = bots["bots"]
            .as_array()
            .unwrap_or_else(|| panic!("bots payload: {bots}"))
            .iter()
            .find(|bot| bot["id"] == "main")
            .unwrap_or_else(|| panic!("bots payload: {bots}"));
        assert_eq!(main["display_name"], "Chief of Staff");
        assert_eq!(main["avatar"], "CS");
        assert_eq!(main["description"], "Synthetic orchestrator profile.");

        let missing = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/admin/bots/nobody",
                true,
                Some(json!({"display_name": "Nobody"})),
            ))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let created = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/conversations",
                true,
                Some(json!({
                    "kind": "section",
                    "slug": "finance",
                    "title": "Finance"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
        let body = read_json(created).await;
        assert_eq!(body["conversation"]["title"], "Finance");
        assert_eq!(body["conversation"]["id"], "sec:finance");
    }

    fn write_bot(root: &std::path::Path, id: &str, prompt: &str, extra: &str) {
        std::fs::create_dir_all(root.join("prompts")).unwrap();
        std::fs::write(root.join("prompts").join(format!("{id}.txt")), prompt).unwrap();
        std::fs::create_dir_all(root.join("bots")).unwrap();
        std::fs::write(
            root.join("bots").join(format!("{id}.toml")),
            format!(
                "name = \"{id}\"\ntrigger = \"command\"\nsystem_prompt = \"prompts/{id}.txt\"\n{extra}"
            ),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn usage_endpoint_rolls_up_model_used_rows_clamps_days_and_filters() {
        let (app, _, store) = app();
        let denied = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/usage", false, None))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let empty = read_json(
            app.clone()
                .oneshot(request(&app, "GET", "/v1/usage", true, None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(empty["totals"]["calls"], 0);
        assert_eq!(empty["totals"]["prompt_tokens"], 0);
        assert!(empty["per_day"].as_array().unwrap().is_empty());
        assert!(empty["per_bot"].as_array().unwrap().is_empty());

        let task = store
            .enqueue_task(
                "turn",
                "main",
                &json!({"conversation_id": "owner"}),
                0,
                Utc::now(),
            )
            .unwrap();
        store
            .record_audit(
                "system",
                "model.used",
                None,
                Some(&task.id),
                None,
                None,
                Some("policy-synthetic"),
                Some(&json!({
                    "provider": "default",
                    "model": "test",
                    "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
                    "history_messages": 2,
                    "prompt_chars_est": 40,
                })),
            )
            .unwrap();
        store
            .record_audit(
                "system",
                "model.used",
                None,
                Some(&task.id),
                None,
                None,
                Some("policy-synthetic"),
                Some(&json!({
                    "usage": {"prompt_tokens": 100, "completion_tokens": 100, "total_tokens": 200}
                })),
            )
            .unwrap();
        let old =
            (Utc::now() - chrono::Duration::days(100)).to_rfc3339_opts(SecondsFormat::Millis, true);
        store
            .connection()
            .unwrap()
            .execute(
                &format!(
                    "UPDATE audit_log SET at = '{old}' WHERE json_extract(details_json, '$.usage.total_tokens') = 200"
                ),
                [],
            )
            .unwrap();

        // days=500 clamps to 90, so the 100-day-old row is excluded.
        let body = read_json(
            app.clone()
                .oneshot(request(&app, "GET", "/v1/usage?days=500", true, None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["totals"]["calls"], 1);
        assert_eq!(body["totals"]["prompt_tokens"], 10);
        assert_eq!(body["totals"]["completion_tokens"], 5);
        assert_eq!(body["totals"]["total_tokens"], 15);
        assert_eq!(body["per_day"].as_array().unwrap().len(), 1);
        assert_eq!(
            body["per_day"][0]["day"],
            Utc::now().format("%Y-%m-%d").to_string()
        );
        assert_eq!(body["per_bot"].as_array().unwrap().len(), 1);
        assert_eq!(body["per_bot"][0]["bot"], "main");
        assert_eq!(body["per_bot"][0]["calls"], 1);

        // days=0 clamps to 1; today's row is still inside the window.
        let body = read_json(
            app.clone()
                .oneshot(request(&app, "GET", "/v1/usage?days=0", true, None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["totals"]["calls"], 1);

        // Bot and conversation filters.
        let body = read_json(
            app.clone()
                .oneshot(request(&app, "GET", "/v1/usage?bot=analyst", true, None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["totals"]["calls"], 0);
        let body = read_json(
            app.clone()
                .oneshot(request(
                    &app,
                    "GET",
                    "/v1/usage?conversation_id=owner",
                    true,
                    None,
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["totals"]["calls"], 1);
        let body = read_json(
            app.clone()
                .oneshot(request(
                    &app,
                    "GET",
                    "/v1/usage?conversation_id=dir%3Aother",
                    true,
                    None,
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["totals"]["calls"], 0);

        // An unsafe bot filter value is rejected.
        let invalid = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/usage?bot=bad%20id", true, None))
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn bot_detail_returns_owner_fields_and_caps_instructions() {
        let root = temp_base();
        write_bot(
            &root,
            "notes",
            "Synthetic notes prompt.",
            "role = \"specialist\"\nskills = [\"ask\"]\ntools = [\"skill_list\"]\nenabled = true\n",
        );
        write_bot(&root, "big", &"x".repeat(40_000), "enabled = true\n");
        let (app, _, store) = app_with_root(true, Policy::empty(), root.clone());

        let denied = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/bots/notes", false, None))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let body = read_json(
            app.clone()
                .oneshot(request(&app, "GET", "/v1/bots/notes", true, None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["id"], "notes");
        assert_eq!(body["role"], "specialist");
        assert_eq!(body["enabled"], true);
        assert_eq!(body["provider"], "default");
        assert_eq!(body["model"], "test");
        assert_eq!(body["fallback_provider"], Value::Null);
        assert_eq!(body["fallback_model"], Value::Null);
        assert_eq!(body["routing_classes"].as_array().unwrap().len(), 0);
        assert_eq!(body["instructions"], "Synthetic notes prompt.");
        assert_eq!(body["instructions_bytes"], "Synthetic notes prompt.".len());
        assert_eq!(body["skills"][0], "ask");
        assert_eq!(body["tools"][0], "skill_list");
        assert_eq!(body["memory_count"], 0);
        assert_eq!(body["artifact_count"], 0);
        // No prompt path or secret material leaks.
        let text = body.to_string();
        assert!(!text.contains("prompts/notes.txt"));
        assert!(!text.contains("synthetic-test-key"));

        crate::memory::add(&store, "fact", "synthetic fact", "test").unwrap();
        let body = read_json(
            app.clone()
                .oneshot(request(&app, "GET", "/v1/bots/notes", true, None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["memory_count"], 1);

        // Over the 32 KB cap the instructions content is withheld, bytes kept.
        let big = read_json(
            app.clone()
                .oneshot(request(&app, "GET", "/v1/bots/big", true, None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(big["instructions"], Value::Null);
        assert_eq!(big["instructions_bytes"], 40_000);

        let missing = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/bots/nobody", true, None))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn admin_bot_config_update_validates_persists_and_requires_restart() {
        let root = temp_base();
        write_bot(
            &root,
            "notes",
            "Synthetic notes prompt.",
            "role = \"specialist\"\nskills = []\ntools = [\"skill_list\"]\nenabled = true\n",
        );
        let (app, config, store) = app_with_root(true, Policy::empty(), root.clone());

        let denied = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/admin/bots/notes/config",
                false,
                Some(json!({"tools": []})),
            ))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let missing = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/admin/bots/nobody/config",
                true,
                Some(json!({"tools": []})),
            ))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let cases: Vec<(Value, StatusCode)> = vec![
            (json!({}), StatusCode::UNPROCESSABLE_ENTITY),
            (
                json!({"tools": ["not_a_tool"]}),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                json!({"tools": ["delegate"]}),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                json!({"tools": (0..33).map(|i| format!("t{i}")).collect::<Vec<_>>()}),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                json!({"skills": ["nope"]}),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                json!({"skills": (0..17).map(|i| format!("s{i}")).collect::<Vec<_>>()}),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                json!({"tools": [], "require_approval": ["skill_list"]}),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ];
        for (payload, expected) in cases {
            let response = app
                .clone()
                .oneshot(request(
                    &app,
                    "PUT",
                    "/v1/admin/bots/notes/config",
                    true,
                    Some(payload),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }

        // Valid partial update: empty tools, one known skill, empty approvals.
        let saved = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/admin/bots/notes/config",
                true,
                Some(json!({"tools": [], "skills": ["ask"], "require_approval": []})),
            ))
            .await
            .unwrap();
        let status = saved.status();
        let saved_body = read_json(saved).await;
        assert_eq!(status, StatusCode::OK, "{saved_body}");
        assert_eq!(saved_body["saved"], true);
        assert_eq!(saved_body["restart_required"], true);

        let overlay =
            crate::admin::AdminOverlay::load(&crate::admin::AdminOverlay::path_for(&config))
                .unwrap();
        let entry = overlay.bot_configs.get("notes").expect("overlay entry");
        assert_eq!(entry.tools, Some(vec![]));
        assert_eq!(entry.skills, Some(vec!["ask".to_string()]));
        assert_eq!(entry.require_approval, Some(vec![]));
        assert!(
            store
                .list_audit(100)
                .unwrap()
                .iter()
                .any(|entry| entry.event == "config.bot_config_saved")
        );

        // The detail endpoint reflects the saved override before the restart.
        let body = read_json(
            app.clone()
                .oneshot(request(&app, "GET", "/v1/bots/notes", true, None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["tools"].as_array().unwrap().len(), 0);
        assert_eq!(body["skills"][0], "ask");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn admin_bot_config_allows_delegate_for_the_orchestrator_only() {
        let (app, config, _) = app();
        let saved = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/admin/bots/main/config",
                true,
                Some(json!({"tools": ["delegate"]})),
            ))
            .await
            .unwrap();
        let status = saved.status();
        let body = read_json(saved).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["restart_required"], true);
        let overlay =
            crate::admin::AdminOverlay::load(&crate::admin::AdminOverlay::path_for(&config))
                .unwrap();
        assert_eq!(
            overlay
                .bot_configs
                .get("main")
                .and_then(|e| e.tools.clone()),
            Some(vec!["delegate".to_string()])
        );
    }

    #[tokio::test]
    async fn providers_and_connectors_report_status_without_secret_values() {
        let (app, _, _) = app();
        let providers = read_json(
            app.clone()
                .oneshot(request(&app, "GET", "/v1/providers", true, None))
                .await
                .unwrap(),
        )
        .await;
        let list = providers["providers"].as_array().unwrap();
        let default = list
            .iter()
            .find(|provider| provider["id"] == "default")
            .expect("default provider");
        assert_eq!(default["secrets"]["api_key"]["name"], "llm_api_key");
        assert_eq!(default["secrets"]["api_key"]["configured"], true);
        assert!(
            default["models"]
                .as_array()
                .unwrap()
                .iter()
                .any(|model| model == "test")
        );
        assert!(!providers.to_string().contains("synthetic-test-key"));

        let connectors = read_json(
            app.clone()
                .oneshot(request(&app, "GET", "/v1/connectors", true, None))
                .await
                .unwrap(),
        )
        .await;
        let list = connectors["connectors"].as_array().unwrap();
        assert_eq!(list.len(), 11);
        let gmail = list
            .iter()
            .find(|connector| connector["id"] == "gmail")
            .expect("gmail connector");
        assert_eq!(gmail["enabled"], false);
        assert_eq!(gmail["redirect_uri"], Value::Null);
        assert_eq!(
            gmail["secrets"]["client_id"]["name"],
            "google_oauth_client_id"
        );
        assert_eq!(gmail["secrets"]["client_id"]["configured"], false);
        let tavily = list
            .iter()
            .find(|connector| connector["id"] == "tavily")
            .expect("tavily connector");
        assert_eq!(tavily["kind"], "api_key");
        assert_eq!(tavily["secrets"]["api_key"]["name"], "tavily_api_key");
        assert!(list.iter().any(|connector| connector["id"] == "firecrawl"));
        assert!(!connectors.to_string().contains("synthetic-test-key"));
    }

    #[tokio::test]
    async fn legacy_conversations_list_as_turn_capable_threads() {
        let (app, _, store) = app();
        store
            .ensure_conversation_accepts_turns("windows-owner")
            .unwrap();
        let response = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/conversations", true, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        let legacy = body["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|conversation| conversation["id"] == "windows-owner")
            .expect("legacy conversation");
        assert_eq!(legacy["kind"], "legacy");
        assert_eq!(legacy["accepts_turns"], true);
    }

    #[test]
    fn event_resume_prefers_query_and_accepts_last_event_id() {
        let mut headers = HeaderMap::new();
        headers.insert("last-event-id", "7".parse().unwrap());
        assert_eq!(resume_cursor(&headers, None), 7);
        assert_eq!(resume_cursor(&headers, Some(12)), 12);
        assert_eq!(resume_cursor(&HeaderMap::new(), None), 0);
        assert_eq!(resume_cursor(&HeaderMap::new(), Some(-3)), 0);
        headers.insert("last-event-id", "not-a-number".parse().unwrap());
        assert_eq!(resume_cursor(&headers, None), 0);
    }

    /// A running fleet that matches the on-disk fleet directory used by
    /// `list_configured_bots` (analyst, maker, watcher enabled; legal disabled).
    fn app_with_team() -> (Router, Arc<Config>, Store) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::env::current_dir().unwrap());
        app_with_fleet(
            true,
            Policy::empty(),
            root,
            vec![
                test_bot("main", crate::orchestration::BotRole::Orchestrator),
                test_bot("analyst", crate::orchestration::BotRole::Specialist),
                test_bot("maker", crate::orchestration::BotRole::Specialist),
                test_bot("watcher", crate::orchestration::BotRole::Specialist),
            ],
        )
    }

    #[tokio::test]
    async fn team_endpoint_round_trip_and_member_validation() {
        let (app, _, store) = app_with_team();
        let created = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/conversations",
                true,
                Some(json!({"kind": "room", "slug": "review"})),
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);

        // No team yet: an empty shape, never a 404.
        let empty = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/conversations/room:review/team",
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(empty.status(), StatusCode::OK);
        let body = read_json(empty).await;
        assert_eq!(body["conversation_id"], "room:review");
        assert_eq!(body["mode"], "selected");
        assert_eq!(body["members"], json!([]));
        assert_eq!(body["max_members"], 5);

        let response = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/conversations/room:review/team",
                true,
                Some(json!({
                    "members": ["analyst", "maker", "watcher"],
                    "mode": "selected"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        assert_eq!(
            body["members"][0],
            json!({"bot": "analyst", "role": "specialist", "enabled": true, "live": true})
        );
        assert_eq!(body["members"][1]["bot"], "maker");
        assert_eq!(body["members"][2]["bot"], "watcher");
        assert_eq!(body["mode"], "selected");

        let stored = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/conversations/room:review/team",
                true,
                None,
            ))
            .await
            .unwrap();
        let body = read_json(stored).await;
        assert_eq!(body["members"].as_array().unwrap().len(), 3);
        assert_eq!(body["members"][1]["live"], true);

        let audit = store.list_audit(50).unwrap();
        let entry = audit
            .iter()
            .find(|entry| entry.event == "team.member_set")
            .expect("team.member_set audit");
        let details = entry.details.as_ref().unwrap();
        assert_eq!(details["members"], json!(["analyst", "maker", "watcher"]));
        assert_eq!(details["mode"], "selected");
        assert!(
            store
                .events_after(0, 100)
                .unwrap()
                .iter()
                .any(|event| event.kind == "team.member_set")
        );

        let cases = [
            (
                json!({"members": [], "mode": "selected"}),
                "at least one member",
            ),
            (
                json!({"members": ["analyst", "analyst"], "mode": "selected"}),
                "unique",
            ),
            (
                json!({"members": ["bot-1", "bot-2", "bot-3", "bot-4", "bot-5", "bot-6"], "mode": "selected"}),
                "at most 5",
            ),
            (json!({"members": ["ghost"], "mode": "selected"}), "ghost"),
            (json!({"members": ["main"], "mode": "selected"}), "main"),
            (json!({"members": ["legal"], "mode": "selected"}), "legal"),
            (
                json!({"members": ["analyst"], "mode": "everyone"}),
                "selected or all",
            ),
        ];
        for (payload, needle) in cases {
            let response = app
                .clone()
                .oneshot(request(
                    &app,
                    "PUT",
                    "/v1/conversations/room:review/team",
                    true,
                    Some(payload),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let value = read_json(response).await;
            let message = value["error"].as_str().unwrap().to_owned();
            assert!(
                message.contains(needle),
                "{message} should mention {needle}"
            );
        }

        // Mode `all` is stored as given.
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/conversations/room:review/team",
                true,
                Some(json!({"members": ["analyst", "maker"], "mode": "all"})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        assert_eq!(body["mode"], "all");
        assert_eq!(body["members"].as_array().unwrap().len(), 2);

        // Group instructions round-trip, newlines included.
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/conversations/room:review/team",
                true,
                Some(json!({
                    "members": ["analyst"],
                    "mode": "selected",
                    "rules": "Line one.\nLine two.",
                })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        assert_eq!(body["rules"], "Line one.\nLine two.");
        let stored = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/conversations/room:review/team",
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(read_json(stored).await["rules"], "Line one.\nLine two.");

        let over = "x".repeat(4001);
        for (rules, needle) in [(over.as_str(), "4000"), ("bad \u{7} char", "control")] {
            let response = app
                .clone()
                .oneshot(request(
                    &app,
                    "PUT",
                    "/v1/conversations/room:review/team",
                    true,
                    Some(json!({"members": ["analyst"], "mode": "selected", "rules": rules})),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let message = read_json(response).await["error"]
                .as_str()
                .unwrap()
                .to_owned();
            assert!(
                message.contains(needle),
                "{message} should mention {needle}"
            );
        }

        // Unknown conversation, non-room conversation, unauthenticated call.
        let missing = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/conversations/room:ghost/team",
                true,
                Some(json!({"members": ["analyst"], "mode": "selected"})),
            ))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let direct = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/conversations",
                true,
                Some(json!({"kind": "direct", "slug": "analyst"})),
            ))
            .await
            .unwrap();
        assert_eq!(direct.status(), StatusCode::OK);
        let wrong_kind = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/conversations/dir:analyst/team",
                true,
                Some(json!({"members": ["analyst"], "mode": "selected"})),
            ))
            .await
            .unwrap();
        assert_eq!(wrong_kind.status(), StatusCode::BAD_REQUEST);

        let unauthorized = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/conversations/room:review/team",
                false,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn audio_transcribe_requires_a_configured_provider() {
        let (app, _, _) = app();
        let no_filename = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/audio/transcribe")
                    .header("authorization", "Bearer test-token-that-is-long-enough-123")
                    .body(Body::from(vec![0u8, 1, 2]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(no_filename.status(), StatusCode::BAD_REQUEST);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/audio/transcribe")
                    .header("authorization", "Bearer test-token-that-is-long-enough-123")
                    .header("x-audio-filename", "note.webm")
                    .body(Body::from(vec![0u8, 1, 2]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = read_json(response).await;
        let message = body["error"].as_str().unwrap().to_owned();
        assert!(message.contains("audio.enabled"), "{message}");
        assert!(!message.contains("synthetic-test-key"), "{message}");
    }

    /// The audio route documents a 20 MiB payload ceiling. The JSON body
    /// limit (2 MiB by default) must not apply to it: a 3 MiB body has to
    /// reach the handler, which then refuses for an unrelated reason.
    #[tokio::test]
    async fn audio_transcribe_accepts_payloads_above_the_json_body_limit() {
        let (app, _, _) = app();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/audio/transcribe")
                    .header("authorization", "Bearer test-token-that-is-long-enough-123")
                    .header("x-audio-filename", "note.webm")
                    .body(Body::from(vec![0u8; 3 * 1024 * 1024]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        // No audio provider is configured in the test fixture, so the handler
        // gets as far as the provider check: the payload was not truncated or
        // rejected by a body limit.
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = read_json(response).await;
        let message = body["error"].as_str().unwrap().to_owned();
        assert!(message.contains("audio.enabled"), "{message}");
    }

    /// A payload above the documented 20 MiB ceiling is rejected with a
    /// message that names the limit, never with the raw body.
    #[tokio::test]
    async fn audio_transcribe_rejects_payloads_above_the_telegram_audio_limit() {
        let (app, _, _) = app();
        let limit = crate::audio::DEFAULT_TELEGRAM_AUDIO_MAX_BYTES as usize;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/audio/transcribe")
                    .header("authorization", "Bearer test-token-that-is-long-enough-123")
                    .header("x-audio-filename", "note.webm")
                    .body(Body::from(vec![0u8; limit + 1024]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = read_json(response).await;
        let message = body["error"].as_str().unwrap().to_owned();
        assert!(message.contains("20971520"), "{message}");
        assert!(message.contains("byte limit"), "{message}");
    }

    /// The JSON endpoints keep the smaller server body limit.
    #[tokio::test]
    async fn json_routes_keep_the_server_body_limit() {
        let (app, _, _) = app();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/conversations")
                    .header("authorization", "Bearer test-token-that-is-long-enough-123")
                    .header("content-type", "application/json")
                    .body(Body::from(vec![b'a'; 3 * 1024 * 1024]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    // ---- Auth endpoints ----

    /// A member account plus a session token minted directly in the store,
    /// for exercising role gates without the OIDC flow.
    fn member_with_session(store: &Store) -> (User, String) {
        let user = store
            .create_user("microsoft:api-member", "member@example.example", "Member")
            .unwrap();
        let token = store
            .mint_session(&user.id, chrono::Duration::hours(1))
            .unwrap();
        (user, token)
    }

    #[tokio::test]
    async fn providers_endpoint_is_public_and_matches_the_config() {
        // Default config: sign-in off, every provider reports disabled.
        let (app, _, _) = app();
        let response = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/auth/providers", false, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        let providers = body["providers"].as_array().unwrap().clone();
        assert_eq!(providers.len(), 3);
        for row in &providers {
            assert_eq!(row["enabled"], json!(false), "auth disabled ⇒ all off");
        }
        assert_eq!(providers[0]["id"], json!("microsoft"));
        assert_eq!(providers[1]["id"], json!("google"));
        assert_eq!(providers[2]["id"], json!("github"));

        // Auth enabled: microsoft on, the others visibly disabled.
        let (app, _, _, _) = auth_app().await;
        let response = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/auth/providers", false, None))
            .await
            .unwrap();
        let body = read_json(response).await;
        let providers = body["providers"].as_array().unwrap().clone();
        assert_eq!(
            providers,
            vec![
                json!({"id": "microsoft", "enabled": true}),
                json!({"id": "google", "enabled": false}),
                json!({"id": "github", "enabled": false}),
            ]
        );
    }

    #[tokio::test]
    async fn login_start_and_callback_fail_closed_when_auth_is_disabled() {
        let (app, _, _) = app();
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/auth/login/start",
                false,
                Some(json!({"provider": "microsoft"})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/auth/callback?state=s&code=c",
                false,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let page = read_body(response).await;
        assert!(page.contains("Sign-in is not enabled"));
        assert!(!page.contains("<script"));
    }

    #[tokio::test]
    async fn login_start_returns_the_authorize_url_and_rejects_bad_providers() {
        let (app, _, store, _) = auth_app().await;
        let admin = store.admin_user_id().unwrap();
        store
            .mint_invite(
                "api-invite-code-0123456789",
                1,
                Utc::now() + chrono::Duration::hours(1),
                &admin,
            )
            .unwrap();
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/auth/login/start",
                false,
                Some(json!({"provider": "microsoft", "invite_code": "api-invite-code-0123456789"})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        let url = body["url"].as_str().unwrap();
        assert!(url.contains("/authorize?"), "{url}");
        assert!(url.contains("code_challenge_method=S256"), "{url}");

        // Unknown provider and spent invites fail closed.
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/auth/login/start",
                false,
                Some(json!({"provider": "google"})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/auth/login/start",
                false,
                Some(json!({"provider": "microsoft", "invite_code": "not-a-real-invite"})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn callback_shows_the_session_token_exactly_once() {
        let (app, _, store, idp) = auth_app().await;
        let admin = store.admin_user_id().unwrap();
        store
            .mint_invite(
                "callback-invite-0123456789",
                1,
                Utc::now() + chrono::Duration::hours(1),
                &admin,
            )
            .unwrap();
        let start = read_json(
            app.clone()
                .oneshot(
                    request(
                        &app,
                        "POST",
                        "/v1/auth/login/start",
                        false,
                        Some(json!({"provider": "microsoft", "invite_code": "callback-invite-0123456789"})),
                    ),
                )
                .await
                .unwrap(),
        )
        .await;
        let authorize_url = start["url"].as_str().unwrap().to_owned();
        let (state, nonce) = crate::auth::test_idp::url_params(&authorize_url);
        let code = "api-callback-code";
        let id_token =
            crate::auth::test_idp::synthetic_id_token(&idp.issuer, &nonce, "subject-api");
        crate::auth::test_idp::stub_token(&idp.server, code, &id_token).await;

        let response = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                &format!("/v1/auth/callback?state={state}&code={code}"),
                false,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(
            response
                .headers()
                .get("content-security-policy")
                .and_then(|v| v.to_str().ok()),
            Some(crate::web::CSP)
        );
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
        let page = read_body(response).await;
        assert!(!page.contains("<script"));
        assert!(page.contains("shown"), "the one-time hint must be present");
        assert!(page.contains("Paste it into your"));

        // The page carries the raw token exactly once; only its hash is
        // stored, and the hash resolves to the newly created member.
        let token = page
            .split("<code>")
            .nth(1)
            .and_then(|rest| rest.split("</code>").next())
            .unwrap()
            .to_owned();
        assert_eq!(token.len(), Store::SESSION_TOKEN_BYTES * 2);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            page.matches(&token).count(),
            1,
            "the session token must appear exactly once"
        );
        let account = store
            .get_user_by_subject("microsoft:subject-api")
            .unwrap()
            .unwrap();
        let resolved = store.session_user(&sha256_hex(&token)).unwrap().unwrap();
        assert_eq!(resolved.id, account.id);

        // An unknown state renders the error page, not a stack trace.
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/auth/callback?state=bogus&code=whatever",
                false,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let page = read_body(response).await;
        assert!(page.contains("could not be completed"));
        assert!(!page.contains("<script"));
    }

    #[tokio::test]
    async fn member_sessions_are_rejected_on_admin_endpoints() {
        let (app, _, store, _) = auth_app().await;
        let (_, member_token) = member_with_session(&store);

        // Unauthenticated: 401 everywhere.
        for (method, uri, body) in [
            ("GET", "/v1/users", None),
            ("POST", "/v1/auth/invites", Some(json!({"uses": 1}))),
        ] {
            let response = app
                .clone()
                .oneshot(request(&app, method, uri, false, body))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri}"
            );
        }

        // Member: 403 on admin endpoints, 200 on ordinary ones.
        for (method, uri, body) in [
            ("GET", "/v1/users", None),
            ("POST", "/v1/auth/invites", Some(json!({"uses": 1}))),
        ] {
            let response = app
                .clone()
                .oneshot(request_with_token(method, uri, &member_token, body))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {uri}");
        }
        let response = app
            .clone()
            .oneshot(request_with_token("GET", "/v1/health", &member_token, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // The operator's static config token is the admin.
        let response = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/users", true, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        let users = body["users"].as_array().unwrap();
        assert_eq!(users.len(), 2, "bootstrap admin + member");
        assert!(users.iter().any(|user| user["role"] == json!("admin")));
    }

    #[tokio::test]
    async fn invites_are_minted_once_and_stored_hashed() {
        let (app, _, store, _) = auth_app().await;
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/auth/invites",
                true,
                Some(json!({"uses": 3})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        let code = body["code"].as_str().unwrap().to_owned();
        assert!(!code.is_empty());
        assert!(store.invite_is_redeemable(&sha256_hex(&code)).unwrap());

        // Bounds are enforced.
        for uses in [0, 101] {
            let response = app
                .clone()
                .oneshot(request(
                    &app,
                    "POST",
                    "/v1/auth/invites",
                    true,
                    Some(json!({"uses": uses})),
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "{uses}"
            );
        }

        // The audit row names the actor and uses, never the code.
        let rows: Vec<(String, String)> = store
            .connection()
            .unwrap()
            .prepare("SELECT actor, details_json FROM audit_log WHERE event = 'auth.invite_minted'")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, store.admin_user_id().unwrap());
        assert!(!rows[0].1.contains(&code));
    }

    #[tokio::test]
    async fn logout_revokes_sessions_but_not_the_config_token() {
        let (app, _, store, _) = auth_app().await;
        let (_, member_token) = member_with_session(&store);

        let response = app
            .clone()
            .oneshot(request_with_token(
                "POST",
                "/v1/auth/logout",
                &member_token,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // The revoked session no longer authorizes.
        let response = app
            .clone()
            .oneshot(request_with_token("GET", "/v1/health", &member_token, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // The static config token has no session row: fail closed.
        let response = app
            .clone()
            .oneshot(request(&app, "POST", "/v1/auth/logout", true, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn admin_can_disable_and_enable_members() {
        let (app, _, store, _) = auth_app().await;
        let (member, member_token) = member_with_session(&store);

        // Disabling a member stops their sessions immediately.
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                &format!("/v1/users/{}/disable", member.id),
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(request_with_token("GET", "/v1/health", &member_token, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // The directory reflects the status.
        let response = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/users", true, None))
            .await
            .unwrap();
        let body = read_json(response).await;
        let users = body["users"].as_array().unwrap().clone();
        let entry = users
            .iter()
            .find(|user| user["id"] == json!(member.id))
            .unwrap();
        assert_eq!(entry["status"], json!("disabled"));

        // Re-enabling restores access.
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                &format!("/v1/users/{}/enable", member.id),
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(request_with_token("GET", "/v1/health", &member_token, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Unknown users and the admin account itself are rejected.
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/users/no-such-user/disable",
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let admin_id = store.admin_user_id().unwrap();
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                &format!("/v1/users/{admin_id}/disable"),
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // Members cannot flip statuses at all.
        let response = app
            .clone()
            .oneshot(request_with_token(
                "POST",
                &format!("/v1/users/{}/disable", member.id),
                &member_token,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn login_page_link_starts_the_microsoft_flow() {
        let (app, _, _, idp) = auth_app().await;
        let response = app
            .clone()
            .oneshot(request(&app, "GET", "/login/microsoft", false, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .unwrap()
            .to_owned();
        assert!(
            location.starts_with(&format!("{}/authorize", idp.issuer)),
            "{location}"
        );
    }
}
