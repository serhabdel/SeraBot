//! Scheduled routines visibility (0.29.0, Wave A1): `GET /v1/routines` lists
//! the acting user's durable scheduler rows.

use super::*;

/// `GET /v1/routines`: name, enabled, next run, last run date and config.
pub(super) async fn list_routines(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let routines = state.store.list_routines(&user.id)?;
    Ok(Json(json!({"routines": routines})))
}
