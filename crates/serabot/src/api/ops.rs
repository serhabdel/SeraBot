//! `/v1/alerts` and `/v1/admin/backups` (0.68). Alerts are the caller's own:
//! where they arrive and the last ones sent. Backups are owner only.

use super::*;
use crate::ops::{AlertSettings, backup_view};

fn alerts_view(state: &ApiState, user_id: &str) -> Result<Value> {
    let settings = state.store.alert_settings(user_id)?;
    let telegram_linked = state.store.telegram_chat_for(user_id)?.is_some();
    let choices: Vec<Value> = state
        .store
        .alert_whatsapp_choices(user_id)?
        .into_iter()
        .map(|(conversation_id, jid)| {
            let label = jid.split('@').next().unwrap_or_default();
            json!({"conversation_id": conversation_id, "label": format!("+{label}")})
        })
        .collect();
    Ok(json!({
        "enabled": state.config.ops.alerts_enabled,
        "settings": settings,
        "telegram_linked": telegram_linked,
        "whatsapp_choices": choices,
        "recent": state.store.recent_alerts(user_id, 20)?,
        "thresholds": {
            "disk_percent": state.config.ops.disk_alert_percent,
            "memory_percent": state.config.ops.memory_alert_percent,
            "failovers_in_15_min": state.config.ops.failover_alert_count,
            "cooldown_mins": state.config.ops.alert_cooldown_mins,
        },
    }))
}

pub(super) async fn alerts_get(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    Ok(Json(alerts_view(&state, &user.id)?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AlertSettingsRequest {
    telegram: bool,
    #[serde(default)]
    whatsapp_conversation_id: Option<String>,
}

pub(super) async fn alerts_put(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<AlertSettingsRequest>,
) -> Result<Json<Value>> {
    let whatsapp = request
        .whatsapp_conversation_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());
    if let Some(id) = &whatsapp {
        validate_conversation_id(id)?;
    }
    state.store.set_alert_settings(
        &user.id,
        &AlertSettings {
            telegram: request.telegram,
            whatsapp_conversation_id: whatsapp,
        },
    )?;
    state.store.record_audit(
        &user.id,
        "alert.settings_changed",
        None,
        None,
        None,
        None,
        None,
        Some(&json!({"telegram": request.telegram})),
    )?;
    Ok(Json(alerts_view(&state, &user.id)?))
}

pub(super) async fn backups_get(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    require_admin(&user)?;
    let mut view = backup_view(&state.config, &state.store)?;
    view["running"] = json!(state.ops.is_some());
    view["in_progress"] = json!(
        state
            .ops
            .as_ref()
            .is_some_and(|ops| ops.backup_in_progress())
    );
    Ok(Json(view))
}

/// Start a backup now (owner only). It runs in the background; the console
/// polls `GET /v1/admin/backups` for the result.
pub(super) async fn backups_run(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    require_admin(&user)?;
    let ops = state
        .ops
        .clone()
        .ok_or_else(|| SeraError::Validation("backups are not running on this server".into()))?;
    if ops.backup_in_progress() {
        return Err(SeraError::InvalidState(
            "a backup is already running".into(),
        ));
    }
    tokio::spawn(async move {
        if let Err(error) = ops.run_backup("manual").await {
            tracing::warn!(%error, "manual backup did not run");
        }
    });
    let mut view = backup_view(&state.config, &state.store)?;
    view["running"] = json!(true);
    view["in_progress"] = json!(true);
    Ok(Json(view))
}
