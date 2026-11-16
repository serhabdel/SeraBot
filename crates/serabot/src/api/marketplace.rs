//! Plugin marketplace routes (v0.36). The catalog is read-only and shared.
//! Installs, enablement and secrets are per user: a user never sees or changes
//! another user's install, and secret values are write-only.

use std::collections::BTreeMap;

use super::*;
use crate::marketplace::{self, AuthKind, PluginManifest, Server};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PluginConfigRequest {
    values: BTreeMap<String, String>,
}

/// The secrets store name for one plugin field. Dotted names are allowed by
/// the store, so each plugin's values are kept apart by slug and field.
fn secret_name(slug: &str, secret_key: &str) -> String {
    format!("marketplace.{slug}.{secret_key}")
}

fn find<'a>(state: &'a ApiState, slug: &str) -> Result<&'a PluginManifest> {
    state
        .marketplace
        .iter()
        .find(|plugin| plugin.slug == slug)
        .ok_or_else(|| SeraError::NotFound(format!("plugin {slug}")))
}

/// The status a caller sees for one plugin.
fn status(
    state: &ApiState,
    user: &ActingUser,
    manifest: &PluginManifest,
) -> Result<(bool, bool, &'static str)> {
    let install = state.store.marketplace_install(&user.id, &manifest.slug)?;
    let (installed, enabled) = match &install {
        Some((enabled, _)) => (true, *enabled),
        None => (false, false),
    };
    let needs_config = installed && !all_fields_configured(state, user, manifest)?;
    let word = if !installed {
        "available"
    } else if needs_config {
        "needs_config"
    } else {
        "installed"
    };
    Ok((installed, enabled, word))
}

fn all_fields_configured(
    state: &ApiState,
    user: &ActingUser,
    manifest: &PluginManifest,
) -> Result<bool> {
    for app in &manifest.apps {
        for auth in &app.auth {
            for field in &auth.fields {
                if !field_configured(state, user, manifest, &field.secret_key)? {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

fn field_configured(
    state: &ApiState,
    user: &ActingUser,
    manifest: &PluginManifest,
    secret_key: &str,
) -> Result<bool> {
    let name = secret_name(&manifest.slug, secret_key);
    Ok(state.users.get(&user.id, &name)?.is_some())
}

fn summary(state: &ApiState, user: &ActingUser, manifest: &PluginManifest) -> Result<Value> {
    let (installed, enabled, word) = status(state, user, manifest)?;
    Ok(json!({
        "slug": manifest.slug,
        "name": manifest.name,
        "tagline": manifest.tagline,
        "category": manifest.category,
        "icon_url": manifest.icon_url,
        "version": manifest.version,
        "min_server_version": manifest.min_server_version,
        "installed": installed,
        "enabled": enabled,
        "status": word,
    }))
}

fn detail(state: &ApiState, user: &ActingUser, manifest: &PluginManifest) -> Result<Value> {
    let (installed, enabled, word) = status(state, user, manifest)?;
    let mut apps = Vec::new();
    for app in &manifest.apps {
        let mut auth = Vec::new();
        for entry in &app.auth {
            let mut fields = Vec::new();
            for field in &entry.fields {
                fields.push(json!({
                    "id": field.id,
                    "label": field.label,
                    "hint": field.hint,
                    "docs_url": field.docs_url,
                    "configured": field_configured(state, user, manifest, &field.secret_key)?,
                }));
            }
            auth.push(json!({
                "id": entry.id,
                "kind": match entry.kind { AuthKind::OauthLink => "oauth-link", AuthKind::ApiKey => "api-key" },
                "fields": fields,
            }));
        }
        apps.push(json!({
            "id": app.id,
            "name": app.name,
            "transport": transport_word(&app.server),
            "auth": auth,
        }));
    }
    Ok(json!({
        "slug": manifest.slug,
        "name": manifest.name,
        "tagline": manifest.tagline,
        "description": manifest.description,
        "category": manifest.category,
        "icon_url": manifest.icon_url,
        "version": manifest.version,
        "min_server_version": manifest.min_server_version,
        "prompts": manifest.prompts,
        "apps": apps,
        "permissions": {"egress": manifest.permissions.egress},
        "website_url": manifest.website_url,
        "privacy_policy_url": manifest.privacy_policy_url,
        "terms_url": manifest.terms_url,
        "installed": installed,
        "enabled": enabled,
        "status": word,
    }))
}

fn transport_word(server: &Server) -> &'static str {
    match server {
        Server::Http { .. } => "http",
        Server::Stdio { .. } => "stdio",
    }
}

fn audit(state: &ApiState, user: &ActingUser, event: &str, slug: &str) -> Result<()> {
    state.store.record_audit(
        &user.id,
        event,
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"plugin": slug})),
    )?;
    Ok(())
}

/// `GET /v1/marketplace/plugins`
pub(super) async fn list_plugins(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let mut plugins = Vec::new();
    for manifest in &state.marketplace {
        plugins.push(summary(&state, &user, manifest)?);
    }
    Ok(Json(json!({"plugins": plugins})))
}

/// `GET /v1/marketplace/plugins/{slug}`
pub(super) async fn get_plugin(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(slug): Path<String>,
) -> Result<Json<Value>> {
    let manifest = find(&state, &slug)?;
    Ok(Json(json!({"plugin": detail(&state, &user, manifest)?})))
}

/// `POST /v1/marketplace/plugins/{slug}/install`. Refused for a plugin whose
/// server is not `http`: those need their own security design first.
pub(super) async fn install_plugin(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(slug): Path<String>,
) -> Result<Json<Value>> {
    let manifest = find(&state, &slug)?;
    if let Some(app) = manifest
        .apps
        .iter()
        .find(|app| matches!(app.server, Server::Stdio { .. }))
    {
        return Err(SeraError::Validation(format!(
            "plugin app {} starts a local program, which this server does not run yet",
            app.id
        )));
    }
    state
        .store
        .marketplace_install_insert(&user.id, &slug, &manifest.version)?;
    audit(&state, &user, "marketplace.installed", &slug)?;
    let (installed, enabled, word) = status(&state, &user, manifest)?;
    Ok(Json(
        json!({"slug": slug, "installed": installed, "enabled": enabled, "status": word}),
    ))
}

async fn set_enabled(
    state: &ApiState,
    user: &ActingUser,
    slug: &str,
    enabled: bool,
) -> Result<Json<Value>> {
    let manifest = find(state, slug)?;
    if !state
        .store
        .marketplace_install_set_enabled(&user.id, slug, enabled)?
    {
        return Err(SeraError::NotFound(format!(
            "plugin {slug} is not installed"
        )));
    }
    audit(
        state,
        user,
        if enabled {
            "marketplace.enabled"
        } else {
            "marketplace.disabled"
        },
        slug,
    )?;
    let (_, now_enabled, word) = status(state, user, manifest)?;
    Ok(Json(
        json!({"slug": slug, "enabled": now_enabled, "status": word}),
    ))
}

/// `POST /v1/marketplace/plugins/{slug}/enable`
pub(super) async fn enable_plugin(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(slug): Path<String>,
) -> Result<Json<Value>> {
    set_enabled(&state, &user, &slug, true).await
}

/// `POST /v1/marketplace/plugins/{slug}/disable`
pub(super) async fn disable_plugin(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(slug): Path<String>,
) -> Result<Json<Value>> {
    set_enabled(&state, &user, &slug, false).await
}

/// `DELETE /v1/marketplace/plugins/{slug}`: removes the caller's install and
/// every secret that install set.
pub(super) async fn uninstall_plugin(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(slug): Path<String>,
) -> Result<Json<Value>> {
    let manifest = find(&state, &slug)?;
    if !state.store.marketplace_install_delete(&user.id, &slug)? {
        return Err(SeraError::NotFound(format!(
            "plugin {slug} is not installed"
        )));
    }
    for app in &manifest.apps {
        for auth in &app.auth {
            for field in &auth.fields {
                let _ = state
                    .users
                    .delete(&user.id, &secret_name(&slug, &field.secret_key))?;
            }
        }
    }
    audit(&state, &user, "marketplace.uninstalled", &slug)?;
    Ok(Json(json!({"slug": slug, "installed": false})))
}

/// `PUT /v1/marketplace/plugins/{slug}/config`: values go to the caller's
/// secrets store only. The response reports which fields are set, never values.
pub(super) async fn put_plugin_config(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(slug): Path<String>,
    Json(request): Json<PluginConfigRequest>,
) -> Result<Json<Value>> {
    let manifest = find(&state, &slug)?;
    if state.store.marketplace_install(&user.id, &slug)?.is_none() {
        return Err(SeraError::NotFound(format!(
            "plugin {slug} is not installed"
        )));
    }
    // Map each accepted field id to its secret key; reject anything else.
    let mut targets: BTreeMap<String, String> = BTreeMap::new();
    for app in &manifest.apps {
        for auth in &app.auth {
            for field in &auth.fields {
                targets.insert(field.id.clone(), field.secret_key.clone());
            }
        }
    }
    for (field_id, value) in &request.values {
        let Some(secret_key) = targets.get(field_id) else {
            return Err(SeraError::Validation(format!(
                "plugin field {field_id} is not a field of this plugin"
            )));
        };
        let name = secret_name(&slug, secret_key);
        if value.trim().is_empty() {
            let _ = state.users.delete(&user.id, &name)?;
        } else {
            state.users.set(&user.id, &name, value.trim())?;
        }
    }
    audit(&state, &user, "marketplace.configured", &slug)?;
    let mut fields = Vec::new();
    for app in &manifest.apps {
        for auth in &app.auth {
            for field in &auth.fields {
                fields.push(json!({
                    "id": field.id,
                    "configured": field_configured(&state, &user, manifest, &field.secret_key)?,
                }));
            }
        }
    }
    let (_, _, word) = status(&state, &user, manifest)?;
    Ok(Json(
        json!({"slug": slug, "status": word, "fields": fields}),
    ))
}

/// The catalog as loaded at boot, and the problems found in any plugin file.
/// Loads from `plugins/` beside the config file when present and merges any
/// built-in shipped manifests not already loaded from disk.
pub fn load(config: &Config) -> (Vec<PluginManifest>, Vec<String>) {
    marketplace::load_catalog_with_builtin(&config.path("plugins"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{Gateway, ToolRegistry};
    use crate::llm::LlmClient;
    use crate::policy::Policy;
    use crate::runtime::NullNotifier;
    use tower::ServiceExt;

    #[test]
    fn load_falls_back_to_builtin_catalog_when_plugins_dir_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            base_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let (plugins, problems) = load(&config);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(plugins.len(), 10);
    }

    #[tokio::test]
    async fn marketplace_list_route_returns_builtin_catalog_without_plugins_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.api.enabled = true;
        config.api.token_hash = sha256_hex("test-token-that-is-long-enough-123");
        config.llm.model = "test".into();
        config.llm.base_url = "https://example.com/v1".into();
        config.base_dir = dir.path().to_path_buf();
        let db_path = dir.path().join("serabot.db");
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
                },
                prompt: "test prompt".into(),
            }],
            Arc::new(NullNotifier),
            tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        let artifacts =
            Arc::new(crate::maker::ArtifactStore::new(dir.path().join("artifacts")).unwrap());
        let secrets =
            SharedSecrets::new(crate::secrets::SecretStore::from_map(BTreeMap::from([(
                config.llm.api_key_secret.clone(),
                "synthetic-llm".into(),
            )])));
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [8u8; 32]).unwrap());
        let state = ApiState::new(
            runtime,
            store.clone(),
            config,
            artifacts,
            secrets,
            users,
            None,
        )
        .unwrap();
        let app = crate::api::router(state);
        let admin_id = store.admin_user_id().unwrap();
        let token = store
            .mint_session(&admin_id, chrono::Duration::hours(1))
            .unwrap();
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/v1/marketplace/plugins")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 128 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let plugins = body["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 10);
    }
}
