use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::config::{LlmConfig, ProviderConfig};
use crate::domain::LlmMessage;
use crate::error::{Result, SeraError};
use crate::gateway::ToolSpec;
use crate::llm::{LlmClient, LlmResponse};
use crate::providers::{
    AuthPlan, ModelCatalog, auth_plan, canonical_kind, check_profile, resolve_base_url,
    resolve_catalog, valid_model_id,
};
use crate::util::sha256_hex;

/// How long a failed live-catalog fetch is remembered before `ensure_catalogs`
/// asks the provider again.
const ENSURE_RETRY_AFTER: Duration = Duration::from_secs(30);

/// Stored conversation preference when the owner enables automatic routing.
pub const AUTO_PROVIDER: &str = "auto";

/// Resolves and constructs per-user provider clients.
///
/// The router holds the admin-defined *catalog* (endpoints, allowlists,
/// egress consent); credentials are per user and resolved here. The
/// production implementation is `providers::UserProviderCredentials`, backed
/// by the envelope-encrypted user store; tests inject a synthetic resolver.
///
/// The contract is fail-closed: `available` is a row-existence check (no
/// decryption), and `client` errors when the acting user has no credential or
/// the stored one is corrupt. There is no fallback to another user's key.
pub trait ProviderCredentials: Send + Sync {
    /// Whether the acting user can use `provider` (own credential or an
    /// active admin grant). Never returns the credential itself.
    fn available(&self, user_id: &str, provider: &str) -> bool;

    /// Build the client for the acting user from their resolved credential.
    fn client(&self, user_id: &str, provider: &str, profile: &ProviderConfig) -> Result<LlmClient>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSelection {
    pub provider: String,
    pub model: String,
}

impl ModelSelection {
    pub fn auto_mode() -> Self {
        Self {
            provider: AUTO_PROVIDER.into(),
            model: String::new(),
        }
    }

    pub fn is_auto_mode(&self) -> bool {
        self.provider == AUTO_PROVIDER
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderView {
    pub id: String,
    pub models: Vec<String>,
    pub data_egress_approved: bool,
    pub kind: String,
    pub catalog_fetch: bool,
    pub catalog_verified: bool,
    /// Where `models` came from: `live` (the provider's own list for this
    /// account) or `fallback` (the configured allowlist).
    pub model_source: &'static str,
    /// How the last live fetch for this account went: `ok`, `key_rejected`,
    /// `unreachable`, or `unchecked` (no fetch yet, or fetching is off).
    pub status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct CatalogEntry {
    pub provider: String,
    pub model: String,
    pub data_egress_approved: bool,
    pub catalog_verified: bool,
    /// Friendly name, context window and capability tags, when the provider's
    /// own list supplied them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CatalogRefresh {
    pub provider: String,
    pub fetched: bool,
    pub verified: bool,
    pub model_count: usize,
}

/// One account's last good live model list for one provider.
#[derive(Clone)]
struct CatalogSnapshot {
    at: Instant,
    models: ModelCatalog,
}

/// Why the last live fetch for an account failed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FetchFailure {
    KeyRejected,
    Unreachable,
}

/// Per-account live catalogs for one provider. A failed fetch drops that
/// account's snapshot, so routing falls back to the configured allowlist.
#[derive(Default)]
struct CatalogState {
    snapshots: BTreeMap<String, CatalogSnapshot>,
    failures: BTreeMap<String, FetchFailure>,
    /// When each account's last fetch failed (for the retry back-off).
    failed_at: BTreeMap<String, Instant>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinnedRoute {
    pub selection: ModelSelection,
    pub fingerprint: String,
}

#[derive(Clone)]
struct Provider {
    id: String,
    profile: ProviderConfig,
    /// The client built from explicit credentials (tests, and any caller that
    /// passes clients to [`ModelRouter::new`]). `None` when the router serves
    /// per-user credentials.
    client: Option<LlmClient>,
    /// Per-user clients built on demand from `credentials`.
    user_clients: Arc<Mutex<BTreeMap<String, LlmClient>>>,
    /// The router's per-user resolver, cloned onto each provider so a
    /// provider can build its own client cache without borrowing the router.
    credentials: Option<Arc<dyn ProviderCredentials>>,
    models: BTreeSet<String>,
    data_egress_approved: bool,
    fingerprint: String,
    kind: String,
    catalog_fetch: bool,
    catalog_ttl: Duration,
    catalog_url: String,
    catalog_strict: bool,
    catalog: Arc<Mutex<CatalogState>>,
}

#[derive(Clone)]
pub struct ModelRouter {
    providers: BTreeMap<String, Provider>,
    default_model: String,
    classes: BTreeMap<String, Vec<String>>,
    /// `Some` means credentials are resolved per acting user; `None` means
    /// every provider was built with an explicit client (tests).
    credentials: Option<Arc<dyn ProviderCredentials>>,
}

impl std::fmt::Debug for ModelRouter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelRouter")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .field("default_model", &self.default_model)
            .field("per_user_credentials", &self.credentials.is_some())
            .finish()
    }
}

impl ModelRouter {
    /// Build a router from explicit clients. Every configured provider must
    /// have one; used by tests and by callers that resolve credentials
    /// themselves.
    pub fn new(config: &LlmConfig, clients: BTreeMap<String, LlmClient>) -> Result<Self> {
        Self::build(config, clients, None)
    }

    /// Build a router that resolves credentials per acting user.
    ///
    /// The catalog stays admin-defined; a provider the acting user has no
    /// credential for is simply not routable for them.
    pub fn with_credentials(
        config: &LlmConfig,
        credentials: Arc<dyn ProviderCredentials>,
    ) -> Result<Self> {
        Self::build(config, BTreeMap::new(), Some(credentials))
    }

    fn build(
        config: &LlmConfig,
        mut clients: BTreeMap<String, LlmClient>,
        credentials: Option<Arc<dyn ProviderCredentials>>,
    ) -> Result<Self> {
        if config.providers.contains_key("default") {
            return Err(SeraError::Config(
                "provider id 'default' is reserved for [llm]".into(),
            ));
        }
        let default = config.default_provider();
        let mut providers = BTreeMap::new();
        for (id, profile) in std::iter::once(("default", &default)).chain(
            config
                .providers
                .iter()
                .map(|(id, profile)| (id.as_str(), profile)),
        ) {
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(SeraError::Config("invalid provider id".into()));
            }
            let kind = canonical_kind(&profile.kind)?;
            check_profile(
                &kind,
                &profile.base_url,
                &profile.auth,
                &profile.refresh_token_secret,
                &profile.oauth_client_secret_secret,
                profile.catalog_fetch,
                profile.catalog_ttl_secs,
            )?;
            crate::cloud_code::validate_project_id(&kind, &profile.project_id)?;
            let endpoint = resolve_base_url(&kind, &profile.base_url)?;
            if profile.api_key_secret.trim().is_empty() || profile.models.is_empty() {
                return Err(SeraError::Config(
                    "providers require a secret reference and explicit allowed models".into(),
                ));
            }
            for model in &profile.models {
                validate_model(model)?;
            }
            let client = match clients.remove(id) {
                Some(client) => {
                    if client.base_url() != endpoint {
                        return Err(SeraError::Config(
                            "provider client endpoint does not match its configuration".into(),
                        ));
                    }
                    Some(client)
                }
                None if credentials.is_some() => None,
                None => {
                    return Err(SeraError::Config(
                        "configured provider credential/client is missing".into(),
                    ));
                }
            };
            let auth_label = match auth_plan(&kind, &profile.auth)? {
                AuthPlan::ApiKey => "api_key",
                AuthPlan::OAuth { .. } => "oauth",
            };
            let resolved_catalog = if profile.catalog_fetch {
                Some(resolve_catalog(&kind, &endpoint)?)
            } else {
                None
            };
            let catalog_strict = resolved_catalog.as_ref().is_some_and(|item| item.strict);
            let catalog_url = resolved_catalog.map(|item| item.url).unwrap_or_default();
            let fingerprint = sha256_hex(serde_json::to_vec(&(
                &endpoint,
                &profile.api_key_secret,
                profile.data_egress_approved,
                &kind,
                profile.catalog_fetch,
                &catalog_url,
                auth_label,
                &profile.project_id,
            ))?);
            providers.insert(
                id.to_owned(),
                Provider {
                    id: id.to_owned(),
                    profile: profile.clone(),
                    client,
                    user_clients: Arc::new(Mutex::new(BTreeMap::new())),
                    credentials: credentials.clone(),
                    models: profile.models.iter().cloned().collect(),
                    data_egress_approved: profile.data_egress_approved,
                    fingerprint,
                    kind,
                    catalog_fetch: profile.catalog_fetch,
                    catalog_ttl: Duration::from_secs(profile.catalog_ttl_secs.max(1)),
                    catalog_url,
                    catalog_strict,
                    catalog: Arc::new(Mutex::new(CatalogState::default())),
                },
            );
        }
        if !clients.is_empty() {
            return Err(SeraError::Config("unconfigured provider client".into()));
        }
        let mut classes = BTreeMap::new();
        let mut allowed_models = BTreeSet::new();
        for provider in providers.values() {
            allowed_models.extend(provider.models.iter().cloned());
        }
        for (class_name, models) in &config.classes {
            if class_name.is_empty() || class_name.len() > 64 {
                return Err(SeraError::Config("invalid llm class name".into()));
            }
            for model in models {
                validate_model(model)?;
                if !allowed_models.contains(model) {
                    return Err(SeraError::Config(format!(
                        "llm.classes.{class_name} lists model not in the allowlist: {model}"
                    )));
                }
            }
            classes.insert(class_name.clone(), models.clone());
        }

        Ok(Self {
            providers,
            default_model: config.model.clone(),
            classes,
            credentials,
        })
    }

    pub(crate) fn add_default_model(&mut self, model: &str) -> Result<()> {
        validate_model(model)?;
        self.providers
            .get_mut("default")
            .expect("default provider exists")
            .models
            .insert(model.to_owned());
        Ok(())
    }

    /// Drop cached per-user clients for one account (credential change,
    /// disconnect, grant revocation).
    pub fn invalidate_user(&self, user_id: &str) {
        for provider in self.providers.values() {
            provider
                .user_clients
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(user_id);
        }
    }

    /// Drop every cached per-user client. Used when a change can affect more
    /// than one account (an admin grant or the admin's own credential).
    pub fn invalidate_all(&self) {
        for provider in self.providers.values() {
            provider
                .user_clients
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clear();
        }
    }

    /// The providers the acting user can route to, in provider-id order.
    pub fn catalog(&self, user_id: &str) -> Vec<ProviderView> {
        self.providers
            .values()
            .filter(|provider| provider.available(user_id))
            .map(|provider| ProviderView {
                id: provider.id.clone(),
                models: provider.advertised(user_id),
                data_egress_approved: provider.data_egress_approved,
                kind: provider.kind.clone(),
                catalog_fetch: provider.catalog_fetch,
                catalog_verified: provider.catalog_verified(user_id),
                model_source: if provider.live_models(user_id).is_some() {
                    "live"
                } else {
                    "fallback"
                },
                status: provider.fetch_status(user_id),
            })
            .collect()
    }

    pub fn catalog_entries(&self, user_id: &str) -> Vec<CatalogEntry> {
        self.providers
            .values()
            .filter(|provider| provider.available(user_id))
            .flat_map(|provider| {
                let verified = provider.catalog_verified(user_id);
                let live = provider.live_models(user_id);
                provider
                    .advertised(user_id)
                    .into_iter()
                    .map(move |model| {
                        let meta = live
                            .as_ref()
                            .and_then(|catalog| catalog.get(&model))
                            .cloned()
                            .unwrap_or_default();
                        CatalogEntry {
                            provider: provider.id.clone(),
                            model,
                            data_egress_approved: provider.data_egress_approved,
                            catalog_verified: verified,
                            name: meta.name,
                            context_length: meta.context_length,
                            tags: meta.tags,
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Make sure every fetching provider the account can use has a live
    /// snapshot before a route is validated. Bounded by `timeout` per call;
    /// a provider whose fetch failed recently is not asked again until
    /// `ENSURE_RETRY_AFTER` has passed, so a down provider never delays every
    /// message. Never fails: on any problem the allowlist stays in force.
    pub async fn ensure_catalogs(&self, user_id: &str, timeout: Duration) {
        for (id, provider) in &self.providers {
            if !provider.catalog_fetch
                || !provider.data_egress_approved
                || !provider.available(user_id)
                || provider.live_models(user_id).is_some()
                || provider.failed_recently(user_id)
            {
                continue;
            }
            match tokio::time::timeout(timeout, self.refresh_provider(id, user_id)).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    debug!(provider = %id, %error, "catalog refresh failed");
                    provider.record_failure(user_id, FetchFailure::Unreachable);
                }
                Err(_) => {
                    debug!(provider = %id, "catalog refresh timed out");
                    provider.record_failure(user_id, FetchFailure::Unreachable);
                }
            }
        }
    }

    pub async fn refresh_catalogs(&self, user_id: &str) -> Result<Vec<CatalogRefresh>> {
        let mut reports = Vec::new();
        for (id, provider) in &self.providers {
            if !provider.catalog_fetch || !provider.available(user_id) {
                continue;
            }
            reports.push(self.refresh_provider(id, user_id).await?);
        }
        Ok(reports)
    }

    /// Refresh one provider's live list for the acting user and report the
    /// outcome. Used by the providers page "Test" button.
    pub async fn test_provider(&self, user_id: &str, id: &str) -> Result<CatalogRefresh> {
        let provider = self
            .providers
            .get(id)
            .ok_or_else(|| SeraError::Validation("unknown model provider".into()))?;
        if !provider.available(user_id) {
            return Err(no_credential_error(id));
        }
        // A test always asks the provider, never the cache.
        provider.forget_snapshot(user_id);
        self.refresh_provider(id, user_id).await
    }

    pub fn catalog_gate(&self, user_id: &str, selection: &ModelSelection) -> Result<()> {
        let provider = self
            .providers
            .get(&selection.provider)
            .ok_or_else(|| SeraError::Validation("unknown model provider".into()))?;
        if !provider.available(user_id) {
            return Err(no_credential_error(&selection.provider));
        }
        if !provider.catalog_fetch || !provider.data_egress_approved {
            return Ok(());
        }
        if provider.allows(user_id, &selection.model) {
            return Ok(());
        }
        Err(model_unavailable_error(provider, &selection.model))
    }

    async fn refresh_provider(&self, id: &str, user_id: &str) -> Result<CatalogRefresh> {
        let provider = &self.providers[id];
        if !provider.catalog_fetch || !provider.data_egress_approved {
            return Ok(CatalogRefresh {
                provider: id.to_owned(),
                fetched: false,
                verified: false,
                model_count: provider.advertised(user_id).len(),
            });
        }
        if provider.is_fresh(user_id) {
            return Ok(CatalogRefresh {
                provider: id.to_owned(),
                fetched: false,
                verified: true,
                model_count: provider.advertised(user_id).len(),
            });
        }
        if provider.catalog_url.is_empty() {
            provider.record_failure(user_id, FetchFailure::Unreachable);
            return Ok(CatalogRefresh {
                provider: id.to_owned(),
                fetched: true,
                verified: false,
                model_count: provider.advertised(user_id).len(),
            });
        }
        // A client that cannot be built (missing or corrupt credential) is a
        // failed fetch like any other: the allowlist stays in force and the
        // caller is never blocked by it.
        let client = match provider.client_for(id, user_id) {
            Ok(client) => client,
            Err(error) => {
                debug!(provider = id, %error, "catalog client unavailable");
                provider.record_failure(user_id, FetchFailure::KeyRejected);
                return Ok(CatalogRefresh {
                    provider: id.to_owned(),
                    fetched: true,
                    verified: false,
                    model_count: provider.advertised(user_id).len(),
                });
            }
        };
        match client
            .fetch_model_catalog(&provider.catalog_url, provider.catalog_strict)
            .await
        {
            Ok(models) => {
                provider.record_snapshot(user_id, models);
                Ok(CatalogRefresh {
                    provider: id.to_owned(),
                    fetched: true,
                    verified: true,
                    model_count: provider.advertised(user_id).len(),
                })
            }
            Err(error) => {
                // The error text carries a status code at most, never a body
                // or a key; it only decides which message the page shows.
                let text = error.to_string();
                let failure = if text.contains("status 401") || text.contains("status 403") {
                    FetchFailure::KeyRejected
                } else {
                    FetchFailure::Unreachable
                };
                provider.record_failure(user_id, failure);
                Ok(CatalogRefresh {
                    provider: id.to_owned(),
                    fetched: true,
                    verified: false,
                    // The configured allowlist is what is offered now.
                    model_count: provider.advertised(user_id).len(),
                })
            }
        }
    }

    /// The acting user's default route: `[llm]` when they hold its
    /// credential, otherwise the first provider they can use that advertises
    /// the model. Falls back to `default` so the caller gets the explicit
    /// "no credential" error rather than a silent other-user route.
    pub fn default_selection(&self, user_id: &str, bot_model: Option<&str>) -> ModelSelection {
        let model = bot_model.unwrap_or(&self.default_model).to_owned();
        if self
            .providers
            .get("default")
            .is_some_and(|provider| provider.available(user_id))
        {
            return ModelSelection {
                provider: "default".into(),
                model,
            };
        }
        if let Some(selection) = self.find_available_model(user_id, &model) {
            return selection;
        }
        if let Some(provider) = self
            .providers
            .values()
            .find(|provider| provider.available(user_id))
            && let Some(model) = provider.advertised(user_id).into_iter().next()
        {
            return ModelSelection {
                provider: provider.id.clone(),
                model,
            };
        }
        ModelSelection {
            provider: "default".into(),
            model,
        }
    }

    /// Deterministic pick from the acting user's available, egress-approved
    /// provider/model pairs.
    ///
    /// Order: bot `model` override → `llm.bot_models` for the bot → `[llm].model`
    /// on `default` → first approved pair sorted by provider id then model id.
    pub fn autoselect(
        &self,
        user_id: &str,
        bot_model: Option<&str>,
        bot_name: &str,
        bot_models: &BTreeMap<String, String>,
    ) -> Result<ModelSelection> {
        if let Some(model) = bot_model
            && let Some(selection) = self.find_approved_model(user_id, model, Some("default"))
        {
            return Ok(selection);
        }
        if let Some(model) = bot_models.get(bot_name)
            && let Some(selection) = self.find_approved_model(user_id, model, Some("default"))
        {
            return Ok(selection);
        }
        if let Some(selection) =
            self.find_approved_model(user_id, &self.default_model, Some("default"))
        {
            return Ok(selection);
        }
        self.first_approved(user_id).ok_or_else(|| {
            if !self
                .providers
                .values()
                .any(|provider| provider.available(user_id))
            {
                SeraError::Forbidden(
                    "no AI provider credentials are configured for this account; add a provider key or ask the admin for a grant"
                        .into(),
                )
            } else {
                SeraError::Forbidden(
                    "no provider with data-egress approval is configured for autoselect".into(),
                )
            }
        })
    }

    /// Resolve a logical model class (`routine`, `synthesis`) to an approved
    /// route for the acting user.
    ///
    /// When `class` is missing, empty, or has no approved models, falls back to
    /// [`Self::autoselect`].
    pub fn select_class(
        &self,
        user_id: &str,
        class: Option<&str>,
        bot_model: Option<&str>,
        bot_name: &str,
        bot_models: &BTreeMap<String, String>,
    ) -> Result<ModelSelection> {
        let Some(class_name) = class.filter(|name| !name.is_empty()) else {
            return self.autoselect(user_id, bot_model, bot_name, bot_models);
        };
        let Some(models) = self.classes.get(class_name) else {
            return self.autoselect(user_id, bot_model, bot_name, bot_models);
        };
        if models.is_empty() {
            return self.autoselect(user_id, bot_model, bot_name, bot_models);
        }
        if let Some(model) = bot_model
            && models.iter().any(|m| m == model)
        {
            if let Some(selection) = self.find_approved_model(user_id, model, Some("default")) {
                return Ok(selection);
            }
            if let Some(selection) = self.find_approved_model(user_id, model, None) {
                return Ok(selection);
            }
        }
        for model in models {
            if let Some(selection) = self.find_approved_model(user_id, model, Some("default")) {
                return Ok(selection);
            }
            if let Some(selection) = self.find_approved_model(user_id, model, None) {
                return Ok(selection);
            }
        }
        self.autoselect(user_id, bot_model, bot_name, bot_models)
    }

    pub fn resolve_selection(
        &self,
        user_id: &str,
        stored: &ModelSelection,
        bot_model: Option<&str>,
        bot_name: &str,
        bot_models: &BTreeMap<String, String>,
    ) -> Result<ModelSelection> {
        if stored.is_auto_mode() {
            return self.autoselect(user_id, bot_model, bot_name, bot_models);
        }
        self.validate_selection(user_id, stored)?;
        Ok(stored.clone())
    }

    fn find_approved_model(
        &self,
        user_id: &str,
        model: &str,
        prefer_provider: Option<&str>,
    ) -> Option<ModelSelection> {
        if let Some(id) = prefer_provider
            && let Some(provider) = self.providers.get(id)
            && provider.is_routable(user_id, model)
        {
            return Some(ModelSelection {
                provider: id.to_owned(),
                model: model.to_owned(),
            });
        }
        for (id, provider) in &self.providers {
            if provider.is_routable(user_id, model) {
                return Some(ModelSelection {
                    provider: id.clone(),
                    model: model.to_owned(),
                });
            }
        }
        None
    }

    /// A provider the acting user can use whose allowlist contains `model`.
    /// Display/default selection only; routing still gates on egress consent
    /// and the verified catalog.
    fn find_available_model(&self, user_id: &str, model: &str) -> Option<ModelSelection> {
        self.providers
            .values()
            .find(|provider| provider.available(user_id) && provider.allows(user_id, model))
            .map(|provider| ModelSelection {
                provider: provider.id.clone(),
                model: model.to_owned(),
            })
    }

    pub fn autoselect_available(&self, user_id: &str) -> bool {
        self.first_approved(user_id).is_some()
    }

    /// Provider whose configured allowlist contains `model` (sorted provider
    /// order). Display-only routing hint; it does not check egress consent,
    /// credentials, or the verified catalog.
    pub fn provider_for_model(&self, model: &str) -> Option<String> {
        self.providers
            .values()
            .find(|provider| provider.models.contains(model))
            .map(|provider| provider.id.clone())
    }

    /// Whether the provider `provider` lists `model`.
    pub fn provider_serves_model(&self, provider: &str, model: &str) -> bool {
        self.providers
            .values()
            .any(|entry| entry.id == provider && entry.models.contains(model))
    }

    /// Names of the logical routing classes whose model list contains `model`.
    pub fn classes_for_model(&self, model: &str) -> Vec<String> {
        self.classes
            .iter()
            .filter(|(_, models)| models.iter().any(|candidate| candidate == model))
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Fallback route derived from routing classes: the first class (sorted)
    /// that lists `model`, then the first other model in that class. `None`
    /// when no class offers an alternative.
    pub fn fallback_for_model(&self, model: &str) -> Option<ModelSelection> {
        for models in self.classes.values() {
            if models.iter().any(|candidate| candidate == model)
                && let Some(other) = models.iter().find(|candidate| candidate.as_str() != model)
            {
                let provider = self.provider_for_model(other)?;
                return Some(ModelSelection {
                    provider,
                    model: other.clone(),
                });
            }
        }
        None
    }

    pub(crate) fn first_approved(&self, user_id: &str) -> Option<ModelSelection> {
        for (id, provider) in &self.providers {
            for model in provider.advertised(user_id) {
                if provider.is_routable(user_id, &model) {
                    return Some(ModelSelection {
                        provider: id.clone(),
                        model,
                    });
                }
            }
        }
        None
    }

    pub fn validate_selection(&self, user_id: &str, selection: &ModelSelection) -> Result<()> {
        let provider = self
            .providers
            .get(&selection.provider)
            .ok_or_else(|| unknown_provider_error(&selection.provider))?;
        if !provider.available(user_id) {
            return Err(no_credential_error(&selection.provider));
        }
        if !provider.allows(user_id, &selection.model) {
            return Err(model_unavailable_error(provider, &selection.model));
        }
        Ok(())
    }

    pub fn pin(&self, selection: &ModelSelection) -> Result<PinnedRoute> {
        self.validate_selection_config(selection)?;
        let provider = &self.providers[&selection.provider];
        if !provider.data_egress_approved {
            return Err(SeraError::Forbidden(
                "provider data-egress consent is not configured".into(),
            ));
        }
        Ok(PinnedRoute {
            selection: selection.clone(),
            fingerprint: provider.fingerprint.clone(),
        })
    }

    /// Config-only selection validation (no credential check). Used by
    /// [`Self::pin`], which binds the provider configuration; the acting
    /// user's credential is checked when the turn completes.
    fn validate_selection_config(&self, selection: &ModelSelection) -> Result<()> {
        let provider = self
            .providers
            .get(&selection.provider)
            .ok_or_else(|| unknown_provider_error(&selection.provider))?;
        if !provider.accepts_config(&selection.model) {
            return Err(model_unavailable_error(provider, &selection.model));
        }
        Ok(())
    }

    pub fn validate_pinned(&self, route: &PinnedRoute) -> Result<()> {
        let current = self.pin(&route.selection)?;
        if current.fingerprint != route.fingerprint {
            return Err(SeraError::Forbidden(
                "pinned provider configuration changed; owner must resubmit the request".into(),
            ));
        }
        Ok(())
    }

    pub(crate) async fn complete(
        &self,
        user_id: &str,
        route: &PinnedRoute,
        messages: &[LlmMessage],
        tools: &[ToolSpec],
        temperature: Option<f32>,
    ) -> Result<LlmResponse> {
        self.validate_pinned(route)?;
        let provider = &self.providers[&route.selection.provider];
        if !provider.available(user_id) {
            return Err(no_credential_error(&route.selection.provider));
        }
        if provider.catalog_fetch {
            self.refresh_provider(&route.selection.provider, user_id)
                .await?;
            if !provider.verified_contains(user_id, &route.selection.model) {
                return Err(model_unavailable_error(provider, &route.selection.model));
            }
        }
        provider
            .client_for(&route.selection.provider, user_id)?
            .complete(messages, tools, Some(&route.selection.model), temperature)
            .await
    }
}

/// A readable name for a provider in user-facing messages: the configured id,
/// or the provider kind for the unnamed `[llm]` provider.
fn provider_label(provider: &Provider) -> String {
    let raw = if provider.id == "default" && !provider.kind.is_empty() {
        provider.kind.as_str()
    } else {
        provider.id.as_str()
    };
    let mut chars = raw.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => raw.to_owned(),
    }
}

fn unknown_provider_error(provider: &str) -> SeraError {
    warn!(
        provider,
        reason = "unknown_provider",
        "model route rejected"
    );
    SeraError::Rejected {
        code: "unknown_provider",
        message: "That model provider isn't set up on this server".into(),
    }
}

/// The model is not offered by the provider for this account right now. The
/// message names only the provider label, never the model id the caller sent.
fn model_unavailable_error(provider: &Provider, model: &str) -> SeraError {
    warn!(
        provider = %provider.id,
        model,
        reason = "model_unavailable",
        "model route rejected"
    );
    SeraError::Rejected {
        code: "model_unavailable",
        message: format!(
            "That model isn't available for {} right now",
            provider_label(provider)
        ),
    }
}

fn no_credential_error(provider: &str) -> SeraError {
    SeraError::Forbidden(format!(
        "no credential is configured for provider '{provider}'; add a key or ask the admin for a grant"
    ))
}

impl Provider {
    /// The acting user's client: the explicit client when the router was
    /// built with one, otherwise a per-user client built once and cached.
    fn client_for(&self, id: &str, user_id: &str) -> Result<LlmClient> {
        if self.credentials().is_none() {
            return self
                .client
                .clone()
                .ok_or_else(|| SeraError::Internal("the provider client is missing".into()));
        }
        let mut cache = self
            .user_clients
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(client) = cache.get(user_id) {
            return Ok(client.clone());
        }
        let credentials = self
            .credentials()
            .ok_or_else(|| SeraError::Internal("the provider credentials are missing".into()))?;
        let client = credentials.client(user_id, id, &self.profile)?;
        cache.insert(user_id.to_owned(), client.clone());
        Ok(client)
    }

    fn credentials(&self) -> Option<&Arc<dyn ProviderCredentials>> {
        // The router owns the resolver; providers borrow it through the
        // shared `Arc` stored on each provider at construction time.
        self.credentials.as_ref()
    }

    fn available(&self, user_id: &str) -> bool {
        match self.credentials() {
            Some(credentials) => credentials.available(user_id, &self.id),
            None => true,
        }
    }

    fn lock_catalog(&self) -> std::sync::MutexGuard<'_, CatalogState> {
        self.catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// The account's last good live list, however old. `None` when the
    /// provider does not fetch, nothing was fetched yet, or the last fetch
    /// failed (the allowlist is the fallback then).
    fn live_models(&self, user_id: &str) -> Option<ModelCatalog> {
        if !self.catalog_fetch {
            return None;
        }
        self.lock_catalog()
            .snapshots
            .get(user_id)
            .map(|snapshot| snapshot.models.clone())
    }

    fn failed_recently(&self, user_id: &str) -> bool {
        self.lock_catalog()
            .failed_at
            .get(user_id)
            .is_some_and(|at| at.elapsed() < ENSURE_RETRY_AFTER)
    }

    fn is_fresh(&self, user_id: &str) -> bool {
        self.lock_catalog()
            .snapshots
            .get(user_id)
            .is_some_and(|snapshot| snapshot.at.elapsed() < self.catalog_ttl)
    }

    fn catalog_verified(&self, user_id: &str) -> bool {
        self.catalog_fetch && self.data_egress_approved && self.is_fresh(user_id)
    }

    fn record_snapshot(&self, user_id: &str, models: ModelCatalog) {
        let mut state = self.lock_catalog();
        state.failures.remove(user_id);
        state.failed_at.remove(user_id);
        state.snapshots.insert(
            user_id.to_owned(),
            CatalogSnapshot {
                at: Instant::now(),
                models,
            },
        );
    }

    fn record_failure(&self, user_id: &str, failure: FetchFailure) {
        let mut state = self.lock_catalog();
        state.snapshots.remove(user_id);
        state.failures.insert(user_id.to_owned(), failure);
        state.failed_at.insert(user_id.to_owned(), Instant::now());
    }

    fn forget_snapshot(&self, user_id: &str) {
        let mut state = self.lock_catalog();
        state.snapshots.remove(user_id);
        state.failures.remove(user_id);
        state.failed_at.remove(user_id);
    }

    fn fetch_status(&self, user_id: &str) -> &'static str {
        if !self.catalog_fetch {
            return "unchecked";
        }
        let state = self.lock_catalog();
        if state.snapshots.contains_key(user_id) {
            return "ok";
        }
        match state.failures.get(user_id) {
            Some(FetchFailure::KeyRejected) => "key_rejected",
            Some(FetchFailure::Unreachable) => "unreachable",
            None => "unchecked",
        }
    }

    /// Whether `model` may be selected and sent by this account: the live
    /// list when the account has one, otherwise the configured allowlist (no
    /// fetching, or the fetch failed).
    fn allows(&self, user_id: &str, model: &str) -> bool {
        if self.catalog_fetch
            && let Some(snapshot) = self.lock_catalog().snapshots.get(user_id)
        {
            return snapshot.models.contains_key(model);
        }
        self.models.contains(model)
    }

    /// Models shown to the user: exactly the models `pin` plus
    /// `catalog_gate` will accept for a save, so the picker never offers a
    /// model the server then rejects.
    ///
    /// A provider without data-egress approval offers nothing (`pin`
    /// refuses it). Otherwise the offer is the account's live list when it
    /// has one, else the configured allowlist.
    fn advertised(&self, user_id: &str) -> Vec<String> {
        if !self.data_egress_approved {
            return Vec::new();
        }
        match self.live_models(user_id) {
            Some(live) => live.into_keys().collect(),
            None => self.models.iter().cloned().collect(),
        }
    }

    fn verified_contains(&self, user_id: &str, model: &str) -> bool {
        if self.catalog_fetch && !self.data_egress_approved {
            return false;
        }
        self.allows(user_id, model)
    }

    fn is_routable(&self, user_id: &str, model: &str) -> bool {
        self.available(user_id)
            && self.data_egress_approved
            && self.verified_contains(user_id, model)
    }

    /// Account-independent check used when pinning a route: a fetching
    /// provider may serve any well-formed id (the per-account list is checked
    /// by `catalog_gate`); a fixed provider only its allowlist.
    fn accepts_config(&self, model: &str) -> bool {
        if self.catalog_fetch {
            valid_model_id(model)
        } else {
            self.models.contains(model)
        }
    }
}

fn validate_model(model: &str) -> Result<()> {
    if valid_model_id(model) {
        Ok(())
    } else {
        Err(SeraError::Config("invalid configured model id".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderConfig;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The acting user for tests built with explicit clients: the router has
    /// no credential resolver, so every provider is available to any id.
    const TEST_USER: &str = "test-user";

    /// A synthetic per-user credential resolver: one API key per user id.
    struct FakeCredentials {
        keys: BTreeMap<String, String>,
        /// Every client build, so cache hits and invalidation are observable.
        built: std::sync::Mutex<Vec<String>>,
    }

    impl FakeCredentials {
        fn new(keys: &[(&str, &str)]) -> Self {
            Self {
                keys: keys
                    .iter()
                    .map(|(user, key)| ((*user).to_owned(), (*key).to_owned()))
                    .collect(),
                built: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn builds(&self) -> Vec<String> {
            self.built
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        }
    }

    impl ProviderCredentials for FakeCredentials {
        fn available(&self, user_id: &str, _provider: &str) -> bool {
            self.keys.contains_key(user_id)
        }

        fn client(
            &self,
            user_id: &str,
            _provider: &str,
            profile: &ProviderConfig,
        ) -> Result<LlmClient> {
            let key = self.keys.get(user_id).ok_or_else(|| {
                SeraError::Forbidden("no credential is configured for this provider".into())
            })?;
            self.built
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(user_id.to_owned());
            let endpoint = resolve_base_url(&profile.kind, &profile.base_url)?;
            LlmClient::new(endpoint, key.clone(), 5, 100)
        }
    }

    #[tokio::test]
    async fn credentials_resolve_per_user_and_never_fall_back() {
        let server = MockServer::start().await;
        let config = LlmConfig {
            base_url: server.uri(),
            model: "shared-model".into(),
            data_egress_approved: true,
            ..LlmConfig::default()
        };
        let credentials = Arc::new(FakeCredentials::new(&[("alice", "alice-key")]));
        let router = ModelRouter::with_credentials(&config, credentials.clone()).unwrap();

        // Availability is per user: Bob sees no provider at all.
        assert_eq!(router.catalog("alice").len(), 1);
        assert!(router.catalog("bob").is_empty());
        assert!(router.autoselect_available("alice"));
        assert!(!router.autoselect_available("bob"));
        let error = router
            .autoselect("bob", None, "main", &BTreeMap::new())
            .unwrap_err();
        assert!(error.to_string().contains("no AI provider credentials"));
        let error = router
            .validate_selection(
                "bob",
                &ModelSelection {
                    provider: "default".into(),
                    model: "shared-model".into(),
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("no credential"));

        // Alice routes with her own key; the mock accepts only that key.
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer alice-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"choices":[{"message":{"content":"ok"}}]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let selection = router
            .autoselect("alice", None, "main", &BTreeMap::new())
            .unwrap();
        let route = router.pin(&selection).unwrap();
        router
            .complete("alice", &route, &[LlmMessage::user("synthetic")], &[], None)
            .await
            .unwrap();
        assert_eq!(credentials.builds(), vec!["alice".to_owned()]);

        // Bob cannot complete the same pinned route: no request is sent and no
        // client is built for him.
        let error = router
            .complete("bob", &route, &[LlmMessage::user("synthetic")], &[], None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no credential"));
        assert_eq!(credentials.builds(), vec!["alice".to_owned()]);
    }

    #[tokio::test]
    async fn per_user_clients_are_cached_and_invalidation_rebuilds_them() {
        let server = MockServer::start().await;
        let config = LlmConfig {
            base_url: server.uri(),
            model: "shared-model".into(),
            data_egress_approved: true,
            ..LlmConfig::default()
        };
        let credentials = Arc::new(FakeCredentials::new(&[
            ("alice", "alice-key"),
            ("bob", "bob-key"),
        ]));
        let router = ModelRouter::with_credentials(&config, credentials.clone()).unwrap();
        let route = |user: &str| {
            let selection = router
                .autoselect(user, None, "main", &BTreeMap::new())
                .unwrap();
            router.pin(&selection).unwrap()
        };
        // Two calls for Alice build one client; Bob gets his own.
        for user in ["alice", "alice", "bob"] {
            let _ = router
                .complete(
                    user,
                    &route(user),
                    &[LlmMessage::user("synthetic")],
                    &[],
                    None,
                )
                .await;
        }
        assert_eq!(
            credentials.builds(),
            vec!["alice".to_owned(), "bob".to_owned()]
        );
        router.invalidate_user("alice");
        let _ = router
            .complete(
                "alice",
                &route("alice"),
                &[LlmMessage::user("synthetic")],
                &[],
                None,
            )
            .await;
        assert_eq!(
            credentials.builds(),
            vec!["alice".to_owned(), "bob".to_owned(), "alice".to_owned()]
        );
        router.invalidate_all();
        let _ = router
            .complete(
                "bob",
                &route("bob"),
                &[LlmMessage::user("synthetic")],
                &[],
                None,
            )
            .await;
        assert_eq!(credentials.builds().len(), 4);
    }

    async fn fixture() -> (
        LlmConfig,
        BTreeMap<String, LlmClient>,
        MockServer,
        MockServer,
    ) {
        let first = MockServer::start().await;
        let second = MockServer::start().await;
        let config = LlmConfig {
            base_url: first.uri(),
            model: "first-model".into(),
            data_egress_approved: true,
            providers: BTreeMap::from([(
                "second".into(),
                ProviderConfig {
                    base_url: second.uri(),
                    api_key_secret: "second_key".into(),
                    models: vec!["second-model".into()],
                    data_egress_approved: true,
                    ..ProviderConfig::default()
                },
            )]),
            ..LlmConfig::default()
        };
        let clients = BTreeMap::from([
            (
                "default".into(),
                LlmClient::new(first.uri(), "first-key", 1, 100).unwrap(),
            ),
            (
                "second".into(),
                LlmClient::new(second.uri(), "second-key", 1, 100).unwrap(),
            ),
        ]);
        (config, clients, first, second)
    }

    #[tokio::test]
    async fn explicit_routes_send_only_their_own_key_and_model() {
        let (config, clients, first, second) = fixture().await;
        for (server, key, model) in [
            (&first, "first-key", "first-model"),
            (&second, "second-key", "second-model"),
        ] {
            Mock::given(method("POST"))
                .and(header("authorization", format!("Bearer {key}")))
                .and(body_partial_json(json!({"model": model})))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"choices":[{"message":{"content":"ok"}}]})),
                )
                .expect(1)
                .mount(server)
                .await;
        }
        let router = ModelRouter::new(&config, clients).unwrap();
        for (provider, model) in [("default", "first-model"), ("second", "second-model")] {
            let route = router
                .pin(&ModelSelection {
                    provider: provider.into(),
                    model: model.into(),
                })
                .unwrap();
            router
                .complete(
                    TEST_USER,
                    &route,
                    &[LlmMessage::user("synthetic")],
                    &[],
                    None,
                )
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn pinned_routes_recheck_allowlist_and_consent_on_every_call() {
        let (mut config, clients, first, second) = fixture().await;
        let router = ModelRouter::new(&config, clients.clone()).unwrap();
        let route = router
            .pin(&ModelSelection {
                provider: "second".into(),
                model: "second-model".into(),
            })
            .unwrap();
        config
            .providers
            .get_mut("second")
            .unwrap()
            .data_egress_approved = false;
        let revoked = ModelRouter::new(&config, clients.clone()).unwrap();
        assert!(
            revoked
                .complete(
                    TEST_USER,
                    &route,
                    &[LlmMessage::user("synthetic")],
                    &[],
                    None
                )
                .await
                .is_err()
        );
        let mut invalid = route.clone();
        invalid.selection.model = "not-allowed".into();
        assert!(
            router
                .complete(
                    TEST_USER,
                    &invalid,
                    &[LlmMessage::user("synthetic")],
                    &[],
                    None
                )
                .await
                .is_err()
        );
        invalid.selection.provider = "unknown".into();
        assert!(
            router
                .complete(
                    TEST_USER,
                    &invalid,
                    &[LlmMessage::user("synthetic")],
                    &[],
                    None
                )
                .await
                .is_err()
        );
        assert!(first.received_requests().await.unwrap().is_empty());
        assert!(second.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_routes_and_unapproved_egress_never_send() {
        let (mut config, clients, first, second) = fixture().await;
        config
            .providers
            .get_mut("second")
            .unwrap()
            .data_egress_approved = false;
        let router = ModelRouter::new(&config, clients).unwrap();
        for (provider, model) in [
            ("unknown", "first-model"),
            ("default", "unknown"),
            ("second", "second-model"),
        ] {
            assert!(
                router
                    .pin(&ModelSelection {
                        provider: provider.into(),
                        model: model.into()
                    })
                    .is_err()
            );
        }
        assert_eq!(first.received_requests().await.unwrap().len(), 0);
        assert_eq!(second.received_requests().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn endpoint_or_secret_reference_changes_reject_pinned_routes() {
        let (mut config, clients, first, second) = fixture().await;
        let router = ModelRouter::new(&config, clients.clone()).unwrap();
        let route = router
            .pin(&router.default_selection(TEST_USER, None))
            .unwrap();
        config.api_key_secret = "rotated_reference".into();
        assert!(
            ModelRouter::new(&config, clients.clone())
                .unwrap()
                .validate_pinned(&route)
                .is_err()
        );
        config.api_key_secret = LlmConfig::default().api_key_secret;
        config.base_url = second.uri();
        let mut changed = clients;
        changed.insert(
            "default".into(),
            LlmClient::new(second.uri(), "first-key", 1, 100).unwrap(),
        );
        assert!(
            ModelRouter::new(&config, changed)
                .unwrap()
                .validate_pinned(&route)
                .is_err()
        );
        assert!(first.received_requests().await.unwrap().is_empty());
        assert!(second.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn autoselect_prefers_bot_override_then_default_provider_model() {
        let (config, clients, _, _) = fixture().await;
        let router = ModelRouter::new(&config, clients).unwrap();
        let mut bot_models = BTreeMap::new();
        bot_models.insert("main".into(), "first-model".into());
        assert_eq!(
            router
                .autoselect(TEST_USER, Some("second-model"), "other", &bot_models)
                .unwrap(),
            ModelSelection {
                provider: "second".into(),
                model: "second-model".into(),
            }
        );
        assert_eq!(
            router
                .autoselect(TEST_USER, None, "main", &bot_models)
                .unwrap(),
            ModelSelection {
                provider: "default".into(),
                model: "first-model".into(),
            }
        );
        assert_eq!(
            router
                .autoselect(TEST_USER, None, "other", &BTreeMap::new())
                .unwrap(),
            ModelSelection {
                provider: "default".into(),
                model: "first-model".into(),
            }
        );
    }

    #[tokio::test]
    async fn select_class_without_config_matches_autoselect() {
        let (config, clients, _, _) = fixture().await;
        let router = ModelRouter::new(&config, clients).unwrap();
        let bot_models = BTreeMap::new();
        assert_eq!(
            router
                .select_class(TEST_USER, None, None, "other", &bot_models)
                .unwrap(),
            router
                .autoselect(TEST_USER, None, "other", &bot_models)
                .unwrap()
        );
        assert_eq!(
            router
                .select_class(TEST_USER, Some("missing"), None, "other", &bot_models)
                .unwrap(),
            router
                .autoselect(TEST_USER, None, "other", &bot_models)
                .unwrap()
        );
    }

    #[tokio::test]
    async fn select_class_skips_unapproved_models_in_class() {
        let (mut config, clients, _, _) = fixture().await;
        config.classes.insert(
            "routine".into(),
            vec!["second-model".into(), "first-model".into()],
        );
        config
            .providers
            .get_mut("second")
            .unwrap()
            .data_egress_approved = false;
        let router = ModelRouter::new(&config, clients).unwrap();
        assert_eq!(
            router
                .select_class(TEST_USER, Some("routine"), None, "other", &BTreeMap::new())
                .unwrap(),
            ModelSelection {
                provider: "default".into(),
                model: "first-model".into(),
            }
        );
    }

    #[tokio::test]
    async fn autoselect_never_picks_unapproved_providers() {
        let (mut config, clients, _, _) = fixture().await;
        config.data_egress_approved = false;
        config
            .providers
            .get_mut("second")
            .unwrap()
            .data_egress_approved = false;
        let router = ModelRouter::new(&config, clients).unwrap();
        assert!(
            router
                .autoselect(TEST_USER, None, "main", &BTreeMap::new())
                .is_err()
        );
    }

    #[tokio::test]
    async fn requires_credentials_matching_endpoints_and_safe_provider_ids() {
        let (mut config, mut clients, _, _) = fixture().await;
        assert!(ModelRouter::new(&config, BTreeMap::new()).is_err());
        let client = clients.remove("second").unwrap();
        let profile = config.providers.remove("second").unwrap();
        config.providers.insert("bad/id".into(), profile);
        clients.insert("bad/id".into(), client);
        assert!(ModelRouter::new(&config, clients).is_err());
    }

    fn chat_ok() -> serde_json::Value {
        json!({"choices":[{"message":{"content":"ok"}}]})
    }

    #[tokio::test]
    async fn fresh_live_catalog_is_authoritative_over_the_allowlist() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"id": "kept"}, {"id": "extra"}, {"id": " bad"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(chat_ok()))
            .expect(1)
            .mount(&server)
            .await;
        let config = LlmConfig {
            base_url: server.uri(),
            model: "kept".into(),
            data_egress_approved: true,
            kind: "openai-compatible".into(),
            catalog_fetch: true,
            catalog_ttl_secs: 60,
            models: vec!["missing".into()],
            ..LlmConfig::default()
        };
        let clients = BTreeMap::from([(
            "default".into(),
            LlmClient::new(server.uri(), "synthetic-key", 1, 100).unwrap(),
        )]);
        let router = ModelRouter::new(&config, clients).unwrap();
        let reports = router.refresh_catalogs(TEST_USER).await.unwrap();
        assert_eq!(reports.len(), 1);
        assert!(reports[0].fetched && reports[0].verified);
        // The refresh report counts the live list (" bad" was dropped by the
        // catalog parser).
        assert_eq!(reports[0].model_count, 2);
        // A fresh verified snapshot offers the live models, not the
        // allowlist ("missing" is allowlisted only).
        assert_eq!(
            router.catalog(TEST_USER)[0].models,
            vec!["extra".to_string(), "kept".to_string()]
        );
        assert_eq!(router.catalog(TEST_USER)[0].model_source, "live");
        assert_eq!(router.catalog(TEST_USER)[0].status, "ok");
        assert!(router.catalog(TEST_USER)[0].catalog_verified);
        // The gate rejects what the live list lacks: "missing" is
        // allowlisted but absent from the snapshot.
        assert!(
            router
                .catalog_gate(
                    TEST_USER,
                    &ModelSelection {
                        provider: "default".into(),
                        model: "missing".into(),
                    }
                )
                .is_err()
        );
        router
            .catalog_gate(
                TEST_USER,
                &ModelSelection {
                    provider: "default".into(),
                    model: "extra".into(),
                },
            )
            .unwrap();
        let kept = router
            .pin(&ModelSelection {
                provider: "default".into(),
                model: "kept".into(),
            })
            .unwrap();
        router
            .complete(
                TEST_USER,
                &kept,
                &[LlmMessage::user("synthetic")],
                &[],
                None,
            )
            .await
            .unwrap();
        let missing = router
            .pin(&ModelSelection {
                provider: "default".into(),
                model: "missing".into(),
            })
            .unwrap();
        assert!(
            router
                .complete(
                    TEST_USER,
                    &missing,
                    &[LlmMessage::user("synthetic")],
                    &[],
                    None
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn failed_catalog_refresh_falls_back_to_the_allowlist() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(chat_ok()))
            .expect(1)
            .mount(&server)
            .await;
        let config = LlmConfig {
            base_url: server.uri(),
            model: "kept".into(),
            data_egress_approved: true,
            catalog_fetch: true,
            catalog_ttl_secs: 60,
            ..LlmConfig::default()
        };
        let clients = BTreeMap::from([(
            "default".into(),
            LlmClient::new(server.uri(), "synthetic-key", 1, 100).unwrap(),
        )]);
        let router = ModelRouter::new(&config, clients).unwrap();
        let reports = router.refresh_catalogs(TEST_USER).await.unwrap();
        assert!(reports[0].fetched && !reports[0].verified);
        // No live list: the configured allowlist is what is offered, and it
        // is not reported as verified.
        assert_eq!(
            router.catalog(TEST_USER)[0].models,
            vec!["kept".to_string()]
        );
        assert_eq!(router.catalog(TEST_USER)[0].model_source, "fallback");
        assert_eq!(router.catalog(TEST_USER)[0].status, "unreachable");
        assert!(!router.catalog(TEST_USER)[0].catalog_verified);
        let route = router
            .pin(&ModelSelection {
                provider: "default".into(),
                model: "kept".into(),
            })
            .unwrap();
        router
            .complete(
                TEST_USER,
                &route,
                &[LlmMessage::user("synthetic")],
                &[],
                None,
            )
            .await
            .unwrap();
        // A model outside the allowlist is still refused.
        assert!(
            router
                .catalog_gate(
                    TEST_USER,
                    &ModelSelection {
                        provider: "default".into(),
                        model: "other".into(),
                    }
                )
                .is_err()
        );
    }

    #[tokio::test]
    async fn unapproved_catalog_fetch_does_not_leave_the_host() {
        let server = MockServer::start().await;
        let config = LlmConfig {
            base_url: server.uri(),
            model: "kept".into(),
            data_egress_approved: false,
            catalog_fetch: true,
            catalog_ttl_secs: 60,
            ..LlmConfig::default()
        };
        let clients = BTreeMap::from([(
            "default".into(),
            LlmClient::new(server.uri(), "synthetic-key", 1, 100).unwrap(),
        )]);
        let router = ModelRouter::new(&config, clients).unwrap();
        let reports = router.refresh_catalogs(TEST_USER).await.unwrap();
        assert!(!reports[0].fetched);
        // No egress consent: `pin` refuses everything, so nothing is offered.
        assert!(router.catalog(TEST_USER)[0].models.is_empty());
        assert!(!router.catalog(TEST_USER)[0].catalog_verified);
        assert!(server.received_requests().await.unwrap().is_empty());
        assert!(
            router
                .pin(&ModelSelection {
                    provider: "default".into(),
                    model: "kept".into(),
                })
                .is_err()
        );
    }

    #[tokio::test]
    async fn failed_refresh_drops_the_stale_live_list_for_the_allowlist() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(503))
            .with_priority(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "kept"}]})),
            )
            .with_priority(1)
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let config = LlmConfig {
            base_url: server.uri(),
            model: "kept".into(),
            data_egress_approved: true,
            catalog_fetch: true,
            catalog_ttl_secs: 1,
            models: vec!["fallback-model".into()],
            ..LlmConfig::default()
        };
        let clients = BTreeMap::from([(
            "default".into(),
            LlmClient::new(server.uri(), "synthetic-key", 1, 100).unwrap(),
        )]);
        let router = ModelRouter::new(&config, clients).unwrap();
        router.refresh_catalogs(TEST_USER).await.unwrap();
        // Fresh snapshot: the live models.
        assert_eq!(
            router.catalog(TEST_USER)[0].models,
            vec!["kept".to_string()]
        );
        assert!(router.catalog(TEST_USER)[0].catalog_verified);
        tokio::time::sleep(Duration::from_millis(1100)).await;
        router.refresh_catalogs(TEST_USER).await.unwrap();
        // Stale snapshot + failed refresh: the live list is dropped and the
        // configured allowlist is offered, unverified.
        assert_eq!(
            router.catalog(TEST_USER)[0].models,
            vec!["fallback-model".to_string(), "kept".to_string()]
        );
        assert_eq!(router.catalog(TEST_USER)[0].model_source, "fallback");
        assert!(!router.catalog(TEST_USER)[0].catalog_verified);
    }

    #[tokio::test]
    async fn minimax_catalog_offers_the_live_models_and_rejects_loose_shapes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer synthetic-m-plan"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "list",
                "data": [
                    {"id": "MiniMax-M3.1-Flash", "object": "model", "owned_by": "minimax"},
                    {"id": "MiniMax-M3", "object": "model", "owned_by": "minimax"},
                    {"id": "not/an id", "object": "model", "owned_by": "synthetic-catalog-secret"}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let base = format!("{}/v1", server.uri());
        let config = LlmConfig {
            base_url: base.clone(),
            model: "MiniMax-M3.1-Flash".into(),
            data_egress_approved: true,
            kind: "minimax".into(),
            catalog_fetch: true,
            catalog_ttl_secs: 60,
            models: vec!["MiniMax-M3".into(), "MiniMax-M2".into()],
            ..LlmConfig::default()
        };
        let clients = BTreeMap::from([(
            "default".into(),
            LlmClient::new(&base, "synthetic-m-plan", 1, 100).unwrap(),
        )]);
        let router = ModelRouter::new(&config, clients).unwrap();
        let reports = router.refresh_catalogs(TEST_USER).await.unwrap();
        assert!(reports[0].fetched && reports[0].verified);
        assert_eq!(reports[0].model_count, 2);
        assert_eq!(
            router.catalog(TEST_USER)[0].models,
            vec!["MiniMax-M3".to_string(), "MiniMax-M3.1-Flash".to_string()]
        );
        assert!(
            router
                .catalog_gate(
                    TEST_USER,
                    &ModelSelection {
                        provider: "default".into(),
                        model: "MiniMax-M2".into(),
                    }
                )
                .is_err()
        );

        let loose = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "models": [{"id": "MiniMax-M3.1-Flash"}]
            })))
            .mount(&loose)
            .await;
        let loose_base = format!("{}/v1", loose.uri());
        let loose_config = LlmConfig {
            base_url: loose_base.clone(),
            model: "MiniMax-M3.1-Flash".into(),
            data_egress_approved: true,
            kind: "mistral".into(),
            catalog_fetch: true,
            catalog_ttl_secs: 60,
            ..LlmConfig::default()
        };
        let loose_router = ModelRouter::new(
            &loose_config,
            BTreeMap::from([(
                "default".into(),
                LlmClient::new(&loose_base, "synthetic-key", 1, 100).unwrap(),
            )]),
        )
        .unwrap();
        let loose_reports = loose_router.refresh_catalogs(TEST_USER).await.unwrap();
        assert!(loose_reports[0].fetched && !loose_reports[0].verified);
        // A loose shape is refused for a strict provider: the allowlist is
        // offered instead, unverified.
        assert_eq!(
            loose_router.catalog(TEST_USER)[0].models,
            vec!["MiniMax-M3.1-Flash".to_string()]
        );
        assert_eq!(loose_router.catalog(TEST_USER)[0].model_source, "fallback");
        assert!(!loose_router.catalog(TEST_USER)[0].catalog_verified);
    }

    #[tokio::test]
    async fn deepseek_catalog_uses_host_root_on_loopback_fixtures() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "list",
                "data": [
                    {"id": "deepseek-flash", "object": "model", "owned_by": "deepseek"},
                    {"id": "deepseek-v4-pro", "object": "model", "owned_by": "deepseek"}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let config = LlmConfig {
            base_url: server.uri(),
            model: "deepseek-flash".into(),
            data_egress_approved: true,
            kind: "deepseek".into(),
            catalog_fetch: true,
            catalog_ttl_secs: 60,
            models: vec!["deepseek-chat".into()],
            ..LlmConfig::default()
        };
        let clients = BTreeMap::from([(
            "default".into(),
            LlmClient::new(server.uri(), "synthetic-key", 1, 100).unwrap(),
        )]);
        let router = ModelRouter::new(&config, clients).unwrap();
        router.refresh_catalogs(TEST_USER).await.unwrap();
        // Fresh snapshot: the live list ("deepseek-chat" is allowlisted but
        // not live).
        assert_eq!(
            router.catalog(TEST_USER)[0].models,
            vec!["deepseek-flash".to_string(), "deepseek-v4-pro".to_string()]
        );
        let paths: Vec<_> = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .map(|request| request.url.path().to_owned())
            .collect();
        assert_eq!(paths, vec!["/models".to_string()]);
    }

    #[tokio::test]
    async fn antigravity_catalog_posts_to_cloud_code_and_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:fetchAvailableModels"))
            .and(header("authorization", "Bearer synthetic-antigravity"))
            .and(body_partial_json(json!({"project": "proj-synthetic"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "models": {
                    "kept-model": {},
                    "extra-model": {},
                    "bad/id": {"note": "synthetic-catalog-secret"}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let base = server.uri();
        let config = LlmConfig {
            base_url: base.clone(),
            model: "kept-model".into(),
            data_egress_approved: true,
            kind: "google-antigravity".into(),
            catalog_fetch: true,
            catalog_ttl_secs: 60,
            project_id: "proj-synthetic".into(),
            models: vec!["missing-model".into()],
            ..LlmConfig::default()
        };
        let clients = BTreeMap::from([(
            "default".into(),
            LlmClient::new(&base, "synthetic-antigravity", 1, 100)
                .unwrap()
                .with_cloud_code("proj-synthetic")
                .unwrap(),
        )]);
        let router = ModelRouter::new(&config, clients).unwrap();
        let reports = router.refresh_catalogs(TEST_USER).await.unwrap();
        assert!(reports[0].fetched && reports[0].verified);
        assert_eq!(reports[0].model_count, 2);
        assert_eq!(
            router.catalog(TEST_USER)[0].models,
            vec!["extra-model".to_string(), "kept-model".to_string()]
        );
        assert!(
            router
                .catalog_gate(
                    TEST_USER,
                    &ModelSelection {
                        provider: "default".into(),
                        model: "missing-model".into(),
                    }
                )
                .is_err()
        );

        let loose = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:fetchAvailableModels"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "list",
                "data": [{"id": "kept-model"}],
                "note": "synthetic-catalog-secret"
            })))
            .mount(&loose)
            .await;
        let loose_router = ModelRouter::new(
            &LlmConfig {
                base_url: loose.uri(),
                model: "kept-model".into(),
                data_egress_approved: true,
                kind: "google-antigravity".into(),
                catalog_fetch: true,
                catalog_ttl_secs: 60,
                ..LlmConfig::default()
            },
            BTreeMap::from([(
                "default".into(),
                LlmClient::new(loose.uri(), "synthetic-key", 1, 100)
                    .unwrap()
                    .with_cloud_code("")
                    .unwrap(),
            )]),
        )
        .unwrap();
        let loose_reports = loose_router.refresh_catalogs(TEST_USER).await.unwrap();
        assert!(loose_reports[0].fetched && !loose_reports[0].verified);
        // The loose shape is refused: the allowlist is offered, unverified.
        assert_eq!(loose_router.catalog(TEST_USER)[0].model_source, "fallback");
        assert!(!loose_router.catalog(TEST_USER)[0].catalog_verified);
    }

    #[tokio::test]
    async fn live_list_replaces_the_allowlist_and_unapproved_egress_offers_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"id": "live-a"}, {"id": "live-b"}]
            })))
            .mount(&server)
            .await;
        let base_config = |egress: bool| LlmConfig {
            base_url: server.uri(),
            model: "allow-a".into(),
            data_egress_approved: egress,
            kind: "openai-compatible".into(),
            catalog_fetch: true,
            catalog_ttl_secs: 60,
            models: vec!["allow-b".into()],
            ..LlmConfig::default()
        };
        let clients = || {
            BTreeMap::from([(
                "default".into(),
                LlmClient::new(server.uri(), "synthetic-key", 1, 100).unwrap(),
            )])
        };
        // Egress not approved: no fetch, nothing offered.
        let router = ModelRouter::new(&base_config(false), clients()).unwrap();
        router.refresh_catalogs(TEST_USER).await.unwrap();
        assert!(router.catalog(TEST_USER)[0].models.is_empty());
        assert!(!router.catalog(TEST_USER)[0].catalog_verified);
        // Fresh verified snapshot with no overlap: the live list is offered.
        let router = ModelRouter::new(&base_config(true), clients()).unwrap();
        router.refresh_catalogs(TEST_USER).await.unwrap();
        assert_eq!(
            router.catalog(TEST_USER)[0].models,
            vec!["live-a".to_string(), "live-b".to_string()]
        );
        assert!(router.catalog(TEST_USER)[0].catalog_verified);
        // A live model outside the allowlist passes; an allowlisted model
        // absent from the live list is rejected.
        router
            .catalog_gate(
                TEST_USER,
                &ModelSelection {
                    provider: "default".into(),
                    model: "live-a".into(),
                },
            )
            .unwrap();
        assert!(
            router
                .catalog_gate(
                    TEST_USER,
                    &ModelSelection {
                        provider: "default".into(),
                        model: "allow-a".into(),
                    }
                )
                .is_err()
        );
    }

    /// Regression: the picker listed Mistral's full live catalog while the
    /// save check enforces the admin allowlist, so most picks were rejected.
    /// Every listed entry must now pass the full save check, for every
    /// provider the acting user can route to; a user with no credential
    /// sees nothing and can save nothing.
    #[tokio::test]
    async fn every_listed_model_passes_the_save_check() {
        let live = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"id": "mistral-large-latest"}, {"id": "mistral-medium-latest"},
                    {"id": "mistral-small-latest"}, {"id": "codestral-latest"},
                    {"id": "pixtral-large-latest"}, {"id": "mistral-embed"},
                    {"id": "voxtral-mini-latest"}
                ]
            })))
            .mount(&live)
            .await;
        let other = MockServer::start().await;
        let config = LlmConfig {
            base_url: live.uri(),
            model: "mistral-large-latest".into(),
            data_egress_approved: true,
            kind: "openai-compatible".into(),
            catalog_fetch: true,
            catalog_ttl_secs: 60,
            models: vec![
                "mistral-medium-latest".into(),
                "mistral-small-latest".into(),
                "codestral-latest".into(),
                "ministral-8b-latest".into(),
            ],
            providers: BTreeMap::from([(
                "own".into(),
                ProviderConfig {
                    base_url: other.uri(),
                    api_key_secret: "own_key".into(),
                    models: vec!["own-a".into(), "own-b".into()],
                    data_egress_approved: true,
                    ..ProviderConfig::default()
                },
            )]),
            ..LlmConfig::default()
        };
        let credentials = Arc::new(FakeCredentials::new(&[("alice", "alice-key")]));
        let router = ModelRouter::with_credentials(&config, credentials).unwrap();
        router.refresh_catalogs("alice").await.unwrap();

        let entries = router.catalog_entries("alice");
        let listed: BTreeSet<(String, String)> = entries
            .iter()
            .map(|entry| (entry.provider.clone(), entry.model.clone()))
            .collect();
        // Live ∩ allowlist on the default provider; the full list for the
        // user's own provider (no catalog fetch).
        let expected: BTreeSet<(String, String)> = [
            ("default", "codestral-latest"),
            ("default", "mistral-large-latest"),
            ("default", "mistral-medium-latest"),
            ("default", "mistral-small-latest"),
            ("default", "pixtral-large-latest"),
            ("default", "voxtral-mini-latest"),
            ("own", "own-a"),
            ("own", "own-b"),
        ]
        .into_iter()
        .map(|(provider, model)| (provider.to_owned(), model.to_owned()))
        .collect();
        assert_eq!(listed, expected);
        // The list and `catalog()` agree.
        for view in router.catalog("alice") {
            for model in &view.models {
                assert!(listed.contains(&(view.id.clone(), model.clone())));
            }
        }
        for entry in &entries {
            let selection = ModelSelection {
                provider: entry.provider.clone(),
                model: entry.model.clone(),
            };
            router.pin(&selection).unwrap();
            router.catalog_gate("alice", &selection).unwrap();
            router.validate_selection("alice", &selection).unwrap();
        }
        // Allowlisted-but-not-live and non-chat models are neither listed nor
        // savable.
        for model in ["ministral-8b-latest", "mistral-embed"] {
            assert!(!listed.contains(&("default".to_owned(), model.to_owned())));
            let selection = ModelSelection {
                provider: "default".into(),
                model: model.into(),
            };
            assert!(
                router.pin(&selection).is_err()
                    || router.catalog_gate("alice", &selection).is_err()
            );
        }
        // Switching providers: both the default and the user's own provider
        // are listed and savable for alice.
        assert!(listed.iter().any(|(provider, _)| provider == "default"));
        assert!(listed.iter().any(|(provider, _)| provider == "own"));
        // Bob holds no credential: nothing is listed and nothing is savable.
        assert!(router.catalog_entries("bob").is_empty());
        assert!(router.catalog("bob").is_empty());
        for (provider, model) in &expected {
            let selection = ModelSelection {
                provider: provider.clone(),
                model: model.clone(),
            };
            assert!(router.catalog_gate("bob", &selection).is_err());
        }
    }

    /// Each account has its own live list (a different key can see different
    /// models), a rejected key reports `key_rejected` and falls back to the
    /// allowlist, and no response body reaches the catalog the desktop reads.
    #[tokio::test]
    async fn live_lists_are_per_account_and_a_rejected_key_is_reported() {
        let live = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("authorization", "Bearer alice-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{
                    "id": "a-model", "name": "A Model", "context_length": 32000,
                    "supported_parameters": ["tools"]
                }]
            })))
            .mount(&live)
            .await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("authorization", "Bearer bob-key"))
            .respond_with(ResponseTemplate::new(401).set_body_string("synthetic-body-secret"))
            .mount(&live)
            .await;
        let config = LlmConfig {
            base_url: live.uri(),
            model: "static-model".into(),
            data_egress_approved: true,
            kind: "openai-compatible".into(),
            catalog_fetch: true,
            catalog_ttl_secs: 60,
            ..LlmConfig::default()
        };
        let credentials = Arc::new(FakeCredentials::new(&[
            ("alice", "alice-key"),
            ("bob", "bob-key"),
        ]));
        let router = ModelRouter::with_credentials(&config, credentials).unwrap();
        router.refresh_catalogs("alice").await.unwrap();
        router.refresh_catalogs("bob").await.unwrap();

        let alice = &router.catalog("alice")[0];
        assert_eq!(alice.models, vec!["a-model".to_string()]);
        assert_eq!((alice.model_source, alice.status), ("live", "ok"));
        let entry = &router.catalog_entries("alice")[0];
        assert_eq!(entry.name.as_deref(), Some("A Model"));
        assert_eq!(entry.context_length, Some(32_000));
        assert_eq!(entry.tags, vec!["tools".to_string()]);

        let bob = &router.catalog("bob")[0];
        assert_eq!(bob.models, vec!["static-model".to_string()]);
        assert_eq!((bob.model_source, bob.status), ("fallback", "key_rejected"));
        let pick = |model: &str| ModelSelection {
            provider: "default".into(),
            model: model.into(),
        };
        router.catalog_gate("alice", &pick("a-model")).unwrap();
        assert!(router.catalog_gate("alice", &pick("static-model")).is_err());
        router.catalog_gate("bob", &pick("static-model")).unwrap();
        assert!(router.catalog_gate("bob", &pick("a-model")).is_err());

        let shown = serde_json::to_string(&(
            router.catalog("bob"),
            router.catalog_entries("bob"),
            router.catalog("alice"),
        ))
        .unwrap();
        assert!(!shown.contains("synthetic-body-secret"));
        assert!(!shown.contains("bob-key") && !shown.contains("alice-key"));

        // The Test button always asks the provider again.
        let report = router.test_provider("bob", "default").await.unwrap();
        assert!(report.fetched && !report.verified);
        assert!(router.test_provider("nobody", "default").await.is_err());
        assert!(router.test_provider("alice", "missing").await.is_err());
    }

    #[tokio::test]
    async fn routing_helpers_resolve_provider_classes_and_fallback() {
        let (mut config, clients, _, _) = fixture().await;
        config.classes.insert(
            "routine".into(),
            vec!["first-model".into(), "second-model".into()],
        );
        let router = ModelRouter::new(&config, clients).unwrap();
        assert_eq!(
            router.provider_for_model("second-model").as_deref(),
            Some("second")
        );
        assert_eq!(
            router.provider_for_model("first-model").as_deref(),
            Some("default")
        );
        assert!(router.provider_for_model("unknown").is_none());
        assert_eq!(
            router.classes_for_model("second-model"),
            vec!["routine".to_string()]
        );
        assert!(router.classes_for_model("other").is_empty());
        assert_eq!(
            router.fallback_for_model("first-model"),
            Some(ModelSelection {
                provider: "second".into(),
                model: "second-model".into(),
            })
        );
        assert_eq!(router.fallback_for_model("other"), None);
    }
}

#[cfg(test)]
mod live_catalog_tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const USER: &str = "owner";

    /// A Mistral-style default provider whose live list holds a model the
    /// short configured allowlist does not.
    async fn mistral_router(server: &MockServer) -> ModelRouter {
        let config = LlmConfig {
            base_url: format!("{}/v1", server.uri()),
            model: "mistral-large-latest".into(),
            data_egress_approved: true,
            kind: "mistral".into(),
            catalog_fetch: true,
            catalog_ttl_secs: 60,
            ..LlmConfig::default()
        };
        let clients = BTreeMap::from([(
            "default".into(),
            LlmClient::new(format!("{}/v1", server.uri()), "synthetic-key", 1, 100).unwrap(),
        )]);
        ModelRouter::new(&config, clients).unwrap()
    }

    fn live(id: &str) -> ModelSelection {
        ModelSelection {
            provider: "default".into(),
            model: id.into(),
        }
    }

    /// Reproduces the 0.21.0 failure: after a restart there is no live list,
    /// so a model the picker offered earlier (live-only) fails validation.
    #[tokio::test]
    async fn live_only_model_is_rejected_until_the_list_is_loaded() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "list",
                "data": [
                    {"id": "mistral-large-latest", "object": "model", "capabilities": {"completion_chat": true}},
                    {"id": "magistral-medium-latest", "object": "model", "capabilities": {"completion_chat": true}}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let router = mistral_router(&server).await;
        let error = router
            .validate_selection(USER, &live("magistral-medium-latest"))
            .unwrap_err();
        assert!(matches!(
            error,
            SeraError::Rejected {
                code: "model_unavailable",
                ..
            }
        ));
        assert_eq!(
            error.to_string(),
            "That model isn't available for Mistral right now"
        );
        router.ensure_catalogs(USER, Duration::from_secs(5)).await;
        router
            .validate_selection(USER, &live("magistral-medium-latest"))
            .unwrap();
        // A second call finds the snapshot and does not ask again.
        router.ensure_catalogs(USER, Duration::from_secs(5)).await;
    }

    #[tokio::test]
    async fn failed_ensure_keeps_the_allowlist_and_backs_off() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let router = mistral_router(&server).await;
        router.ensure_catalogs(USER, Duration::from_secs(5)).await;
        let after_first = server.received_requests().await.unwrap().len();
        assert!(after_first >= 1);
        router.ensure_catalogs(USER, Duration::from_secs(5)).await;
        // The failure is remembered: the second call does not ask again.
        assert_eq!(server.received_requests().await.unwrap().len(), after_first);
        router
            .validate_selection(USER, &live("mistral-large-latest"))
            .unwrap();
        assert_eq!(router.catalog(USER)[0].status, "unreachable");
    }

    #[tokio::test]
    async fn slow_provider_is_bounded_by_the_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"data": [{"id": "late"}]}))
                    .set_delay(Duration::from_secs(5)),
            )
            .mount(&server)
            .await;
        let router = mistral_router(&server).await;
        let started = Instant::now();
        router
            .ensure_catalogs(USER, Duration::from_millis(150))
            .await;
        assert!(started.elapsed() < Duration::from_secs(3));
        router
            .validate_selection(USER, &live("mistral-large-latest"))
            .unwrap();
        assert!(router.validate_selection(USER, &live("late")).is_err());
    }

    /// The imported-credential shape: the account "has" the provider but the
    /// stored credential cannot build a client. Refreshing must not error.
    #[tokio::test]
    async fn unbuildable_client_never_fails_a_refresh() {
        struct Broken;
        impl ProviderCredentials for Broken {
            fn available(&self, _user: &str, _provider: &str) -> bool {
                true
            }
            fn client(&self, _u: &str, _p: &str, _profile: &ProviderConfig) -> Result<LlmClient> {
                Err(SeraError::Forbidden("corrupt credential".into()))
            }
        }
        let server = MockServer::start().await;
        let config = LlmConfig {
            base_url: format!("{}/v1", server.uri()),
            model: "mistral-large-latest".into(),
            data_egress_approved: true,
            kind: "mistral".into(),
            catalog_fetch: true,
            ..LlmConfig::default()
        };
        let router = ModelRouter::with_credentials(&config, Arc::new(Broken)).unwrap();
        let reports = router.refresh_catalogs(USER).await.unwrap();
        assert!(!reports[0].verified);
        router.ensure_catalogs(USER, Duration::from_secs(1)).await;
        router
            .validate_selection(USER, &live("mistral-large-latest"))
            .unwrap();
    }
}
