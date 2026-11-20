//! `GET /v1/bots/health?days=7|30` (0.70): per-bot health for the acting member.

use super::*;

#[derive(Debug, Deserialize)]
pub(super) struct BotHealthQuery {
    #[serde(default)]
    days: Option<i64>,
}

/// Success rate, turn times, failing tools and failovers for each of the
/// member's own bots. The admin sees their own bots too; nobody reads
/// another member's rows. Counts and tool names only.
pub(super) async fn bot_health_report(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<BotHealthQuery>,
) -> Result<Json<Value>> {
    let days = match query.days.unwrap_or(7) {
        7 => 7,
        30 => 30,
        _ => return Err(SeraError::Validation("days must be 7 or 30".into())),
    };
    let since = Utc::now() - chrono::Duration::days(days);
    let bots = state.store.bot_health(&user.id, since)?;
    let attention = bots.iter().filter(|bot| bot.needs_attention).count();
    let body = json!({"days": days, "bots": bots, "needs_attention": attention});
    state
        .runtime
        .scan_owner_output(&user.id, &body.to_string())?;
    Ok(Json(body))
}
