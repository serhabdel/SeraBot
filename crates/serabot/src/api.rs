use std::collections::{BTreeMap, VecDeque};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
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

use crate::admin::AdminStore;
use crate::config::{Config, OAuthConnectorSettings, ProviderConfig, scaffold_specialist_bot};
use crate::domain::Task;
use crate::error::{Result, SeraError};
use crate::gateway::Decision;
use crate::maker::ArtifactStore;
use crate::memory;
use crate::model_router::ModelSelection;
use crate::orchestration::{ConversationKind, scoped_conversation_id, validate_conversation_id};
use crate::runtime::{ApprovalNotice, Notifier, Runtime};
use crate::secrets::SharedSecrets;
use crate::skills::{SkillCatalog, create_skill, discard_created_skill};
use crate::store::{Store, StoredConversation};
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
}

impl ApiState {
    pub fn new(
        runtime: Runtime,
        store: Store,
        config: Arc<Config>,
        artifacts: Arc<ArtifactStore>,
        secrets: SharedSecrets,
    ) -> Self {
        Self {
            runtime,
            store,
            artifacts,
            secrets,
            token_hash: config.api.token_hash.to_ascii_lowercase(),
            rate: Arc::new(Mutex::new(VecDeque::new())),
            rate_limit: config.server.rate_limit_per_minute.max(1) as usize,
            admin: AdminStore::open(&config).ok(),
            config,
        }
    }
}

pub fn router(state: ApiState) -> Router {
    let shared = Arc::new(state);
    let protected = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/status", get(status))
        .route("/v1/messages", post(send_message))
        .route("/v1/bots", get(list_bots).post(create_bot))
        .route("/v1/skills", get(list_skills).post(create_skill_pack))
        .route(
            "/v1/conversations",
            get(list_conversations).post(create_conversation),
        )
        .route(
            "/v1/conversations/{id}",
            patch(rename_conversation).delete(archive_conversation),
        )
        .route("/v1/memories", get(list_memories).post(add_memory))
        .route("/v1/memories/{id}", delete(delete_memory))
        .route("/v1/providers", get(list_providers))
        .route("/v1/admin/providers", post(admin_add_provider))
        .route("/v1/admin/providers/{id}", delete(admin_remove_provider))
        .route("/v1/admin/connectors/{id}", put(admin_set_connector))
        .route("/v1/admin/audio", put(admin_set_audio))
        .route("/v1/admin/restart", post(admin_restart))
        .route("/v1/connectors", get(list_connectors))
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
        ));
    crate::web::router()
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
            "telegram_enabled": state.config.telegram.enabled,
            "catalog_fetch": state.config.llm.catalog_fetch,
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
    headers: HeaderMap,
    Query(query): Query<EventQuery>,
) -> Sse<impl futures::Stream<Item = std::result::Result<SseEvent, Infallible>>> {
    let last = resume_cursor(&headers, query.after);
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
            json!({
                "id": bot.id,
                "role": bot.role,
                "enabled": bot.enabled,
                "live": state.runtime.bot_loaded(&bot.id),
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
    let title = request
        .title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
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
    let providers = catalog
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
            json!({
                "id": provider.id,
                "kind": provider.kind,
                "models": provider.models,
                "routing_classes": routing_classes,
                "data_egress_approved": provider.data_egress_approved,
                "catalog_fetch": provider.catalog_fetch,
                "catalog_verified": provider.catalog_verified,
                "managed": managed,
                "secrets": {
                    "api_key": secret_status(&state.secrets, config.map(|c| c.api_key_secret.as_str())),
                    "refresh_token": secret_status(&state.secrets, config.map(|c| c.refresh_token_secret.as_str())),
                    "oauth_client_id": secret_status(&state.secrets, config.map(|c| c.oauth_client_id_secret.as_str())),
                    "oauth_client_secret": secret_status(&state.secrets, config.map(|c| c.oauth_client_secret_secret.as_str())),
                },
            })
        })
        .collect::<Vec<_>>();
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
    views.push(json!({
        "id": "agentmail",
        "kind": "api_key",
        "enabled": agentmail.enabled,
        "redirect_uri": Value::Null,
        "api_base": agentmail.api_base,
        "default_inbox_id": agentmail.default_inbox_id,
        "secrets": {
            "api_key": secret_status(&state.secrets, Some(agentmail.api_key_secret.as_str())),
        },
    }));
    Ok(Json(json!({"connectors": views})))
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
        ..ProviderConfig::default()
    };
    let kind = profile.kind.clone();
    let catalog_fetch = profile.catalog_fetch;
    admin
        .add_provider(
            &state.config,
            &state.secrets,
            &request.id,
            profile,
            request.api_key.as_deref(),
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

#[derive(Deserialize)]
struct ConnectorRequest {
    enabled: bool,
    #[serde(default)]
    values: BTreeMap<String, String>,
}

async fn admin_set_connector(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<ConnectorRequest>,
) -> AdminResult<Json<Value>> {
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
        "config.connector_saved",
        json!({"connector": id, "enabled": request.enabled, "credentials": request.values.keys().collect::<Vec<_>>()}),
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
    if constant_time_eq(&candidate, &state.token_hash) {
        return next.run(request).await;
    }
    let now = chrono::Utc::now();
    match state.store.device_token_by_hash(&candidate) {
        Ok(Some(record)) if record.active_at(now) => {
            if let Err(error) = state.store.touch_device_token(&record.id, now) {
                warn!(%error, "device token last-used update failed");
            }
            next.run(request).await
        }
        Ok(_) => unauthorized(),
        Err(error) => {
            warn!(%error, "device token lookup failed");
            unauthorized()
        }
    }
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
        let database =
            std::env::temp_dir().join(format!("serabot-api-{}.db", uuid::Uuid::new_v4()));
        config.database.path = database.to_string_lossy().into_owned();
        let config = Arc::new(config);
        let store = Store::open(&database, 384, 5000).unwrap();
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
        let state = ApiState::new(
            runtime,
            store.clone(),
            config.clone(),
            artifacts,
            SharedSecrets::new(crate::secrets::SecretStore::from_map(
                std::collections::BTreeMap::from([(
                    config.llm.api_key_secret.clone(),
                    "synthetic-test-key".to_string(),
                )]),
            )),
        );
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
        assert_eq!(saved.status(), StatusCode::CREATED);
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
        assert_eq!(list.len(), 8);
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
}
