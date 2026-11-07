//! Per-user Composio routes: key, linked apps, app search, hosted link.
//! The key is write-only; every call acts on the acting user's own account.

use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ComposioKeyRequest {
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ComposioConnectRequest {
    toolkit: String,
}

#[derive(Deserialize)]
pub(super) struct ComposioSearchParams {
    #[serde(default)]
    q: String,
}

pub(super) async fn set_composio_key(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<ComposioKeyRequest>,
) -> Result<Json<Value>> {
    state.composio.connect_key(&user.id, &request.key).await?;
    audit_auth(
        &state,
        &user.id,
        "connector.token_connected",
        json!({"connector": "composio"}),
    )?;
    Ok(Json(json!({"id": "composio", "connected": true})))
}

pub(super) async fn disconnect_composio(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let was_connected = state.composio.disconnect_key(&user.id)?;
    audit_auth(
        &state,
        &user.id,
        "connector.disconnected",
        json!({"connector": "composio", "was_connected": was_connected}),
    )?;
    Ok(Json(
        json!({"id": "composio", "connected": false, "was_connected": was_connected}),
    ))
}

pub(super) async fn composio_accounts(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    Ok(Json(state.composio.accounts(&user.id).await?))
}

pub(super) async fn composio_toolkits(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(params): Query<ComposioSearchParams>,
) -> Result<Json<Value>> {
    Ok(Json(
        state.composio.search_toolkits(&user.id, &params.q).await?,
    ))
}

pub(super) async fn composio_connect(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<ComposioConnectRequest>,
) -> Result<Json<Value>> {
    let link = state
        .composio
        .connect_link(&user.id, &request.toolkit)
        .await?;
    audit_auth(
        &state,
        &user.id,
        "connector.consent_started",
        json!({"connector": "composio", "toolkit": link["toolkit"]}),
    )?;
    Ok(Json(link))
}

pub(super) async fn composio_disconnect_account(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(account): Path<String>,
) -> Result<Json<Value>> {
    state
        .composio
        .disconnect_account(&user.id, &account)
        .await?;
    audit_auth(
        &state,
        &user.id,
        "connector.disconnected",
        json!({"connector": "composio_app"}),
    )?;
    Ok(Json(json!({"id": account, "removed": true})))
}
