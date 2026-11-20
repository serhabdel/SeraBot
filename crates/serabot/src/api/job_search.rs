//! `/v1/jobs/*` (0.70): the acting member's job profile, sources, matches,
//! application pipeline and an on-demand watch run. Every row is the
//! caller's own; there is no way to name another member.
//! (`/v1/jobs` itself, without a suffix, is the running-work panel.)

use super::*;
use crate::job_watch::{RunMode, SOURCE_DELAY, job_fetcher, run_watch};
use crate::jobs::JobProfile;

/// A manual run is refused when the previous one ended less than this ago.
const COOLDOWN_SECS: i64 = 60;

fn overview(state: &ApiState, user_id: &str) -> Result<Value> {
    let last = state
        .store
        .job_watch_state(user_id)?
        .map(|(at, mode, report)| json!({"at": at, "mode": mode, "report": report}));
    Ok(json!({
        "profile": state.store.job_profile(user_id)?,
        "sources": state.store.job_sources(user_id)?,
        "matches": state.store.recent_job_matches(user_id, 20)?,
        "counts": state.store.job_application_counts(user_id)?
            .into_iter()
            .map(|(status, count)| json!({"status": status, "count": count}))
            .collect::<Vec<_>>(),
        "last_run": last,
    }))
}

pub(super) async fn jobs_overview(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    Ok(Json(overview(&state, &user.id)?))
}

pub(super) async fn jobs_profile_get(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    Ok(Json(json!({"profile": state.store.job_profile(&user.id)?})))
}

pub(super) async fn jobs_profile_put(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(profile): Json<JobProfile>,
) -> Result<Json<Value>> {
    let saved = state.store.set_job_profile(&user.id, &profile)?;
    state.store.record_audit(
        &user.id,
        "jobs.profile_saved",
        None,
        None,
        None,
        None,
        None,
        None,
    )?;
    Ok(Json(json!({"profile": saved})))
}

pub(super) async fn jobs_sources_get(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    Ok(Json(json!({"sources": state.store.job_sources(&user.id)?})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceSwitch {
    auto_watch: bool,
}

pub(super) async fn jobs_source_put(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(key): Path<String>,
    Json(request): Json<SourceSwitch>,
) -> Result<Json<Value>> {
    state
        .store
        .set_job_source_auto_watch(&user.id, &key, request.auto_watch)?;
    Ok(Json(json!({"sources": state.store.job_sources(&user.id)?})))
}

pub(super) async fn jobs_applications_get(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    Ok(Json(json!({
        "applications": state.store.list_job_applications(&user.id, None)?,
        "counts": state.store.job_application_counts(&user.id)?
            .into_iter()
            .map(|(status, count)| json!({"status": status, "count": count}))
            .collect::<Vec<_>>(),
    })))
}

/// Run the member's watch now (every source, robots.txt noted).
pub(super) async fn jobs_watch(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    if let Some((at, _, _)) = state.store.job_watch_state(&user.id)?
        && let Ok(at) = chrono::DateTime::parse_from_rfc3339(&at)
        && (chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_seconds() < COOLDOWN_SECS
    {
        return Err(SeraError::InvalidState(
            "the watch ran a moment ago; wait a minute and try again".into(),
        ));
    }
    let fetcher = job_fetcher(&state.config)?;
    let report = run_watch(
        &state.store,
        &fetcher,
        &state.config.watcher.user_agent,
        &user.id,
        RunMode::Manual,
        None,
        SOURCE_DELAY,
    )
    .await?;
    Ok(Json(
        json!({"report": report, "overview": overview(&state, &user.id)?}),
    ))
}
