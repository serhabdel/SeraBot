//! Scheduled routines: list (0.29.0 Wave A1) and per-user CRUD (0.30.0 Wave B1).

use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PutRoutineRequest {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    time: Option<String>,
    #[serde(default)]
    timezone: Option<String>,
}

/// `GET /v1/routines`: name, enabled, next run, last run date and config.
pub(super) async fn list_routines(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let routines = state.store.list_routines(&user.id)?;
    Ok(Json(json!({"routines": routines})))
}

/// `PUT /v1/routines/{name}`: enable/disable and set local time+timezone.
pub(super) async fn put_routine(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(name): Path<String>,
    Json(body): Json<PutRoutineRequest>,
) -> Result<Json<Value>> {
    if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
        return Err(SeraError::Validation("invalid routine name".into()));
    }
    let kind = crate::config::classify_member_routine(&name)?;
    if body.enabled.is_none() && body.time.is_none() && body.timezone.is_none() {
        return Err(SeraError::Validation("no routine fields to update".into()));
    }

    let exists = state.store.routine_exists(&user.id, &name)?;
    let default_time = match kind {
        crate::config::MemberRoutineKind::Brief => state.config.schedule.brief_time.clone(),
        crate::config::MemberRoutineKind::Watch(index) => {
            crate::config::default_watch_time(&state.config.schedule, index)
        }
    };
    let time = body.time.as_deref().unwrap_or(default_time.as_str());
    let tz_name = body
        .timezone
        .as_deref()
        .unwrap_or(state.config.schedule.timezone.as_str());
    let tz: chrono_tz::Tz = tz_name
        .parse()
        .map_err(|_| SeraError::Validation(format!("invalid timezone: {tz_name:?}")))?;

    let schedule_change = body.time.is_some() || body.timezone.is_some();
    if schedule_change || (body.enabled == Some(true) && !exists) {
        crate::scheduler::ensure_daily_routine(&state.store, &user.id, &name, time, tz)?;
    }
    if let Some(enabled) = body.enabled {
        if !enabled && !exists && !schedule_change {
            return Err(SeraError::NotFound(format!("routine {name}")));
        }
        if enabled || exists || schedule_change {
            crate::scheduler::set_routine_enabled(&state.store, &user.id, &name, enabled)?;
        }
    }

    let routine = state
        .store
        .list_routines(&user.id)?
        .into_iter()
        .find(|row| row.name == name)
        .ok_or_else(|| SeraError::NotFound(format!("routine {name}")))?;
    state.store.record_audit(
        &user.id,
        "routine.updated",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"name": name, "enabled": routine.enabled})),
    )?;
    Ok(Json(json!({"routine": routine})))
}
