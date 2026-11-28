use super::*;
use crate::connectors::ConnectorKind;

/// Server-level secret status: admins see the age secret name alongside
/// presence; non-admins receive `name: null` while preserving `configured`
/// so UI capability checks still reflect whether a server connector is active.
fn visible_secret_status(secrets: &SharedSecrets, name: Option<&str>, is_admin: bool) -> Value {
    if is_admin {
        return secret_status(secrets, name);
    }
    let configured = name
        .filter(|name| !name.is_empty())
        .is_some_and(|name| secrets.has(name));
    json!({"name": Value::Null, "configured": configured})
}

pub(super) async fn list_providers(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let is_admin = user.is_admin();
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
            let kind = crate::providers::canonical_kind(&provider.kind)?;
            let signin = signin_flows(&state, is_admin, &provider.id, &kind);
            let risk = signin_risk(&kind, &signin);
            let has_oauth_signin = !signin.is_empty();
            let is_oauth =
                config.is_some_and(|c| {
                    c.auth.eq_ignore_ascii_case("oauth")
                        || matches!(
                            c.kind.as_str(),
                            "google-antigravity" | "muse-code" | "openai-codex" | "github-copilot"
                        )
                }) || matches!(
                    provider.kind.as_str(),
                    "google-antigravity" | "muse-code" | "openai-codex" | "github-copilot"
                );
            let mut secrets = json!({
                "api_key": visible_secret_status(&state.secrets, config.map(|c| c.api_key_secret.as_str()), is_admin),
            });
            if is_oauth
                || has_oauth_signin
                || config.is_some_and(|c| !c.refresh_token_secret.is_empty())
            {
                secrets["refresh_token"] = visible_secret_status(
                    &state.secrets,
                    config.map(|c| c.refresh_token_secret.as_str()),
                    is_admin,
                );
            }
            if is_oauth
                || has_oauth_signin
                || config.is_some_and(|c| !c.oauth_client_id_secret.is_empty())
            {
                secrets["oauth_client_id"] = visible_secret_status(
                    &state.secrets,
                    config.map(|c| c.oauth_client_id_secret.as_str()),
                    is_admin,
                );
            }
            if is_oauth
                || has_oauth_signin
                || config.is_some_and(|c| !c.oauth_client_secret_secret.is_empty())
            {
                secrets["oauth_client_secret"] = visible_secret_status(
                    &state.secrets,
                    config.map(|c| c.oauth_client_secret_secret.as_str()),
                    is_admin,
                );
            }
            let mut view = json!({
                "id": provider.id,
                "kind": provider.kind,
                "auth": if is_oauth { "oauth" } else { "api_key" },
                "base_url": config.map(|c| c.base_url.as_str()).unwrap_or(""),
                "models": provider.models,
                "routing_classes": routing_classes,
                "data_egress_approved": provider.data_egress_approved,
                "catalog_fetch": provider.catalog_fetch,
                "catalog_verified": provider.catalog_verified,
                "model_source": provider.model_source,
                "status": provider.status,
                "health": provider.health,
                "last_error": provider.last_error,
                "last_checked": provider.last_checked,
                "model_count": provider.models.len(),
                "managed": managed,
                "secrets": secrets,
                "credential": credential_view(&state, &user.id, &provider.id)?,
                "accounts": provider_account_views(&state, &user.id, &provider.id)?,
                "signin": signin,
                "signin_risk": risk,
            });
            add_signin_status(&mut view, &state, is_admin, &provider.id, &kind);
            Ok(view)
        })
        .collect::<Result<Vec<_>>>()?;

    if let Some(admin) = &state.admin {
        for (id, profile) in admin.snapshot().providers {
            if !providers
                .iter()
                .any(|p| p.get("id").and_then(Value::as_str) == Some(&id))
            {
                let kind = crate::providers::canonical_kind(&profile.kind)?;
                let signin = signin_flows(&state, is_admin, &id, &kind);
                let risk = signin_risk(&kind, &signin);
                let has_oauth_signin = !signin.is_empty();
                let is_oauth = profile.auth.eq_ignore_ascii_case("oauth")
                    || matches!(
                        profile.kind.as_str(),
                        "google-antigravity" | "muse-code" | "openai-codex" | "github-copilot"
                    );
                let mut secrets = json!({
                    "api_key": visible_secret_status(&state.secrets, Some(profile.api_key_secret.as_str()), is_admin),
                });
                if is_oauth || has_oauth_signin || !profile.refresh_token_secret.is_empty() {
                    secrets["refresh_token"] = visible_secret_status(
                        &state.secrets,
                        Some(profile.refresh_token_secret.as_str()),
                        is_admin,
                    );
                }
                if is_oauth || has_oauth_signin || !profile.oauth_client_id_secret.is_empty() {
                    secrets["oauth_client_id"] = visible_secret_status(
                        &state.secrets,
                        Some(profile.oauth_client_id_secret.as_str()),
                        is_admin,
                    );
                }
                if is_oauth || has_oauth_signin || !profile.oauth_client_secret_secret.is_empty() {
                    secrets["oauth_client_secret"] = visible_secret_status(
                        &state.secrets,
                        Some(profile.oauth_client_secret_secret.as_str()),
                        is_admin,
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
                let mut view = json!({
                    "id": id,
                    "kind": profile.kind,
                    "auth": if is_oauth { "oauth" } else { "api_key" },
                    "base_url": profile.base_url,
                    "models": profile.models,
                    "routing_classes": routing_classes,
                    "data_egress_approved": profile.data_egress_approved,
                    "catalog_fetch": profile.catalog_fetch,
                    "catalog_verified": false,
                    "model_source": "fallback",
                    "status": "unchecked",
                    "health": "unknown",
                    "last_error": Value::Null,
                    "last_checked": Value::Null,
                    "model_count": profile.models.len(),
                    "managed": true,
                    "secrets": secrets,
                    "credential": credential_view(&state, &user.id, &id)?,
                    "accounts": provider_account_views(&state, &user.id, &id)?,
                    "signin": signin,
                    "signin_risk": risk,
                });
                add_signin_status(&mut view, &state, is_admin, &id, &kind);
                providers.push(view);
            }
        }
    }
    for (id, profile) in &llm.providers {
        if providers
            .iter()
            .any(|p| p.get("id").and_then(Value::as_str) == Some(id.as_str()))
        {
            continue;
        }
        let kind = crate::providers::canonical_kind(&profile.kind)?;
        let signin = signin_flows(&state, is_admin, id, &kind);
        let risk = signin_risk(&kind, &signin);
        let has_oauth_signin = !signin.is_empty();
        let is_oauth = matches!(
            crate::providers::auth_plan(&kind, &profile.auth)?,
            crate::providers::AuthPlan::OAuth { .. }
        );
        let mut secrets = json!({
            "api_key": visible_secret_status(&state.secrets, Some(profile.api_key_secret.as_str()), is_admin),
        });
        if is_oauth || has_oauth_signin || !profile.refresh_token_secret.is_empty() {
            secrets["refresh_token"] = visible_secret_status(
                &state.secrets,
                Some(profile.refresh_token_secret.as_str()),
                is_admin,
            );
        }
        if is_oauth || has_oauth_signin || !profile.oauth_client_id_secret.is_empty() {
            secrets["oauth_client_id"] = visible_secret_status(
                &state.secrets,
                Some(profile.oauth_client_id_secret.as_str()),
                is_admin,
            );
        }
        if is_oauth || has_oauth_signin || !profile.oauth_client_secret_secret.is_empty() {
            secrets["oauth_client_secret"] = visible_secret_status(
                &state.secrets,
                Some(profile.oauth_client_secret_secret.as_str()),
                is_admin,
            );
        }
        let routing_classes = llm
            .classes
            .iter()
            .filter(|(_, models)| {
                models
                    .iter()
                    .any(|model| profile.models.iter().any(|advertised| advertised == model))
            })
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let managed = state
            .admin
            .as_ref()
            .is_some_and(|admin| admin.manages_provider(id));
        let mut view = json!({
            "id": id,
            "kind": profile.kind,
            "auth": if is_oauth { "oauth" } else { "api_key" },
            "base_url": profile.base_url,
            "models": profile.models,
            "routing_classes": routing_classes,
            "data_egress_approved": profile.data_egress_approved,
            "catalog_fetch": profile.catalog_fetch,
            "catalog_verified": false,
            "model_source": "fallback",
            "status": "unchecked",
            "health": "unknown",
            "last_error": Value::Null,
            "last_checked": Value::Null,
            "model_count": profile.models.len(),
            "managed": managed,
            "secrets": secrets,
            "credential": credential_view(&state, &user.id, id)?,
            "accounts": provider_account_views(&state, &user.id, id)?,
            "signin": signin,
            "signin_risk": risk,
        });
        add_signin_status(&mut view, &state, is_admin, id, &kind);
        providers.push(view);
    }

    // A provider whose accounts are all disabled leaves the router's list;
    // keep it on the page so an account can be turned back on.
    let parked = parked_provider_views(&state, &user.id, is_admin, &providers)?;
    providers.extend(parked);

    Ok(Json(json!({"providers": providers})))
}

pub(super) async fn list_connectors(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let is_admin = user.is_admin();
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
        .map(|(id, connector)| oauth_connector_view(&state, id, connector, is_admin))
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
        "default_inbox_id": if is_admin && !inbox_id.is_empty() { json!(inbox_id) } else { Value::Null },
        "secrets": {
            "api_key": visible_secret_status(&state.secrets, Some(agentmail.api_key_secret.as_str()), is_admin),
        },
    }));
    // ElevenLabs sign-in: a per-account connector. `connected` is this
    // account's own sign-in; the tokens never appear.
    views.push(json!({
        "id": "elevenlabs",
        "kind": "oauth",
        "enabled": state.config.elevenlabs.mcp_enabled,
        "redirect_uri": Value::Null,
        "connected": crate::elevenlabs_mcp::is_signed_in(&state.users, &user.id)?,
        // Kept for one release so an older console still finds its OAuth mode.
        "auth_modes": ["oauth"],
        "secrets": {},
        "oauth": {
            "client_id": state.config.elevenlabs.oauth_client_id,
            "authorize_url": crate::elevenlabs_oauth::ELEVENLABS_AUTHORIZE_URL,
            "cimd_path": crate::elevenlabs_oauth::CIMD_PATH,
            "callback_uri": crate::elevenlabs_oauth::oauth_callback_uri(),
            "scope": state.config.elevenlabs.oauth_scope,
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
    ] {
        views.push(json!({
            "id": id,
            "kind": "api_key",
            "enabled": enabled,
            "redirect_uri": Value::Null,
            "api_base": api_base,
            "secrets": {
                "api_key": visible_secret_status(&state.secrets, Some(key_secret), is_admin),
            },
        }));
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
        let mut view = json!({
            "id": id,
            "kind": "token",
            "enabled": true,
            "redirect_uri": Value::Null,
            "connected": personal.state == "connected",
            "personal": personal.to_json(),
        });
        // GitHub also signs in natively (device flow); the pasted token stays
        // as the fallback. Same card, one extra block.
        if id == "github" {
            view["native"] = state.github.view(&user.id)?;
        }
        views.push(view);
    }
    let checks = state.store.connector_checks(&user.id)?;
    for view in &mut views {
        let group = view["id"].as_str().map(connector_group).unwrap_or("Other");
        view["group"] = json!(group);
        let id = view["id"].as_str().unwrap_or("");
        let check = checks.iter().find(|(connector, _, _, _)| connector == id);
        annotate_connector_health(
            view,
            check.map(|(_, ok, message, at)| (*ok, message.as_str(), at.as_str())),
        );
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
///
/// A provider the owner saved from the console lives in the admin overlay; it
/// is found here at once, so its sign-in does not wait for a restart.
pub(super) fn provider_profile(state: &ApiState, id: &str) -> Result<ProviderConfig> {
    validate_provider(id)?;
    if id == "default" {
        return Ok(state.config.llm.default_provider());
    }
    if let Some(profile) = state.config.llm.providers.get(id) {
        return Ok(profile.clone());
    }
    state
        .admin
        .as_ref()
        .and_then(|admin| admin.provider(id))
        .ok_or_else(|| SeraError::NotFound(format!("provider {id}")))
}

/// A provider's profile as last saved. An owner-managed provider's console
/// copy wins over the one loaded at startup, so a sign-in setting saved in the
/// console (Claude's plan sign-in, for one) applies to sign-in at once. The
/// model router still reads the startup copy until the server restarts; the
/// listing says so with `pending_restart`.
pub(super) fn live_provider_profile(state: &ApiState, id: &str) -> Result<ProviderConfig> {
    validate_provider(id)?;
    if id != "default"
        && let Some(profile) = state.admin.as_ref().and_then(|admin| admin.provider(id))
    {
        return Ok(profile);
    }
    provider_profile(state, id)
}

/// Whether the console holds a newer copy of this provider than the one the
/// live model router serves, so bots use the change only after a restart.
/// A console save normally swaps the router at once; this stays true only
/// when that rebuild was refused.
pub(super) fn provider_pending_restart(state: &ApiState, id: &str) -> bool {
    let Some(saved) = state.admin.as_ref().and_then(|admin| admin.provider(id)) else {
        return false;
    };
    let router = state.runtime.router();
    match router.provider_profile(id) {
        None => true,
        Some(served) => serde_json::to_value(&saved).ok() != serde_json::to_value(served).ok(),
    }
}

/// Ask one provider for its live model list with the acting user's own key.
///
/// The reply says how it went in plain terms and never carries a key, a
/// response body or a provider error text.
pub(super) async fn test_provider_endpoint(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    validate_provider(&id)?;
    let report = state.runtime.test_provider(&user.id, &id).await?;
    let catalog = state.runtime.model_catalog(&user.id)?;
    let view = catalog.iter().find(|provider| provider.id == id);
    Ok(Json(json!({
        "id": id,
        "ok": report.verified,
        "status": view.map(|provider| provider.status).unwrap_or("unchecked"),
        "health": view.map(|provider| provider.health).unwrap_or("unknown"),
        "last_error": view.and_then(|provider| provider.last_error),
        "last_checked": view.and_then(|provider| provider.last_checked.clone()),
        "model_source": view.map(|provider| provider.model_source).unwrap_or("fallback"),
        "model_count": report.model_count,
    })))
}

/// The account's own default route, when it has chosen one.
pub(super) async fn get_default_route(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    state.runtime.ensure_model_catalogs(&user.id).await;
    Ok(Json(
        json!({"selection": state.runtime.default_route(&user.id)?}),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DefaultRouteRequest {
    provider: String,
    model: String,
}

/// Make one provider and model the account's default for new conversations.
pub(super) async fn set_default_route(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<DefaultRouteRequest>,
) -> Result<Json<Value>> {
    validate_provider(&request.provider)?;
    state.runtime.refresh_model_catalogs(&user.id).await?;
    let selection = state.runtime.set_default_route(
        &user.id,
        ModelSelection {
            provider: request.provider,
            model: request.model,
            account: None,
        },
    )?;
    Ok(Json(json!({"selection": selection})))
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
    provider_profile(&state, &id)?;
    let credential = ProviderCredential {
        api_key: request.api_key.trim().to_owned(),
        refresh_token: String::new(),
        ..Default::default()
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
    provider_profile(&state, &request.provider)?;
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
    pub(super) state: String,
    #[serde(default)]
    pub(super) code: Option<String>,
    #[serde(default)]
    pub(super) error: Option<String>,
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

/// The member's native GitHub sign-in state. No token ever appears.
pub(super) async fn github_native_status(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    Ok(Json(state.github.view(&user.id)?))
}

/// Start GitHub's device flow: the answer carries the code the member types on
/// `github.com/login/device`. Approval is picked up by a background poller.
pub(super) async fn github_device_start(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let started = state.github.start_device(&user.id).await?;
    audit_auth(
        &state,
        &user.id,
        "connector.github_signin_started",
        json!({"connector": "github"}),
    )?;
    Ok(Json(serde_json::to_value(started)?))
}

/// Where the member's newest GitHub sign-in stands.
pub(super) async fn github_device_status(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let status = state
        .github
        .device_status(&user.id)
        .ok_or_else(|| SeraError::NotFound("GitHub sign-in".into()))?;
    let mut body = json!({"status": status.word()});
    match &status {
        crate::github_connector::DeviceStatus::Connected { login } => body["login"] = json!(login),
        crate::github_connector::DeviceStatus::Denied { reason } => body["reason"] = json!(reason),
        _ => {}
    }
    Ok(Json(body))
}

/// Disconnect GitHub: the sealed tokens and the login row are deleted. The
/// answer carries the app's connections page on GitHub, where the member can
/// also revoke the authorization; the console opens it.
pub(super) async fn github_native_disconnect(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let manage_url = state.github.manage_url();
    let was_connected = state.github.disconnect(&user.id)?;
    audit_auth(
        &state,
        &user.id,
        "connector.disconnected",
        json!({"connector": "github", "was_connected": was_connected, "native": true}),
    )?;
    Ok(Json(json!({
        "id": "github",
        "connected": false,
        "was_connected": was_connected,
        "manage_url": manage_url,
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
        let disconnected = state.personal.disconnect(&user.id, kind).await?;
        clear_calendar_cooldowns_for_user(&state, &user.id);
        disconnected
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

pub(super) async fn personal_callback_inner(
    state: &ApiState,
    params: ConsentCallbackParams,
) -> Response {
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
            clear_calendar_cooldowns_for_user(state, &user_id);
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
fn annotate_connector_health(view: &mut Value, check: Option<(bool, &str, &str)>) {
    let enabled = view.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let connected = view.get("connected").and_then(Value::as_bool);
    let key_configured = view
        .pointer("/secrets/api_key/configured")
        .and_then(Value::as_bool);
    let (health, last_error, last_checked) = if let Some((ok, message, at)) = check {
        if ok {
            ("ok", None, Some(at))
        } else {
            ("error", Some(message), Some(at))
        }
    } else if !enabled {
        ("disabled", None, None)
    } else if connected == Some(true) || key_configured == Some(true) {
        ("ok", None, None)
    } else if connected == Some(false) || key_configured == Some(false) {
        ("not_configured", None, None)
    } else {
        ("unknown", None, None)
    };
    view["health"] = json!(health);
    view["last_error"] = json!(last_error);
    view["last_checked"] = json!(last_checked);
}

/// `POST /v1/connectors/{id}/test` for the signed-in user.
///
/// Per-user connectors report that user's own sign-in. Server-wide API keys
/// are probed only for the owner; a member is told to ask the owner. The
/// stored message is one of the fixed strings below.
pub(super) async fn test_connector(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let id = id.trim();
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(SeraError::Validation(
            "connector id must be a short name".into(),
        ));
    }
    let (ok, message) = connector_test_outcome(&state, &user, id).await?;
    state
        .store
        .record_connector_check(&user.id, id, ok, &message)?;
    state.store.record_audit(
        &user.id,
        "connector.tested",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"connector": id, "ok": ok})),
    )?;
    Ok(Json(json!({
        "id": id,
        "ok": ok,
        "health": if ok { "ok" } else { "error" },
        "message": message,
        "checked_at": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    })))
}

async fn connector_test_outcome(
    state: &ApiState,
    user: &ActingUser,
    id: &str,
) -> Result<(bool, String)> {
    if id == "elevenlabs" {
        return Ok(
            if crate::elevenlabs_mcp::is_signed_in(&state.users, &user.id)? {
                (true, "Signed in with ElevenLabs.".into())
            } else {
                (
                    false,
                    "Not signed in with ElevenLabs. Connect it, then test again.".into(),
                )
            },
        );
    }
    if id == "composio" {
        return Ok(if state.composio.connected(&user.id)? {
            (true, "Composio is connected for this account.".into())
        } else {
            (
                false,
                "Composio is not connected. Add a key, then test again.".into(),
            )
        });
    }
    if id == "onedrive" {
        let Some(registry) = state.onedrive.as_ref() else {
            return Ok((false, "OneDrive is not enabled on this server.".into()));
        };
        if !registry.connected(&user.id)? {
            return Ok((
                false,
                "OneDrive is not connected. Sign in, then test again.".into(),
            ));
        }
        let client = registry.client_for(&user.id)?;
        let client = client.lock().await;
        let base_folder = state.config.onedrive.folder.clone();
        return match client.list_folder(None, 1).await {
            Ok(base_res) if !base_res.items.is_empty() => Ok((
                true,
                format!(
                    "OneDrive is connected for this account (folder '{}' has {} item(s)).",
                    base_folder,
                    base_res.items.len()
                ),
            )),
            Ok(_) => match client.list_folder(Some("/"), 1).await {
                Ok(root_res) => Ok((
                    true,
                    format!(
                        "OneDrive is connected for this account (root has {} item(s); folder '{}' is currently empty).",
                        root_res.items.len(),
                        base_folder
                    ),
                )),
                Err(error) => Ok((false, format!("OneDrive root check failed: {error}"))),
            },
            Err(error) => Ok((false, format!("OneDrive connection check failed: {error}"))),
        };
    }
    if id == "whatsapp" {
        return match state
            .token_connectors
            .verify_stored(&user.id, "whatsapp")
            .await
        {
            Ok(None) => Ok((
                false,
                "No WhatsApp credentials are saved for this account. Add them, then test again."
                    .into(),
            )),
            Ok(Some(Some(account))) => Ok((true, format!("Connected as {account}."))),
            Ok(Some(None)) => Ok((
                true,
                "WhatsApp Business Cloud API credentials verified.".into(),
            )),
            Err(error) => Ok((false, error.to_string())),
        };
    }
    if crate::token_connectors::TokenConnectors::ids().contains(&id) {
        let personal = state.token_connectors.view(&user.id, id)?;
        return Ok(if personal.state == "connected" {
            (true, "This account's token is saved.".into())
        } else {
            (
                false,
                "No token is saved for this account. Add one, then test again.".into(),
            )
        });
    }
    if let Some(kind) = crate::connectors::ConnectorKind::from_section(id)
        && let Some(view) = state.personal.view(&user.id, kind)?
    {
        return Ok(if view.state == "connected" {
            let message = match view.account {
                Some(account) => format!("Signed in as {account}."),
                None => "Signed in.".into(),
            };
            (true, message)
        } else {
            (
                false,
                view.hint.unwrap_or_else(|| {
                    "Not signed in. Connect the account, then test again.".into()
                }),
            )
        });
    }
    if !user.is_admin() {
        return Err(SeraError::Rejected {
            code: "owner_only",
            message: "Only the owner can test a server-wide connector. Ask the owner to test it."
                .into(),
        });
    }
    let roles = connector_secret_roles(&state.config, id)?;
    let missing = roles.iter().find(|(_, name)| !state.secrets.has(name));
    match (missing, api_key_probe(&state.config, id)) {
        (Some(_), _) => Ok((
            false,
            "The server is missing this connector's key. Add it in Connectors.".into(),
        )),
        (None, Some(probe)) => {
            let key = roles
                .iter()
                .find(|(role, _)| *role == "api_key" || *role == "access_token")
                .and_then(|(_, name)| state.secrets.get(name))
                .unwrap_or_default();
            match probe_api_key(probe, &key).await {
                Ok(()) => Ok((true, "The key was accepted.".into())),
                Err(_) => Ok((
                    false,
                    "The key was rejected. Paste a new key and test again.".into(),
                )),
            }
        }
        (None, None) => Ok((
            true,
            "The saved credentials are present. Sign-in was not repeated.".into(),
        )),
    }
}

pub(super) fn connector_group(id: &str) -> &'static str {
    match id {
        "elevenlabs" => "Voice & Media",
        "tavily" | "firecrawl" => "Web Research",
        "agentmail" => "Email",
        "composio" => "Apps & tools",
        "github" | "todoist" => "Code and tasks",
        "slack" | "whatsapp" => "Messages",
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
    let checked_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let (ok, message) = if id == "composio"
        || id == "onedrive"
        || id == "elevenlabs"
        || crate::token_connectors::TokenConnectors::ids().contains(&id.as_str())
    {
        connector_test_outcome(&state, &user, &id)
            .await
            .map_err(admin_error)?
    } else if let Some(view) = crate::connectors::ConnectorKind::from_section(&id)
        .and_then(|kind| state.personal.view(&user.id, kind).transpose())
        .transpose()
        .map_err(admin_error)?
    {
        if view.state == "connected" {
            let message = match view.account {
                Some(account) => format!("Signed in as {account}."),
                None => "Signed in for this account.".to_owned(),
            };
            (true, message)
        } else {
            (
                false,
                view.hint
                    .unwrap_or_else(|| "Not signed in. Connect this account first.".to_owned()),
            )
        }
    } else {
        let roles = connector_secret_roles(&state.config, &id).map_err(admin_error)?;
        let missing = roles.iter().find(|(_, name)| !state.secrets.has(name));
        match (missing, api_key_probe(&state.config, &id)) {
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
        }
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

/// Owner "remove saved credentials": delete the connector's server-wide
/// secrets from the age store. Per-account sign-in tokens are not touched
/// here; those go through `DELETE /v1/admin/connectors/{id}/oauth`. Secret
/// values are never returned, only the removed secret names.
pub(super) async fn admin_remove_connector_secrets(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> AdminResult<Json<Value>> {
    require_admin(&user).map_err(admin_error)?;
    let roles = connector_secret_roles(&state.config, &id).map_err(admin_error)?;
    let names: Vec<String> = roles.iter().map(|(_, name)| name.clone()).collect();
    if names.is_empty() {
        return Err(admin_error(SeraError::Validation(
            "this connector keeps its credentials per account; use Disconnect".into(),
        )));
    }
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    state.secrets.remove(&name_refs).map_err(admin_error)?;
    audit_config(
        &state,
        &user.id,
        "config.connector_secrets_removed",
        json!({"connector": id, "secrets": names}),
    )?;
    Ok(Json(json!({"id": id, "removed": names})))
}

pub(super) fn oauth_connector_view(
    state: &ApiState,
    id: &str,
    connector: &dyn OAuthConnectorSettings,
    is_admin: bool,
) -> Value {
    let redirect = connector.redirect_uri().trim();
    json!({
        "id": id,
        "kind": "oauth",
        "enabled": connector.oauth_enabled(),
        "redirect_uri": if redirect.is_empty() { Value::Null } else { json!(redirect) },
        "secrets": {
            "client_id": visible_secret_status(&state.secrets, Some(connector.client_id_secret_name()), is_admin),
            "client_secret": visible_secret_status(&state.secrets, Some(connector.client_secret_secret_name()), is_admin),
            "refresh_token": visible_secret_status(&state.secrets, Some(connector.refresh_token_secret_name()), is_admin),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{Gateway, ToolRegistry};
    use crate::llm::LlmClient;
    use crate::policy::Policy;
    use crate::runtime::NullNotifier;
    use tower::ServiceExt;

    fn test_app_with_connectors() -> (Router, Store) {
        let (app, store, _) = test_app_with_connectors_and_users();
        (app, store)
    }

    fn test_app_with_connectors_and_users() -> (Router, Store, Arc<UserSecrets>) {
        let dir = std::env::temp_dir().join(format!("serabot-conn-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut config = Config::default();
        config.api.enabled = true;
        config.api.token_hash = sha256_hex("test-token-that-is-long-enough-123");
        config.llm.model = "test".into();
        config.llm.base_url = "https://example.com/v1".into();
        config.base_dir = dir.clone();
        config.tavily.enabled = true;
        config.connectors.agentmail.enabled = true;
        config.connectors.agentmail.default_inbox_id = "bot-inbox@agentmail.invalid".into();
        let db_path = dir.join("serabot.db");
        config.database.path = db_path.to_string_lossy().into_owned();
        let config = Arc::new(config);
        let store = Store::open(&db_path, 384, 5000).unwrap();
        let gateway = Gateway::new(
            store.clone(),
            Arc::new(Policy::empty()),
            ToolRegistry::default(),
            1,
            false,
        );
        let llm = LlmClient::new("https://example.com/v1", "test", 5, 50)
            .unwrap()
            .with_model("test");
        let runtime = Runtime::new(
            config.clone(),
            store.clone(),
            gateway,
            crate::model_router::ModelRouter::new(
                &config.llm,
                BTreeMap::from([("default".into(), llm)]),
            )
            .unwrap(),
            vec![crate::config::LoadedBot {
                config: BotConfig {
                    title: None,
                    description: None,
                    name: "main".into(),
                    trigger: "command".into(),
                    model: None,
                    system_prompt: "prompts/main.txt".into(),
                    skills: vec![],
                    tools: vec![],
                    require_approval: vec![],
                    enabled: true,
                    temperature: None,
                    thinking: None,
                    role: BotRole::Orchestrator,
                    delegates: vec![],
                    route: None,
                    fallbacks: Vec::new(),
                    failover: Default::default(),
                    catalog_only: false,
                },
                prompt: "test prompt".into(),
            }],
            Arc::new(NullNotifier),
            tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        let artifacts = Arc::new(ArtifactStore::new(dir.join("artifacts")).unwrap());
        let secrets = SharedSecrets::new(crate::secrets::SecretStore::from_map(BTreeMap::from([
            (config.llm.api_key_secret.clone(), "synthetic-llm".into()),
            ("tavily_api_key".into(), "synthetic-tavily".into()),
            ("agentmail_api_key".into(), "synthetic-agentmail".into()),
            ("google_oauth_client_id".into(), "synthetic-gid".into()),
        ])));
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [8u8; 32]).unwrap());
        let state = ApiState::new(
            runtime,
            store.clone(),
            config,
            artifacts,
            secrets,
            users.clone(),
            None,
        )
        .unwrap();
        (crate::api::router(state), store, users)
    }

    async fn get_json(app: &Router, token: &str, uri: &str) -> Value {
        let resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri(uri)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "GET {uri}");
        let bytes = axum::body::to_bytes(resp.into_body(), 128 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn find_by_id<'a>(list: &'a [Value], id: &str) -> &'a Value {
        list.iter()
            .find(|item| item["id"] == id)
            .unwrap_or_else(|| panic!("missing id {id}"))
    }

    #[tokio::test]
    async fn connectors_and_providers_redact_server_secret_names_and_inbox_for_non_admins() {
        let (app, store) = test_app_with_connectors();
        let admin_id = store.admin_user_id().unwrap();
        let member = store
            .create_user("sub-conn-lp", "lp@example.example", "LP Member")
            .unwrap();
        let admin_token = store
            .mint_session(&admin_id, chrono::Duration::hours(1))
            .unwrap();
        let member_token = store
            .mint_session(&member.id, chrono::Duration::hours(1))
            .unwrap();

        // Admin sees server-level age secret names and agentmail default_inbox_id.
        let admin_conn = get_json(&app, &admin_token, "/v1/connectors").await;
        let admin_list = admin_conn["connectors"].as_array().unwrap();
        let admin_agentmail = find_by_id(admin_list, "agentmail");
        assert_eq!(
            admin_agentmail["default_inbox_id"],
            "bot-inbox@agentmail.invalid"
        );
        assert_eq!(
            admin_agentmail["secrets"]["api_key"]["name"],
            "agentmail_api_key"
        );
        assert_eq!(admin_agentmail["secrets"]["api_key"]["configured"], true);
        let admin_tavily = find_by_id(admin_list, "tavily");
        assert_eq!(admin_tavily["secrets"]["api_key"]["name"], "tavily_api_key");
        assert_eq!(admin_tavily["secrets"]["api_key"]["configured"], true);
        let admin_gmail = find_by_id(admin_list, "gmail");
        assert_eq!(
            admin_gmail["secrets"]["client_id"]["name"],
            "google_oauth_client_id"
        );

        // Non-admin does NOT see age secret names or agentmail.default_inbox_id,
        // while capability flags (`configured`, `enabled`, `health`, `personal`) stay intact.
        let member_conn = get_json(&app, &member_token, "/v1/connectors").await;
        let member_list = member_conn["connectors"].as_array().unwrap();
        let member_agentmail = find_by_id(member_list, "agentmail");
        assert!(member_agentmail["default_inbox_id"].is_null());
        assert!(member_agentmail["secrets"]["api_key"]["name"].is_null());
        assert_eq!(member_agentmail["secrets"]["api_key"]["configured"], true);
        assert_eq!(member_agentmail["enabled"], true);
        assert_eq!(member_agentmail["health"], "ok");

        let member_tavily = find_by_id(member_list, "tavily");
        assert!(member_tavily["secrets"]["api_key"]["name"].is_null());
        assert_eq!(member_tavily["secrets"]["api_key"]["configured"], true);
        assert_eq!(member_tavily["enabled"], true);
        assert_eq!(member_tavily["health"], "ok");

        let member_gmail = find_by_id(member_list, "gmail");
        assert!(member_gmail["secrets"]["client_id"]["name"].is_null());
        assert_eq!(member_gmail["secrets"]["client_id"]["configured"], true);
        assert!(member_gmail["secrets"]["client_secret"]["name"].is_null());
        assert!(member_gmail["secrets"]["refresh_token"]["name"].is_null());
        assert!(member_gmail["personal"].is_object());

        let member_text = member_conn.to_string();
        for redacted in [
            "bot-inbox@agentmail.invalid",
            "agentmail_api_key",
            "tavily_api_key",
            "google_oauth_client_id",
        ] {
            assert!(
                !member_text.contains(redacted),
                "member payload leaked {redacted}: {member_text}"
            );
        }

        // Providers also redact server-level age secret names for non-admins.
        let admin_prov = get_json(&app, &admin_token, "/v1/providers").await;
        let admin_default = find_by_id(admin_prov["providers"].as_array().unwrap(), "default");
        assert_eq!(admin_default["secrets"]["api_key"]["name"], "llm_api_key");
        assert_eq!(admin_default["secrets"]["api_key"]["configured"], true);

        let member_prov = get_json(&app, &member_token, "/v1/providers").await;
        let member_default = find_by_id(member_prov["providers"].as_array().unwrap(), "default");
        assert!(member_default["secrets"]["api_key"]["name"].is_null());
        assert_eq!(member_default["secrets"]["api_key"]["configured"], true);
        assert!(!member_prov.to_string().contains("llm_api_key"));
    }

    #[tokio::test]
    async fn elevenlabs_connector_is_oauth_only_and_per_user_for_admin_and_member() {
        let (app, store, users) = test_app_with_connectors_and_users();
        let admin_id = store.admin_user_id().unwrap();
        let member = store
            .create_user("sub-eleven-member", "member@example.example", "Member")
            .unwrap();
        let admin_token = store
            .mint_session(&admin_id, chrono::Duration::hours(1))
            .unwrap();
        let member_token = store
            .mint_session(&member.id, chrono::Duration::hours(1))
            .unwrap();

        // Sign in ONLY the non-admin member; admin remains disconnected.
        crate::elevenlabs_mcp::store_sign_in(
            &users,
            &member.id,
            "synthetic-member-access",
            "synthetic-member-refresh",
            Some(3600),
        )
        .unwrap();

        let admin_conn = get_json(&app, &admin_token, "/v1/connectors").await;
        let admin_list = admin_conn["connectors"].as_array().unwrap();
        assert!(!admin_list.iter().any(|c| c["id"] == "elevenlabs_api_key"));
        let admin_el = find_by_id(admin_list, "elevenlabs");
        assert_eq!(admin_el["kind"], "oauth");
        assert_eq!(admin_el["auth_modes"], json!(["oauth"]));
        assert_eq!(admin_el["connected"], false);
        assert!(admin_el["oauth"]["authorize_url"].is_string());

        let member_conn = get_json(&app, &member_token, "/v1/connectors").await;
        let member_list = member_conn["connectors"].as_array().unwrap();
        assert!(!member_list.iter().any(|c| c["id"] == "elevenlabs_api_key"));
        let member_el = find_by_id(member_list, "elevenlabs");
        assert_eq!(member_el["kind"], "oauth");
        assert_eq!(member_el["auth_modes"], json!(["oauth"]));
        assert_eq!(member_el["connected"], true);
        assert!(member_el["oauth"]["authorize_url"].is_string());
        assert!(!member_conn.to_string().contains("synthetic-member-access"));

        // Non-admin member can test their own ElevenLabs connection via /v1/connectors/elevenlabs/test.
        let test_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/connectors/elevenlabs/test")
                    .header("authorization", format!("Bearer {member_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(test_resp.status(), StatusCode::OK);
        let test_bytes = axum::body::to_bytes(test_resp.into_body(), 128 * 1024)
            .await
            .unwrap();
        let test_json: Value = serde_json::from_slice(&test_bytes).unwrap();
        assert_eq!(test_json["ok"], true);
    }

    #[tokio::test]
    async fn non_admin_member_can_start_and_disconnect_own_elevenlabs_oauth_only() {
        let (app, store, users) = test_app_with_connectors_and_users();
        let admin_id = store.admin_user_id().unwrap();
        let member = store
            .create_user(
                "sub-eleven-oauth",
                "oauth-member@example.example",
                "OAuth Member",
            )
            .unwrap();
        let member_token = store
            .mint_session(&member.id, chrono::Duration::hours(1))
            .unwrap();

        // Both admin and member start signed in with ElevenLabs.
        crate::elevenlabs_mcp::store_sign_in(
            &users,
            &admin_id,
            "synthetic-admin-access",
            "synthetic-admin-refresh",
            Some(3600),
        )
        .unwrap();
        crate::elevenlabs_mcp::store_sign_in(
            &users,
            &member.id,
            "synthetic-member-access",
            "synthetic-member-refresh",
            Some(3600),
        )
        .unwrap();

        // Non-admin member can start ElevenLabs OAuth PKCE for their own account.
        let start_body = json!({
            "challenge": "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "state": "synthetic-state-1234",
            "redirect_uri": crate::elevenlabs_oauth::oauth_callback_uri(),
        });
        let start_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/admin/connectors/elevenlabs/oauth/start")
                    .header("authorization", format!("Bearer {member_token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(start_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start_resp.status(), StatusCode::OK);
        let start_bytes = axum::body::to_bytes(start_resp.into_body(), 128 * 1024)
            .await
            .unwrap();
        let start_json: Value = serde_json::from_slice(&start_bytes).unwrap();
        assert_eq!(start_json["flow"], "pkce");
        assert!(
            start_json["url"]
                .as_str()
                .unwrap()
                .starts_with("https://elevenlabs.io/app/oauth/authorize?")
        );

        // Non-admin member can disconnect their own ElevenLabs OAuth without touching admin's.
        let del_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("DELETE")
                    .uri("/v1/admin/connectors/elevenlabs/oauth")
                    .header("authorization", format!("Bearer {member_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(del_resp.status(), StatusCode::OK);
        let del_bytes = axum::body::to_bytes(del_resp.into_body(), 128 * 1024)
            .await
            .unwrap();
        let del_json: Value = serde_json::from_slice(&del_bytes).unwrap();
        assert_eq!(del_json["signed_in"], false);
        assert_eq!(del_json["removed"], true);

        assert!(!crate::elevenlabs_mcp::is_signed_in(&users, &member.id).unwrap());
        assert!(crate::elevenlabs_mcp::is_signed_in(&users, &admin_id).unwrap());
    }

    #[tokio::test]
    async fn personal_google_connectors_test_cleanly_on_user_and_admin_endpoints() {
        let (app, store, users) = test_app_with_connectors_and_users();
        let admin_id = store.admin_user_id().unwrap();
        let admin_token = store
            .mint_session(&admin_id, chrono::Duration::hours(1))
            .unwrap();
        let member = store
            .create_user("sub-google-test", "gtest@example.example", "Google Test")
            .unwrap();
        let member_token = store
            .mint_session(&member.id, chrono::Duration::hours(1))
            .unwrap();

        let cred = serde_json::json!({
            "refresh_token": "synthetic-refresh-token-1234",
            "account": "alice@example.com",
            "connectors": ["google_drive", "gmail"]
        })
        .to_string();
        users.set(&admin_id, "conn.google", &cred).unwrap();
        users.set(&member.id, "conn.google", &cred).unwrap();

        for id in ["google_drive", "gmail"] {
            let user_resp = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri(format!("/v1/connectors/{id}/test"))
                        .header("authorization", format!("Bearer {member_token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(user_resp.status(), StatusCode::OK, "{id}");
            let user_bytes = axum::body::to_bytes(user_resp.into_body(), 128 * 1024)
                .await
                .unwrap();
            let user_json: Value = serde_json::from_slice(&user_bytes).unwrap();
            assert_eq!(user_json["ok"], true);
            assert_eq!(user_json["message"], "Signed in as alice@example.com.");

            let admin_resp = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri(format!("/v1/admin/connectors/{id}/test"))
                        .header("authorization", format!("Bearer {admin_token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(admin_resp.status(), StatusCode::OK, "{id}");
            let admin_bytes = axum::body::to_bytes(admin_resp.into_body(), 128 * 1024)
                .await
                .unwrap();
            let admin_json: Value = serde_json::from_slice(&admin_bytes).unwrap();
            assert_eq!(admin_json["ok"], true);
            assert_eq!(admin_json["message"], "Signed in as alice@example.com.");
        }
    }

    #[tokio::test]
    async fn list_providers_includes_configured_provider_with_zero_accounts() {
        let dir = std::env::temp_dir().join(format!("serabot-conn-toml-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut config = Config::default();
        config.api.enabled = true;
        config.api.token_hash = sha256_hex("test-token-that-is-long-enough-123");
        config.llm.model = "test".into();
        config.llm.base_url = "https://example.com/v1".into();
        config.base_dir = dir.clone();
        config.llm.providers.insert(
            "claude-sdk".into(),
            ProviderConfig {
                kind: "claude-agent-sdk".into(),
                base_url: "http://127.0.0.1:8791".into(),
                api_key_secret: "claude_gateway_token".into(),
                models: vec!["claude-sonnet-4-5".into()],
                data_egress_approved: true,
                catalog_fetch: false,
                ..Default::default()
            },
        );
        let db_path = dir.join("serabot.db");
        config.database.path = db_path.to_string_lossy().into_owned();
        let config = Arc::new(config);
        let store = Store::open(&db_path, 384, 5000).unwrap();
        let gateway = Gateway::new(
            store.clone(),
            Arc::new(Policy::empty()),
            ToolRegistry::default(),
            1,
            false,
        );
        let secrets =
            SharedSecrets::new(crate::secrets::SecretStore::from_map(BTreeMap::from([(
                config.llm.api_key_secret.clone(),
                "synthetic-llm".into(),
            )])));
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [8u8; 32]).unwrap());
        let credentials = Arc::new(crate::providers::UserProviderCredentials::new(
            users.clone(),
            secrets.clone(),
            5,
            64,
        ));
        let model_router =
            crate::model_router::ModelRouter::with_credentials(&config.llm, credentials).unwrap();
        let runtime = Runtime::new(
            config.clone(),
            store.clone(),
            gateway,
            model_router,
            vec![crate::config::LoadedBot {
                config: BotConfig {
                    title: None,
                    description: None,
                    name: "main".into(),
                    trigger: "command".into(),
                    model: None,
                    system_prompt: "prompts/main.txt".into(),
                    skills: vec![],
                    tools: vec![],
                    require_approval: vec![],
                    enabled: true,
                    temperature: None,
                    thinking: None,
                    role: BotRole::Orchestrator,
                    delegates: vec![],
                    route: None,
                    fallbacks: Vec::new(),
                    failover: Default::default(),
                    catalog_only: false,
                },
                prompt: "test prompt".into(),
            }],
            Arc::new(NullNotifier),
            tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        let artifacts = Arc::new(ArtifactStore::new(dir.join("artifacts")).unwrap());
        let state = ApiState::new(runtime, store, config, artifacts, secrets, users, None).unwrap();
        let app = crate::api::router(state);

        let listed = get_json(&app, "test-token-that-is-long-enough-123", "/v1/providers").await;
        let providers = listed["providers"].as_array().unwrap();
        let sdk = providers
            .iter()
            .find(|p| p["id"] == "claude-sdk")
            .expect("claude-sdk from serabot.toml must be listed even with 0 accounts");
        assert_eq!(sdk["kind"], "claude-agent-sdk");
        assert_eq!(sdk["accounts"], json!([]));
        assert_eq!(sdk["status"], "unchecked");
        assert_eq!(sdk["health"], "unknown");
        assert_eq!(sdk["model_source"], "fallback");
    }
}
