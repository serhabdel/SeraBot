//! Monitoring source health (0.29.0, Wave A2) and manual watch enqueue (0.30.0 B2).

use super::*;
use crate::runtime::ScheduledPayload;
use crate::store::SourceRunStatus;
use crate::util::redact_url_queries;

const MAX_SOURCE_ERROR_CHARS: usize = 240;

/// Strip HTML-ish watcher errors before they reach the API. URL queries and
/// fragments are removed too: a run error can name a feed with an access key,
/// and rows stored by earlier builds may still carry one.
fn sanitize_source_error(raw: Option<&str>) -> Option<String> {
    let redacted = redact_url_queries(raw?.trim());
    let text = redacted.trim();
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

/// The feed URL a caller may see. Admins see every feed. A member sees only a
/// feed they added themselves (`owned`); admin-defined feeds show `null` to
/// members. Feed URLs can carry access tokens in their query string.
pub(super) fn visible_feed_url(acting: &ActingUser, owned: bool, url: &str) -> Value {
    if acting.is_admin() || owned {
        json!(url)
    } else {
        Value::Null
    }
}

fn last_run_json(run: Option<&SourceRunStatus>) -> Value {
    run.map_or(Value::Null, |run| {
        json!({
            "ok": run.ok,
            "error": sanitize_source_error(run.error.as_deref()),
            "items_seen": run.items_seen,
            "started_at": run.started_at,
            "finished_at": run.finished_at,
        })
    })
}

/// `GET /v1/monitoring/sources`: configured sources, the caller's own feeds,
/// and last-run status. Names and kinds are shared; feed URLs are admin-only,
/// except a member's own feed, which shows its URL to that member.
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
    let mut sources = configured
        .into_iter()
        .map(|source| {
            let last = by_source.remove(&source.name);
            json!({
                "name": source.name,
                "kind": source.kind,
                "enabled": source.enabled,
                "url": visible_feed_url(&user, false, &source.url),
                "last_run": last_run_json(last.as_ref()),
            })
        })
        .collect::<Vec<_>>();
    for row in state.store.list_user_sources(&user.id)? {
        let Some(url) = row.url.as_deref().filter(|_| row.origin == "member") else {
            continue;
        };
        let last = by_source.remove(&row.name);
        sources.push(json!({
            "name": row.name,
            "kind": row.kind,
            "enabled": row.enabled,
            "url": visible_feed_url(&user, true, url),
            "last_run": last_run_json(last.as_ref()),
        }));
    }
    Ok(Json(json!({"sources": sources})))
}

#[derive(Deserialize)]
pub(super) struct DigestQuery {
    /// Window in hours back from now (default 24, at most 168).
    pub hours: Option<u32>,
    /// Start of the window as RFC 3339. Wins over `hours`, and is clamped to a week.
    pub since: Option<String>,
}

/// `GET /v1/digest`: items the caller's watched sources reported since a time,
/// with the sources whose latest run failed.
pub(super) async fn get_digest(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<DigestQuery>,
) -> Result<Json<Value>> {
    let now = chrono::Utc::now();
    let since = match query.since.as_deref() {
        Some(raw) => {
            let parsed = chrono::DateTime::parse_from_rfc3339(raw.trim())
                .map_err(|_| SeraError::Validation("since must be an RFC 3339 time".into()))?
                .with_timezone(&chrono::Utc);
            parsed.max(crate::digest::window_start(now, crate::digest::MAX_HOURS))
        }
        None => {
            crate::digest::window_start(now, query.hours.unwrap_or(crate::digest::DEFAULT_HOURS))
        }
    };
    let digest = crate::digest::build(&state.store, &user.id, since)?;
    Ok(Json(crate::digest::to_json(&digest)))
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
