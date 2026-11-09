//! Monitoring source health (0.29.0, Wave A2): configured watcher sources plus
//! the acting user's latest `source_runs` row for each.

use super::*;

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
    let flat: String = text
        .chars()
        .filter(|ch| !ch.is_control())
        .collect();
    if flat.contains('<') {
        return Some("run failed (response withheld)".into());
    }
    Some(truncate_chars(&flat, MAX_SOURCE_ERROR_CHARS))
}

/// `GET /v1/monitoring/sources`: configured sources and last-run status.
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
                "url": source.url,
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
