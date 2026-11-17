//! Per-user watcher source overlay API (Big Wave B3).

use super::*;

/// Body of `PATCH /v1/sources/{name}`. Either field may be sent alone.
#[derive(Deserialize)]
pub(super) struct SourcePatchBody {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct AddRssBody {
    pub name: String,
    pub url: String,
}

/// `GET /v1/sources` — effective merged sources for the acting user. A feed URL
/// is shown to admins and to the member who added that feed; other members get `null`.
pub(super) async fn list_sources(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let sources =
        crate::sources::effective_sources_for_user(&state.config, &state.store, &user.id)?;
    let own: std::collections::HashSet<String> = state
        .store
        .list_user_sources(&user.id)?
        .into_iter()
        .filter(|row| row.origin == "member")
        .map(|row| row.name)
        .collect();
    let rows = sources
        .into_iter()
        .map(|source| {
            json!({
                "name": source.name,
                "kind": source.kind,
                "enabled": source.enabled,
                "url": super::monitoring::visible_feed_url(
                    &user,
                    own.contains(&source.name),
                    &source.url,
                ),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"sources": rows})))
}

/// `PATCH /v1/sources/{name}`. The caller's own feed takes `enabled` and a new
/// `url` (checked against the host allowlist, as on add). An admin-defined
/// source takes `enabled` only, as an overlay for this user.
pub(super) async fn patch_source(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(name): Path<String>,
    Json(body): Json<SourcePatchBody>,
) -> Result<Json<Value>> {
    if body.enabled.is_none() && body.url.is_none() {
        return Err(SeraError::Validation("no source fields to update".into()));
    }
    let own = state
        .store
        .list_user_sources(&user.id)?
        .into_iter()
        .any(|row| row.origin == "member" && row.name == name);
    if own {
        let url = match body.url.as_deref() {
            Some(raw) => Some(crate::sources::validate_member_rss_url(&state.config, raw)?),
            None => None,
        };
        if !state
            .store
            .update_member_rss_source(&user.id, &name, body.enabled, url.as_deref())?
        {
            return Err(SeraError::NotFound(format!("source {name}")));
        }
        return Ok(Json(json!({"name": name, "enabled": body.enabled})));
    }
    if body.url.is_some() {
        return Err(SeraError::Validation(
            "only a feed you added can change its URL".into(),
        ));
    }
    let Some(enabled) = body.enabled else {
        return Err(SeraError::Validation("no source fields to update".into()));
    };
    let admin_sources = crate::config::load_sources(&state.config)?;
    let Some(admin) = admin_sources.iter().find(|s| s.name == name) else {
        return Err(SeraError::NotFound(format!("source {name}")));
    };
    state
        .store
        .upsert_user_source_overlay(&user.id, &name, enabled, &admin.kind)?;
    Ok(Json(json!({"name": name, "enabled": enabled})))
}

/// `POST /v1/sources/rss` — add a member RSS feed on the host allowlist.
pub(super) async fn add_member_rss(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(body): Json<AddRssBody>,
) -> Result<Json<Value>> {
    if body.name.trim().is_empty()
        || body.name.chars().count() > 128
        || body.name.chars().any(char::is_control)
    {
        return Err(SeraError::Validation("invalid source name".into()));
    }
    let url = crate::sources::validate_member_rss_url(&state.config, &body.url)?;
    if crate::config::load_sources(&state.config)?
        .iter()
        .any(|s| s.name == body.name)
    {
        return Err(SeraError::Validation(
            "name conflicts with an admin source".into(),
        ));
    }
    state
        .runtime
        .gateway()
        .scan_egress(&user.id, &body.name)
        .map_err(|_| SeraError::Validation("source name blocked by policy".into()))?;
    state
        .runtime
        .gateway()
        .scan_egress(&user.id, &url)
        .map_err(|_| SeraError::Validation("url blocked by policy".into()))?;
    match state
        .store
        .add_member_rss_source(&user.id, &body.name, &url)
    {
        Ok(()) => Ok(Json(
            json!({"name": body.name, "url": url, "kind": "rss", "enabled": true}),
        )),
        Err(SeraError::Database(rusqlite::Error::SqliteFailure(err, _)))
            if err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
        {
            Err(SeraError::InvalidState("source already exists".into()))
        }
        Err(other) => Err(other),
    }
}

/// `DELETE /v1/sources/{name}` — remove a member-added RSS feed.
pub(super) async fn delete_member_source(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    if !state.store.delete_member_rss_source(&user.id, &name)? {
        return Err(SeraError::NotFound(format!("source {name}")));
    }
    Ok(Json(json!({"deleted": name})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{Gateway, ToolRegistry};
    use crate::llm::LlmClient;
    use crate::policy::Policy;
    use crate::runtime::NullNotifier;
    use tower::ServiceExt;

    fn test_app_for_sources() -> (Router, Store) {
        let dir = std::env::temp_dir().join(format!("serabot-src-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut config = Config::default();
        config.api.enabled = true;
        config.api.token_hash = sha256_hex("test-token-that-is-long-enough-123");
        config.llm.model = "test".into();
        config.llm.base_url = "https://example.com/v1".into();
        config.base_dir = dir.clone();
        config.watcher.rss_allowlist_hosts = vec!["feeds.example.invalid".into()];
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
        let secrets = SharedSecrets::new(crate::secrets::SecretStore::from_map(BTreeMap::new()));
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [6u8; 32]).unwrap());
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
        (crate::api::router(state), store)
    }

    #[tokio::test]
    async fn add_member_rss_rejects_control_characters_in_name() {
        let (app, store) = test_app_for_sources();
        let member = store
            .create_user("sub-src-ctrl", "src-ctrl@example.example", "Src")
            .unwrap();
        let token = store
            .mint_session(&member.id, chrono::Duration::hours(1))
            .unwrap();

        for bad_name in ["feed\nline", "feed\rret", "feed\x00nul", "feed\ttab"] {
            let response = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri("/v1/sources/rss")
                        .header("authorization", format!("Bearer {token}"))
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({
                                "name": bad_name,
                                "url": "https://feeds.example.invalid/rss.xml",
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "expected 422 for name {bad_name:?}"
            );
        }

        let ok_response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/sources/rss")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "clean-feed",
                            "url": "https://feeds.example.invalid/rss.xml",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok_response.status(), StatusCode::OK);
    }
}
