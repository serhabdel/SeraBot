use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::warn;

use crate::config::{Config, scaffold_specialist_bot};
use crate::domain::Task;
use crate::error::{Result, SeraError};
use crate::gateway::Decision;
use crate::maker::ArtifactStore;
use crate::model_router::ModelSelection;
use crate::orchestration::{ConversationKind, scoped_conversation_id};
use crate::runtime::{ApprovalNotice, Notifier, Runtime};
use crate::skills::{SkillCatalog, create_skill, discard_created_skill};
use crate::store::Store;
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
    token_hash: String,
    rate: Arc<Mutex<VecDeque<Instant>>>,
    rate_limit: usize,
}

impl ApiState {
    pub fn new(
        runtime: Runtime,
        store: Store,
        config: Arc<Config>,
        artifacts: Arc<ArtifactStore>,
    ) -> Self {
        Self {
            runtime,
            store,
            artifacts,
            token_hash: config.api.token_hash.to_ascii_lowercase(),
            rate: Arc::new(Mutex::new(VecDeque::new())),
            rate_limit: config.server.rate_limit_per_minute.max(1) as usize,
            config,
        }
    }
}

pub fn router(state: ApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/status", get(status))
        .route("/v1/messages", post(send_message))
        .route("/v1/bots", get(list_bots).post(create_bot))
        .route("/v1/skills", get(list_skills).post(create_skill_pack))
        .route("/v1/conversations", post(create_conversation))
        .route("/v1/models", get(models))
        .route("/v1/conversations/{id}/model", put(select_model))
        .route(
            "/v1/conversations/{id}/messages",
            get(conversation_messages),
        )
        .route("/v1/tasks/{id}", get(task))
        .route("/v1/approvals", get(list_approvals))
        .route("/v1/approvals/{id}/decide", post(decide_approval))
        .route("/v1/events", get(events))
        .route("/v1/audit", get(audit))
        .route("/v1/artifacts/{hash}", get(artifact))
        .route("/v1/artifacts/{hash}/approve", post(approve_artifact))
        .layer(middleware::from_fn_with_state(shared.clone(), authorize))
        .layer(middleware::from_fn_with_state(shared.clone(), rate_limit))
        .layer(RequestBodyLimitLayer::new(
            shared.config.server.body_limit_bytes,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(shared)
}

async fn health(State(state): State<Arc<ApiState>>) -> Response {
    match state.store.health() {
        Ok(()) => Json(json!({
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
            "database": state.store.path().display().to_string(),
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

fn validate_conversation_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
    {
        return Err(SeraError::Validation("invalid conversation_id".into()));
    }
    Ok(())
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
            {
                if state.runtime.scan_owner_output(name).is_ok() {
                    body["name"] = json!(name);
                }
            }
            body
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"messages": messages})))
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
    Query(query): Query<EventQuery>,
) -> Sse<impl futures::Stream<Item = std::result::Result<SseEvent, Infallible>>> {
    let last = query.after.unwrap_or(0).max(0);
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let store = state.store.clone();
    let stream = futures::stream::unfold((last, store), move |(last, store)| async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
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
            json!({
                "id": bot.id,
                "role": bot.role,
                "enabled": bot.enabled,
                "live": state.runtime.bot_loaded(&bot.id),
                "tools": bot.tools,
                "skills": bot.skills,
            })
        })
        .collect::<Vec<_>>();
    let body = json!({ "bots": bots });
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
    state
        .store
        .create_conversation(&id, kind.as_str(), parent_id)?;
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
            "accepts_turns": kind.accepts_turns(),
        })),
    )?;
    Ok(Json(json!({
        "conversation": {
            "id": record.id,
            "kind": record.kind,
            "parent_id": record.parent_id,
            "accepts_turns": kind.accepts_turns(),
        }
    })))
}

async fn authorize(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    request: Request,
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
    if !constant_time_eq(&candidate, &state.token_hash) {
        return unauthorized();
    }
    next.run(request).await
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "unauthorized"})),
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
        app_with_root(egress_approved, policy, std::env::current_dir().unwrap())
    }

    fn app_with_root(
        egress_approved: bool,
        policy: Policy,
        base_dir: std::path::PathBuf,
    ) -> (Router, Arc<Config>, Store) {
        let mut config = Config::default();
        config.api.enabled = true;
        config.api.token_hash = sha256_hex("test-token-that-is-long-enough-123");
        config.llm.data_egress_approved = egress_approved;
        config.llm.model = "test".into();
        config.llm.base_url = "https://example.com/v1".into();
        config.llm.models = vec!["test-alternative".into()];
        config.base_dir = base_dir;
        let config = Arc::new(config);
        let store = Store::open(
            std::env::temp_dir().join(format!("serabot-api-{}.db", uuid::Uuid::new_v4())),
            384,
            5000,
        )
        .unwrap();
        let bot = LoadedBot {
            config: BotConfig {
                name: "main".into(),
                trigger: "command".into(),
                model: None,
                system_prompt: "prompts/main.txt".into(),
                skills: vec![],
                tools: vec![],
                require_approval: vec![],
                enabled: true,
                temperature: None,
                role: Default::default(),
                delegates: vec![],
            },
            prompt: "test prompt".into(),
        };
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
            vec![bot],
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
        let state = ApiState::new(runtime, store.clone(), config.clone(), artifacts);
        (router(state), config, store)
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
        let (app, _, _) = app();
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
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["ok"], true);
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

    async fn read_json(response: Response) -> Value {
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        serde_json::from_slice(&bytes).unwrap()
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
}
