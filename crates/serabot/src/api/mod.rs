use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Extension, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use chrono::{DateTime, SecondsFormat, Utc};
use futures::StreamExt;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::warn;

use crate::admin::{
    AdminStore, BotConfigOverlay, BotProfile, ProviderSecrets, connector_secret_roles,
};
use crate::auth::{self, AuthService};
use crate::config::{
    BotConfig, Config, OAuthConnectorSettings, ProviderConfig, build_specialist_bot,
};
use crate::connectors::{ApiKeyProbe, probe_api_key};
use crate::domain::Task;
use crate::error::{Result, SeraError};
use crate::gateway::Decision;
use crate::maker::ArtifactStore;
use crate::memory;
use crate::model_router::ModelSelection;
use crate::onedrive::{
    CONSENT_CALLBACK_PATH, ONEDRIVE_PROVIDER, OneDriveConsent, OneDriveRegistry,
};
use crate::orchestration::{
    BotRole, ConversationKind, safe_bot_label, scoped_conversation_id, validate_conversation_id,
};
use crate::personal::PersonalConnectors;
use crate::runtime::{ApprovalNotice, Notifier, Runtime};
use crate::secrets::SharedSecrets;
use crate::skills::create_user_skill;
use crate::store::{Store, StoredConversation, User, UserRole, UserStatus};
use crate::user_secrets::{ProviderCredential, UserSecrets, validate_provider};
use crate::util::{constant_time_eq, sha256_hex, truncate_chars};

mod accounts;
mod admin;
mod approvals;
mod bot_chats;
mod bots;
mod browser;
mod cards;
mod composio;
mod connectors;
mod conversations;
mod files;
mod jobs;
mod local;
mod logs;
mod marketplace;
mod media;
mod memories;
mod messages;
mod monitoring;
mod preview;
mod profiles;
mod routines;
mod settings;
mod skills;
mod sources;
mod status;
mod tasks;
#[cfg(test)]
mod tests;
mod traces;
mod vault;
mod workflows;

use accounts::*;
use admin::*;
use approvals::*;
use bot_chats::*;
use bots::*;
use browser::*;
use cards::*;
use composio::*;
use connectors::*;
use conversations::*;
use jobs::*;
use logs::*;
use marketplace::*;
use media::*;
use memories::*;
use messages::*;
use monitoring::*;
use preview::*;
use routines::*;
use settings::*;
use skills::*;
use sources::*;
use status::*;
use tasks::*;
use traces::*;
use vault::*;
use workflows::*;

#[derive(Clone)]
pub struct ApiNotifier {
    store: Store,
}

impl ApiNotifier {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl Notifier for ApiNotifier {
    async fn send_text(&self, user_id: &str, conversation_id: &str, text: &str) -> Result<()> {
        self.store.append_event(
            user_id,
            "message",
            &json!({"conversation_id": conversation_id, "text": text}),
        )?;
        Ok(())
    }

    async fn approval_requested(
        &self,
        user_id: &str,
        conversation_id: &str,
        notice: &ApprovalNotice,
    ) -> Result<()> {
        self.store.append_event(
            user_id,
            "approval",
            &json!({
                "conversation_id": conversation_id,
                "approval_id": notice.approval.id,
                "action_id": notice.action.id,
                "tool": notice.approval.tool,
                "summary": notice.approval.summary,
                "destination": notice.action.destination,
                "expires_at": notice.approval.expires_at,
            }),
        )?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct ApiState {
    runtime: Runtime,
    store: Store,
    config: Arc<Config>,
    artifacts: Arc<ArtifactStore>,
    secrets: SharedSecrets,
    /// Per-user envelope-encrypted credential store (phase 4).
    users: Arc<UserSecrets>,
    /// Per-user OneDrive client registry; `None` while `[onedrive]` is off.
    onedrive: Option<Arc<OneDriveRegistry>>,
    /// OneDrive consent (auth code + PKCE); `None` while `[onedrive]` is off.
    onedrive_consent: Option<Arc<OneDriveConsent>>,
    /// Per-user Outlook, Google, and Notion connections.
    personal: Arc<PersonalConnectors>,
    /// Per-user Composio key and linked apps.
    composio: Arc<crate::composio::ComposioService>,
    /// The plugin catalog, loaded and validated at boot.
    marketplace: Vec<crate::marketplace::PluginManifest>,
    /// Per-user GitHub, Todoist, Slack and Exa tokens.
    token_connectors: Arc<crate::token_connectors::TokenConnectors>,
    /// The ElevenLabs MCP service, shared with the tools; `None` while the
    /// "ElevenLabs" connector is off. Sign-in and sign-out drop its session.
    elevenlabs: Option<Arc<crate::elevenlabs_mcp::ElevenLabsMcp>>,
    token_hash: String,
    /// Per-user request buckets for the per-minute limit. Keyed by the
    /// acting user id (never a token); public sign-in routes without an
    /// authenticated user share one `anonymous` bucket.
    rate: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
    rate_limit: usize,
    user_rate_limit: usize,
    poll_rate_limit: usize,
    admin: Option<AdminStore>,
    /// The OIDC sign-in service; `None` while `[auth] enabled = false`, so
    /// every sign-in endpoint fails closed.
    auth: Option<Arc<AuthService>>,
    /// Providers with a device sign-in still running in the background.
    oauth_inflight: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// Per-user server browser; None while [browser] is off.
    browser: Option<Arc<crate::browser::BrowserManager>>,
    /// Model call behind the onboarding interview; None uses the runtime.
    /// Tests script it so no request leaves the process.
    onboarding_drafter: Option<Arc<dyn crate::onboarding::Drafter>>,
    /// Desktop relay for the local folder tools (0.18).
    local: Arc<crate::local_relay::LocalRelay>,
    /// Host sampler and cached storage scan behind GET /v1/admin/status.
    sys_status: Arc<crate::sysstatus::StatusMonitor>,
    /// Card ids with an answer or decline in flight (see `cards::CardClaim`).
    card_claims: Arc<std::sync::Mutex<HashSet<String>>>,
    /// Password manager CLIs for vault references (see `vault_providers`).
    vault_providers: Arc<crate::vault_providers::Providers>,
}

impl ApiState {
    /// Fails fast when sign-in is enabled but misconfigured: a missing auth
    /// client id, a bad redirect, or an unusable provider must stop startup
    /// rather than surface at sign-in time.
    pub fn new(
        runtime: Runtime,
        store: Store,
        config: Arc<Config>,
        artifacts: Arc<ArtifactStore>,
        secrets: SharedSecrets,
        users: Arc<UserSecrets>,
        onedrive: Option<Arc<OneDriveRegistry>>,
    ) -> Result<Self> {
        let auth = if config.auth.enabled {
            let fleet = runtime.fleet();
            Some(Arc::new(
                AuthService::new(&config, store.clone(), &secrets)?.with_fleet(fleet),
            ))
        } else {
            None
        };
        let onedrive_consent =
            OneDriveConsent::new(&config, &secrets, users.clone())?.map(Arc::new);
        let personal = Arc::new(PersonalConnectors::new(&config, &secrets, users.clone())?);
        let composio = Arc::new(crate::composio::ComposioService::new(
            &config,
            users.clone(),
        )?);
        let token_connectors = Arc::new(crate::token_connectors::TokenConnectors::new(
            &config,
            users.clone(),
        )?);
        let (marketplace, plugin_problems) = marketplace::load(&config);
        for problem in &plugin_problems {
            tracing::warn!(%problem, "plugin skipped");
        }
        Ok(Self {
            runtime,
            store,
            artifacts,
            secrets,
            users,
            onedrive,
            onedrive_consent,
            personal,
            composio,
            marketplace,
            token_connectors,
            elevenlabs: None,
            token_hash: config.api.token_hash.to_ascii_lowercase(),
            rate: Arc::new(Mutex::new(HashMap::new())),
            rate_limit: config.server.rate_limit_per_minute.max(1) as usize,
            user_rate_limit: config.server.user_rate_limit_per_minute.max(1) as usize,
            poll_rate_limit: config.server.poll_rate_limit_per_minute.max(1) as usize,
            admin: AdminStore::open(&config).ok(),
            auth,
            oauth_inflight: Arc::default(),
            browser: None,
            onboarding_drafter: None,
            local: crate::local_relay::LocalRelay::new(),
            sys_status: Arc::new(crate::sysstatus::StatusMonitor::new()),
            card_claims: Arc::default(),
            vault_providers: Arc::new(crate::vault_providers::Providers::new(&config.vault)),
            config,
        })
    }

    /// Share the relay the local folder tools use, so the desktop's polls
    /// reach the same queue as the tools' calls.
    pub fn with_local_relay(mut self, local: Arc<crate::local_relay::LocalRelay>) -> Self {
        self.local = local;
        self
    }

    /// Share the per-user connector service the tools already use, so a
    /// disconnect also drops the tools' cached access tokens.
    pub fn with_personal(mut self, personal: Arc<PersonalConnectors>) -> Self {
        self.personal = personal;
        self
    }

    /// Share the ElevenLabs MCP service the tools use, so a sign-in or
    /// sign-out drops that account's cached session and catalog.
    pub fn with_elevenlabs(
        mut self,
        elevenlabs: Option<Arc<crate::elevenlabs_mcp::ElevenLabsMcp>>,
    ) -> Self {
        self.elevenlabs = elevenlabs;
        self
    }

    /// Replace the model call used by the onboarding interview.
    pub fn with_onboarding_drafter(
        mut self,
        drafter: Option<Arc<dyn crate::onboarding::Drafter>>,
    ) -> Self {
        self.onboarding_drafter = drafter;
        self
    }

    /// Attach the per-user browser manager (owner live-view routes).
    pub fn with_browser(mut self, browser: Option<Arc<crate::browser::BrowserManager>>) -> Self {
        self.browser = browser;
        self
    }
}

pub fn router(state: ApiState) -> Router {
    let shared = Arc::new(state);
    let protected = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/status", get(status))
        .route("/v1/usage", get(usage))
        .route("/v1/admin/status", get(admin_status))
        .route("/v1/browser", get(browser_status))
        .route("/v1/browser/screenshot", get(browser_screenshot))
        .route("/v1/browser/takeover", post(browser_takeover))
        .route("/v1/browser/handback", post(browser_handback))
        .route("/v1/browser/input", post(browser_input))
        .route("/v1/browser/close", post(browser_close))
        .route("/v1/browser/sessions", get(sessions_list))
        .route(
            "/v1/browser/sessions/{id}/screenshot",
            get(session_screenshot),
        )
        .route("/v1/browser/sessions/{id}/takeover", post(session_takeover))
        .route("/v1/browser/sessions/{id}/handback", post(session_handback))
        .route("/v1/browser/sessions/{id}/input", post(session_input))
        .route("/v1/browser/sessions/{id}/close", post(session_close))
        .route("/v1/browser/sessions/{id}/open", post(session_open))
        .route("/v1/browser/sessions/{id}/tabs", post(session_tab_open))
        .route(
            "/v1/browser/sessions/{id}/tabs/{tab}/switch",
            post(session_tab_switch),
        )
        .route(
            "/v1/browser/sessions/{id}/tabs/{tab}/close",
            post(session_tab_close),
        )
        .route(
            "/v1/browser/profiles",
            get(profiles_list).post(profile_create),
        )
        .route(
            "/v1/browser/profiles/{id}",
            put(profile_rename).delete(profile_delete),
        )
        .route("/v1/messages", post(send_message))
        .route("/v1/bots", get(list_bots).post(create_bot))
        .route("/v1/marketplace/plugins", get(list_plugins))
        .route(
            "/v1/marketplace/plugins/{slug}",
            get(get_plugin).delete(uninstall_plugin),
        )
        .route(
            "/v1/marketplace/plugins/{slug}/install",
            post(install_plugin),
        )
        .route("/v1/marketplace/plugins/{slug}/enable", post(enable_plugin))
        .route(
            "/v1/marketplace/plugins/{slug}/disable",
            post(disable_plugin),
        )
        .route(
            "/v1/marketplace/plugins/{slug}/config",
            put(put_plugin_config),
        )
        .route(
            "/v1/bots/{bot}/chats",
            get(list_bot_chats).post(create_bot_chat),
        )
        .route("/v1/bots/{bot}/chat-settings", put(put_bot_chat_settings))
        .route("/v1/bots/catalog", get(bot_catalog))
        .route("/v1/bots/catalog/{name}/install", post(install_catalog_bot))
        .route(
            "/v1/bots/{id}",
            get(bot_detail).put(update_bot).delete(delete_bot),
        )
        .route("/v1/skills", get(list_skills).post(create_skill_pack))
        .route(
            "/v1/conversations",
            get(list_conversations).post(create_conversation),
        )
        .route(
            "/v1/conversations/{id}",
            patch(rename_conversation).delete(archive_conversation),
        )
        .route(
            "/v1/conversations/{id}/unarchive",
            post(unarchive_conversation),
        )
        .route("/v1/tasks", get(list_tasks).post(create_task))
        .route("/v1/tasks/prefs", get(get_task_prefs).put(put_task_prefs))
        .route("/v1/tasks/sync/status", get(sync_status))
        .route("/v1/tasks/sync/run", post(run_sync))
        .route("/v1/tasks/sync/oauth/start", post(sync_oauth_start))
        .route("/v1/tasks/sync/oauth/complete", post(sync_oauth_complete))
        .route(
            "/v1/tasks/sync/oauth/{provider}",
            delete(sync_oauth_disconnect),
        )
        .route("/v1/tasks/calendar", get(calendar_events))
        .route("/v1/tasks/{id}", patch(patch_task).delete(delete_task))
        .route("/v1/sources", get(list_sources))
        .route("/v1/sources/rss", post(add_member_rss))
        .route(
            "/v1/sources/{name}",
            patch(patch_source).delete(delete_member_source),
        )
        .route("/v1/memories", get(list_memories).post(add_memory))
        .route("/v1/memories/stats", get(memory_stats))
        .route("/v1/memory/settings", get(memory_settings))
        .route(
            "/v1/memory/mem0/key",
            put(set_mem0_key).delete(delete_mem0_key),
        )
        .route(
            "/v1/memories/{id}",
            patch(patch_memory).delete(delete_memory),
        )
        .route("/v1/vault", get(list_vault))
        .route("/v1/vault/providers", get(list_vault_providers))
        .route(
            "/v1/vault/providers/{id}",
            put(put_vault_provider).delete(delete_vault_provider),
        )
        .route("/v1/vault/providers/{id}/test", post(test_vault_provider))
        .route("/v1/vault/{name}", put(put_vault).delete(delete_vault))
        .route("/v1/providers", get(list_providers))
        .route("/v1/providers/{id}/key", put(set_provider_key))
        .route("/v1/providers/{id}/test", post(test_provider_endpoint))
        .route("/v1/providers/{id}/oauth/start", post(provider_oauth_start))
        .route(
            "/v1/providers/{id}/oauth/complete",
            post(provider_oauth_complete),
        )
        .route(
            "/v1/providers/default-route",
            get(get_default_route).put(set_default_route),
        )
        .route("/v1/providers/{id}", delete(delete_provider_credential))
        .route("/v1/admin/grants", get(list_grants).post(grant_provider))
        .route(
            "/v1/admin/grants/{grantee}/{provider}",
            delete(revoke_grant_endpoint),
        )
        .route("/v1/connectors/onedrive", delete(disconnect_onedrive))
        .route("/v1/connectors/composio", delete(disconnect_composio))
        .route("/v1/connectors/composio/key", put(set_composio_key))
        .route("/v1/connectors/composio/accounts", get(composio_accounts))
        .route(
            "/v1/connectors/composio/accounts/{account}",
            delete(composio_disconnect_account),
        )
        .route("/v1/connectors/composio/toolkits", get(composio_toolkits))
        .route("/v1/connectors/composio/connect", post(composio_connect))
        .route("/v1/connectors/{id}", delete(disconnect_personal))
        .route("/v1/connectors/{id}/start", post(start_personal_consent))
        .route("/v1/connectors/{id}/token", put(set_personal_token))
        .route(
            "/v1/connectors/onedrive/start",
            post(start_onedrive_consent),
        )
        .route("/v1/telegram", get(telegram_status))
        .route(
            "/v1/telegram/link",
            post(start_telegram_link).delete(unlink_telegram),
        )
        .route("/v1/admin/providers", post(admin_add_provider))
        .route("/v1/admin/providers/{id}", delete(admin_remove_provider))
        .route(
            "/v1/admin/providers/{id}/oauth/start",
            post(admin_provider_oauth_start),
        )
        .route(
            "/v1/admin/providers/{id}/oauth/complete",
            post(admin_provider_oauth_complete),
        )
        .route("/v1/admin/connectors/{id}", put(admin_set_connector))
        .route("/v1/admin/connectors/{id}/test", post(admin_test_connector))
        .route(
            "/v1/admin/connectors/{id}/secrets",
            delete(admin_remove_connector_secrets),
        )
        .route(
            "/v1/admin/connectors/{id}/oauth/start",
            post(admin_connector_oauth_start),
        )
        .route(
            "/v1/admin/connectors/{id}/oauth/complete",
            post(admin_connector_oauth_complete),
        )
        .route(
            "/v1/admin/connectors/{id}/oauth",
            delete(admin_connector_oauth_disconnect),
        )
        .route("/v1/admin/audio", put(admin_set_audio))
        .route(
            "/v1/settings/voice",
            get(voice_settings).put(admin_set_voice),
        )
        .route(
            "/v1/conversations/{id}/reply-mode",
            get(get_reply_mode).put(set_reply_mode),
        )
        .route("/v1/admin/bots/{id}", put(admin_set_bot_profile))
        .route("/v1/admin/bots/{id}/config", put(admin_update_bot_config))
        .route("/v1/admin/restart", post(admin_restart))
        .route("/v1/connectors", get(list_connectors))
        .route("/v1/connectors/{id}/test", post(test_connector))
        .route("/v1/models", get(models))
        .route("/v1/conversations/{id}/model", put(select_model))
        .route("/v1/conversations/{id}/thinking", put(set_thinking))
        .route(
            "/v1/conversations/{id}/messages",
            get(conversation_messages),
        )
        .route(
            "/v1/conversations/{id}/team",
            get(conversation_team).put(set_conversation_team),
        )
        .route("/v1/conversations/{id}/library", get(conversation_library))
        .route("/v1/conversations/{id}/plan", get(get_conversation_plan))
        .route(
            "/v1/conversations/{id}/activity",
            get(conversation_activity),
        )
        .route(
            "/v1/settings/turn-limits",
            get(get_turn_limits)
                .put(put_turn_limits)
                .delete(reset_turn_limits),
        )
        .route("/v1/conversations/{id}/cards", get(list_conversation_cards))
        .route(
            "/v1/conversations/{id}/media-jobs",
            get(list_conversation_media_jobs),
        )
        .route("/v1/cards/{id}/answer", post(answer_card))
        .route("/v1/cards/{id}/decline", post(decline_card))
        // A secret value is at most 8 KiB; the route reads no more than 16 KiB.
        .route(
            "/v1/cards/{id}/secret",
            post(submit_secret_card).layer(axum::extract::DefaultBodyLimit::max(
                SECRET_BODY_LIMIT_BYTES,
            )),
        )
        .route("/v1/conversations/{id}/cancel", post(cancel_conversation))
        .route(
            "/v1/conversations/{id}/auto-approve",
            get(get_auto_approve).put(set_auto_approve),
        )
        .route("/v1/tasks/{id}", get(task))
        .route("/v1/approvals", get(list_approvals))
        .route("/v1/approvals/{id}/decide", post(decide_approval))
        .route("/v1/standing-grants", get(list_standing_grants))
        .route(
            "/v1/standing-grants/{id}",
            delete(revoke_standing_grant_endpoint),
        )
        .route("/v1/events", get(events))
        .route("/v1/audit", get(audit))
        .route("/v1/artifacts/{hash}", get(artifact))
        .route("/v1/artifacts/{hash}/preview", get(artifact_preview))
        .route("/v1/artifacts/{hash}/approve", post(approve_artifact))
        .route("/v1/jobs", get(list_jobs))
        .route("/v1/routines", get(list_routines))
        .route(
            "/v1/routines/{name}",
            put(put_routine).delete(delete_routine),
        )
        .route("/v1/routines/{name}/run", post(run_routine))
        .route("/v1/monitoring/sources", get(list_monitoring_sources))
        .route("/v1/digest", get(get_digest))
        .route("/v1/monitoring/run", post(run_all_sources))
        .route("/v1/workflows/active", get(list_active_workflows))
        .route("/v1/traces", get(list_traces))
        .route("/v1/logs", get(list_logs))
        .route("/v1/me", get(auth_me))
        .route("/v1/auth/logout", post(auth_logout))
        .route("/v1/auth/invites", post(auth_invites))
        .route("/v1/users", get(list_users))
        .route("/v1/users/{id}/disable", post(disable_user))
        .route("/v1/users/{id}/enable", post(enable_user))
        .layer(RequestBodyLimitLayer::new(
            shared.config.server.body_limit_bytes,
        ));
    // Voice capture posts raw bytes up to the Telegram voice ceiling; the JSON
    // endpoints keep the smaller server body limit.
    //
    // The route layer alone is not enough: axum's own `DefaultBodyLimit`
    // (2 MiB) is what a `Bytes` extraction actually enforces, so the audio
    // route raises that limit too and the handler reads the body through
    // `to_bytes` with an explicit ceiling of its own.
    let audio = Router::new()
        .route("/v1/audio/transcribe", post(transcribe_audio))
        .layer(axum::extract::DefaultBodyLimit::max(
            crate::audio::DEFAULT_TELEGRAM_AUDIO_MAX_BYTES as usize,
        ));
    // Sign-in entry points are public: the caller by definition has no
    // session token yet. They share the JSON body limit and the per-minute
    // rate limit, but not the bearer-token middleware.
    let public_auth = Router::new()
        .route("/v1/auth/providers", get(auth_providers))
        .route("/v1/auth/login/start", post(auth_login_start))
        .route("/v1/auth/callback", get(auth_callback))
        .route("/v1/auth/handoff", post(auth_handoff))
        .route("/login/microsoft", get(auth_login_microsoft))
        // The OneDrive consent redirect is a browser round trip from
        // Microsoft: no session exists, and the `state` value binds it to the
        // user exactly like the sign-in callback.
        .route(CONSENT_CALLBACK_PATH, get(onedrive_callback))
        .route(
            crate::personal::GOOGLE_CALLBACK_PATH,
            get(personal_callback),
        )
        .route(
            crate::personal::NOTION_CALLBACK_PATH,
            get(personal_callback),
        )
        .layer(RequestBodyLimitLayer::new(
            shared.config.server.body_limit_bytes,
        ))
        .layer(middleware::from_fn_with_state(shared.clone(), rate_limit));
    // `authorize` runs first so the rate limiter can key on the authenticated
    // user; an unknown token is rejected before it can create a bucket.
    let protected = protected
        .merge(audio)
        .merge(profiles::routes())
        .merge(files::routes())
        .merge(local::routes())
        .layer(middleware::from_fn_with_state(shared.clone(), rate_limit))
        .layer(middleware::from_fn_with_state(shared.clone(), authorize));
    crate::web::router()
        .merge(public_auth)
        .merge(protected)
        .layer(TraceLayer::new_for_http())
        .with_state(shared)
}

async fn health(State(state): State<Arc<ApiState>>) -> Response {
    match state.store.health() {
        Ok(()) => Json(json!({
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
            "build": crate::web::build_sha(),
            "audio_enabled": state.config.audio.enabled,
            "audio_transcription_model": state.config.audio.transcription_model,
            "audio_understanding_model": state.config.audio.understanding_model,
            "firecrawl_enabled": state.config.firecrawl.enabled,
            "tavily_enabled": state.config.tavily.enabled,
            "telegram_enabled": state.config.telegram.enabled,
            "catalog_fetch": state.config.llm.catalog_fetch,
            "bot_chat_binding": true,
            "bot_profiles": true,
            "conversation_titles": true,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "error": "database unavailable"})),
        )
            .into_response()
            .tap(|_| warn!(%error, "health check failed")),
    }
}

trait Tap: Sized {
    fn tap(self, f: impl FnOnce(&Self)) -> Self {
        f(&self);
        self
    }
}
impl<T> Tap for T {}

/// Names only. Secret values never leave the age store.
fn secret_status(secrets: &SharedSecrets, name: Option<&str>) -> Value {
    match name.filter(|name| !name.is_empty()) {
        Some(name) => json!({"name": name, "configured": secrets.has(name)}),
        None => json!({"name": Value::Null, "configured": false}),
    }
}

/// The acting user's credential status for one provider: own row, active
/// admin grant, or neither. Presence only — never a value.
fn credential_view(state: &ApiState, user_id: &str, provider: &str) -> Result<Value> {
    let status = state.users.credential_status(user_id, provider)?;
    Ok(json!({
        "configured": status.usable(),
        "own": status.own,
        "granted": status.granted,
    }))
}

/// The identity behind an authenticated request, threaded into handlers
/// through request extensions. Carries only ids and the role — never a raw
/// token (the `token_hash` field is the sha256 the server stores).
#[derive(Clone, Debug)]
pub struct ActingUser {
    pub id: String,
    pub role: UserRole,
    /// The device-token row id when the bearer token is a session; `None`
    /// for the static config token, which has no revocable session row.
    pub session_id: Option<String>,
    pub token_hash: String,
}

impl ActingUser {
    pub fn is_admin(&self) -> bool {
        self.role == UserRole::Admin
    }
}

/// Gate a handler on the admin role. Members receive an explicit 403.
fn require_admin(user: &ActingUser) -> Result<()> {
    if user.role != UserRole::Admin {
        return Err(SeraError::Forbidden(
            "this endpoint requires the admin role".into(),
        ));
    }
    Ok(())
}

/// Resolve the user behind a bearer token, or fail closed.
///
/// The static config token is the operator's own credential and maps to the
/// bootstrap admin. Device tokens map to the owning user recorded on the
/// row (existing operator tokens were backfilled to the admin by migration
/// 0008). Unknown owners, disabled accounts, and revoked or expired tokens
/// all reject identically, so a rejected caller learns nothing about which
/// check failed.
async fn authorize(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Response {
    if !state.config.api.enabled {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "api disabled"})),
        )
            .into_response();
    }
    let Some(header) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return unauthorized();
    };
    let Some(token) = header.strip_prefix("Bearer ") else {
        return unauthorized();
    };
    if token.len() < 32 || token.len() > 512 {
        return unauthorized();
    }
    let candidate = sha256_hex(token);
    if constant_time_eq(&candidate, &state.token_hash) {
        let Some(admin) = active_user(&state, None) else {
            return unauthorized();
        };
        request.extensions_mut().insert(ActingUser {
            id: admin.id,
            role: admin.role,
            session_id: None,
            token_hash: candidate,
        });
        return next.run(request).await;
    }
    let now = chrono::Utc::now();
    match state.store.device_token_by_hash(&candidate) {
        Ok(Some(record)) if record.active_at(now) => {
            let Some(user) = active_user(&state, record.user_id.as_deref()) else {
                return unauthorized();
            };
            if let Err(error) = state.store.touch_device_token(&record.id, now) {
                warn!(%error, "device token last-used update failed");
            }
            request.extensions_mut().insert(ActingUser {
                id: user.id,
                role: user.role,
                session_id: Some(record.id),
                token_hash: candidate,
            });
            next.run(request).await
        }
        Ok(_) => unauthorized(),
        Err(error) => {
            warn!(%error, "device token lookup failed");
            unauthorized()
        }
    }
}

/// The active user behind a token, or `None` when the owner is missing or
/// disabled. A `None` owner id maps to the bootstrap admin (pre-migration
/// rows were backfilled, but the lookup stays explicit either way).
fn active_user(state: &ApiState, user_id: Option<&str>) -> Option<User> {
    let user = match user_id {
        Some(id) => state.store.get_user(id).ok()??,
        None => state.store.admin_user().ok()??,
    };
    (user.status == UserStatus::Active).then_some(user)
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "unauthorized"})),
    )
        .into_response()
}

// ---- Auth endpoints (public entry, protected sessions) ----

/// Per-minute request limit, one bucket per acting user.
///
/// The bucket key is the authenticated user id, so one user cannot exhaust
/// another's budget and multiple sessions of the same user share the
/// configured limit instead of multiplying it. Requests that reach this
/// middleware without an `ActingUser` — the public sign-in routes — share one
/// anonymous bucket. Buckets are pruned on every request, so idle sessions do
/// not accumulate.
async fn rate_limit(State(state): State<Arc<ApiState>>, request: Request, next: Next) -> Response {
    let user = request
        .extensions()
        .get::<ActingUser>()
        .map(|u| u.id.clone());
    let (key, limit) = match user {
        Some(id) if is_poll_request(request.method(), request.uri().path()) => {
            (format!("poll:{id}"), state.poll_rate_limit)
        }
        Some(id) => (id, state.user_rate_limit),
        None => ("anonymous".to_owned(), state.rate_limit),
    };
    {
        let mut buckets = state.rate.lock().await;
        let now = Instant::now();
        let window = Duration::from_secs(60);
        buckets.retain(|_, entries| {
            while entries
                .front()
                .is_some_and(|entry| now.duration_since(*entry) > window)
            {
                entries.pop_front();
            }
            !entries.is_empty()
        });
        let entries = buckets.entry(key).or_default();
        if entries.len() >= limit {
            let retry = entries
                .front()
                .map(|oldest| {
                    window
                        .saturating_sub(now.duration_since(*oldest))
                        .as_secs()
                        .clamp(1, 60)
                })
                .unwrap_or(60);
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", retry.to_string())],
                Json(json!({"error": "rate limit exceeded"})),
            )
                .into_response();
        }
        entries.push_back(now);
    }
    next.run(request).await
}

/// Cheap idempotent polling reads the desktop repeats every few seconds.
/// They get their own bucket so polling can never starve real actions.
fn is_poll_request(method: &axum::http::Method, path: &str) -> bool {
    if method != axum::http::Method::GET {
        return false;
    }
    matches!(
        path,
        "/v1/health"
            | "/v1/browser"
            | "/v1/browser/screenshot"
            | "/v1/browser/sessions"
            | "/v1/browser/profiles"
            | "/v1/events"
            | "/v1/approvals"
    ) || (path.starts_with("/v1/browser/sessions/") && path.ends_with("/screenshot"))
        || path
            .strip_prefix("/v1/conversations/")
            .is_some_and(|rest| rest.ends_with("/messages") && rest.matches('/').count() == 1)
        || path.starts_with("/v1/tasks/")
}

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: String,
    code: &'static str,
}

impl IntoResponse for SeraError {
    fn into_response(self) -> Response {
        if let SeraError::Upstream { code, message, .. } = &self {
            // A bad key, plan, or request is the caller's to fix (422). An
            // outage is a gateway failure (502). A rate limit is a 503 and
            // not 502: the call was never run, so clients must not treat it
            // as an unknown outcome. Never 401: the desktop treats that as an
            // expired session.
            let status = match *code {
                "composio_unavailable" | "connector_unavailable" => StatusCode::BAD_GATEWAY,
                "composio_rate_limited" | "connector_rate_limited" => {
                    StatusCode::SERVICE_UNAVAILABLE
                }
                _ => StatusCode::UNPROCESSABLE_ENTITY,
            };
            return (
                status,
                Json(ApiErrorBody {
                    error: message.clone(),
                    code,
                }),
            )
                .into_response();
        }
        if let SeraError::Rejected { code, message } = &self {
            warn!(code, reason = %message, "api request rejected");
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ApiErrorBody {
                    error: message.clone(),
                    code,
                }),
            )
                .into_response();
        }
        if let SeraError::Validation(text) = &self {
            // Only server-authored static text is shown; any other
            // validation message may carry user content and stays generic.
            // The reason is logged only for the audited static texts.
            match safe_validation_text(text) {
                Some(safe) => {
                    warn!(reason = %safe, "api request failed validation");
                    return (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        Json(ApiErrorBody {
                            error: safe,
                            code: "validation",
                        }),
                    )
                        .into_response();
                }
                None => warn!("api request failed validation (reason withheld)"),
            }
        }
        if let SeraError::External(text) = &self {
            let (code, message) = actionable_external(text);
            warn!(code, "external service failed");
            return (
                StatusCode::BAD_GATEWAY,
                Json(ApiErrorBody {
                    error: message.into(),
                    code,
                }),
            )
                .into_response();
        }
        let (status, code, message) = match &self {
            SeraError::NotFound(detail) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(ApiErrorBody {
                        error: public_not_found(detail),
                        code: "not_found",
                    }),
                )
                    .into_response();
            }
            SeraError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden", "forbidden"),
            SeraError::PolicyViolation => (
                StatusCode::FORBIDDEN,
                "policy_violation",
                "blocked by content policy",
            ),
            SeraError::Validation(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation",
                "invalid request",
            ),
            SeraError::InvalidState(_) => (
                StatusCode::CONFLICT,
                "invalid_state",
                "the requested operation is not valid in the current state",
            ),
            _ => {
                warn!(%self, "api request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal server error",
                )
            }
        };
        (
            status,
            Json(ApiErrorBody {
                error: message.to_string(),
                code,
            }),
        )
            .into_response()
    }
}

/// Validation messages that are fixed server-authored text (no user content,
/// no secrets) and so safe to show. Anything not listed stays generic.
const SAFE_VALIDATION_TEXTS: &[&str] = &[
    "message is empty",
    "message is too long",
    "too many attachments",
    "duplicate attachment",
    "pick a provider and model for the default route",
    "unknown model provider",
    "model is not allowed for this provider",
];

/// Field-level rejections whose wording is fixed in the server (the length
/// limits are constants). Only these prefixes are shown; they name the field
/// that failed and never echo the value that was sent.
const SAFE_VALIDATION_PREFIXES: &[&str] = &[
    "title must be ",
    "title must not ",
    "notes must be ",
    "status must be ",
    "due must be ",
    "sync_provider must be ",
    "provider must be ",
    "refresh_every_turns must be ",
    "pinned must be ",
    "memory kind must be ",
    "category must be ",
    "connector id must be ",
];

/// A gateway failure the owner can act on, without the upstream body.
fn actionable_external(text: &str) -> (&'static str, &'static str) {
    let lower = text.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        (
            "timeout",
            "The service timed out. Wait a moment and try again.",
        )
    } else if lower.contains("rate") {
        (
            "rate_limited",
            "The service is busy. Wait a moment and try again.",
        )
    } else if lower.contains("connect") || lower.contains("dns") || lower.contains("unreachable") {
        (
            "unreachable",
            "Could not reach the service. Check the connection and try again.",
        )
    } else {
        (
            "external",
            "A connected service failed. Try again, or use Test connection on that provider or connector.",
        )
    }
}

/// Name a missing record when the detail is a server-authored id, not user prose.
fn public_not_found(detail: &str) -> String {
    const PREFIXES: &[&str] = &[
        "bot ",
        "tool ",
        "provider ",
        "task ",
        "memory ",
        "connector ",
        "conversation ",
        "media job ",
    ];
    if let Some(rest) = PREFIXES
        .iter()
        .find_map(|prefix| detail.strip_prefix(prefix))
        && !rest.is_empty()
        && rest.len() <= 80
        && rest.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, ' ' | '_' | '-' | ':' | '.')
        })
    {
        return format!("Not found: {detail}. Check the name and try again.");
    }
    "not found".into()
}

fn safe_validation_text(text: &str) -> Option<String> {
    if let Some(exact) = SAFE_VALIDATION_TEXTS
        .iter()
        .copied()
        .find(|candidate| *candidate == text)
    {
        return Some(exact.to_owned());
    }
    let field_message = SAFE_VALIDATION_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix));
    (field_message && text.len() <= 160 && !text.chars().any(char::is_control))
        .then(|| text.to_owned())
}

pub fn sanitize_for_event(value: &str) -> String {
    truncate_chars(value, 2000)
}

#[cfg(test)]
mod error_text_tests {
    use super::*;

    async fn body_of(error: SeraError) -> (StatusCode, Value) {
        let response = error.into_response();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn rejected_errors_carry_their_code_and_message() {
        let (status, body) = body_of(SeraError::Rejected {
            code: "model_unavailable",
            message: "That model isn't available for Mistral right now".into(),
        })
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["code"], "model_unavailable");
        assert_eq!(
            body["error"],
            "That model isn't available for Mistral right now"
        );
    }

    #[tokio::test]
    async fn only_audited_static_validation_text_is_shown() {
        let (_, body) = body_of(SeraError::Validation("message is empty".into())).await;
        assert_eq!(body["error"], "message is empty");
        assert_eq!(body["code"], "validation");
        // Anything else may carry user content and stays generic.
        let (_, body) = body_of(SeraError::Validation("unknown bot secret-name".into())).await;
        assert_eq!(body["error"], "invalid request");
    }
}
