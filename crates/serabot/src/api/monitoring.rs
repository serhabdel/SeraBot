//! Monitoring source health (0.29.0, Wave A2) and manual watch enqueue (0.30.0 B2).

use super::*;
use crate::runtime::ScheduledPayload;
use crate::store::SourceRunStatus;
use crate::util::redact_url_queries;

const MAX_SOURCE_ERROR_CHARS: usize = 240;

/// Strip HTML-ish watcher errors before they reach the API. URL queries and
/// fragments are removed too: a run error can name a feed with an access key,
/// and rows stored by earlier builds may still carry one.
fn sanitize_source_error(raw: Option<&str>) -> Option<String> {
    let redacted = redact_url_queries(raw?.trim());
    let text = redacted.trim();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("<html") || lower.contains("<!doctype") {
        return Some("run failed (response withheld)".into());
    }
    let flat: String = text.chars().filter(|ch| !ch.is_control()).collect();
    if flat.contains('<') {
        return Some("run failed (response withheld)".into());
    }
    Some(truncate_chars(&flat, MAX_SOURCE_ERROR_CHARS))
}

/// The feed URL a caller may see. Admins see every feed. A member sees only a
/// feed they added themselves (`owned`); admin-defined feeds show `null` to
/// members. Feed URLs can carry access tokens in their query string.
pub(super) fn visible_feed_url(acting: &ActingUser, owned: bool, url: &str) -> Value {
    if acting.is_admin() || owned {
        json!(url)
    } else {
        Value::Null
    }
}

fn last_run_json(run: Option<&SourceRunStatus>) -> Value {
    run.map_or(Value::Null, |run| {
        json!({
            "ok": run.ok,
            "error": sanitize_source_error(run.error.as_deref()),
            "items_seen": run.items_seen,
            "started_at": run.started_at,
            "finished_at": run.finished_at,
        })
    })
}

/// `GET /v1/monitoring/sources`: effective sources for the caller (admin
/// sources with per-user overlays plus the caller's own feeds) and last-run
/// status. Names and kinds are shared; feed URLs are admin-only, except a
/// member's own feed, which shows its URL to that member.
pub(super) async fn list_monitoring_sources(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let effective =
        crate::sources::effective_sources_for_user(&state.config, &state.store, &user.id)?;
    let own: HashSet<String> = state
        .store
        .list_user_sources(&user.id)?
        .into_iter()
        .filter(|row| row.origin == "member")
        .map(|row| row.name)
        .collect();
    let runs = state.store.list_source_run_status(&user.id)?;
    let mut by_source: HashMap<String, _> = runs
        .into_iter()
        .map(|row| (row.source.clone(), row))
        .collect();
    let sources = effective
        .into_iter()
        .map(|source| {
            let is_own = own.contains(&source.name);
            let last = by_source.remove(&source.name);
            json!({
                "name": source.name,
                "kind": source.kind,
                "enabled": source.enabled,
                "url": visible_feed_url(&user, is_own, &source.url),
                "own": is_own,
                "last_run": last_run_json(last.as_ref()),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"sources": sources})))
}

#[derive(Deserialize)]
pub(super) struct DigestQuery {
    /// Window in hours back from now (default 24, at most 168).
    pub hours: Option<u32>,
    /// Start of the window as RFC 3339. Wins over `hours`, and is clamped to a week.
    pub since: Option<String>,
}

/// `GET /v1/digest`: items the caller's watched sources reported since a time,
/// with the sources whose latest run failed.
pub(super) async fn get_digest(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<DigestQuery>,
) -> Result<Json<Value>> {
    let now = chrono::Utc::now();
    let since = match query.since.as_deref() {
        Some(raw) => {
            let parsed = chrono::DateTime::parse_from_rfc3339(raw.trim())
                .map_err(|_| SeraError::Validation("since must be an RFC 3339 time".into()))?
                .with_timezone(&chrono::Utc);
            parsed.max(crate::digest::window_start(now, crate::digest::MAX_HOURS))
        }
        None => {
            crate::digest::window_start(now, query.hours.unwrap_or(crate::digest::DEFAULT_HOURS))
        }
    };
    let digest = crate::digest::build(&state.store, &user.id, since)?;
    Ok(Json(crate::digest::to_json(&digest)))
}

/// `POST /v1/monitoring/run`: enqueue one `watch` task for the acting user
/// (same effect as `/watch` in chat).
pub(super) async fn run_all_sources(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let conversation_id = state.store.default_scheduled_conversation_id(&user.id)?;
    let task = state.store.enqueue_task_with_quota(
        &user.id,
        "watch",
        "watcher",
        &serde_json::to_value(ScheduledPayload {
            conversation_id: conversation_id.clone(),
            user_id: user.id.clone(),
        })?,
        5,
        chrono::Utc::now(),
        state.config.users.tasks_per_user_per_day,
    )?;
    state.store.record_audit(
        &user.id,
        "watch.manual_enqueue",
        None,
        Some(&task.id),
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"conversation_id": conversation_id})),
    )?;
    Ok(Json(json!({"task_id": task.id, "kind": "watch"})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{Gateway, ToolRegistry};
    use crate::llm::LlmClient;
    use crate::policy::Policy;
    use crate::runtime::NullNotifier;
    use tower::ServiceExt;

    fn test_app_with_sources() -> (Router, Store) {
        let dir = std::env::temp_dir().join(format!("serabot-mon-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(
            dir.join("config").join("sources.toml"),
            concat!(
                "[[sources]]\n",
                "name = \"admin-enabled\"\n",
                "url = \"https://example.invalid/enabled.xml?key=secret-1\"\n",
                "enabled = true\n\n",
                "[[sources]]\n",
                "name = \"admin-toggled\"\n",
                "url = \"https://example.invalid/toggled.xml?key=secret-2\"\n",
                "enabled = true\n",
            ),
        )
        .unwrap();

        let mut config = Config::default();
        config.api.enabled = true;
        config.api.token_hash = sha256_hex("test-token-that-is-long-enough-123");
        config.llm.model = "test".into();
        config.llm.base_url = "https://example.com/v1".into();
        config.base_dir = dir.clone();
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
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [7u8; 32]).unwrap());
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
    async fn monitoring_sources_reflect_per_user_overlays_and_member_rss() {
        let (app, store) = test_app_with_sources();
        let admin_id = store.admin_user_id().unwrap();
        let member = store
            .create_user("sub-mon-overlay", "mon-overlay@example.example", "Mon")
            .unwrap();
        let admin_token = store
            .mint_session(&admin_id, chrono::Duration::hours(1))
            .unwrap();
        let member_token = store
            .mint_session(&member.id, chrono::Duration::hours(1))
            .unwrap();

        // Member enables one admin source, explicitly disables the other, and adds a member RSS feed.
        store
            .upsert_user_source_overlay(&member.id, "admin-enabled", true, "rss")
            .unwrap();
        store
            .upsert_user_source_overlay(&member.id, "admin-toggled", false, "rss")
            .unwrap();
        store
            .add_member_rss_source(
                &member.id,
                "member-rss",
                "https://example.invalid/member.xml",
            )
            .unwrap();

        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/v1/monitoring/sources")
                    .header("authorization", format!("Bearer {member_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let rows = body["sources"].as_array().unwrap();

        let enabled_row = rows.iter().find(|r| r["name"] == "admin-enabled").unwrap();
        assert_eq!(enabled_row["enabled"], true);
        assert_eq!(enabled_row["own"], false);
        assert!(enabled_row["url"].is_null());

        let disabled_row = rows.iter().find(|r| r["name"] == "admin-toggled").unwrap();
        assert_eq!(disabled_row["enabled"], false);
        assert_eq!(disabled_row["own"], false);
        assert!(disabled_row["url"].is_null());

        let rss_row = rows.iter().find(|r| r["name"] == "member-rss").unwrap();
        assert_eq!(rss_row["enabled"], true);
        assert_eq!(rss_row["own"], true);
        assert_eq!(rss_row["kind"], "rss");
        assert_eq!(rss_row["url"], "https://example.invalid/member.xml");

        // Admin still sees admin TOML defaults and does not see the member's RSS feed.
        let admin_resp = app
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/v1/monitoring/sources")
                    .header("authorization", format!("Bearer {admin_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(admin_resp.status(), StatusCode::OK);
        let admin_bytes = axum::body::to_bytes(admin_resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let admin_body: Value = serde_json::from_slice(&admin_bytes).unwrap();
        let admin_rows = admin_body["sources"].as_array().unwrap();
        assert!(admin_rows.iter().all(|r| r["name"] != "member-rss"));
        assert_eq!(
            admin_rows
                .iter()
                .find(|r| r["name"] == "admin-toggled")
                .unwrap()["enabled"],
            true
        );
    }
}
