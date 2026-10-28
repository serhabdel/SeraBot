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
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::warn;

use crate::config::Config;
use crate::domain::Task;
use crate::error::{Result, SeraError};
use crate::gateway::Decision;
use crate::maker::ArtifactStore;
use crate::runtime::{ApprovalNotice, Notifier, Runtime};
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
    if request.conversation_id.is_empty()
        || request.conversation_id.len() > 128
        || !request
            .conversation_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
    {
        return Err(SeraError::Validation("invalid conversation_id".into()));
    }
    let task = state.runtime.submit_text(
        &request.conversation_id,
        &request.text,
        request.bot.as_deref(),
    )?;
    Ok(Json(json!({"task": task_view(&task)})))
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
        let mut config = Config::default();
        config.api.enabled = true;
        config.api.token_hash = sha256_hex("test-token-that-is-long-enough-123");
        config.llm.data_egress_approved = true;
        config.base_dir = std::env::current_dir().unwrap();
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
            },
            prompt: "test prompt".into(),
        };
        let gateway = Gateway::new(
            store.clone(),
            Arc::new(Policy::empty()),
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
            llm,
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
}
