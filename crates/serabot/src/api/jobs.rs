//! The Jobs panel data (0.28.0, phase C1): `GET /v1/jobs` lists the acting
//! user's queued/running tasks and their media jobs across all conversations,
//! so the console main view can show ongoing work. Everything is scoped to
//! the acting user; another user's rows never appear.

use super::*;

/// Task statuses that count as in flight for the jobs panel.
const ACTIVE_TASK_STATUSES: [&str; 3] = ["queued", "running", "awaiting_approval"];
/// Conversations scanned for media jobs (the per-conversation list is capped
/// at 100 rows each).
const JOBS_CONVERSATION_SCAN: usize = 200;

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

    // Media jobs across conversations: the store lists them per conversation
    // (owner-scoped), so fan out over the acting user's conversations.
    let conversations =
        state
            .store
            .list_conversations(&user.id, JOBS_CONVERSATION_SCAN, None, true)?;
    let mut media_jobs = Vec::new();
    for conversation in &conversations {
        media_jobs.extend(state.store.list_media_jobs(&user.id, &conversation.id)?);
    }
    media_jobs.sort_by_key(|job| std::cmp::Reverse(job.updated_at));
    media_jobs.truncate(limit);

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
