//! Owner API for the per-user vault. Values are write-only: no response
//! ever carries one. Password manager tokens are write-only too.

use zeroize::Zeroizing;

use super::*;
use crate::vault::{self, VaultInput, VaultKind, VaultSource};
use crate::vault_providers::{self, Provider};

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
    /// The value for a local entry. Empty for a password manager reference.
    #[serde(default)]
    value: String,
    #[serde(default)]
    domains: Vec<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    source: VaultSource,
    /// The item pointer for a password manager entry.
    #[serde(default)]
    reference: Option<String>,
}

// The value must never reach a log line through `{:?}`.
impl std::fmt::Debug for PutVaultRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PutVaultRequest")
            .field("kind", &self.kind)
            .field("source", &self.source)
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
        source: request.source,
        reference: request.reference,
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

fn password_manager(id: &str) -> Result<Provider> {
    Provider::from_id(id).ok_or_else(|| SeraError::NotFound(format!("password manager {id}")))
}

/// `GET /v1/vault/providers`: each password manager, whether the owner has a
/// token saved, and whether its CLI is installed on the server. Never a token.
pub(super) async fn list_vault_providers(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let providers = Provider::ALL
        .into_iter()
        .map(|provider| {
            vault_providers::status(&state.users, &state.vault_providers, &user.id, provider)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Json(json!({"providers": providers})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderTokenRequest {
    token: String,
}

// The token must never reach a log line through `{:?}`.
impl std::fmt::Debug for ProviderTokenRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderTokenRequest")
            .field("token", &"<redacted>")
            .finish()
    }
}

/// `PUT /v1/vault/providers/{id}`: save the owner's access token for a
/// password manager. The token is sealed; the response is the status only.
pub(super) async fn put_vault_provider(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<ProviderTokenRequest>,
) -> Result<Json<Value>> {
    let provider = password_manager(&id)?;
    let token = Zeroizing::new(request.token.trim().to_owned());
    vault_providers::set_token(&state.users, &user.id, provider, &token)?;
    Ok(Json(json!(vault_providers::status(
        &state.users,
        &state.vault_providers,
        &user.id,
        provider
    )?)))
}

/// `DELETE /v1/vault/providers/{id}`: forget the owner's token for a password
/// manager. References to it stop filling until a new token is saved.
pub(super) async fn delete_vault_provider(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let provider = password_manager(&id)?;
    if !vault_providers::clear_token(&state.users, &user.id, provider)? {
        return Err(SeraError::NotFound(format!(
            "{} access token",
            provider.label()
        )));
    }
    Ok(Json(json!({"deleted": true})))
}

/// `POST /v1/vault/providers/{id}/test`: exit-code style probe; never a token
/// or CLI stdout.
pub(super) async fn test_vault_provider(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let provider = password_manager(&id)?;
    let report = vault_providers::probe(
        &state.users,
        &state.vault_providers,
        &user.id,
        provider,
    )
    .await?;
    Ok(Json(json!(report)))
}
