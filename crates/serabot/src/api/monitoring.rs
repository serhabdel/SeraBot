//! Monitoring source health (0.29.0, Wave A2) and manual watch enqueue (0.30.0 B2).

use super::*;
use crate::runtime::ScheduledPayload;

const MAX_SOURCE_ERROR_CHARS: usize = 240;

/// Strip HTML-ish watcher errors before they reach the API.
fn sanitize_source_error(raw: Option<&str>) -> Option<String> {
    let text = raw?.trim();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("<html") || lower.contains("<!doctype") {
        return Some("run failed (response withheld)".into());
    }
    let flat: String = text.chars().filter(|ch| !ch.is_control()).collect();
    if flat.contains('<') {
        return Some("run failed (response withheld)".into());
    }
    Some(truncate_chars(&flat, MAX_SOURCE_ERROR_CHARS))
}

/// The feed URL a caller may see: admins get it, members get `null`. Feed URLs
/// can carry access tokens in their query string, so they stay admin-only.
fn visible_feed_url(acting: &ActingUser, url: &str) -> Value {
    if acting.is_admin() {
        json!(url)
    } else {
        Value::Null
    }
}

/// `GET /v1/monitoring/sources`: configured sources and last-run status.
/// Source names and kinds are shared config; feed URLs are admin-only.
pub(super) async fn list_monitoring_sources(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let configured = crate::config::load_sources(&state.config)?;
    let runs = state.store.list_source_run_status(&user.id)?;
    let mut by_source: HashMap<String, _> = runs
        .into_iter()
        .map(|row| (row.source.clone(), row))
        .collect();
    let sources = configured
        .into_iter()
        .map(|source| {
            let last = by_source.remove(&source.name);
            json!({
                "name": source.name,
                "kind": source.kind,
                "enabled": source.enabled,
                "url": visible_feed_url(&user, &source.url),
                "last_run": last.as_ref().map(|run| json!({
                    "ok": run.ok,
                    "error": sanitize_source_error(run.error.as_deref()),
                    "items_seen": run.items_seen,
                    "started_at": run.started_at,
                    "finished_at": run.finished_at,
                })),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"sources": sources})))
}

/// `POST /v1/monitoring/run`: enqueue one `watch` task for the acting user
/// (same effect as `/watch` in chat).
pub(super) async fn run_all_sources(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let conversation_id = state.store.default_scheduled_conversation_id(&user.id)?;
    let task = state.store.enqueue_task_with_quota(
        &user.id,
        "watch",
        "watcher",
        &serde_json::to_value(ScheduledPayload {
            conversation_id: conversation_id.clone(),
            user_id: user.id.clone(),
        })?,
        5,
        chrono::Utc::now(),
        state.config.users.tasks_per_user_per_day,
    )?;
    state.store.record_audit(
        &user.id,
        "watch.manual_enqueue",
        None,
        Some(&task.id),
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"conversation_id": conversation_id})),
    )?;
    Ok(Json(json!({"task_id": task.id, "kind": "watch"})))
}
