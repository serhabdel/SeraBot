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
        "auto_send_files": state.store.auto_send_own(user_id)?,
        "weekly_digest": state.store.weekly_digest(user_id)?,
        "digest": {
            "enabled": state.config.ops.digest_enabled,
            "weekday": state.config.ops.digest_weekday,
            "hour_utc": state.config.ops.digest_hour_utc,
        },
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
    /// Send files to my own chats without asking (0.69).
    #[serde(default)]
    auto_send_files: Option<bool>,
    /// Weekly digest on or off (0.69).
    #[serde(default)]
    weekly_digest: Option<bool>,
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
    if let Some(on) = request.auto_send_files {
        state.store.set_auto_send_own(&user.id, on)?;
    }
    if let Some(on) = request.weekly_digest {
        state.store.set_weekly_digest(&user.id, on)?;
    }
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

// ---------------------------------------------------------------------------
// Costs (0.69)
// ---------------------------------------------------------------------------

fn cost_view(state: &ApiState, user_id: &str) -> Result<Value> {
    let now = Utc::now();
    let month = state
        .store
        .cost_report(Some(user_id), crate::costs::month_start(now))?;
    let budget = state.store.monthly_budget(user_id)?;
    Ok(json!({
        "month_usd": month.usd,
        "month_unpriced_tokens": month.unpriced_tokens,
        "month_lines": month.lines,
        "budget_usd": budget,
        "budget_level": budget.and_then(|budget| crate::costs::budget_level(month.usd, budget)),
        "prices": state.store.model_prices()?,
    }))
}

/// `GET /v1/usage/costs`: this month's estimated cost, the member's budget
/// and the owner's price table.
pub(super) async fn costs_get(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    Ok(Json(cost_view(&state, &user.id)?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BudgetRequest {
    /// USD per month; `null` clears the budget.
    monthly_usd: Option<f64>,
}

/// `PUT /v1/usage/budget`: set or clear the caller's monthly budget.
pub(super) async fn budget_put(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<BudgetRequest>,
) -> Result<Json<Value>> {
    state
        .store
        .set_monthly_budget(&user.id, request.monthly_usd)?;
    state.store.record_audit(
        &user.id,
        "usage.budget_changed",
        None,
        None,
        None,
        None,
        None,
        Some(&json!({"monthly_usd": request.monthly_usd})),
    )?;
    Ok(Json(cost_view(&state, &user.id)?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PricesRequest {
    prices: Vec<crate::costs::ModelPrice>,
}

/// `PUT /v1/admin/prices` (owner only): replace the price table.
pub(super) async fn prices_put(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<PricesRequest>,
) -> Result<Json<Value>> {
    require_admin(&user)?;
    state.store.set_model_prices(&request.prices)?;
    state.store.record_audit(
        &user.id,
        "usage.prices_changed",
        None,
        None,
        None,
        None,
        None,
        Some(&json!({"count": request.prices.len()})),
    )?;
    Ok(Json(cost_view(&state, &user.id)?))
}

/// `POST /v1/alerts/digest`: send the caller their weekly digest now, as a
/// preview. Returns the text too.
pub(super) async fn digest_now(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let owner = user.role == UserRole::Admin;
    let text = crate::ops::weekly_digest_text(&state.store, &user.id, owner, Utc::now())?;
    state.runtime.scan_owner_output(&user.id, &text)?;
    let delivered = match &state.ops {
        Some(ops) => {
            ops.send_digest_to(&user.id, &text).await?;
            true
        }
        None => false,
    };
    Ok(Json(json!({"text": text, "delivered": delivered})))
}
