use super::*;
use crate::connectors::ConnectorKind;

pub(super) async fn list_providers(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let catalog = state.runtime.model_catalog(&user.id)?;
    let llm = &state.config.llm;
    let mut settings: BTreeMap<String, ProviderConfig> = llm.providers.clone();
    settings.insert("default".to_owned(), llm.default_provider());
    let mut providers = catalog
        .into_iter()
        .map(|provider| -> Result<Value> {
            let managed = state
                .admin
                .as_ref()
                .is_some_and(|admin| admin.manages_provider(&provider.id));
            let config = settings.get(&provider.id);
            let routing_classes = llm
                .classes
                .iter()
                .filter(|(_, models)| {
                    models
                        .iter()
                        .any(|model| provider.models.iter().any(|advertised| advertised == model))
                })
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            let is_oauth =
                config.is_some_and(|c| {
                    c.auth.eq_ignore_ascii_case("oauth")
                        || matches!(c.kind.as_str(), "google-antigravity" | "muse-code")
                }) || matches!(provider.kind.as_str(), "google-antigravity" | "muse-code");
            let mut secrets = json!({
                "api_key": secret_status(&state.secrets, config.map(|c| c.api_key_secret.as_str())),
            });
            if is_oauth || config.is_some_and(|c| !c.refresh_token_secret.is_empty()) {
                secrets["refresh_token"] = secret_status(
                    &state.secrets,
                    config.map(|c| c.refresh_token_secret.as_str()),
                );
            }
            if is_oauth || config.is_some_and(|c| !c.oauth_client_id_secret.is_empty()) {
                secrets["oauth_client_id"] = secret_status(
                    &state.secrets,
                    config.map(|c| c.oauth_client_id_secret.as_str()),
                );
            }
            if is_oauth || config.is_some_and(|c| !c.oauth_client_secret_secret.is_empty()) {
                secrets["oauth_client_secret"] = secret_status(
                    &state.secrets,
                    config.map(|c| c.oauth_client_secret_secret.as_str()),
                );
            }
            Ok(json!({
                "id": provider.id,
                "kind": provider.kind,
                "auth": if is_oauth { "oauth" } else { "api_key" },
                "base_url": config.map(|c| c.base_url.as_str()).unwrap_or(""),
                "models": provider.models,
                "routing_classes": routing_classes,
                "data_egress_approved": provider.data_egress_approved,
                "catalog_fetch": provider.catalog_fetch,
                "catalog_verified": provider.catalog_verified,
                "managed": managed,
                "secrets": secrets,
                "credential": credential_view(&state, &user.id, &provider.id)?,
            }))
        })
        .collect::<Result<Vec<_>>>()?;

    if let Some(admin) = &state.admin {
        for (id, profile) in admin.snapshot().providers {
            if !providers
                .iter()
                .any(|p| p.get("id").and_then(Value::as_str) == Some(&id))
            {
                let is_oauth = profile.auth.eq_ignore_ascii_case("oauth")
                    || matches!(profile.kind.as_str(), "google-antigravity" | "muse-code");
                let mut secrets = json!({
                    "api_key": secret_status(&state.secrets, Some(profile.api_key_secret.as_str())),
                });
                if is_oauth || !profile.refresh_token_secret.is_empty() {
                    secrets["refresh_token"] =
                        secret_status(&state.secrets, Some(profile.refresh_token_secret.as_str()));
                }
                if is_oauth || !profile.oauth_client_id_secret.is_empty() {
                    secrets["oauth_client_id"] = secret_status(
                        &state.secrets,
                        Some(profile.oauth_client_id_secret.as_str()),
                    );
                }
                if is_oauth || !profile.oauth_client_secret_secret.is_empty() {
                    secrets["oauth_client_secret"] = secret_status(
                        &state.secrets,
                        Some(profile.oauth_client_secret_secret.as_str()),
                    );
                }
                let routing_classes = llm
                    .classes
                    .iter()
                    .filter(|(_, models)| {
                        models.iter().any(|model| {
                            profile.models.iter().any(|advertised| advertised == model)
                        })
                    })
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>();
                providers.push(json!({
                    "id": id,
                    "kind": profile.kind,
                    "auth": if is_oauth { "oauth" } else { "api_key" },
                    "base_url": profile.base_url,
                    "models": profile.models,
                    "routing_classes": routing_classes,
                    "data_egress_approved": profile.data_egress_approved,
                    "catalog_fetch": profile.catalog_fetch,
                    "catalog_verified": false,
                    "managed": true,
                    "secrets": secrets,
                    "credential": credential_view(&state, &user.id, &id)?,
                }));
            }
        }
    }

    Ok(Json(json!({"providers": providers})))
}

pub(super) async fn list_connectors(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let connectors = &state.config.connectors;
    let oauth: [(&str, &dyn OAuthConnectorSettings); 7] = [
        ("google_drive", &connectors.google_drive),
        ("google_sheets", &connectors.google_sheets),
        ("google_calendar", &connectors.google_calendar),
        ("gmail", &connectors.gmail),
        ("outlook", &connectors.outlook),
        ("outlook_calendar", &connectors.outlook_calendar),
        ("notion", &connectors.notion),
    ];
    let mut views = oauth
        .into_iter()
        .map(|(id, connector)| oauth_connector_view(&state, id, connector))
        .collect::<Vec<_>>();
    // Per-user products report this account's own state; "not set up" carries
    // an admin hint instead of a sign-in action.
    for view in &mut views {
        let Some(kind) = view["id"].as_str().and_then(ConnectorKind::from_section) else {
            continue;
        };
        if let Some(personal) = state.personal.view(&user.id, kind)? {
            view["connected"] = json!(personal.state == "connected");
            view["personal"] = personal.to_json();
        }
    }
    let agentmail = &connectors.agentmail;
    let api_base = agentmail.api_base.trim();
    let inbox_id = agentmail.default_inbox_id.trim();
    views.push(json!({
        "id": "agentmail",
        "kind": "api_key",
        "enabled": agentmail.enabled,
        "redirect_uri": Value::Null,
        "api_base": if api_base.is_empty() { Value::Null } else { json!(api_base) },
        "default_inbox_id": if inbox_id.is_empty() { Value::Null } else { json!(inbox_id) },
        "secrets": {
            "api_key": secret_status(&state.secrets, Some(agentmail.api_key_secret.as_str())),
        },
    }));
    for (id, enabled, api_base, key_secret) in [
        (
            "tavily",
            state.config.tavily.enabled,
            state.config.tavily.api_base.as_str(),
            state.config.tavily.api_key_secret.as_str(),
        ),
        (
            "firecrawl",
            state.config.firecrawl.enabled,
            state.config.firecrawl.api_base.as_str(),
            state.config.firecrawl.api_key_secret.as_str(),
        ),
        (
            "elevenlabs",
            state.config.elevenlabs.enabled,
            state.config.elevenlabs.api_base.as_str(),
            state.config.elevenlabs.api_key_secret.as_str(),
        ),
    ] {
        views.push(json!({
            "id": id,
            "kind": "api_key",
            "enabled": enabled,
            "redirect_uri": Value::Null,
            "api_base": api_base,
            "secrets": {
                "api_key": secret_status(&state.secrets, Some(key_secret)),
            },
        }));
        if id == "elevenlabs" {
            let view = views.last_mut().expect("just pushed");
            view["auth"] = json!(match state.config.elevenlabs.auth {
                crate::config::ElevenLabsAuth::ApiKey => "api_key",
                crate::config::ElevenLabsAuth::Oauth => "oauth",
            });
            view["auth_modes"] = json!(["api_key", "oauth"]);
            view["secrets"]["access_token"] = secret_status(
                &state.secrets,
                Some(state.config.elevenlabs.oauth_access_token_secret.as_str()),
            );
            view["secrets"]["refresh_token"] = secret_status(
                &state.secrets,
                Some(state.config.elevenlabs.oauth_refresh_token_secret.as_str()),
            );
            view["oauth"] = json!({
                "client_id": state.config.elevenlabs.oauth_client_id,
                "authorize_url": crate::elevenlabs::ELEVENLABS_AUTHORIZE_URL,
                "cimd_path": crate::elevenlabs::CIMD_PATH,
                "callback_uri": crate::elevenlabs::oauth_callback_uri(),
                "scope": state.config.elevenlabs.oauth_scope,
            });
        }
    }
    // Composio: a per-user key, so the card reports only this account's state.
    views.push(json!({
        "id": "composio",
        "kind": "api_key_user",
        "enabled": true,
        "redirect_uri": Value::Null,
        "connected": state.composio.connected(&user.id)?,
        "key_kind": state.composio.key_kind(&user.id)?.map(crate::composio::KeyKind::as_str),
        "manage_url": crate::composio::CONNECT_URL,
        "help_url": crate::composio::HELP_URL,
    }));
    // Per-user token connectors (GitHub, Todoist, Slack, Exa): this account's
    // own state only. The token itself never appears.
    for id in crate::token_connectors::TokenConnectors::ids() {
        let personal = state.token_connectors.view(&user.id, id)?;
        views.push(json!({
            "id": id,
            "kind": "token",
            "enabled": true,
            "redirect_uri": Value::Null,
            "connected": personal.state == "connected",
            "personal": personal.to_json(),
        }));
    }
    for view in &mut views {
        let group = view["id"].as_str().map(connector_group).unwrap_or("Other");
        view["group"] = json!(group);
    }
    // Per-user OneDrive status (phase 4). The token never appears: only
    // whether the acting user has connected their own drive.
    if state.config.onedrive.enabled {
        let connected = state
            .onedrive
            .as_ref()
            .map(|registry| registry.connected(&user.id))
            .transpose()?
            .unwrap_or(false);
        views.insert(
            0,
            json!({
                "id": "onedrive",
                "kind": "oauth",
                "enabled": true,
                "group": "Productivity",
                "connected": connected,
                "redirect_uri": state
                    .config
                    .onedrive
                    .effective_redirect_base(&state.config.auth)
                    .trim()
                    .trim_end_matches('/')
                    .to_owned()
                    + crate::onedrive::CONSENT_CALLBACK_PATH,
                "secrets": {
                    "refresh_token": {
                        // Per-user sealed credential (user_secrets), not an
                        // age-store name; kept to the secret-name charset the
                        // clients validate.
                        "name": "user_onedrive_refresh_token",
                        "configured": connected,
                    },
                },
            }),
        );
    }
    Ok(Json(json!({"connectors": views})))
}

// ---- Per-user credentials, OneDrive consent, Telegram linking (phase 4) ----

/// The configured profile for a provider id (`default` is `[llm]` itself).
pub(super) fn provider_profile(config: &Config, id: &str) -> Result<ProviderConfig> {
    validate_provider(id)?;
    if id == "default" {
        return Ok(config.llm.default_provider());
    }
    config
        .llm
        .providers
        .get(id)
        .cloned()
        .ok_or_else(|| SeraError::NotFound(format!("provider {id}")))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SetProviderKeyRequest {
    api_key: String,
}

/// Store the acting user's API key for one configured provider.
///
/// The key is sealed under the user's DEK; only its presence is ever
/// reported. Setting a key replaces any previous credential for that
/// provider (including an OAuth token pair), which is the explicit way to
/// switch a provider to key auth.
pub(super) async fn set_provider_key(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<SetProviderKeyRequest>,
) -> Result<Json<Value>> {
    provider_profile(&state.config, &id)?;
    let credential = ProviderCredential {
        api_key: request.api_key.trim().to_owned(),
        refresh_token: String::new(),
    };
    state.users.set(&user.id, &id, &credential.encode()?)?;
    state.runtime.invalidate_model_credentials(Some(&user.id));
    audit_auth(
        &state,
        &user.id,
        "provider.credential_set",
        json!({"provider": id}),
    )?;
    Ok(Json(json!({"id": id, "configured": true})))
}

/// Delete the acting user's own credential for a provider. A grant the user
/// holds is untouched (and reported back).
pub(super) async fn delete_provider_credential(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    validate_provider(&id)?;
    let deleted = state.users.delete(&user.id, &id)?;
    state.runtime.invalidate_model_credentials(Some(&user.id));
    let status = state.users.credential_status(&user.id, &id)?;
    audit_auth(
        &state,
        &user.id,
        "provider.credential_deleted",
        json!({"provider": id, "deleted": deleted}),
    )?;
    Ok(Json(json!({
        "id": id,
        "deleted": deleted,
        "configured": status.usable(),
        "granted": status.granted,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GrantRequest {
    grantee: String,
    provider: String,
}

pub(super) fn grant_view(grant: &crate::store::ProviderGrant) -> Value {
    json!({
        "grantee": grant.grantee,
        "provider": grant.provider,
        "created_at": grant.created_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        "revoked_at": grant
            .revoked_at
            .map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true)),
        "active": grant.active(),
    })
}

/// Grant a member access to an admin-owned provider (admin only).
///
/// The grant is a permission, never a secret: resolution reads the admin's
/// credential at call time and the member never learns the value. Audited and
/// revocable.
pub(super) async fn grant_provider(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<GrantRequest>,
) -> Result<Json<Value>> {
    require_admin(&user)?;
    crate::util::validate_id(&request.grantee)?;
    provider_profile(&state.config, &request.provider)?;
    let grant = state
        .users
        .grant(&user.id, &request.grantee, &request.provider)?;
    state.runtime.invalidate_model_credentials(None);
    audit_auth(
        &state,
        &user.id,
        "provider.granted",
        json!({"grantee": grant.grantee, "provider": grant.provider}),
    )?;
    Ok(Json(json!({"grant": grant_view(&grant)})))
}

/// Revoke one grant (admin only). Revocation takes effect immediately: cached
/// per-user clients are dropped before the response is written.
pub(super) async fn revoke_grant_endpoint(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path((grantee, provider)): Path<(String, String)>,
) -> Result<Json<Value>> {
    require_admin(&user)?;
    crate::util::validate_id(&grantee)?;
    validate_provider(&provider)?;
    let revoked = state.users.revoke_grant(&user.id, &grantee, &provider)?;
    state.runtime.invalidate_model_credentials(None);
    audit_auth(
        &state,
        &user.id,
        "provider.grant_revoked",
        json!({"grantee": grantee, "provider": provider, "revoked": revoked}),
    )?;
    Ok(Json(json!({
        "grantee": grantee,
        "provider": provider,
        "revoked": revoked,
    })))
}

/// Every grant the acting admin has made, revoked ones included. Admin only;
/// carries no secret.
pub(super) async fn list_grants(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    require_admin(&user)?;
    let grants = state.store.list_provider_grants(&user.id)?;
    let grants: Vec<Value> = grants.iter().map(grant_view).collect();
    Ok(Json(json!({"grants": grants})))
}

/// Start the OneDrive consent flow for the acting user: auth code + PKCE
/// against the shared app registration.
pub(super) async fn start_onedrive_consent(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let consent = state
        .onedrive_consent
        .as_ref()
        .ok_or_else(|| SeraError::Forbidden("OneDrive is not enabled on this deployment".into()))?;
    let url = consent.start(&user.id).await?;
    audit_auth(
        &state,
        &user.id,
        "connector.onedrive_consent_started",
        json!({}),
    )?;
    Ok(Json(json!({"url": url})))
}

#[derive(Debug, Deserialize)]
pub(super) struct ConsentCallbackParams {
    state: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// The OneDrive consent redirect target. Public (the browser arrives from
/// Microsoft); the `state` value binds the callback to the user who started
/// it. The refresh token is sealed before the success page renders and never
/// appears in the response.
pub(super) async fn onedrive_callback(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<ConsentCallbackParams>,
) -> Response {
    // Outlook consents share OneDrive's registered redirect; the state value
    // says which flow started the round trip.
    if state.personal.owns_state(&params.state).await {
        return personal_callback_inner(&state, params).await;
    }
    let Some(consent) = state.onedrive_consent.as_ref() else {
        return auth_error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "OneDrive is not enabled on this deployment.",
        );
    };
    if params.error.is_some() {
        return auth_error_page(
            StatusCode::BAD_REQUEST,
            "OneDrive consent was not granted. Nothing was connected.",
        );
    }
    let Some(code) = params.code.as_deref() else {
        return auth_error_page(
            StatusCode::BAD_REQUEST,
            "The OneDrive consent response was incomplete. Start again from the console.",
        );
    };
    match consent.complete(&params.state, code).await {
        Ok(user_id) => {
            if let Some(registry) = &state.onedrive {
                registry.invalidate(&user_id);
            }
            if let Err(error) =
                audit_auth(&state, &user_id, "connector.onedrive_connected", json!({}))
            {
                warn!(%error, "onedrive connect audit write failed");
            }
            html_response(
                StatusCode::OK,
                auth_page(
                    "OneDrive connected",
                    "<p class=\"lede\">Your OneDrive is connected. Uploads from your bots land in your own drive under the configured folder.</p>\n<p class=\"meta\">You can close this page and return to the client.</p>",
                ),
            )
        }
        Err(error) => {
            warn!(%error, "onedrive consent callback failed");
            auth_error_page(
                StatusCode::BAD_REQUEST,
                "OneDrive could not be connected. The link may have expired or been used; start again from the console.",
            )
        }
    }
}

/// Disconnect the acting user's OneDrive: delete the sealed refresh token and
/// drop the cached client.
pub(super) async fn disconnect_onedrive(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let deleted = state.users.delete(&user.id, ONEDRIVE_PROVIDER)?;
    if let Some(registry) = &state.onedrive {
        registry.invalidate(&user.id);
    }
    audit_auth(
        &state,
        &user.id,
        "connector.onedrive_disconnected",
        json!({"was_connected": deleted}),
    )?;
    Ok(Json(json!({
        "id": "onedrive",
        "connected": false,
        "was_connected": deleted,
    })))
}

/// The per-user connectors a console id can name.
fn personal_kind(id: &str) -> Result<ConnectorKind> {
    ConnectorKind::from_section(id).ok_or_else(|| SeraError::NotFound(format!("connector {id}")))
}

/// Start a per-user consent (Outlook, Google, Notion OAuth). While the
/// provider is not set up the answer is a typed `not_set_up` conflict that
/// carries the admin hint; no provider page is ever opened for it.
pub(super) async fn start_personal_consent(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Response> {
    let kind = personal_kind(&id)?;
    if let Err(hint) = state.personal.consent_ready(kind) {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({"error": "not set up yet", "code": "not_set_up", "hint": hint})),
        )
            .into_response());
    }
    let url = state.personal.start(&user.id, kind).await?;
    audit_auth(
        &state,
        &user.id,
        "connector.consent_started",
        json!({"connector": id}),
    )?;
    Ok(Json(json!({"url": url})).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PersonalTokenRequest {
    token: String,
}

/// Connect Notion, GitHub, Todoist, Slack or Exa with a pasted token (write-only: the
/// token is verified, sealed for this user, and never returned).
pub(super) async fn set_personal_token(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<PersonalTokenRequest>,
) -> Result<Json<Value>> {
    if crate::token_connectors::TokenConnectors::handles(&id) {
        let account = state
            .token_connectors
            .connect(&user.id, &id, &request.token)
            .await?;
        audit_auth(
            &state,
            &user.id,
            "connector.token_connected",
            json!({"connector": id}),
        )?;
        return Ok(Json(
            json!({"id": id, "connected": true, "account": account}),
        ));
    }
    if personal_kind(&id)? != ConnectorKind::Notion {
        return Err(SeraError::NotFound(format!("connector {id}")));
    }
    let account = state
        .personal
        .connect_notion_token(&user.id, &request.token)
        .await?;
    audit_auth(
        &state,
        &user.id,
        "connector.token_connected",
        json!({"connector": id}),
    )?;
    Ok(Json(
        json!({"id": id, "connected": true, "account": account}),
    ))
}

/// Disconnect the acting user's own connection to one product.
pub(super) async fn disconnect_personal(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let was_connected = if crate::token_connectors::TokenConnectors::handles(&id) {
        state.token_connectors.disconnect(&user.id, &id)?
    } else {
        let kind = personal_kind(&id)?;
        state.personal.disconnect(&user.id, kind).await?
    };
    audit_auth(
        &state,
        &user.id,
        "connector.disconnected",
        json!({"connector": id, "was_connected": was_connected}),
    )?;
    Ok(Json(json!({
        "id": id,
        "connected": false,
        "was_connected": was_connected,
    })))
}

/// Google and Notion consent redirect (Outlook shares OneDrive's route).
pub(super) async fn personal_callback(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<ConsentCallbackParams>,
) -> Response {
    personal_callback_inner(&state, params).await
}

async fn personal_callback_inner(state: &ApiState, params: ConsentCallbackParams) -> Response {
    if params.error.is_some() {
        // Drop the pending state so a declined consent cannot be replayed.
        let _ = state.personal.take_state(&params.state).await;
        return auth_error_page(
            StatusCode::BAD_REQUEST,
            "The connection was not approved. Nothing was connected.",
        );
    }
    let Some(code) = params.code.as_deref() else {
        return auth_error_page(
            StatusCode::BAD_REQUEST,
            "The response was incomplete. Start again from the console.",
        );
    };
    match state.personal.complete(&params.state, code).await {
        Ok((user_id, kind)) => {
            if let Err(error) = audit_auth(
                state,
                &user_id,
                "connector.connected",
                json!({"connector": kind.section()}),
            ) {
                warn!(%error, "connector connect audit write failed");
            }
            html_response(
                StatusCode::OK,
                auth_page(
                    "Connected",
                    "<p class=\"lede\">Your account is connected. Your bots can use it after you approve each use.</p>\n<p class=\"meta\">You can close this page and return to the client.</p>",
                ),
            )
        }
        Err(error) => {
            warn!(%error, "connector consent callback failed");
            auth_error_page(
                StatusCode::BAD_REQUEST,
                "The account could not be connected. The link may have expired or been used; start again from the console.",
            )
        }
    }
}

/// Telegram linking lifetime: long enough to switch apps, short enough that a
/// leaked code is useless.
pub(super) const TELEGRAM_LINK_TTL_MINUTES: i64 = 10;

/// The acting user's Telegram binding status. The chat ids are the user's own
/// and are not secrets; the bot token never appears.
pub(super) async fn telegram_status(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let binding = state.store.telegram_binding_for_user(&user.id)?;
    Ok(Json(json!({
        "enabled": state.config.telegram.enabled,
        "linked": binding.is_some(),
        "telegram_user_id": binding.as_ref().map(|binding| binding.telegram_user_id),
        "telegram_chat_id": binding.as_ref().map(|binding| binding.telegram_chat_id),
        "linked_at": binding
            .as_ref()
            .map(|binding| binding.linked_at.to_rfc3339_opts(SecondsFormat::Secs, true)),
    })))
}

/// Mint a short-lived, single-use linking code. The user sends
/// `/start <code>` to the bot; the polling loop consumes the code and records
/// the binding. Only the code's hash is stored.
pub(super) async fn start_telegram_link(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    if !state.config.telegram.enabled {
        return Err(SeraError::Forbidden(
            "Telegram is not enabled on this deployment".into(),
        ));
    }
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let code = hex::encode(bytes);
    let expires_at = Utc::now() + chrono::Duration::minutes(TELEGRAM_LINK_TTL_MINUTES);
    state
        .store
        .put_telegram_link_code(&sha256_hex(&code), &user.id, expires_at)?;
    audit_auth(
        &state,
        &user.id,
        "telegram.link_code_issued",
        json!({"expires_at": expires_at.to_rfc3339_opts(SecondsFormat::Secs, true)}),
    )?;
    Ok(Json(json!({
        "code": code,
        "expires_at": expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        "instructions": "Send /start <code> to the SeraBot Telegram bot.",
    })))
}

/// Unbind the acting user's Telegram chat. Later messages from that chat are
/// ignored exactly like any other unbound chat.
pub(super) async fn unlink_telegram(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let was_linked = state.store.unlink_telegram(&user.id)?;
    audit_auth(
        &state,
        &user.id,
        "telegram.unlinked",
        json!({"was_linked": was_linked}),
    )?;
    Ok(Json(json!({"linked": false, "was_linked": was_linked})))
}

/// Console grouping only; it carries no security meaning.
pub(super) fn connector_group(id: &str) -> &'static str {
    match id {
        "elevenlabs" => "Voice & Media",
        "tavily" | "firecrawl" => "Web Research",
        "agentmail" => "Email",
        "composio" => "Apps & tools",
        "github" | "todoist" => "Code and tasks",
        "slack" => "Messages",
        "exa" => "Web Research",
        _ => "Productivity",
    }
}

pub(super) fn api_key_probe<'a>(config: &'a Config, id: &str) -> Option<ApiKeyProbe<'a>> {
    match id {
        "agentmail" => Some(ApiKeyProbe::AgentMail(
            &config.connectors.agentmail.api_base,
        )),
        "tavily" => Some(ApiKeyProbe::Tavily(&config.tavily.api_base)),
        "firecrawl" => Some(ApiKeyProbe::Firecrawl(&config.firecrawl.api_base)),
        "elevenlabs" => Some(match config.elevenlabs.auth {
            crate::config::ElevenLabsAuth::ApiKey => {
                ApiKeyProbe::ElevenLabs(&config.elevenlabs.api_base)
            }
            crate::config::ElevenLabsAuth::Oauth => {
                ApiKeyProbe::ElevenLabsOauth(&config.elevenlabs.api_base)
            }
        }),
        _ => None,
    }
}

/// Owner "test connection". Makes at most one read-only, time-bounded request
/// for API-key connectors. OAuth connectors only report whether their secrets
/// are stored. Messages are fixed strings; no secret or provider body leaves.
pub(super) async fn admin_test_connector(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
    let roles = connector_secret_roles(&state.config, &id).map_err(admin_error)?;
    let checked_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let missing = roles.iter().find(|(_, name)| !state.secrets.has(name));
    let (ok, message) = match (missing, api_key_probe(&state.config, &id)) {
        (Some((_, name)), _) => (false, format!("Missing secret: {name}")),
        (None, Some(probe)) => {
            let key = roles
                .iter()
                .find(|(role, _)| *role == "api_key" || *role == "access_token")
                .and_then(|(_, name)| state.secrets.get(name))
                .unwrap_or_default();
            match probe_api_key(probe, &key).await {
                Ok(()) => (true, "Key accepted".to_owned()),
                Err(message) => (false, message),
            }
        }
        (None, None) => (
            true,
            "Client ID, client secret and refresh token are stored. The OAuth token exchange was not run.".to_owned(),
        ),
    };
    audit_config(
        &state,
        &user.id,
        "config.connector_tested",
        json!({"connector": id, "ok": ok}),
    )?;
    Ok(Json(json!({
        "id": id,
        "ok": ok,
        "message": message,
        "checked_at": checked_at,
    })))
}

pub(super) fn oauth_connector_view(
    state: &ApiState,
    id: &str,
    connector: &dyn OAuthConnectorSettings,
) -> Value {
    let redirect = connector.redirect_uri().trim();
    json!({
        "id": id,
        "kind": "oauth",
        "enabled": connector.oauth_enabled(),
        "redirect_uri": if redirect.is_empty() { Value::Null } else { json!(redirect) },
        "secrets": {
            "client_id": secret_status(&state.secrets, Some(connector.client_id_secret_name())),
            "client_secret": secret_status(&state.secrets, Some(connector.client_secret_secret_name())),
            "refresh_token": secret_status(&state.secrets, Some(connector.refresh_token_secret_name())),
        },
    })
}
