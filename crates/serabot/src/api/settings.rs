//! Owner settings for per-turn work limits (0.24): how many tool calls and
//! model rounds one bot turn may use. The values are per user. The stored
//! override is validated against the server ceiling, and each change is
//! audited with the value it replaced.

use super::*;
use crate::turn_limits::{TurnLimits, config_defaults, turn_limits_view, validate_turn_limits};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TurnLimitsRequest {
    max_tool_calls_per_turn: usize,
    max_tool_rounds: usize,
}

/// `GET /v1/settings/turn-limits`: the limits that apply to the acting user's
/// turns, with the config default and the server ceiling.
pub(super) async fn get_turn_limits(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let owner = state.store.get_user_turn_limits(&user.id)?;
    Ok(Json(turn_limits_view(
        config_defaults(&state.config),
        owner,
    )))
}

/// `PUT /v1/settings/turn-limits`: `{"max_tool_calls_per_turn": n,
/// "max_tool_rounds": n}`. Each value must be within the server ceiling.
pub(super) async fn put_turn_limits(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<TurnLimitsRequest>,
) -> Result<Json<Value>> {
    let limits = validate_turn_limits(TurnLimits {
        max_tool_calls_per_turn: request.max_tool_calls_per_turn,
        max_tool_rounds: request.max_tool_rounds,
    })?;
    let before = state.store.get_user_turn_limits(&user.id)?;
    state.store.set_user_turn_limits(&user.id, limits)?;
    state.store.record_audit(
        &user.id,
        "settings.turn_limits_updated",
        None,
        None,
        None,
        None,
        None,
        Some(&json!({"before": before, "after": limits})),
    )?;
    Ok(Json(turn_limits_view(
        config_defaults(&state.config),
        Some(limits),
    )))
}

/// `DELETE /v1/settings/turn-limits`: drop the acting user's override, so the
/// config defaults apply again.
pub(super) async fn reset_turn_limits(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let before = state.store.get_user_turn_limits(&user.id)?;
    state.store.clear_user_turn_limits(&user.id)?;
    state.store.record_audit(
        &user.id,
        "settings.turn_limits_reset",
        None,
        None,
        None,
        None,
        None,
        Some(&json!({"before": before})),
    )?;
    Ok(Json(turn_limits_view(config_defaults(&state.config), None)))
}
