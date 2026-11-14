//! The Jobs panel data (0.28.0, phase C1): `GET /v1/jobs` lists the acting
//! user's queued/running tasks and their media jobs across all conversations,
//! so the console main view can show ongoing work. Everything is scoped to
//! the acting user; another user's rows never appear.

use super::*;

/// Task statuses that count as in flight for the jobs panel.
const ACTIVE_TASK_STATUSES: [&str; 3] = ["queued", "running", "awaiting_approval"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JobsQuery {
    #[serde(default)]
    limit: Option<usize>,
    /// Optional task status filter: queued, running or awaiting_approval.
    #[serde(default)]
    status: Option<String>,
}

/// `GET /v1/jobs`: queued/running tasks plus media jobs across the acting
/// user's conversations, newest activity first.
pub(super) async fn list_jobs(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<JobsQuery>,
) -> Result<Json<Value>> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let status_filter = match query.status.as_deref() {
        None => None,
        Some(status) if ACTIVE_TASK_STATUSES.contains(&status) => Some(status),
        Some(_) => {
            return Err(SeraError::Validation(
                "status must be queued, running or awaiting_approval".into(),
            ));
        }
    };

    let tasks = list_active_tasks(&state.store, &user.id, status_filter, limit)?;
    let media_jobs = state.store.list_media_jobs_for_user(&user.id, limit)?;

    let reliability_limit = 50;
    let uncertain_tasks = list_uncertain_tasks(&state.store, &user.id, reliability_limit)?;
    let unknown_actions = list_outcome_unknown_actions(&state.store, &user.id, reliability_limit)?;

    Ok(Json(json!({
        "tasks": tasks,
        "media_jobs": media_jobs,
        "limits": {"tasks": limit, "media_jobs": limit},
        "reliability": {
            "uncertain_tasks": uncertain_tasks,
            "unknown_actions": unknown_actions,
            "traces_path": "/v1/traces",
        },
    })))
}

/// The acting user's in-flight task rows as JSON, read through the same
/// scoped connection the personal-task list uses. Another user's tasks are
/// never selected.
fn list_active_tasks(
    store: &Store,
    user_id: &str,
    status: Option<&str>,
    limit: usize,
) -> Result<Vec<Value>> {
    let connection = store.connection()?;
    let mut sql = String::from(
        "SELECT id, kind, bot, payload_json, status, priority, attempts, max_attempts,
                run_after, lease_until, worker_id, uncertain, last_error, created_at, updated_at
         FROM tasks WHERE user_id = ?1 AND status IN ('queued','running','awaiting_approval')",
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(user_id.to_owned())];
    if let Some(status) = status {
        sql.push_str(" AND status = ?2");
        params.push(Box::new(status.to_owned()));
    }
    sql.push_str(" ORDER BY created_at ASC, rowid ASC LIMIT ?");
    params.push(Box::new((limit.clamp(1, 500)) as i64));

    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(
        rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
        |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "kind": row.get::<_, String>(1)?,
                "bot": row.get::<_, String>(2)?,
                "payload": serde_json::from_str::<Value>(&row.get::<_, String>(3)?)
                    .unwrap_or(Value::Null),
                "status": row.get::<_, String>(4)?,
                "priority": row.get::<_, i64>(5)?,
                "attempts": row.get::<_, i64>(6)?,
                "max_attempts": row.get::<_, i64>(7)?,
                "run_after": row.get::<_, String>(8)?,
                "lease_until": row.get::<_, Option<String>>(9)?,
                "worker_id": row.get::<_, Option<String>>(10)?,
                "uncertain": row.get::<_, i64>(11)? != 0,
                "last_error": row.get::<_, Option<String>>(12)?,
                "created_at": row.get::<_, String>(13)?,
                "updated_at": row.get::<_, String>(14)?,
            }))
        },
    )?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn list_uncertain_tasks(store: &Store, user_id: &str, limit: usize) -> Result<Vec<Value>> {
    let connection = store.connection()?;
    let mut statement = connection.prepare(
        "SELECT id, kind, bot, payload_json, status, last_error, updated_at
         FROM tasks WHERE user_id = ?1 AND uncertain = 1
         ORDER BY updated_at DESC, rowid DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(rusqlite::params![user_id, limit as i64], |row| {
        let payload: Value = serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or(Value::Null);
        let conversation_id = payload
            .get("conversation_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        Ok(json!({
            "id": row.get::<_, String>(0)?,
            "kind": row.get::<_, String>(1)?,
            "bot": row.get::<_, String>(2)?,
            "status": row.get::<_, String>(4)?,
            "last_error": row.get::<_, Option<String>>(5)?,
            "updated_at": row.get::<_, String>(6)?,
            "conversation_id": conversation_id,
            "uncertain": true,
        }))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn list_outcome_unknown_actions(store: &Store, user_id: &str, limit: usize) -> Result<Vec<Value>> {
    let connection = store.connection()?;
    let mut statement = connection.prepare(
        "SELECT a.id, a.tool, a.task_id, a.turn_id, a.destination, a.canonical_hash, a.updated_at,
                t.conversation_id
         FROM actions a
         LEFT JOIN turns t ON t.id = a.turn_id AND t.user_id = ?1
         WHERE a.user_id = ?1 AND a.status = 'outcome_unknown'
         ORDER BY a.updated_at DESC, a.rowid DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(rusqlite::params![user_id, limit as i64], |row| {
        let conversation_id: Option<String> = row.get(7)?;
        Ok(json!({
            "id": row.get::<_, String>(0)?,
            "tool": row.get::<_, String>(1)?,
            "task_id": row.get::<_, Option<String>>(2)?,
            "turn_id": row.get::<_, Option<String>>(3)?,
            "destination": row.get::<_, Option<String>>(4)?,
            "canonical_hash": row.get::<_, String>(5)?,
            "updated_at": row.get::<_, String>(6)?,
            "conversation_id": conversation_id,
            "status": "outcome_unknown",
        }))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{Gateway, ToolRegistry};
    use crate::llm::LlmClient;
    use crate::media_jobs::MediaKind;
    use crate::policy::Policy;
    use crate::runtime::NullNotifier;
    use tower::ServiceExt;

    fn test_app_for_jobs() -> (Router, Store) {
        let dir = std::env::temp_dir().join(format!("serabot-jobs-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut config = Config::default();
        config.api.enabled = true;
        config.api.token_hash = sha256_hex("test-token-that-is-long-enough-123");
        config.llm.model = "test".into();
        config.llm.base_url = "https://example.com/v1".into();
        config.base_dir = dir.clone();
        let db_path = dir.join("serabot.db");
        config.database.path = db_path.to_string_lossy().into_owned();
        let config = Arc::new(config);
        let store = Store::open(&db_path, 384, 5000).unwrap();
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
            crate::model_router::ModelRouter::new(
                &config.llm,
                BTreeMap::from([("default".into(), llm)]),
            )
            .unwrap(),
            vec![crate::config::LoadedBot {
                config: BotConfig {
                    title: None,
                    description: None,
                    name: "main".into(),
                    trigger: "command".into(),
                    model: None,
                    system_prompt: "prompts/main.txt".into(),
                    skills: vec![],
                    tools: vec![],
                    require_approval: vec![],
                    enabled: true,
                    temperature: None,
                    thinking: None,
                    role: BotRole::Orchestrator,
                    delegates: vec![],
                },
                prompt: "test prompt".into(),
            }],
            Arc::new(NullNotifier),
            tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        let artifacts = Arc::new(ArtifactStore::new(dir.join("artifacts")).unwrap());
        let secrets = SharedSecrets::new(crate::secrets::SecretStore::from_map(BTreeMap::new()));
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [5u8; 32]).unwrap());
        let state = ApiState::new(
            runtime,
            store.clone(),
            config,
            artifacts,
            secrets,
            users,
            None,
        )
        .unwrap();
        (crate::api::router(state), store)
    }

    #[tokio::test]
    async fn list_jobs_batches_media_jobs_across_conversations_and_respects_limit() {
        let (app, store) = test_app_for_jobs();
        let alice = store
            .create_user("sub-jobs-batch-a", "jba@example.example", "Alice")
            .unwrap();
        let bob = store
            .create_user("sub-jobs-batch-b", "jbb@example.example", "Bob")
            .unwrap();
        let alice_token = store
            .mint_session(&alice.id, chrono::Duration::hours(1))
            .unwrap();

        for idx in 0..3 {
            let conv = format!("dir:conv-{idx}");
            store
                .create_conversation(&alice.id, &conv, "direct", None)
                .unwrap();
            store
                .create_media_job(
                    &alice.id,
                    &conv,
                    "main",
                    "synthetic",
                    MediaKind::Image,
                    &format!("ext-a-{idx}"),
                )
                .unwrap();
        }
        store
            .create_conversation(&bob.id, "dir:bob", "direct", None)
            .unwrap();
        store
            .create_media_job(
                &bob.id,
                "dir:bob",
                "main",
                "synthetic",
                MediaKind::Video,
                "ext-bob",
            )
            .unwrap();

        let resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/v1/jobs?limit=2")
                    .header("authorization", format!("Bearer {alice_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let media_jobs = body["media_jobs"].as_array().unwrap();
        assert_eq!(media_jobs.len(), 2);
        assert!(
            media_jobs
                .iter()
                .all(|job| job["external_id"].as_str().unwrap().starts_with("ext-a-"))
        );

        let all_resp = app
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/v1/jobs")
                    .header("authorization", format!("Bearer {alice_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(all_resp.status(), StatusCode::OK);
        let all_bytes = axum::body::to_bytes(all_resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let all_body: Value = serde_json::from_slice(&all_bytes).unwrap();
        assert_eq!(all_body["media_jobs"].as_array().unwrap().len(), 3);
    }
}
