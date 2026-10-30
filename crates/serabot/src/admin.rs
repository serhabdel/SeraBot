//! Owner-managed configuration overlay.
//!
//! `/etc/serabot/serabot.toml` stays read-only. The owner console writes only
//! non-secret settings to `admin-overrides.json` beside the SQLite database,
//! which is merged over the file at startup. Credential values still travel to
//! the age store through [`SharedSecrets::persist`] and are never echoed.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::audio::resolve_audio_provider;
use crate::config::{Config, OAuthConnectorSettings, ProviderConfig};
use crate::error::{Result, SeraError};
use crate::providers::{canonical_kind, check_profile};
use crate::secrets::SharedSecrets;

pub const OVERLAY_FILE: &str = "admin-overrides.json";
const MAX_OVERLAY_BYTES: u64 = 1_048_576;
const MAX_CREDENTIAL_LEN: usize = 8_192;

#[derive(Debug, Clone, Default)]
pub struct ProviderSecrets<'a> {
    pub api_key: Option<&'a str>,
    pub refresh_token: Option<&'a str>,
    pub oauth_client_id: Option<&'a str>,
    pub oauth_client_secret: Option<&'a str>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdminOverlay {
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub connectors: BTreeMap<String, bool>,
    #[serde(default)]
    pub audio: Option<AudioOverlay>,
    #[serde(default)]
    pub bots: BTreeMap<String, BotProfile>,
}

/// Owner-facing identity for a bot. The bot file stays authoritative for
/// behavior; this only changes how the console presents the bot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BotProfile {
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub avatar: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioOverlay {
    pub enabled: bool,
    pub transcription_model: String,
    pub understanding_model: String,
}

impl AdminOverlay {
    pub fn path_for(config: &Config) -> PathBuf {
        let database = config.database_path();
        database
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(|parent| parent.join(OVERLAY_FILE))
            .unwrap_or_else(|| config.base_dir.join(OVERLAY_FILE))
    }

    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.len() > MAX_OVERLAY_BYTES => Err(SeraError::Config(format!(
                "{} is larger than the admin overlay limit",
                path.display()
            ))),
            Ok(_) => {
                let bytes = std::fs::read(path)?;
                serde_json::from_slice(&bytes).map_err(|_| {
                    SeraError::Config(format!("{} is not valid admin overrides", path.display()))
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(SeraError::Io(error)),
        }
    }

    pub fn apply(&self, config: &mut Config) -> Result<()> {
        for (id, profile) in &self.providers {
            validate_provider_id(id)?;
            if id == "default" {
                return Err(SeraError::Config(
                    "provider id 'default' is reserved for [llm]".into(),
                ));
            }
            config.llm.providers.insert(id.clone(), profile.clone());
        }
        for (id, enabled) in &self.connectors {
            match id.as_str() {
                "tavily" => config.tavily.enabled = *enabled,
                "firecrawl" => config.firecrawl.enabled = *enabled,
                "elevenlabs" => config.elevenlabs.enabled = *enabled,
                _ => config.connectors.set_enabled(id, *enabled)?,
            }
        }
        if let Some(audio) = &self.audio {
            if audio.enabled
                && (audio.transcription_model.trim().is_empty()
                    || audio.understanding_model.trim().is_empty())
            {
                return Err(SeraError::Config(
                    "admin audio overrides need both model names".into(),
                ));
            }
            config.audio.enabled = audio.enabled;
            if !audio.transcription_model.trim().is_empty() {
                config.audio.transcription_model = audio.transcription_model.clone();
            }
            if !audio.understanding_model.trim().is_empty() {
                config.audio.understanding_model = audio.understanding_model.clone();
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct AdminStore {
    path: PathBuf,
    overlay: Arc<Mutex<AdminOverlay>>,
    write_lock: Arc<Mutex<()>>,
}

impl AdminStore {
    pub fn open(config: &Config) -> Result<Self> {
        let path = AdminOverlay::path_for(config);
        let overlay = AdminOverlay::load(&path)?;
        Ok(Self {
            path,
            overlay: Arc::new(Mutex::new(overlay)),
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn manages_provider(&self, id: &str) -> bool {
        self.overlay
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .providers
            .contains_key(id)
    }

    pub fn bot_profile(&self, id: &str) -> Option<BotProfile> {
        self.overlay
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .bots
            .get(id)
            .cloned()
    }

    pub fn set_bot_profile(&self, id: &str, profile: BotProfile) -> Result<()> {
        let display_name = profile.display_name.trim();
        if display_name.is_empty()
            || display_name.chars().count() > 64
            || display_name.chars().any(char::is_control)
        {
            return Err(SeraError::Validation(
                "bot display name must be 1-64 visible characters".into(),
            ));
        }
        if profile.description.chars().count() > 2_000
            || profile
                .description
                .chars()
                .any(|c| c != '\n' && c != '\r' && c != '\t' && c.is_control())
        {
            return Err(SeraError::Validation(
                "bot description is too long or contains control characters".into(),
            ));
        }
        if profile.avatar.chars().count() > 16 || profile.avatar.chars().any(char::is_control) {
            return Err(SeraError::Validation(
                "bot avatar must be a short label or emoji".into(),
            ));
        }
        let profile = BotProfile {
            display_name: display_name.to_owned(),
            ..profile
        };
        self.mutate(|overlay| {
            overlay.bots.insert(id.to_owned(), profile);
            Ok(())
        })
    }

    pub fn add_provider(
        &self,
        base: &Config,
        secrets: &SharedSecrets,
        id: &str,
        profile: ProviderConfig,
        api_key: Option<&str>,
    ) -> Result<()> {
        self.add_provider_with_secrets(
            base,
            secrets,
            id,
            profile,
            ProviderSecrets {
                api_key,
                refresh_token: None,
                oauth_client_id: None,
                oauth_client_secret: None,
            },
        )
    }

    pub fn add_provider_with_secrets(
        &self,
        base: &Config,
        secrets: &SharedSecrets,
        id: &str,
        mut profile: ProviderConfig,
        provider_secrets: ProviderSecrets<'_>,
    ) -> Result<()> {
        validate_provider_id(id)?;
        if id == "default" {
            return Err(SeraError::Config(
                "provider id 'default' is reserved for [llm]".into(),
            ));
        }
        let managed = self.manages_provider(id);
        if !managed && base.llm.providers.contains_key(id) {
            return Err(SeraError::Validation(format!(
                "provider '{id}' is configured in serabot.toml"
            )));
        }
        let kind = canonical_kind(&profile.kind)?;
        let is_oauth = matches!(kind.as_str(), "google-antigravity" | "muse-code")
            || profile.auth.trim().eq_ignore_ascii_case("oauth");
        if is_oauth {
            if profile.auth.trim().is_empty() {
                profile.auth = "oauth".to_owned();
            }
            if kind == "google-antigravity" {
                if profile.api_key_secret.trim().is_empty() {
                    profile.api_key_secret = "antigravity_access_token".to_owned();
                }
                if profile.refresh_token_secret.trim().is_empty() {
                    profile.refresh_token_secret = "antigravity_refresh_token".to_owned();
                }
                if profile.oauth_client_secret_secret.trim().is_empty() {
                    profile.oauth_client_secret_secret =
                        "antigravity_oauth_client_secret".to_owned();
                }
            } else if kind == "muse-code" {
                if profile.api_key_secret.trim().is_empty() {
                    profile.api_key_secret = "muse_code_api_key".to_owned();
                }
                if profile.refresh_token_secret.trim().is_empty() {
                    profile.refresh_token_secret = "muse_code_identity".to_owned();
                }
            } else {
                return Err(SeraError::Config(
                    "oauth auth is only supported for muse-code and google-antigravity".into(),
                ));
            }
        } else {
            profile.auth = "api_key".to_owned();
            profile.refresh_token_secret.clear();
            profile.oauth_client_id_secret.clear();
            profile.oauth_client_secret_secret.clear();
            profile.project_id.clear();
        }
        profile.kind = kind;
        profile.data_egress_approved = true;
        profile.base_url = profile.base_url.trim().to_owned();
        profile.api_key_secret = profile.api_key_secret.trim().to_owned();
        profile.refresh_token_secret = profile.refresh_token_secret.trim().to_owned();
        profile.oauth_client_id_secret = profile.oauth_client_id_secret.trim().to_owned();
        profile.oauth_client_secret_secret = profile.oauth_client_secret_secret.trim().to_owned();
        profile.project_id = profile.project_id.trim().to_owned();

        if !crate::config::valid_secret_name(&profile.api_key_secret) {
            return Err(SeraError::Validation(
                "api_key_secret must be a secret-store key name".into(),
            ));
        }
        if !profile.refresh_token_secret.is_empty()
            && !crate::config::valid_secret_name(&profile.refresh_token_secret)
        {
            return Err(SeraError::Validation(
                "refresh_token_secret must be a secret-store key name".into(),
            ));
        }
        if !profile.oauth_client_id_secret.is_empty()
            && !crate::config::valid_secret_name(&profile.oauth_client_id_secret)
        {
            return Err(SeraError::Validation(
                "oauth_client_id_secret must be a secret-store key name".into(),
            ));
        }
        if !profile.oauth_client_secret_secret.is_empty()
            && !crate::config::valid_secret_name(&profile.oauth_client_secret_secret)
        {
            return Err(SeraError::Validation(
                "oauth_client_secret_secret must be a secret-store key name".into(),
            ));
        }
        if profile.models.is_empty() || profile.models.len() > 256 {
            return Err(SeraError::Validation(
                "provide between 1 and 256 model ids".into(),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for model in &profile.models {
            if !crate::providers::valid_model_id(model) {
                return Err(SeraError::Validation("invalid model id".into()));
            }
            if !seen.insert(model.clone()) {
                return Err(SeraError::Validation("duplicate model id".into()));
            }
        }
        if !(1..=86_400).contains(&profile.catalog_ttl_secs) {
            profile.catalog_ttl_secs = 1_800;
        }
        check_profile(
            &profile.kind,
            &profile.base_url,
            &profile.auth,
            &profile.refresh_token_secret,
            &profile.oauth_client_secret_secret,
            profile.catalog_fetch,
            profile.catalog_ttl_secs,
        )?;

        let mut to_persist: Vec<(&str, &str)> = Vec::new();

        if let Some(key) = provider_secrets.api_key {
            let key = key.trim();
            if !key.is_empty() {
                if !credential_ok(key) {
                    return Err(SeraError::Validation(
                        "the API key is empty, too long, or contains control characters".into(),
                    ));
                }
                let shared = base.llm.api_key_secret == profile.api_key_secret
                    || base.llm.providers.iter().any(|(other, configured)| {
                        other != id && configured.api_key_secret == profile.api_key_secret
                    })
                    || self.snapshot().providers.iter().any(|(other, configured)| {
                        other != id && configured.api_key_secret == profile.api_key_secret
                    });
                if shared {
                    return Err(SeraError::Validation(format!(
                        "secret {} already belongs to another provider; choose a new secret name",
                        profile.api_key_secret
                    )));
                }
                to_persist.push((profile.api_key_secret.as_str(), key));
            }
        }

        if let Some(token) = provider_secrets.refresh_token {
            let token = token.trim();
            if !token.is_empty() && !profile.refresh_token_secret.is_empty() {
                if !credential_ok(token) {
                    return Err(SeraError::Validation(
                        "the refresh token is empty, too long, or contains control characters"
                            .into(),
                    ));
                }
                to_persist.push((profile.refresh_token_secret.as_str(), token));
            }
        }

        if let Some(client_id) = provider_secrets.oauth_client_id {
            let client_id = client_id.trim();
            if !client_id.is_empty() && !profile.oauth_client_id_secret.is_empty() {
                if !credential_ok(client_id) {
                    return Err(SeraError::Validation(
                        "the oauth client id is empty, too long, or contains control characters"
                            .into(),
                    ));
                }
                to_persist.push((profile.oauth_client_id_secret.as_str(), client_id));
            }
        }

        if let Some(client_secret) = provider_secrets.oauth_client_secret {
            let client_secret = client_secret.trim();
            if !client_secret.is_empty() && !profile.oauth_client_secret_secret.is_empty() {
                if !credential_ok(client_secret) {
                    return Err(SeraError::Validation(
                        "the oauth client secret is empty, too long, or contains control characters".into(),
                    ));
                }
                to_persist.push((profile.oauth_client_secret_secret.as_str(), client_secret));
            }
        }

        if !to_persist.is_empty() {
            secrets.persist(&to_persist)?;
        }

        if is_oauth {
            if !secrets.has(&profile.refresh_token_secret) && !secrets.has(&profile.api_key_secret)
            {
                return Err(SeraError::Validation(format!(
                    "secret {} is empty; provide credentials",
                    profile.refresh_token_secret
                )));
            }
        } else if !secrets.has(&profile.api_key_secret) {
            return Err(SeraError::Validation(format!(
                "secret {} is empty; provide an API key",
                profile.api_key_secret
            )));
        }

        self.save_provider(id, profile)
    }

    pub fn remove_provider(&self, id: &str) -> Result<()> {
        self.mutate(|overlay| {
            overlay
                .providers
                .remove(id)
                .map(|_| ())
                .ok_or_else(|| SeraError::NotFound(format!("managed provider {id}")))
        })
    }

    pub fn set_connector(
        &self,
        base: &Config,
        secrets: &SharedSecrets,
        id: &str,
        enabled: bool,
        values: &BTreeMap<String, String>,
    ) -> Result<()> {
        let roles = connector_secret_roles(base, id)?;
        for key in values.keys() {
            if !roles.iter().any(|(role, _)| role == key) {
                return Err(SeraError::Validation(format!(
                    "unknown connector credential role '{key}'"
                )));
            }
        }
        for (role, name) in &roles {
            let Some(value) = values.get(*role) else {
                continue;
            };
            if !credential_ok(value) {
                return Err(SeraError::Validation(format!(
                    "{role} is empty, too long, or contains control characters"
                )));
            }
            if !crate::config::valid_secret_name(name) {
                return Err(SeraError::Config(format!(
                    "connectors.{id} uses an invalid secret name"
                )));
            }
            secrets.persist(&[(name.as_str(), value)])?;
        }
        if enabled {
            for (role, name) in &roles {
                if !secrets.has(name) {
                    return Err(SeraError::Validation(format!(
                        "connectors.{id} needs {role} secret {name} before it can be enabled"
                    )));
                }
            }
        }

        self.mutate(|overlay| {
            overlay.connectors.insert(id.to_owned(), enabled);
            Ok(())
        })
    }

    pub fn set_audio(
        &self,
        base: &Config,
        secrets: &SharedSecrets,
        enabled: bool,
        transcription_model: Option<String>,
        understanding_model: Option<String>,
    ) -> Result<()> {
        let mut candidate = base.clone();
        self.snapshot().apply(&mut candidate)?;
        if let Some(model) = transcription_model {
            if !crate::providers::valid_model_id(&model) {
                return Err(SeraError::Validation(
                    "invalid transcription model id".into(),
                ));
            }
            candidate.audio.transcription_model = model;
        }
        if let Some(model) = understanding_model {
            if !crate::providers::valid_model_id(&model) {
                return Err(SeraError::Validation(
                    "invalid understanding model id".into(),
                ));
            }
            candidate.audio.understanding_model = model;
        }
        candidate.audio.enabled = enabled;
        let provider = resolve_audio_provider(&candidate.llm, &candidate.audio)?;
        if enabled && !secrets.has(&provider.api_key_secret) {
            return Err(SeraError::Validation(format!(
                "audio needs secret {} before it can be enabled",
                provider.api_key_secret
            )));
        }

        self.mutate(|overlay| {
            overlay.audio = Some(AudioOverlay {
                enabled,
                transcription_model: candidate.audio.transcription_model,
                understanding_model: candidate.audio.understanding_model,
            });
            Ok(())
        })
    }

    pub fn connector_roles(&self, base: &Config, id: &str) -> Result<Vec<(&'static str, String)>> {
        connector_secret_roles(base, id)
    }

    pub fn snapshot(&self) -> AdminOverlay {
        self.overlay
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn save_provider(&self, id: &str, profile: ProviderConfig) -> Result<()> {
        self.mutate(|overlay| {
            overlay.providers.insert(id.to_owned(), profile);
            Ok(())
        })
    }

    /// Serialize read-modify-write cycles so two consoles cannot drop each
    /// other's change. The file is only replaced after a successful write.
    fn mutate<T>(&self, update: impl FnOnce(&mut AdminOverlay) -> Result<T>) -> Result<T> {
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut next = self.snapshot();
        let value = update(&mut next)?;
        self.write(&next)?;
        self.replace(next);
        Ok(value)
    }

    fn replace(&self, next: AdminOverlay) {
        *self
            .overlay
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = next;
    }

    fn write(&self, overlay: &AdminOverlay) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(overlay)?;
        if bytes.len() as u64 > MAX_OVERLAY_BYTES {
            return Err(SeraError::Config("admin overrides are too large".into()));
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("json.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        if let Err(error) = std::fs::rename(&temporary, &self.path) {
            let _ = std::fs::remove_file(&self.path);
            std::fs::rename(&temporary, &self.path).map_err(|_| SeraError::Io(error))?;
        }
        Ok(())
    }
}

pub fn validate_provider_id(id: &str) -> Result<()> {
    if id == "auto" {
        return Err(SeraError::Validation(
            "provider id 'auto' is reserved for automatic routing".into(),
        ));
    }
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(SeraError::Validation(
            "provider id must be 1-64 letters, digits, '_' or '-'".into(),
        ));
    }
    Ok(())
}

fn credential_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CREDENTIAL_LEN
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

pub fn connector_secret_roles(config: &Config, id: &str) -> Result<Vec<(&'static str, String)>> {
    let connectors = &config.connectors;
    let oauth: Option<&dyn OAuthConnectorSettings> = match id {
        "google_drive" => Some(&connectors.google_drive),
        "google_sheets" => Some(&connectors.google_sheets),
        "google_calendar" => Some(&connectors.google_calendar),
        "gmail" => Some(&connectors.gmail),
        "outlook" => Some(&connectors.outlook),
        "outlook_calendar" => Some(&connectors.outlook_calendar),
        "notion" => Some(&connectors.notion),
        _ => None,
    };
    if let Some(oauth) = oauth {
        return Ok(vec![
            ("client_id", oauth.client_id_secret_name().to_owned()),
            (
                "client_secret",
                oauth.client_secret_secret_name().to_owned(),
            ),
            (
                "refresh_token",
                oauth.refresh_token_secret_name().to_owned(),
            ),
        ]);
    }
    if id == "agentmail" {
        return Ok(vec![(
            "api_key",
            connectors.agentmail.api_key_secret.clone(),
        )]);
    }
    if id == "tavily" {
        return Ok(vec![("api_key", config.tavily.api_key_secret.clone())]);
    }
    if id == "firecrawl" {
        return Ok(vec![("api_key", config.firecrawl.api_key_secret.clone())]);
    }
    if id == "elevenlabs" {
        return Ok(vec![("api_key", config.elevenlabs.api_key_secret.clone())]);
    }
    Err(SeraError::NotFound(format!("connector {id}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_root(root: &Path) -> Config {
        let mut config = Config {
            base_dir: root.to_path_buf(),
            ..Config::default()
        };
        config.database.path = root.join("serabot.db").to_string_lossy().into_owned();
        config
    }

    fn secrets(names: &[(&str, &str)]) -> SharedSecrets {
        SharedSecrets::new(crate::secrets::SecretStore::from_map(
            names
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        ))
    }

    #[test]
    fn overlay_round_trips_providers_connectors_and_audio() {
        let root = std::env::temp_dir().join(format!("serabot-admin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let config = config_with_root(&root);
        let store = AdminStore::open(&config).unwrap();
        let creds = secrets(&[("CUSTOM_KEY", "synthetic-key")]);
        let profile = ProviderConfig {
            base_url: "https://api.example.com/v1".into(),
            api_key_secret: "CUSTOM_KEY".into(),
            models: vec!["model-a".into()],
            data_egress_approved: true,
            kind: "openai-compatible".into(),
            catalog_fetch: true,
            ..ProviderConfig::default()
        };
        store
            .add_provider(&config, &creds, "custom", profile, None)
            .unwrap();
        store
            .set_audio(&config, &creds, false, Some("audio-model".into()), None)
            .unwrap();

        let path = AdminOverlay::path_for(&config);
        let loaded = AdminOverlay::load(&path).unwrap();
        assert!(loaded.providers.contains_key("custom"));
        assert_eq!(
            loaded.audio.as_ref().unwrap().transcription_model,
            "audio-model"
        );
        let mut merged = config.clone();
        loaded.apply(&mut merged).unwrap();
        assert!(merged.llm.providers.contains_key("custom"));
        assert!(!merged.audio.enabled);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn provider_needs_a_stored_secret_and_rejects_reserved_or_file_ids() {
        let root = std::env::temp_dir().join(format!("serabot-admin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let config = config_with_root(&root);
        let store = AdminStore::open(&config).unwrap();
        let creds = secrets(&[]);
        let profile = ProviderConfig {
            base_url: "https://api.example.com/v1".into(),
            api_key_secret: "MISSING_KEY".into(),
            models: vec!["model-a".into()],
            data_egress_approved: true,
            kind: "openai-compatible".into(),
            ..ProviderConfig::default()
        };
        assert!(
            store
                .add_provider(&config, &creds, "custom", profile.clone(), None)
                .is_err()
        );
        let with_key = secrets(&[("MISSING_KEY", "synthetic-key")]);
        assert!(
            store
                .add_provider(&config, &with_key, "default", profile.clone(), None)
                .is_err()
        );
        assert!(
            store
                .add_provider(&config, &with_key, "Bad Id", profile, None)
                .is_err()
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn enabling_a_connector_requires_its_configured_secrets() {
        let root = std::env::temp_dir().join(format!("serabot-admin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let config = config_with_root(&root);
        let store = AdminStore::open(&config).unwrap();
        let empty = SharedSecrets::new(
            crate::secrets::SecretStore::open_paths(
                root.join("secrets.age"),
                root.join("identity.txt"),
            )
            .unwrap(),
        );
        let roles = store.connector_roles(&config, "agentmail").unwrap();
        assert!(roles.iter().any(|(role, _)| *role == "api_key"));
        assert!(
            store
                .set_connector(&config, &empty, "agentmail", true, &BTreeMap::new())
                .is_err()
        );
        assert!(
            store
                .set_connector(&config, &empty, "agentmail", false, &BTreeMap::new())
                .is_ok()
        );
        let mut values = BTreeMap::new();
        values.insert("api_key".to_owned(), "synthetic-key".to_owned());
        assert!(
            store
                .set_connector(&config, &empty, "agentmail", true, &values)
                .is_ok()
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn tavily_and_firecrawl_are_owner_manageable_search_connectors() {
        let root = std::env::temp_dir().join(format!("serabot-admin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let config = config_with_root(&root);
        let store = AdminStore::open(&config).unwrap();
        let empty = SharedSecrets::new(
            crate::secrets::SecretStore::open_paths(
                root.join("secrets.age"),
                root.join("identity.txt"),
            )
            .unwrap(),
        );
        for id in ["tavily", "firecrawl"] {
            let roles = store.connector_roles(&config, id).unwrap();
            assert!(
                roles
                    .iter()
                    .any(|(role, name)| *role == "api_key" && name == &format!("{id}_api_key")),
                "{id} roles: {roles:?}"
            );
            assert!(
                store
                    .set_connector(&config, &empty, id, true, &BTreeMap::new())
                    .is_err(),
                "{id} must not enable without its secret"
            );
            let mut values = BTreeMap::new();
            values.insert("api_key".to_owned(), "synthetic-key".to_owned());
            assert!(
                store
                    .set_connector(&config, &empty, id, true, &values)
                    .is_ok(),
                "{id} enables once its key is stored"
            );
        }
        let mut merged = config.clone();
        store.snapshot().apply(&mut merged).unwrap();
        assert!(merged.tavily.enabled);
        assert!(merged.firecrawl.enabled);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn elevenlabs_is_owner_manageable_voice_connector() {
        let root = std::env::temp_dir().join(format!("serabot-admin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let config = config_with_root(&root);
        let store = AdminStore::open(&config).unwrap();
        let empty = SharedSecrets::new(
            crate::secrets::SecretStore::open_paths(
                root.join("secrets.age"),
                root.join("identity.txt"),
            )
            .unwrap(),
        );
        let roles = store.connector_roles(&config, "elevenlabs").unwrap();
        assert_eq!(
            roles,
            vec![("api_key", config.elevenlabs.api_key_secret.clone())]
        );
        assert!(
            store
                .set_connector(&config, &empty, "elevenlabs", true, &BTreeMap::new())
                .is_err(),
            "elevenlabs must not enable without its secret"
        );
        let mut values = BTreeMap::new();
        values.insert("api_key".to_owned(), "synthetic-key".to_owned());
        assert!(
            store
                .set_connector(&config, &empty, "elevenlabs", true, &values)
                .is_ok(),
            "elevenlabs enables once its key is stored"
        );
        let mut merged = config.clone();
        store.snapshot().apply(&mut merged).unwrap();
        assert!(merged.elevenlabs.enabled);
        std::fs::remove_dir_all(&root).ok();
    }
}
