//! Owner API for the per-user vault. Values are write-only: no response
//! ever carries one.

use zeroize::Zeroizing;

use super::*;
use crate::vault::{self, VaultInput, VaultKind};

pub(super) async fn list_vault(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let entries = vault::list(&state.users, &user.id)?;
    Ok(Json(json!({"entries": entries})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PutVaultRequest {
    kind: VaultKind,
    #[serde(default)]
    username: Option<String>,
    value: String,
    #[serde(default)]
    domains: Vec<String>,
    #[serde(default)]
    note: Option<String>,
}

// The value must never reach a log line through `{:?}`.
impl std::fmt::Debug for PutVaultRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PutVaultRequest")
            .field("kind", &self.kind)
            .field("value", &"<redacted>")
            .finish_non_exhaustive()
    }
}

pub(super) async fn put_vault(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(name): Path<String>,
    Json(request): Json<PutVaultRequest>,
) -> Result<Json<Value>> {
    let input = VaultInput {
        kind: request.kind,
        username: request.username,
        value: Zeroizing::new(request.value),
        domains: request.domains,
        note: request.note,
    };
    let entry = vault::put(&state.users, &user.id, &name, &input)?;
    Ok(Json(json!(entry)))
}

pub(super) async fn delete_vault(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    if !vault::delete(&state.users, &user.id, &name)? {
        return Err(SeraError::NotFound(format!("vault entry {name}")));
    }
    Ok(Json(json!({"deleted": true})))
}
