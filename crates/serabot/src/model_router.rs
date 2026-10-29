use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::config::{LlmConfig, ProviderConfig};
use crate::domain::LlmMessage;
use crate::error::{Result, SeraError};
use crate::gateway::ToolSpec;
use crate::llm::{LlmClient, LlmResponse, validate_base_url};
use crate::util::sha256_hex;

/// Stored conversation preference when the owner enables automatic routing.
pub const AUTO_PROVIDER: &str = "auto";

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
}

#[derive(Debug, Clone, Serialize)]
pub struct CatalogEntry {
    pub provider: String,
    pub model: String,
    pub data_egress_approved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinnedRoute {
    pub selection: ModelSelection,
    pub fingerprint: String,
}

#[derive(Clone)]
struct Provider {
    client: LlmClient,
    models: BTreeSet<String>,
    data_egress_approved: bool,
    fingerprint: String,
}

#[derive(Clone)]
pub struct ModelRouter {
    providers: BTreeMap<String, Provider>,
    default_model: String,
}

impl ModelRouter {
    pub fn new(config: &LlmConfig, mut clients: BTreeMap<String, LlmClient>) -> Result<Self> {
        if config.providers.contains_key("default") {
            return Err(SeraError::Config(
                "provider id 'default' is reserved for [llm]".into(),
            ));
        }
        let default = ProviderConfig {
            base_url: config.base_url.clone(),
            api_key_secret: config.api_key_secret.clone(),
            models: std::iter::once(config.model.clone())
                .chain(config.models.iter().cloned())
                .chain(config.bot_models.values().cloned())
                .collect(),
            data_egress_approved: config.data_egress_approved,
        };
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
            let endpoint = validate_base_url(&profile.base_url)?;
            if profile.api_key_secret.trim().is_empty() || profile.models.is_empty() {
                return Err(SeraError::Config(
                    "providers require a secret reference and explicit allowed models".into(),
                ));
            }
            for model in &profile.models {
                validate_model(model)?;
            }
            let client = clients.remove(id).ok_or_else(|| {
                SeraError::Config("configured provider credential/client is missing".into())
            })?;
            if client.base_url() != endpoint {
                return Err(SeraError::Config(
                    "provider client endpoint does not match its configuration".into(),
                ));
            }
            let fingerprint = sha256_hex(serde_json::to_vec(&(
                endpoint,
                &profile.api_key_secret,
                profile.data_egress_approved,
            ))?);
            providers.insert(
                id.to_owned(),
                Provider {
                    client,
                    models: profile.models.iter().cloned().collect(),
                    data_egress_approved: profile.data_egress_approved,
                    fingerprint,
                },
            );
        }
        if !clients.is_empty() {
            return Err(SeraError::Config("unconfigured provider client".into()));
        }
        Ok(Self {
            providers,
            default_model: config.model.clone(),
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

    pub fn catalog(&self) -> Vec<ProviderView> {
        self.providers
            .iter()
            .map(|(id, provider)| ProviderView {
                id: id.clone(),
                models: provider.models.iter().cloned().collect(),
                data_egress_approved: provider.data_egress_approved,
            })
            .collect()
    }

    pub fn catalog_entries(&self) -> Vec<CatalogEntry> {
        self.providers
            .iter()
            .flat_map(|(id, provider)| {
                provider.models.iter().map(|model| CatalogEntry {
                    provider: id.clone(),
                    model: model.clone(),
                    data_egress_approved: provider.data_egress_approved,
                })
            })
            .collect()
    }

    pub fn default_selection(&self, bot_model: Option<&str>) -> ModelSelection {
        ModelSelection {
            provider: "default".into(),
            model: bot_model.unwrap_or(&self.default_model).to_owned(),
        }
    }

    /// Deterministic pick from configured, egress-approved provider/model pairs.
    ///
    /// Order: bot `model` override → `llm.bot_models` for the bot → `[llm].model` on
    /// `default` → first approved pair sorted by provider id then model id.
    pub fn autoselect(
        &self,
        bot_model: Option<&str>,
        bot_name: &str,
        bot_models: &BTreeMap<String, String>,
    ) -> Result<ModelSelection> {
        if let Some(model) = bot_model {
            if let Some(selection) = self.find_approved_model(model, Some("default")) {
                return Ok(selection);
            }
        }
        if let Some(model) = bot_models.get(bot_name) {
            if let Some(selection) = self.find_approved_model(model, Some("default")) {
                return Ok(selection);
            }
        }
        if let Some(selection) = self.find_approved_model(&self.default_model, Some("default")) {
            return Ok(selection);
        }
        self.first_approved().ok_or_else(|| {
            SeraError::Forbidden(
                "no provider with data-egress approval is configured for autoselect".into(),
            )
        })
    }

    pub fn resolve_selection(
        &self,
        stored: &ModelSelection,
        bot_model: Option<&str>,
        bot_name: &str,
        bot_models: &BTreeMap<String, String>,
    ) -> Result<ModelSelection> {
        if stored.is_auto_mode() {
            return self.autoselect(bot_model, bot_name, bot_models);
        }
        self.validate_selection(stored)?;
        Ok(stored.clone())
    }

    fn find_approved_model(
        &self,
        model: &str,
        prefer_provider: Option<&str>,
    ) -> Option<ModelSelection> {
        if let Some(id) = prefer_provider {
            if let Some(provider) = self.providers.get(id) {
                if provider.data_egress_approved && provider.models.contains(model) {
                    return Some(ModelSelection {
                        provider: id.to_owned(),
                        model: model.to_owned(),
                    });
                }
            }
        }
        for (id, provider) in &self.providers {
            if provider.data_egress_approved && provider.models.contains(model) {
                return Some(ModelSelection {
                    provider: id.clone(),
                    model: model.to_owned(),
                });
            }
        }
        None
    }

    pub fn autoselect_available(&self) -> bool {
        self.first_approved().is_some()
    }

    fn first_approved(&self) -> Option<ModelSelection> {
        for (id, provider) in &self.providers {
            for model in &provider.models {
                if provider.data_egress_approved {
                    return Some(ModelSelection {
                        provider: id.clone(),
                        model: model.clone(),
                    });
                }
            }
        }
        None
    }

    pub fn validate_selection(&self, selection: &ModelSelection) -> Result<()> {
        let provider = self
            .providers
            .get(&selection.provider)
            .ok_or_else(|| SeraError::Validation("unknown model provider".into()))?;
        if !provider.models.contains(&selection.model) {
            return Err(SeraError::Validation(
                "model is not allowed for this provider".into(),
            ));
        }
        Ok(())
    }

    pub fn pin(&self, selection: &ModelSelection) -> Result<PinnedRoute> {
        self.validate_selection(selection)?;
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
        route: &PinnedRoute,
        messages: &[LlmMessage],
        tools: &[ToolSpec],
        temperature: Option<f32>,
    ) -> Result<LlmResponse> {
        self.validate_pinned(route)?;
        self.providers[&route.selection.provider]
            .client
            .complete(messages, tools, Some(&route.selection.model), temperature)
            .await
    }
}

fn validate_model(model: &str) -> Result<()> {
    if model.is_empty()
        || model.len() > 256
        || model.trim() != model
        || model.chars().any(char::is_control)
    {
        return Err(SeraError::Config("invalid configured model id".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderConfig;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
                .complete(&route, &[LlmMessage::user("synthetic")], &[], None)
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
                .complete(&route, &[LlmMessage::user("synthetic")], &[], None)
                .await
                .is_err()
        );
        let mut invalid = route.clone();
        invalid.selection.model = "not-allowed".into();
        assert!(
            router
                .complete(&invalid, &[LlmMessage::user("synthetic")], &[], None)
                .await
                .is_err()
        );
        invalid.selection.provider = "unknown".into();
        assert!(
            router
                .complete(&invalid, &[LlmMessage::user("synthetic")], &[], None)
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
        let route = router.pin(&router.default_selection(None)).unwrap();
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
                .autoselect(Some("second-model"), "other", &bot_models)
                .unwrap(),
            ModelSelection {
                provider: "second".into(),
                model: "second-model".into(),
            }
        );
        assert_eq!(
            router.autoselect(None, "main", &bot_models).unwrap(),
            ModelSelection {
                provider: "default".into(),
                model: "first-model".into(),
            }
        );
        assert_eq!(
            router.autoselect(None, "other", &BTreeMap::new()).unwrap(),
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
        assert!(router.autoselect(None, "main", &BTreeMap::new()).is_err());
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
}
