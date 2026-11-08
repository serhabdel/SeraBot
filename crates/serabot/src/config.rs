//! Configuration and fleet templates.
//!
//! `bots/*.toml` + `prompts/*.txt` are **seed templates** for the per-user
//! fleets (phase 3 of the multi-user reconstruction): `load_bots` reads and
//! validates them once at startup, and `fleet::FleetRegistry` copies them
//! into each user's `bots` rows. The database is authoritative after seeding;
//! the admin console's `bot_config_overrides` are applied only when the
//! admin's own fleet is loaded, never to the templates.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, SeraError};
use crate::orchestration::{BotFleetMember, BotRole, validate_bot_fleet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub secrets: SecretsConfig,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub embeddings: EmbeddingsConfig,
    #[serde(default)]
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub api: ApiConfig,
    /// Multi-user OIDC sign-in. Disabled by default; the single-operator
    /// token flow keeps working unchanged while it is off.
    #[serde(default)]
    pub auth: AuthConfig,
    /// Per-user quotas and limits.
    #[serde(default)]
    pub users: UsersConfig,
    #[serde(default)]
    pub policy: PolicyConfig,
    #[serde(default)]
    pub watcher: WatcherConfig,
    #[serde(default)]
    pub onedrive: OneDriveConfig,
    #[serde(default)]
    pub firecrawl: FirecrawlConfig,
    #[serde(default)]
    pub tavily: TavilyConfig,
    #[serde(default)]
    pub elevenlabs: ElevenLabsConfig,
    /// Composio apps (per-user key; see docs/composio.md).
    #[serde(default)]
    pub composio: ComposioConfig,
    /// Per-user token connectors: GitHub, Todoist, Slack, Exa.
    #[serde(default)]
    pub token_connectors: TokenConnectorsConfig,
    #[serde(default)]
    pub render: RenderConfig,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub schedule: ScheduleConfig,
    #[serde(default)]
    pub documents: DocumentsConfig,
    #[serde(default)]
    pub audio: AudioConfig,
    #[serde(default)]
    pub ocr: OcrConfig,
    #[serde(default)]
    pub skills: SkillsConfig,
    #[serde(default)]
    pub browser: BrowserConfig,
    /// Password manager CLIs for vault references (see docs/vault.md).
    #[serde(default)]
    pub vault: VaultConfig,
    /// Tool approval policy (see docs/approvals.md).
    #[serde(default)]
    pub approvals: ApprovalsConfig,
    #[serde(default)]
    pub connectors: ConnectorsConfig,
    /// Fleet file locations. Relative to `base_dir`; the owner API never accepts paths.
    #[serde(default)]
    pub fleet: FleetConfig,
    /// Agent memory: recall provider, automatic extraction and summaries
    /// (see docs/memory.md).
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(skip)]
    pub base_dir: PathBuf,
    /// Admin-console bot config overrides (tools/skills/require_approval),
    /// applied when the admin's own fleet is loaded from the database. Never
    /// applied to the seed templates, so members do not inherit them.
    /// Populated from the admin overlay at load; never read from TOML.
    #[serde(skip)]
    pub bot_config_overrides: BTreeMap<String, crate::admin::BotConfigOverlay>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            database: DatabaseConfig::default(),
            secrets: SecretsConfig::default(),
            llm: LlmConfig::default(),
            embeddings: EmbeddingsConfig::default(),
            telegram: TelegramConfig::default(),
            api: ApiConfig::default(),
            auth: AuthConfig::default(),
            users: UsersConfig::default(),
            policy: PolicyConfig::default(),
            watcher: WatcherConfig::default(),
            onedrive: OneDriveConfig::default(),
            firecrawl: FirecrawlConfig::default(),
            tavily: TavilyConfig::default(),
            elevenlabs: ElevenLabsConfig::default(),
            composio: ComposioConfig::default(),
            token_connectors: TokenConnectorsConfig::default(),
            render: RenderConfig::default(),
            runtime: RuntimeConfig::default(),
            schedule: ScheduleConfig::default(),
            documents: DocumentsConfig::default(),
            audio: AudioConfig::default(),
            ocr: OcrConfig::default(),
            skills: SkillsConfig::default(),
            browser: BrowserConfig::default(),
            vault: VaultConfig::default(),
            approvals: ApprovalsConfig::default(),
            connectors: ConnectorsConfig::default(),
            fleet: FleetConfig::default(),
            memory: MemoryConfig::default(),
            base_dir: PathBuf::from("."),
            bot_config_overrides: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)?;
        let mut config: Self = toml::from_str(&raw)
            .map_err(|error| SeraError::Config(format!("{}: {error}", path.display())))?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        config.base_dir = parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf());
        config.validate()?;
        let overlay_path = crate::admin::AdminOverlay::path_for(&config);
        match crate::admin::AdminOverlay::load(&overlay_path) {
            Ok(overlay) => {
                let mut merged = config.clone();
                match overlay.apply(&mut merged).and_then(|()| merged.validate()) {
                    Ok(()) => config = merged,
                    Err(error) => {
                        // Never crash-loop the service on a bad overlay. The
                        // console can rewrite it and restart once the cause
                        // (for example a missing secret) is fixed.
                        tracing::error!(
                            %error,
                            path = %overlay_path.display(),
                            "admin overrides were not applied"
                        );
                    }
                }
            }
            Err(error) => {
                tracing::error!(
                    %error,
                    path = %overlay_path.display(),
                    "admin overrides could not be read"
                );
            }
        }
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.server.bind.is_empty() {
            return Err(SeraError::Config("server.bind must not be empty".into()));
        }
        if self.database.path.is_empty() {
            return Err(SeraError::Config("database.path must not be empty".into()));
        }
        if self.api.enabled
            && (self.api.token_hash.len() != 64
                || !self.api.token_hash.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(SeraError::Config(
                "api.token_hash must be a 64-character hex SHA-256 hash".into(),
            ));
        }
        self.auth.validate()?;
        self.users.validate()?;
        self.memory.validate()?;
        if !self.server.bind.starts_with("127.")
            && !self.server.bind.starts_with("localhost")
            && !self.server.bind.starts_with("[::1]")
            && !self.server.allow_non_loopback
        {
            return Err(SeraError::Config(
                "non-loopback server bind is disabled; terminate TLS in a reverse proxy or set server.allow_non_loopback explicitly"
                    .into(),
            ));
        }
        if self.llm.base_url.is_empty() || self.llm.model.is_empty() {
            return Err(SeraError::Config(
                "llm.base_url and llm.model are required".into(),
            ));
        }
        crate::providers::check_profile(
            &self.llm.kind,
            &self.llm.base_url,
            &self.llm.auth,
            &self.llm.refresh_token_secret,
            &self.llm.oauth_client_secret_secret,
            self.llm.catalog_fetch,
            self.llm.catalog_ttl_secs,
        )?;
        crate::cloud_code::validate_project_id(&self.llm.kind, &self.llm.project_id)?;
        for (id, profile) in &self.llm.providers {
            crate::providers::check_profile(
                &profile.kind,
                &profile.base_url,
                &profile.auth,
                &profile.refresh_token_secret,
                &profile.oauth_client_secret_secret,
                profile.catalog_fetch,
                profile.catalog_ttl_secs,
            )
            .map_err(|error| SeraError::Config(format!("llm.providers.{id}: {error}")))?;
            crate::cloud_code::validate_project_id(&profile.kind, &profile.project_id)
                .map_err(|error| SeraError::Config(format!("llm.providers.{id}: {error}")))?;
        }
        if self.embeddings.dimension == 0 || self.embeddings.dimension > 4096 {
            return Err(SeraError::Config(
                "embeddings.dimension must be between 1 and 4096".into(),
            ));
        }
        if self.telegram.enabled
            && (self.telegram.owner_user_id <= 0 || self.telegram.owner_chat_id == 0)
        {
            return Err(SeraError::Config(
                "telegram.owner_user_id and telegram.owner_chat_id are required".into(),
            ));
        }
        if self.runtime.max_concurrent_turns == 0 || self.runtime.max_concurrent_turns > 2 {
            return Err(SeraError::Config(
                "runtime.max_concurrent_turns must be 1 or 2".into(),
            ));
        }
        if self.render.max_pages == 0 || self.render.max_pages > 200 {
            return Err(SeraError::Config(
                "render.max_pages must be between 1 and 200".into(),
            ));
        }
        if self.documents.chunk_chars < 200
            || self.documents.chunk_overlap >= self.documents.chunk_chars
        {
            return Err(SeraError::Config(
                "documents.chunk_chars must be at least 200 and exceed chunk_overlap".into(),
            ));
        }
        if self.schedule.timezone.parse::<chrono_tz::Tz>().is_err() {
            return Err(SeraError::Config(
                "schedule.timezone must be a valid IANA timezone".into(),
            ));
        }
        for time in
            std::iter::once(&self.schedule.brief_time).chain(self.schedule.watch_times.iter())
        {
            parse_hhmm(time).ok_or_else(|| {
                SeraError::Config(format!("invalid schedule time '{time}'; expected HH:MM"))
            })?;
        }
        if self.firecrawl.enabled {
            if self.firecrawl.api_key_secret.trim().is_empty() {
                return Err(SeraError::Config(
                    "firecrawl.api_key_secret must not be empty when firecrawl is enabled".into(),
                ));
            }
            if self.firecrawl.max_crawl_limit == 0 || self.firecrawl.max_crawl_limit > 500 {
                return Err(SeraError::Config(
                    "firecrawl.max_crawl_limit must be between 1 and 500".into(),
                ));
            }
            if self.firecrawl.max_search_limit == 0 || self.firecrawl.max_search_limit > 50 {
                return Err(SeraError::Config(
                    "firecrawl.max_search_limit must be between 1 and 50".into(),
                ));
            }
        }
        if self.audio.enabled {
            if self.audio.transcription_model.is_empty()
                || self.audio.understanding_model.is_empty()
            {
                return Err(SeraError::Config(
                    "audio.transcription_model and audio.understanding_model are required when audio is enabled"
                        .into(),
                ));
            }
            if self.audio.max_bytes == 0 {
                return Err(SeraError::Config(
                    "audio.max_bytes must be greater than zero".into(),
                ));
            }
            if self.audio.telegram_intake && self.audio.telegram_max_bytes == 0 {
                return Err(SeraError::Config(
                    "audio.telegram_max_bytes must be greater than zero when telegram_intake is enabled"
                        .into(),
                ));
            }
        }
        if self.audio.telegram_intake && !self.telegram.enabled {
            return Err(SeraError::Config(
                "audio.telegram_intake requires telegram.enabled".into(),
            ));
        }
        if !matches!(self.audio.reply_mode.trim(), "text" | "voice") {
            return Err(SeraError::Config(
                "audio.reply_mode must be 'text' or 'voice'".into(),
            ));
        }
        if self.audio.reply_mode.trim() == "voice" && self.audio.voice_id.trim().is_empty() {
            return Err(SeraError::Config(
                "audio.reply_mode = 'voice' requires audio.voice_id".into(),
            ));
        }
        if self.audio.max_speech_chars == 0 || self.audio.max_speech_chars > 20_000 {
            return Err(SeraError::Config(
                "audio.max_speech_chars must be between 1 and 20000".into(),
            ));
        }
        if self.ocr.enabled {
            if self.ocr.model.trim().is_empty() {
                return Err(SeraError::Config(
                    "ocr.model is required when OCR is enabled".into(),
                ));
            }
            if self.ocr.max_bytes == 0 {
                return Err(SeraError::Config(
                    "ocr.max_bytes must be greater than zero".into(),
                ));
            }
        }
        if self.elevenlabs.enabled {
            if self.elevenlabs.api_base.is_empty() {
                return Err(SeraError::Config(
                    "elevenlabs.api_base must not be empty when elevenlabs is enabled".into(),
                ));
            }
            if self.elevenlabs.max_tts_chars == 0 {
                return Err(SeraError::Config(
                    "elevenlabs.max_tts_chars must be greater than zero".into(),
                ));
            }
            if self.elevenlabs.poll_max_attempts == 0 {
                return Err(SeraError::Config(
                    "elevenlabs.poll_max_attempts must be greater than zero".into(),
                ));
            }
            if self.elevenlabs.auth == ElevenLabsAuth::Oauth {
                if crate::mcp_client::parse_url(&self.elevenlabs.mcp_url).is_none() {
                    return Err(SeraError::Config(
                        "elevenlabs.mcp_url must be an http(s) URL without credentials".into(),
                    ));
                }
                let client_id = self.elevenlabs.oauth_client_id.trim();
                match reqwest::Url::parse(client_id) {
                    Ok(url)
                        if url.scheme() == "https"
                            && url.host_str().is_some()
                            && url.username().is_empty()
                            && url.password().is_none()
                            && url.query().is_none()
                            && url.fragment().is_none() => {}
                    _ => {
                        return Err(SeraError::Config(
                            "elevenlabs.oauth_client_id must be a credential-less https URL (CIMD client_id)"
                                .into(),
                        ));
                    }
                }
                for name in [
                    &self.elevenlabs.oauth_access_token_secret,
                    &self.elevenlabs.oauth_refresh_token_secret,
                ] {
                    if !valid_secret_name(name) {
                        return Err(SeraError::Config(
                            "elevenlabs OAuth token secret names must be secret-store key names"
                                .into(),
                        ));
                    }
                }
            }
        }
        self.browser.validate()?;
        self.connectors.validate()?;
        self.token_connectors.validate()?;
        validate_relative_dir("fleet.bots_dir", &self.fleet.bots_dir)?;
        validate_relative_dir("fleet.prompts_dir", &self.fleet.prompts_dir)?;
        Ok(())
    }

    pub fn path(&self, value: impl AsRef<Path>) -> PathBuf {
        let value = value.as_ref();
        if value.is_absolute() {
            value.to_path_buf()
        } else {
            self.base_dir.join(value)
        }
    }

    pub fn secrets_path(&self) -> PathBuf {
        self.path(&self.secrets.encrypted_file)
    }

    pub fn identity_path(&self) -> PathBuf {
        self.path(&self.secrets.identity_file)
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.path(&self.render.artifacts_dir)
    }

    pub fn database_path(&self) -> PathBuf {
        self.path(&self.database.path)
    }
}

/// Tool approval policy. Only destructive tools need approval by default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApprovalsConfig {
    /// When true, a bot's stored `require_approval` list can add approval
    /// for non-destructive tools. When false (default) those lists are
    /// ignored for non-destructive tools.
    #[serde(default)]
    pub respect_bot_lists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_body_limit")]
    pub body_limit_bytes: usize,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
    /// Per-minute request budget for each acting user (default 60). Public
    /// sign-in routes share one anonymous bucket. Multiple sessions of the
    /// same user share the user's budget rather than multiplying it.
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_minute: u32,
    /// Per-minute budget for each authenticated user's actions (default 1200).
    #[serde(default = "default_user_rate_limit")]
    pub user_rate_limit_per_minute: u32,
    /// Separate per-minute budget for each user's cheap idempotent polling
    /// reads (default 600), so polling never starves real actions.
    #[serde(default = "default_poll_rate_limit")]
    pub poll_rate_limit_per_minute: u32,
    #[serde(default = "default_event_retention_days")]
    pub event_retention_days: i64,
    #[serde(default)]
    pub allow_non_loopback: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            body_limit_bytes: default_body_limit(),
            request_timeout_secs: default_request_timeout(),
            rate_limit_per_minute: default_rate_limit(),
            user_rate_limit_per_minute: default_user_rate_limit(),
            poll_rate_limit_per_minute: default_poll_rate_limit(),
            event_retention_days: default_event_retention_days(),
            allow_non_loopback: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    #[serde(default = "default_db_path")]
    pub path: String,
    #[serde(default = "default_db_connections")]
    pub max_connections: u32,
    #[serde(default = "default_busy_timeout")]
    pub busy_timeout_ms: u32,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: default_db_path(),
            max_connections: default_db_connections(),
            busy_timeout_ms: default_busy_timeout(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretsConfig {
    #[serde(default = "default_secrets_file")]
    pub encrypted_file: String,
    #[serde(default = "default_identity_file")]
    pub identity_file: String,
    /// Server master key for the per-user credential store (phase 4). 32 raw
    /// bytes, generated on first run with mode 0600 on unix. It is **not** the
    /// age identity: the age store keeps server-level secrets, this key wraps
    /// each user's DEK. Never logged, never exposed by any API.
    #[serde(default = "default_master_key_file")]
    pub master_key_file: String,
}

impl Default for SecretsConfig {
    fn default() -> Self {
        Self {
            encrypted_file: default_secrets_file(),
            identity_file: default_identity_file(),
            master_key_file: default_master_key_file(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    #[serde(default = "default_llm_base_url")]
    pub base_url: String,
    #[serde(default = "default_llm_model")]
    pub model: String,
    #[serde(default = "default_llm_key_secret")]
    pub api_key_secret: String,
    #[serde(default = "default_llm_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_max_tool_rounds")]
    pub max_tool_rounds: usize,
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u32,
    #[serde(default = "default_data_egress_approved")]
    pub data_egress_approved: bool,
    #[serde(default)]
    pub bot_models: BTreeMap<String, String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub classes: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub catalog_fetch: bool,
    #[serde(default = "default_catalog_ttl_secs")]
    pub catalog_ttl_secs: u64,
    #[serde(default)]
    pub auth: String,
    #[serde(default)]
    pub refresh_token_secret: String,
    #[serde(default)]
    pub oauth_client_id_secret: String,
    #[serde(default)]
    pub oauth_client_secret_secret: String,
    /// Cloud Code companion project. Empty omits `project` from the envelope.
    #[serde(default)]
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key_secret: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub data_egress_approved: bool,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub catalog_fetch: bool,
    #[serde(default = "default_catalog_ttl_secs")]
    pub catalog_ttl_secs: u64,
    #[serde(default)]
    pub auth: String,
    #[serde(default)]
    pub refresh_token_secret: String,
    #[serde(default)]
    pub oauth_client_id_secret: String,
    #[serde(default)]
    pub oauth_client_secret_secret: String,
    /// Cloud Code companion project. Empty omits `project` from the envelope.
    #[serde(default)]
    pub project_id: String,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            api_key_secret: String::new(),
            models: Vec::new(),
            data_egress_approved: false,
            kind: String::new(),
            catalog_fetch: false,
            catalog_ttl_secs: default_catalog_ttl_secs(),
            auth: String::new(),
            refresh_token_secret: String::new(),
            oauth_client_id_secret: String::new(),
            oauth_client_secret_secret: String::new(),
            project_id: String::new(),
        }
    }
}

impl LlmConfig {
    pub fn default_provider(&self) -> ProviderConfig {
        ProviderConfig {
            base_url: self.base_url.clone(),
            api_key_secret: self.api_key_secret.clone(),
            models: std::iter::once(self.model.clone())
                .chain(self.models.iter().cloned())
                .chain(self.bot_models.values().cloned())
                .collect(),
            data_egress_approved: self.data_egress_approved,
            kind: self.kind.clone(),
            catalog_fetch: self.catalog_fetch,
            catalog_ttl_secs: self.catalog_ttl_secs,
            auth: self.auth.clone(),
            refresh_token_secret: self.refresh_token_secret.clone(),
            oauth_client_id_secret: self.oauth_client_id_secret.clone(),
            oauth_client_secret_secret: self.oauth_client_secret_secret.clone(),
            project_id: self.project_id.clone(),
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            base_url: default_llm_base_url(),
            model: default_llm_model(),
            api_key_secret: default_llm_key_secret(),
            timeout_secs: default_llm_timeout(),
            max_tool_rounds: default_max_tool_rounds(),
            max_output_tokens: default_max_output_tokens(),
            data_egress_approved: false,
            bot_models: BTreeMap::new(),
            models: Vec::new(),
            providers: BTreeMap::new(),
            kind: String::new(),
            catalog_fetch: false,
            catalog_ttl_secs: default_catalog_ttl_secs(),
            auth: String::new(),
            refresh_token_secret: String::new(),
            oauth_client_id_secret: String::new(),
            oauth_client_secret_secret: String::new(),
            project_id: String::new(),
            classes: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsConfig {
    #[serde(default = "default_skills_dir")]
    pub dir: String,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            dir: default_skills_dir(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetConfig {
    #[serde(default = "default_bots_dir")]
    pub bots_dir: String,
    #[serde(default = "default_prompts_dir")]
    pub prompts_dir: String,
}

impl Default for FleetConfig {
    fn default() -> Self {
        Self {
            bots_dir: default_bots_dir(),
            prompts_dir: default_prompts_dir(),
        }
    }
}

include!("config_browser_connectors.inc.rs");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingsConfig {
    #[serde(default = "default_embedding_model")]
    pub model: String,
    #[serde(default = "default_embedding_dimension")]
    pub dimension: usize,
    #[serde(default = "default_embedding_cache")]
    pub cache_dir: String,
    #[serde(default = "default_embedding_batch")]
    pub batch_size: usize,
}

impl Default for EmbeddingsConfig {
    fn default() -> Self {
        Self {
            model: default_embedding_model(),
            dimension: default_embedding_dimension(),
            cache_dir: default_embedding_cache(),
            batch_size: default_embedding_batch(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_telegram_secret")]
    pub bot_token_secret: String,
    #[serde(default)]
    pub owner_user_id: i64,
    #[serde(default)]
    pub owner_chat_id: i64,
    #[serde(default = "default_poll_timeout")]
    pub poll_timeout_secs: u64,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token_secret: default_telegram_secret(),
            owner_user_id: 0,
            owner_chat_id: 0,
            poll_timeout_secs: default_poll_timeout(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    #[serde(default = "default_api_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub token_hash: String,
    #[serde(default = "default_approval_expiry")]
    pub approval_expiry_hours: i64,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            enabled: default_api_enabled(),
            token_hash: String::new(),
            approval_expiry_hours: default_approval_expiry(),
        }
    }
}

/// Provider ids this build knows. Only `microsoft` can be enabled in v1;
/// `google` and `github` are rendered by the clients as coming soon.
pub const KNOWN_AUTH_PROVIDERS: [&str; 3] = ["microsoft", "google", "github"];
/// The subset of `KNOWN_AUTH_PROVIDERS` that can actually serve sign-in in
/// v1. Enabling another provider is additive configuration later; for now
/// listing it is allowed and it is reported disabled.
pub const ENABLED_AUTH_PROVIDERS: [&str; 1] = ["microsoft"];

/// Multi-user sign-in settings. Defaults keep everything off: while
/// `enabled = false` the server behaves exactly like the single-operator
/// deployment and every auth endpoint fails closed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    #[serde(default = "default_auth_enabled")]
    pub enabled: bool,
    /// Provider ids the deployment lists. Unknown ids are rejected at load;
    /// `google`/`github` are accepted but reported disabled in v1.
    #[serde(default = "default_auth_providers")]
    pub providers: Vec<String>,
    /// Microsoft tenant: `consumers`, `organizations`, a tenant id, or a
    /// domain. Shared with the OneDrive registration.
    #[serde(default = "default_auth_tenant")]
    pub tenant: String,
    /// Age-store key holding the public client id. The value is never
    /// logged and never leaves the process.
    #[serde(default = "default_auth_client_id_secret")]
    pub client_id_secret: String,
    /// Public base URL of this server (https). The OIDC redirect lands on
    /// `{base_url}/v1/auth/callback`, so this is the URL behind the reverse
    /// proxy, not the loopback bind.
    #[serde(default)]
    pub base_url: String,
    /// Session lifetime for browser sign-ins (default 720h = 30 days).
    #[serde(default = "default_auth_session_ttl")]
    pub session_ttl_hours: u64,
    /// Default invite lifetime (default 48h).
    #[serde(default = "default_auth_invite_ttl")]
    pub invite_ttl_hours: u64,
    /// Maximum number of accounts, invitations included. The product
    /// boundary is a small trusted circle (≤ ~10); the cap bounds runaway
    /// growth with an explicit error.
    #[serde(default = "default_auth_max_users")]
    pub max_users: usize,
    /// Optional email of the operator. While the bootstrap admin row has no
    /// Microsoft subject yet, the first sign-in whose verified email equals
    /// this (case-insensitive) claims that admin row: no invite needed, all
    /// backfilled data stays with the admin. Empty = no claim.
    #[serde(default)]
    pub admin_email: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: default_auth_enabled(),
            providers: default_auth_providers(),
            tenant: default_auth_tenant(),
            client_id_secret: default_auth_client_id_secret(),
            base_url: String::new(),
            session_ttl_hours: default_auth_session_ttl(),
            invite_ttl_hours: default_auth_invite_ttl(),
            max_users: default_auth_max_users(),
            admin_email: String::new(),
        }
    }
}

impl AuthConfig {
    /// The providers that can actually serve sign-ins right now: the
    /// configured list intersected with what this build implements.
    pub fn effective_providers(&self) -> Vec<&'static str> {
        ENABLED_AUTH_PROVIDERS
            .into_iter()
            .filter(|id| {
                self.providers
                    .iter()
                    .any(|configured| configured.eq_ignore_ascii_case(id))
            })
            .collect()
    }

    fn validate(&self) -> Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        for provider in &self.providers {
            let id = provider.trim();
            if id.is_empty() || !seen.insert(id.to_ascii_lowercase()) {
                return Err(SeraError::Config(
                    "auth.providers entries must be unique and non-empty".into(),
                ));
            }
            if !KNOWN_AUTH_PROVIDERS.contains(&id) {
                return Err(SeraError::Config(format!(
                    "auth.providers contains unknown provider '{id}'; known providers are microsoft, google, github"
                )));
            }
        }
        let tenant = self.tenant.trim();
        if tenant.is_empty()
            || tenant.len() > 128
            || tenant == "."
            || tenant == ".."
            || !tenant
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
        {
            return Err(SeraError::Config(
                "auth.tenant must be a tenant id or domain name".into(),
            ));
        }
        if !valid_secret_name(&self.client_id_secret) {
            return Err(SeraError::Config(
                "auth.client_id_secret must be a secret-store key name".into(),
            ));
        }
        // base_url is validated even while disabled so a typo cannot hide
        // until someone flips auth on.
        if !self.base_url.is_empty() {
            let url = reqwest::Url::parse(self.base_url.trim())
                .map_err(|_| SeraError::Config("auth.base_url must be a valid URL".into()))?;
            if url.scheme() != "https" {
                return Err(SeraError::Config(
                    "auth.base_url must use https; the OIDC redirect is public".into(),
                ));
            }
            if url.host_str().is_none_or(str::is_empty)
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(SeraError::Config(
                    "auth.base_url must be a bare URL: host only, no path, credentials, query, or fragment"
                        .into(),
                ));
            }
        }
        if self.session_ttl_hours == 0 || self.session_ttl_hours > 24 * 365 {
            return Err(SeraError::Config(
                "auth.session_ttl_hours must be between 1 and 8760".into(),
            ));
        }
        if self.invite_ttl_hours == 0 || self.invite_ttl_hours > 24 * 30 {
            return Err(SeraError::Config(
                "auth.invite_ttl_hours must be between 1 and 720".into(),
            ));
        }
        if self.max_users == 0 || self.max_users > 50 {
            return Err(SeraError::Config(
                "auth.max_users must be between 1 and 50".into(),
            ));
        }
        let admin_email = self.admin_email.trim();
        if !admin_email.is_empty()
            && (admin_email.len() > 320
                || !admin_email.contains('@')
                || admin_email
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace()))
        {
            return Err(SeraError::Config(
                "auth.admin_email must be an email address".into(),
            ));
        }
        if self.enabled && self.effective_providers().is_empty() {
            return Err(SeraError::Config(
                "auth.providers must include 'microsoft' when auth is enabled; google and github are not enableable yet"
                    .into(),
            ));
        }
        if self.enabled && self.base_url.trim().is_empty() {
            return Err(SeraError::Config(
                "auth.base_url is required when auth is enabled; it is the public https URL the OIDC redirect returns to"
                    .into(),
            ));
        }
        Ok(())
    }
}

/// Per-user quota settings. Enforced from phase 5; validated now so a bad
/// value cannot hide until the quota turns on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsersConfig {
    /// Daily task budget per user (default 500). Enforced atomically at the
    /// user-facing enqueue points. The window is the **UTC calendar day**,
    /// the same bucketing `usage_rollup` uses for its `per_day` rows; a user
    /// at the cap gets an explicit validation error, never a silent drop.
    #[serde(default = "default_users_task_quota")]
    pub tasks_per_user_per_day: u64,
}

impl Default for UsersConfig {
    fn default() -> Self {
        Self {
            tasks_per_user_per_day: default_users_task_quota(),
        }
    }
}

impl UsersConfig {
    fn validate(&self) -> Result<()> {
        if self.tasks_per_user_per_day == 0 || self.tasks_per_user_per_day > 10_000 {
            return Err(SeraError::Config(
                "users.tasks_per_user_per_day must be between 1 and 10000".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyConfig {
    #[serde(default = "default_blocklist_file")]
    pub blocklist_file: String,
    #[serde(default)]
    pub notify_owner_on_violation: bool,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            blocklist_file: default_blocklist_file(),
            notify_owner_on_violation: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherConfig {
    #[serde(default = "default_sources_file")]
    pub sources_file: String,
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
    #[serde(default = "default_fetch_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_fetch_bytes")]
    pub max_response_bytes: usize,
    #[serde(default)]
    pub morning_brief_enabled: bool,
    /// When true, a watch run or morning brief with zero new/changed items and
    /// zero failures sends no owner notification (the run is still recorded).
    /// Default false keeps the current always-notify behavior.
    #[serde(default)]
    pub silent_when_empty: bool,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            sources_file: default_sources_file(),
            user_agent: default_user_agent(),
            timeout_secs: default_fetch_timeout(),
            max_response_bytes: default_fetch_bytes(),
            morning_brief_enabled: false,
            silent_when_empty: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OneDriveConfig {
    /// Offer OneDrive to every account (phase 4). The admin's existing global
    /// refresh token keeps working: it is imported into the admin's
    /// credential row once at startup.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_graph_base")]
    pub graph_base: String,
    /// Microsoft tenant for the shared app registration (`common`,
    /// `consumers`, `organizations`, a tenant id, or a domain).
    #[serde(default = "default_tenant")]
    pub tenant: String,
    /// Age-store key holding the public app-registration client id. Shared
    /// with `[auth]` (one Azure app for sign-in and OneDrive consent).
    #[serde(default = "default_graph_client_secret")]
    pub client_id_secret: String,
    /// Age-store key holding the legacy global refresh token. It is imported
    /// into the admin's per-user credential on the first startup after the
    /// upgrade, then only the user store is used.
    #[serde(default = "default_graph_refresh_secret")]
    pub refresh_token_secret: String,
    /// Destination folder inside **each user's own** drive. The folder name
    /// is a template applied per drive; `..`, absolute paths, and separator
    /// tricks are rejected by the client.
    #[serde(default = "default_onedrive_folder")]
    pub folder: String,
    #[serde(default = "default_conflict_behavior")]
    pub conflict_behavior: String,
    /// Public base URL for the consent redirect. Empty falls back to
    /// `[auth].base_url`; when both are empty the consent endpoint fails with
    /// an explicit configuration error (the legacy global flow needs none).
    #[serde(default)]
    pub redirect_base_url: String,
}

impl Default for OneDriveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            graph_base: default_graph_base(),
            tenant: default_tenant(),
            client_id_secret: default_graph_client_secret(),
            refresh_token_secret: default_graph_refresh_secret(),
            folder: default_onedrive_folder(),
            conflict_behavior: default_conflict_behavior(),
            redirect_base_url: String::new(),
        }
    }
}

impl OneDriveConfig {
    /// The public base URL consent redirects land on: the explicit OneDrive
    /// override first, then the auth base URL (one app registration).
    pub fn effective_redirect_base<'a>(&'a self, auth: &'a AuthConfig) -> &'a str {
        let own = self.redirect_base_url.trim();
        if own.is_empty() {
            auth.base_url.trim()
        } else {
            own
        }
    }
}

/// Composio connection settings. Keys are per user (sealed in the user
/// store), so there is no secret name here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposioConfig {
    #[serde(default = "default_composio_api_base")]
    pub api_base: String,
    /// The Composio Connect MCP endpoint, used for Connect (`ck_`) keys.
    #[serde(default = "default_composio_mcp_base")]
    pub mcp_base: String,
    #[serde(default = "default_composio_timeout")]
    pub timeout_secs: u64,
    /// Largest Composio response body read, in bytes.
    #[serde(default = "default_composio_max_bytes")]
    pub max_response_bytes: usize,
    /// Toolkit version sent with every tool execution. Unset means `latest`.
    /// Set it to pin a specific Composio toolkit version (for example
    /// `00000000_00`, the version Composio uses when none is sent).
    #[serde(default)]
    pub tool_version: Option<String>,
}

impl Default for ComposioConfig {
    fn default() -> Self {
        Self {
            api_base: default_composio_api_base(),
            mcp_base: default_composio_mcp_base(),
            timeout_secs: default_composio_timeout(),
            max_response_bytes: default_composio_max_bytes(),
            tool_version: None,
        }
    }
}

fn default_composio_api_base() -> String {
    "https://backend.composio.dev".into()
}

fn default_composio_mcp_base() -> String {
    "https://connect.composio.dev/mcp".into()
}

fn default_composio_timeout() -> u64 {
    30
}

fn default_composio_max_bytes() -> usize {
    256 * 1024
}

/// API bases for the per-user token connectors (GitHub, Todoist, Slack, Exa).
/// Tokens are pasted by each user and sealed per user; only the bases, the
/// timeout and the response cap live here. Bases must be HTTPS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenConnectorsConfig {
    #[serde(default = "default_github_api_base")]
    pub github_api_base: String,
    #[serde(default = "default_todoist_api_base")]
    pub todoist_api_base: String,
    #[serde(default = "default_slack_api_base")]
    pub slack_api_base: String,
    #[serde(default = "default_exa_api_base")]
    pub exa_api_base: String,
    #[serde(default = "default_token_connectors_timeout")]
    pub timeout_secs: u64,
    /// Largest provider response body read, in bytes.
    #[serde(default = "default_token_connectors_max_bytes")]
    pub max_response_bytes: usize,
}

impl Default for TokenConnectorsConfig {
    fn default() -> Self {
        Self {
            github_api_base: default_github_api_base(),
            todoist_api_base: default_todoist_api_base(),
            slack_api_base: default_slack_api_base(),
            exa_api_base: default_exa_api_base(),
            timeout_secs: default_token_connectors_timeout(),
            max_response_bytes: default_token_connectors_max_bytes(),
        }
    }
}

impl TokenConnectorsConfig {
    fn validate(&self) -> Result<()> {
        for (name, base) in [
            ("github_api_base", &self.github_api_base),
            ("todoist_api_base", &self.todoist_api_base),
            ("slack_api_base", &self.slack_api_base),
            ("exa_api_base", &self.exa_api_base),
        ] {
            let url = url::Url::parse(base.trim()).map_err(|_| {
                SeraError::Config(format!("token_connectors.{name} must be an https URL"))
            })?;
            if url.scheme() != "https"
                || url.host_str().is_none_or(str::is_empty)
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(SeraError::Config(format!(
                    "token_connectors.{name} must be https without credentials, a query, or a fragment"
                )));
            }
        }
        if !(4 * 1024..=4 * 1024 * 1024).contains(&self.max_response_bytes) {
            return Err(SeraError::Config(
                "token_connectors.max_response_bytes must be between 4096 and 4194304".into(),
            ));
        }
        Ok(())
    }
}

fn default_github_api_base() -> String {
    "https://api.github.com".into()
}
fn default_todoist_api_base() -> String {
    "https://api.todoist.com".into()
}
fn default_slack_api_base() -> String {
    "https://slack.com/api".into()
}
fn default_exa_api_base() -> String {
    "https://api.exa.ai".into()
}
fn default_token_connectors_timeout() -> u64 {
    30
}
fn default_token_connectors_max_bytes() -> usize {
    1024 * 1024
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirecrawlConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_firecrawl_api_base")]
    pub api_base: String,
    #[serde(default = "default_firecrawl_key_secret")]
    pub api_key_secret: String,
    #[serde(default = "default_firecrawl_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_firecrawl_max_markdown")]
    pub max_markdown_chars: usize,
    #[serde(default = "default_firecrawl_max_map_links")]
    pub max_map_links: usize,
    #[serde(default = "default_firecrawl_max_map_limit")]
    pub max_map_limit: u32,
    #[serde(default = "default_firecrawl_max_search_limit")]
    pub max_search_limit: u32,
    #[serde(default = "default_firecrawl_max_crawl_limit")]
    pub max_crawl_limit: u32,
    #[serde(default = "default_firecrawl_crawl_poll_ms")]
    pub crawl_poll_interval_ms: u64,
    #[serde(default = "default_firecrawl_crawl_max_wait_secs")]
    pub crawl_max_wait_secs: u64,
}

impl Default for FirecrawlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_base: default_firecrawl_api_base(),
            api_key_secret: default_firecrawl_key_secret(),
            timeout_secs: default_firecrawl_timeout(),
            max_markdown_chars: default_firecrawl_max_markdown(),
            max_map_links: default_firecrawl_max_map_links(),
            max_map_limit: default_firecrawl_max_map_limit(),
            max_search_limit: default_firecrawl_max_search_limit(),
            max_crawl_limit: default_firecrawl_max_crawl_limit(),
            crawl_poll_interval_ms: default_firecrawl_crawl_poll_ms(),
            crawl_max_wait_secs: default_firecrawl_crawl_max_wait_secs(),
        }
    }
}

/// `[vault]`: where the password manager CLIs live. Empty means look the
/// binary up on `PATH` (`op` for 1Password, `pass-cli` for Proton Pass).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VaultConfig {
    #[serde(default)]
    pub op_path: String,
    #[serde(default)]
    pub pass_cli_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TavilyConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_tavily_api_base")]
    pub api_base: String,
    #[serde(default = "default_tavily_key_secret")]
    pub api_key_secret: String,
    #[serde(default = "default_tavily_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_tavily_max_results")]
    pub max_results: u32,
    #[serde(default = "default_tavily_max_content_chars")]
    pub max_content_chars: usize,
}

impl Default for TavilyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_base: default_tavily_api_base(),
            api_key_secret: default_tavily_key_secret(),
            timeout_secs: default_tavily_timeout(),
            max_results: default_tavily_max_results(),
            max_content_chars: default_tavily_max_content_chars(),
        }
    }
}

/// How the ElevenLabs connector authenticates. `api_key` sends the
/// `xi-api-key` header; `oauth` sends `Authorization: Bearer` from an
/// ElevenLabs OAuth sign-in (the REST API rejects requests that carry both).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElevenLabsAuth {
    #[default]
    ApiKey,
    Oauth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElevenLabsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_elevenlabs_api_base")]
    pub api_base: String,
    #[serde(default = "default_elevenlabs_api_key_secret")]
    pub api_key_secret: String,
    #[serde(default)]
    pub auth: ElevenLabsAuth,
    /// ElevenLabs hosted MCP endpoint (Streamable HTTP, OAuth bearer). Used
    /// only with OAuth sign-in.
    #[serde(default = "default_elevenlabs_mcp_url")]
    pub mcp_url: String,
    /// CIMD client_id: the https URL of the client metadata document this
    /// server publishes publicly (see `web::cimd_document`).
    #[serde(default = "default_elevenlabs_oauth_client_id")]
    pub oauth_client_id: String,
    #[serde(default = "default_elevenlabs_oauth_scope")]
    pub oauth_scope: String,
    #[serde(default = "default_elevenlabs_oauth_access_token_secret")]
    pub oauth_access_token_secret: String,
    #[serde(default = "default_elevenlabs_oauth_refresh_token_secret")]
    pub oauth_refresh_token_secret: String,
    #[serde(default = "default_elevenlabs_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_elevenlabs_voice_id")]
    pub default_voice_id: String,
    #[serde(default = "default_elevenlabs_tts_model")]
    pub tts_model: String,
    #[serde(default = "default_elevenlabs_stt_model")]
    pub stt_model: String,
    #[serde(default = "default_elevenlabs_image_model")]
    pub image_model: String,
    #[serde(default = "default_elevenlabs_video_model")]
    pub video_model: String,
    #[serde(default = "default_elevenlabs_max_tts_chars")]
    pub max_tts_chars: usize,
    #[serde(default = "default_elevenlabs_poll_interval_ms")]
    pub poll_interval_ms: u64,
    #[serde(default = "default_elevenlabs_poll_max_attempts")]
    pub poll_max_attempts: u32,
}

impl Default for ElevenLabsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_base: default_elevenlabs_api_base(),
            api_key_secret: default_elevenlabs_api_key_secret(),
            auth: ElevenLabsAuth::default(),
            mcp_url: default_elevenlabs_mcp_url(),
            oauth_client_id: default_elevenlabs_oauth_client_id(),
            oauth_scope: default_elevenlabs_oauth_scope(),
            oauth_access_token_secret: default_elevenlabs_oauth_access_token_secret(),
            oauth_refresh_token_secret: default_elevenlabs_oauth_refresh_token_secret(),
            timeout_secs: default_elevenlabs_timeout_secs(),
            default_voice_id: default_elevenlabs_voice_id(),
            tts_model: default_elevenlabs_tts_model(),
            stt_model: default_elevenlabs_stt_model(),
            image_model: default_elevenlabs_image_model(),
            video_model: default_elevenlabs_video_model(),
            max_tts_chars: default_elevenlabs_max_tts_chars(),
            poll_interval_ms: default_elevenlabs_poll_interval_ms(),
            poll_max_attempts: default_elevenlabs_poll_max_attempts(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderConfig {
    #[serde(default = "default_brand_file")]
    pub brand_file: String,
    #[serde(default = "default_artifacts_dir")]
    pub artifacts_dir: String,
    #[serde(default = "default_max_deck_pages")]
    pub max_pages: usize,
    /// Delete artifacts older than this many days. `0` keeps them forever.
    #[serde(default)]
    pub artifact_retention_days: u64,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            brand_file: default_brand_file(),
            artifacts_dir: default_artifacts_dir(),
            max_pages: default_max_deck_pages(),
            artifact_retention_days: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    #[serde(default = "default_max_turns")]
    pub max_concurrent_turns: usize,
    #[serde(default = "default_max_tool_calls")]
    pub max_tool_calls_per_turn: usize,
    #[serde(default = "default_tool_iterations")]
    pub max_tool_iterations: usize,
    #[serde(default = "default_lease")]
    pub task_lease_secs: u64,
    #[serde(default = "default_poll")]
    pub worker_poll_ms: u64,
    #[serde(default = "default_history_limit")]
    pub history_limit: usize,
    #[serde(default)]
    pub owner_standing_consent: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_concurrent_turns: 2,
            max_tool_calls_per_turn: default_max_tool_calls(),
            max_tool_iterations: default_tool_iterations(),
            task_lease_secs: default_lease(),
            worker_poll_ms: default_poll(),
            history_limit: default_history_limit(),
            owner_standing_consent: false,
        }
    }
}

/// `[memory]`: recall provider and the post-turn background work. Built-in
/// SQLite memory is always on; `provider = "mem0"` adds the Mem0 Platform as
/// an extra recall source for users who saved a Mem0 key (`conn.mem0`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// `builtin` (local only, the default) or `mem0`.
    #[serde(default = "default_memory_provider")]
    pub provider: String,
    /// Extract durable facts from owner turns after the reply is stored.
    #[serde(default = "default_true")]
    pub auto_extract: bool,
    /// Facts injected per turn (1..=20).
    #[serde(default = "default_recall_k")]
    pub recall_k: usize,
    /// Summarize a conversation after this many new messages; `0` turns
    /// summaries off, otherwise 10..=500.
    #[serde(default = "default_summary_every")]
    pub summary_every: usize,
    /// Mem0 Platform base URL. Must be https, or http on loopback (tests).
    #[serde(default = "default_mem0_base")]
    pub mem0_api_base: String,
    #[serde(default = "default_mem0_timeout")]
    pub mem0_timeout_secs: u64,
    /// Upper bound for the background extraction and summary call.
    #[serde(default = "default_extract_timeout")]
    pub extract_timeout_secs: u64,
    /// Model for extraction and summaries. Unset: the bot's own model.
    #[serde(default)]
    pub extract_model: Option<String>,
    /// Minimum seconds between two extraction runs for one user (0..=3600).
    #[serde(default = "default_extract_min_interval")]
    pub extract_min_interval_secs: u64,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            provider: default_memory_provider(),
            auto_extract: true,
            recall_k: default_recall_k(),
            summary_every: default_summary_every(),
            mem0_api_base: default_mem0_base(),
            mem0_timeout_secs: default_mem0_timeout(),
            extract_timeout_secs: default_extract_timeout(),
            extract_model: None,
            extract_min_interval_secs: default_extract_min_interval(),
        }
    }
}

impl MemoryConfig {
    pub fn validate(&self) -> Result<()> {
        if self.provider != "builtin" && self.provider != "mem0" {
            return Err(SeraError::Config(
                "memory.provider must be \"builtin\" or \"mem0\"".into(),
            ));
        }
        if !(1..=20).contains(&self.recall_k) {
            return Err(SeraError::Config("memory.recall_k must be 1..=20".into()));
        }
        if self.summary_every != 0 && !(10..=500).contains(&self.summary_every) {
            return Err(SeraError::Config(
                "memory.summary_every must be 0 (off) or 10..=500".into(),
            ));
        }
        let base = self.mem0_api_base.trim();
        let loopback = base.starts_with("http://127.0.0.1") || base.starts_with("http://localhost");
        if !(base.starts_with("https://") || loopback) || base.contains('@') {
            return Err(SeraError::Config(
                "memory.mem0_api_base must be an https URL without credentials".into(),
            ));
        }
        if !(2..=30).contains(&self.mem0_timeout_secs) {
            return Err(SeraError::Config(
                "memory.mem0_timeout_secs must be 2..=30".into(),
            ));
        }
        if !(5..=120).contains(&self.extract_timeout_secs) {
            return Err(SeraError::Config(
                "memory.extract_timeout_secs must be 5..=120".into(),
            ));
        }
        if !(0..=3600).contains(&self.extract_min_interval_secs) {
            return Err(SeraError::Config(
                "memory.extract_min_interval_secs must be 0..=3600".into(),
            ));
        }
        if let Some(model) = &self.extract_model
            && (model.is_empty() || model.len() > 128 || model.chars().any(char::is_whitespace))
        {
            return Err(SeraError::Config(
                "memory.extract_model must be a model id without spaces".into(),
            ));
        }
        Ok(())
    }
}

fn default_true() -> bool {
    true
}

fn default_memory_provider() -> String {
    "builtin".into()
}

fn default_recall_k() -> usize {
    8
}

fn default_summary_every() -> usize {
    30
}

fn default_mem0_base() -> String {
    "https://api.mem0.ai".into()
}

fn default_mem0_timeout() -> u64 {
    8
}

fn default_extract_timeout() -> u64 {
    45
}

fn default_extract_min_interval() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleConfig {
    #[serde(default = "default_tick")]
    pub tick_secs: u64,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default = "default_brief_time")]
    pub brief_time: String,
    #[serde(default = "default_source_stagger")]
    pub source_stagger_secs: u64,
    #[serde(default = "default_watch_times")]
    pub watch_times: Vec<String>,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            tick_secs: default_tick(),
            timezone: default_timezone(),
            brief_time: default_brief_time(),
            source_stagger_secs: default_source_stagger(),
            watch_times: default_watch_times(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentsConfig {
    #[serde(default = "default_watch_dir")]
    pub watch_dir: String,
    #[serde(default = "default_max_document_bytes")]
    pub max_bytes: u64,
    #[serde(default = "default_chunk_chars")]
    pub chunk_chars: usize,
    #[serde(default = "default_chunk_overlap")]
    pub chunk_overlap: usize,
    #[serde(default = "default_retrieval_k")]
    pub retrieval_k: usize,
}

impl Default for DocumentsConfig {
    fn default() -> Self {
        Self {
            watch_dir: default_watch_dir(),
            max_bytes: default_max_document_bytes(),
            chunk_chars: default_chunk_chars(),
            chunk_overlap: default_chunk_overlap(),
            retrieval_k: default_retrieval_k(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Reserved provider id: `default` uses [llm] credentials and egress consent.
    #[serde(default = "default_audio_provider")]
    pub provider: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_secret: Option<String>,
    #[serde(default = "default_audio_transcription_model")]
    pub transcription_model: String,
    #[serde(default = "default_audio_understanding_model")]
    pub understanding_model: String,
    /// Text-to-speech model for voice replies (Mistral Voxtral by default).
    #[serde(default = "default_audio_speech_model")]
    pub speech_model: String,
    /// Voice id for speech synthesis. Empty disables voice replies even when
    /// the provider is ready.
    #[serde(default)]
    pub voice_id: String,
    /// Default reply mode for new conversations: `text` or `voice`.
    #[serde(default = "default_reply_mode")]
    pub reply_mode: String,
    /// Cap on the characters sent to the speech endpoint per reply.
    #[serde(default = "default_max_speech_chars")]
    pub max_speech_chars: usize,
    #[serde(default = "default_audio_max_bytes")]
    pub max_bytes: u64,
    /// Save owner voice/audio messages into the inbox (Telegram only).
    #[serde(default)]
    pub telegram_intake: bool,
    #[serde(default = "default_telegram_audio_max_bytes")]
    pub telegram_max_bytes: u64,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_audio_provider(),
            base_url: None,
            api_key_secret: None,
            transcription_model: default_audio_transcription_model(),
            understanding_model: default_audio_understanding_model(),
            speech_model: default_audio_speech_model(),
            voice_id: String::new(),
            reply_mode: default_reply_mode(),
            max_speech_chars: default_max_speech_chars(),
            max_bytes: default_audio_max_bytes(),
            telegram_intake: false,
            telegram_max_bytes: default_telegram_audio_max_bytes(),
        }
    }
}

/// Optical character recognition for documents with no text layer.
///
/// Disabled by default. When enabled, `documents.rs` falls back to it only
/// when the built-in extractor returns less than `min_text_chars`, so a normal
/// text PDF never costs an OCR call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Reserved provider id: `default` uses [llm] credentials and egress consent.
    #[serde(default = "default_audio_provider")]
    pub provider: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_secret: Option<String>,
    #[serde(default = "default_ocr_model")]
    pub model: String,
    #[serde(default = "default_ocr_max_bytes")]
    pub max_bytes: u64,
    /// Extracted text below this many characters triggers the OCR fallback.
    #[serde(default = "default_ocr_min_text_chars")]
    pub min_text_chars: usize,
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_audio_provider(),
            base_url: None,
            api_key_secret: None,
            model: default_ocr_model(),
            max_bytes: default_ocr_max_bytes(),
            min_text_chars: default_ocr_min_text_chars(),
        }
    }
}

/// Longest shipped bot title, in characters.
pub const MAX_BOT_TITLE_CHARS: usize = 40;
/// Longest shipped bot description, in characters.
pub const MAX_BOT_TEMPLATE_DESCRIPTION_CHARS: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotConfig {
    pub name: String,
    /// Human title shown in the console in place of the raw id. Optional, so
    /// rows stored before 0.23 still parse; the console falls back to the id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// One-line description shown in the console. Optional, like `title`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "default_trigger")]
    pub trigger: String,
    #[serde(default)]
    pub model: Option<String>,
    pub system_prompt: String,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub require_approval: Vec<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Thinking level for models that support one (off, low, medium,
    /// high, ...). Ignored when the chosen model has no such level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default)]
    pub role: BotRole,
    #[serde(default)]
    pub delegates: Vec<String>,
}

impl BotConfig {
    /// Check the optional `title` and `description` of a shipped template:
    /// a title is 1 to 40 characters and a description at most 200, with no
    /// control characters. Absent fields are fine.
    pub fn validate_metadata(&self) -> Result<()> {
        if let Some(title) = &self.title {
            let count = title.trim().chars().count();
            if count == 0 || count > MAX_BOT_TITLE_CHARS || title.chars().any(char::is_control) {
                return Err(SeraError::Config(format!(
                    "bot '{}' title must be 1 to {MAX_BOT_TITLE_CHARS} characters",
                    self.name
                )));
            }
        }
        if let Some(description) = &self.description
            && (description.chars().count() > MAX_BOT_TEMPLATE_DESCRIPTION_CHARS
                || description.chars().any(char::is_control))
        {
            return Err(SeraError::Config(format!(
                "bot '{}' description must be at most {MAX_BOT_TEMPLATE_DESCRIPTION_CHARS} characters",
                self.name
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct LoadedBot {
    pub config: BotConfig,
    pub prompt: String,
}

pub fn load_bots(config: &Config, dir: impl AsRef<Path>) -> Result<Vec<LoadedBot>> {
    let dir = config.path(dir);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort();

    let mut bots = Vec::new();
    for path in paths {
        let raw = std::fs::read_to_string(&path)?;
        let bot: BotConfig = toml::from_str(&raw)
            .map_err(|error| SeraError::Config(format!("{}: {error}", path.display())))?;
        if bot.name.is_empty() || bot.name.len() > 64 {
            return Err(SeraError::Config(format!(
                "{}: invalid bot name",
                path.display()
            )));
        }
        if !matches!(bot.trigger.as_str(), "command" | "cron" | "file") {
            return Err(SeraError::Config(format!(
                "{}: trigger must be command, cron, or file",
                path.display()
            )));
        }
        if bot.system_prompt.is_empty() {
            return Err(SeraError::Config(format!(
                "{}: system_prompt is required",
                path.display()
            )));
        }
        bot.validate_metadata()
            .map_err(|error| SeraError::Config(format!("{}: {error}", path.display())))?;
        let prompt_path = config.path(&bot.system_prompt);
        let canonical = prompt_path.canonicalize().map_err(|error| {
            SeraError::Config(format!(
                "{}: cannot resolve prompt {}: {error}",
                path.display(),
                prompt_path.display()
            ))
        })?;
        if !canonical.starts_with(&config.base_dir) {
            return Err(SeraError::Config(format!(
                "{}: prompt escapes the configuration directory",
                path.display()
            )));
        }
        let prompt = std::fs::read_to_string(&canonical)?;
        if prompt.len() > 100_000 {
            return Err(SeraError::Config(format!(
                "{}: prompt is too large",
                path.display()
            )));
        }
        bots.push(LoadedBot {
            config: bot,
            prompt,
        });
    }

    let mut names = std::collections::HashSet::new();
    for bot in &bots {
        if !names.insert(bot.config.name.clone()) {
            return Err(SeraError::Config(format!(
                "duplicate bot name: {}",
                bot.config.name
            )));
        }
    }

    let fleet: Vec<BotFleetMember> = bots
        .iter()
        .map(|bot| BotFleetMember {
            name: bot.config.name.clone(),
            enabled: bot.config.enabled,
            role: bot.config.role,
            delegates: bot.config.delegates.clone(),
        })
        .collect();
    validate_bot_fleet(&fleet)?;

    Ok(bots)
}

/// Reject a bot row that names a tool this binary cannot register.
///
/// `known` is the union of every tool the binary can register, so a tool that
/// is merely disabled by configuration still passes; the runtime already warns
/// about those. A misspelled or removed name fails startup instead of silently
/// disappearing from the turn's tool notice.
pub fn validate_bot_tools(bots: &[LoadedBot], known: &[&str]) -> Result<()> {
    let known: std::collections::HashSet<&str> = known.iter().copied().collect();
    for bot in bots {
        for tool in &bot.config.tools {
            if !known.contains(tool.as_str()) {
                return Err(SeraError::Config(format!(
                    "bot '{}' lists unknown tool '{}'",
                    bot.config.name, tool
                )));
            }
        }
        for tool in &bot.config.require_approval {
            if !bot.config.tools.contains(tool) {
                return Err(SeraError::Config(format!(
                    "bot '{}' requires approval for '{}' but does not list it",
                    bot.config.name, tool
                )));
            }
        }
    }
    Ok(())
}

/// Console-facing bot summary. Prompt text is never part of a listing; the
/// detail endpoint reads the owner's row separately.
#[derive(Debug, Clone, Serialize)]
pub struct ConfiguredBot {
    pub id: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub role: String,
    pub enabled: bool,
    pub model: Option<String>,
    pub tools: Vec<String>,
    pub skills: Vec<String>,
    pub require_approval: Vec<String>,
}

/// Overlay the admin-console bot config (tools/skills/require_approval) over
/// the admin's own fleet rows. Only fields the console actually saved
/// override; members' fleets are loaded without it.
pub fn apply_bot_config_override(config: &Config, bot: &mut BotConfig) {
    if let Some(overrides) = config.bot_config_overrides.get(&bot.name) {
        if let Some(tools) = &overrides.tools {
            bot.tools = tools.clone();
        }
        if let Some(skills) = &overrides.skills {
            bot.skills = skills.clone();
        }
        if let Some(require_approval) = &overrides.require_approval {
            bot.require_approval = require_approval.clone();
        }
    }
}

/// Specialist tools the owner API may write into a scaffolded bot file.
/// `delegate` is orchestrator-only and is intentionally absent.
const SPECIALIST_TOOL_ALLOWLIST: &[&str] = &[
    "skill_list",
    "skill_search",
    "skill_load",
    "memory_save",
    "memory_list",
    "memory_search",
    "memory_update",
    "memory_delete",
    "rag_search",
    "xlsx_analyze",
    "timesheet_analyze",
    "document_index",
    "fetch_url",
    "watch_now",
    "audio_transcribe",
    "audio_understand",
    "firecrawl_scrape",
    "firecrawl_map",
    "firecrawl_search",
    "firecrawl_crawl",
    "firecrawl_extract",
    "elevenlabs_list_voices",
    "elevenlabs_text_to_speech",
    "elevenlabs_sound_effect",
    "elevenlabs_transcribe",
    "elevenlabs_generate_image",
    "elevenlabs_generate_video",
    "elevenlabs_mcp_list_tools",
    "elevenlabs_mcp_call",
    "deck_build",
    "docx_build",
    "docx_import",
    "docx_pdf",
    "docx_inspect",
    "docx_read",
    "docx_edit",
    "xlsx_build",
    "xlsx_import",
    "xlsx_inspect",
    "xlsx_read",
    "xlsx_edit",
    "pptx_import",
    "pptx_pdf",
    "pptx_inspect",
    "pptx_edit",
    "pdf_inspect",
    "pdf_edit",
    "pdf_merge",
    "image_inspect",
    "image_resize",
    "image_convert",
    "telegram_send_document",
    "onedrive_upload",
    "onedrive_list",
    "browser_navigate",
    "browser_snapshot",
    "browser_click",
    "browser_type",
    "browser_press",
    "browser_screenshot",
    "browser_close",
    "browser_tabs",
    "browser_new_tab",
    "google_drive_list",
    "google_sheets_read",
    "google_calendar_list_events",
    "gmail_list_messages",
    "outlook_list_messages",
    "outlook_calendar_list_events",
    "notion_query_database",
    "agentmail_inboxes_list",
    "agentmail_send",
    "github_search_repos",
    "github_list_issues",
    "github_get_issue",
    "github_create_issue",
    "github_comment_issue",
    "github_get_file",
    "todoist_list_tasks",
    "todoist_add_task",
    "todoist_complete_task",
    "slack_list_channels",
    "slack_read_channel",
    "slack_post_message",
    "exa_search",
    "exa_get_contents",
    "local_list",
    "local_search",
    "local_stat",
    "local_read",
    "local_write",
    "local_import",
    "local_export",
];

const DEFAULT_SPECIALIST_TOOLS: &[&str] = &["skill_list", "skill_search", "skill_load"];

const MAX_BOT_DESCRIPTION_CHARS: usize = 2000;
const MAX_SCAFFOLD_TOOLS: usize = 32;
const MAX_SCAFFOLD_SKILLS: usize = 16;

/// A specialist built from the fixed template, ready to become a per-user
/// `bots` row. No filesystem paths: the prompt lives in the row.
#[derive(Debug, Clone)]
pub struct ScaffoldedSpecialist {
    pub id: String,
    pub config: BotConfig,
    pub prompt: String,
    pub tools: Vec<String>,
    pub skills: Vec<String>,
    pub require_approval: Vec<String>,
}

/// Build the fixed specialist template as a per-user fleet row. `known_skills`
/// is the acting user's effective catalog ids (shipped ∪ their own rows).
/// The bot stays disabled. The caller writes the row through the acting
/// user's scoped store method.
pub fn build_specialist_bot(
    id: &str,
    description: &str,
    tools: Option<&[String]>,
    skills: Option<&[String]>,
    known_skills: &[String],
    policy: &crate::policy::Policy,
) -> Result<ScaffoldedSpecialist> {
    validate_bot_id(id)?;
    if description.chars().count() > MAX_BOT_DESCRIPTION_CHARS {
        return Err(SeraError::Validation("description is too long".into()));
    }
    if description
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(SeraError::Validation(
            "description contains unsupported control characters".into(),
        ));
    }
    let description = description.trim();
    let tools = match tools {
        Some(tools) => normalize_tool_list(tools)?,
        None => DEFAULT_SPECIALIST_TOOLS
            .iter()
            .map(|tool| (*tool).to_owned())
            .collect(),
    };
    let skills = normalize_skill_list(known_skills, skills.unwrap_or(&[]))?;
    let prompt = specialist_prompt(description);
    if !policy.is_allowed(description) || !policy.is_allowed(&prompt) {
        return Err(SeraError::PolicyViolation);
    }
    let require_approval: Vec<String> = tools
        .iter()
        .filter(|tool| crate::gateway::is_destructive_tool(tool))
        .cloned()
        .collect();
    let config = BotConfig {
        name: id.to_owned(),
        // A bot the owner names has no shipped title; the id is the fallback.
        title: None,
        description: None,
        trigger: "command".into(),
        model: None,
        // The prompt is stored in the row, not read from a file.
        system_prompt: String::new(),
        skills: skills.clone(),
        tools: tools.clone(),
        require_approval: require_approval.clone(),
        enabled: false,
        temperature: None,
        thinking: None,
        role: BotRole::Specialist,
        delegates: Vec::new(),
    };
    Ok(ScaffoldedSpecialist {
        id: id.to_owned(),
        config,
        prompt,
        tools,
        skills,
        require_approval,
    })
}

fn validate_bot_id(id: &str) -> Result<()> {
    crate::orchestration::validate_slug(id)?;
    if id == crate::orchestration::ORCHESTRATOR_BOT || is_windows_device_name(id) {
        return Err(SeraError::Validation("bot id is reserved".into()));
    }
    Ok(())
}

fn is_windows_device_name(id: &str) -> bool {
    matches!(id, "con" | "prn" | "aux" | "nul")
        || (id.len() == 4
            && (id.starts_with("com") || id.starts_with("lpt"))
            && id.as_bytes()[3].is_ascii_digit())
}

fn normalize_tool_list(tools: &[String]) -> Result<Vec<String>> {
    if tools.len() > MAX_SCAFFOLD_TOOLS {
        return Err(SeraError::Validation("too many tools".into()));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        if tool == "delegate" {
            return Err(SeraError::Validation(
                "specialists cannot be granted delegate".into(),
            ));
        }
        if !SPECIALIST_TOOL_ALLOWLIST.contains(&tool.as_str()) {
            return Err(SeraError::Validation(format!(
                "tool '{tool}' is not on the specialist allowlist"
            )));
        }
        if !seen.insert(tool.clone()) {
            return Err(SeraError::Validation(format!("duplicate tool '{tool}'")));
        }
        out.push(tool.clone());
    }
    Ok(out)
}

fn normalize_skill_list(known: &[String], skills: &[String]) -> Result<Vec<String>> {
    if skills.len() > MAX_SCAFFOLD_SKILLS {
        return Err(SeraError::Validation("too many skills".into()));
    }
    if skills.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(skills.len());
    for skill in skills {
        crate::skills::validate_skill_id(skill)?;
        if !known.iter().any(|id| id == skill) {
            return Err(SeraError::Validation(format!("unknown skill '{skill}'")));
        }
        if !seen.insert(skill.clone()) {
            return Err(SeraError::Validation(format!("duplicate skill '{skill}'")));
        }
        out.push(skill.clone());
    }
    Ok(out)
}

fn specialist_prompt(description: &str) -> String {
    let mut body = String::from(
        "You are a specialist bot. Follow the owner request, use only allowlisted tools, and do not delegate.\n",
    );
    if !description.is_empty() {
        body.push('\n');
        body.push_str(description);
        body.push('\n');
    }
    body
}

fn validate_relative_dir(label: &str, value: &str) -> Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || !path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return Err(SeraError::Config(format!(
            "{label} must be a relative directory inside the configuration directory"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    pub name: String,
    pub url: String,
    #[serde(default = "default_source_kind")]
    pub kind: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_source_selectors")]
    pub item_selector: String,
    #[serde(default = "default_title_selector")]
    pub title_selector: String,
    #[serde(default = "default_date_selector")]
    pub date_selector: String,
    #[serde(default = "default_link_selector")]
    pub link_selector: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SourcesFile {
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
}

pub fn load_sources(config: &Config) -> Result<Vec<SourceConfig>> {
    let path = config.path(&config.watcher.sources_file);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    let file: SourcesFile = toml::from_str(&raw)
        .map_err(|error| SeraError::Config(format!("{}: {error}", path.display())))?;
    let mut names = std::collections::HashSet::new();
    for source in &file.sources {
        if source.name.is_empty() || !names.insert(source.name.clone()) {
            return Err(SeraError::Config("source names must be unique".into()));
        }
        if !matches!(source.kind.as_str(), "html" | "rss") {
            return Err(SeraError::Config(format!(
                "source {}: kind must be html or rss",
                source.name
            )));
        }
    }
    Ok(file.sources)
}

fn default_bind() -> String {
    "127.0.0.1:8787".into()
}
fn default_body_limit() -> usize {
    2 * 1024 * 1024
}
fn default_request_timeout() -> u64 {
    30
}
fn default_user_rate_limit() -> u32 {
    1200
}

fn default_poll_rate_limit() -> u32 {
    600
}

fn default_rate_limit() -> u32 {
    60
}
fn default_event_retention_days() -> i64 {
    30
}
fn default_db_path() -> String {
    "data/serabot.db".into()
}
fn default_db_connections() -> u32 {
    4
}
fn default_busy_timeout() -> u32 {
    5000
}
fn default_secrets_file() -> String {
    "private/secrets.age".into()
}
fn default_identity_file() -> String {
    "private/age-identity.txt".into()
}
fn default_master_key_file() -> String {
    "private/master.key".into()
}
fn default_llm_base_url() -> String {
    "https://api.mistral.ai/v1".into()
}
fn default_llm_model() -> String {
    "mistral-large-latest".into()
}
fn default_llm_key_secret() -> String {
    "llm_api_key".into()
}
fn default_llm_timeout() -> u64 {
    120
}
fn default_catalog_ttl_secs() -> u64 {
    1800
}
fn default_skills_dir() -> String {
    "skills".into()
}
fn default_bots_dir() -> String {
    "bots".into()
}
fn default_prompts_dir() -> String {
    "prompts".into()
}
fn default_max_tool_rounds() -> usize {
    10
}
fn default_max_output_tokens() -> u32 {
    32768
}
fn default_data_egress_approved() -> bool {
    false
}
fn default_embedding_model() -> String {
    "multilingual-e5-small".into()
}
fn default_embedding_dimension() -> usize {
    384
}
fn default_embedding_cache() -> String {
    "models".into()
}
fn default_embedding_batch() -> usize {
    16
}
fn default_telegram_secret() -> String {
    "telegram_bot_token".into()
}
fn default_poll_timeout() -> u64 {
    25
}
fn default_api_enabled() -> bool {
    true
}
fn default_approval_expiry() -> i64 {
    24
}
fn default_auth_enabled() -> bool {
    false
}
fn default_auth_providers() -> Vec<String> {
    vec!["microsoft".into()]
}
fn default_auth_tenant() -> String {
    "consumers".into()
}
fn default_auth_client_id_secret() -> String {
    "auth_client_id".into()
}
fn default_auth_session_ttl() -> u64 {
    720
}
fn default_auth_invite_ttl() -> u64 {
    48
}
fn default_auth_max_users() -> usize {
    10
}
fn default_users_task_quota() -> u64 {
    500
}
fn default_blocklist_file() -> String {
    "private/blocklist.txt".into()
}
fn default_sources_file() -> String {
    "config/sources.toml".into()
}
fn default_user_agent() -> String {
    "SeraBot/0.1 (personal research agent)".into()
}
fn default_fetch_timeout() -> u64 {
    30
}
fn default_fetch_bytes() -> usize {
    5 * 1024 * 1024
}
fn default_graph_base() -> String {
    "https://graph.microsoft.com/v1.0".into()
}
fn default_tenant() -> String {
    "consumers".into()
}
fn default_graph_client_secret() -> String {
    "onedrive_client_id".into()
}
fn default_graph_refresh_secret() -> String {
    "onedrive_refresh_token".into()
}
fn default_onedrive_folder() -> String {
    "SeraBot".into()
}
fn default_conflict_behavior() -> String {
    "rename".into()
}
fn default_firecrawl_api_base() -> String {
    "https://api.firecrawl.dev".into()
}
fn default_firecrawl_key_secret() -> String {
    "firecrawl_api_key".into()
}
fn default_firecrawl_timeout() -> u64 {
    120
}
fn default_firecrawl_max_markdown() -> usize {
    80_000
}
fn default_firecrawl_max_map_links() -> usize {
    500
}
fn default_firecrawl_max_map_limit() -> u32 {
    500
}
fn default_firecrawl_max_search_limit() -> u32 {
    10
}
fn default_firecrawl_max_crawl_limit() -> u32 {
    25
}
fn default_firecrawl_crawl_poll_ms() -> u64 {
    2_000
}
fn default_firecrawl_crawl_max_wait_secs() -> u64 {
    300
}
fn default_tavily_api_base() -> String {
    "https://api.tavily.com".into()
}
fn default_tavily_key_secret() -> String {
    "tavily_api_key".into()
}
fn default_tavily_timeout() -> u64 {
    60
}
fn default_tavily_max_results() -> u32 {
    10
}
fn default_tavily_max_content_chars() -> usize {
    20_000
}
fn default_brand_file() -> String {
    "config/brand.toml".into()
}
fn default_artifacts_dir() -> String {
    "data/artifacts".into()
}
fn default_max_deck_pages() -> usize {
    30
}
fn default_max_turns() -> usize {
    2
}
fn default_max_tool_calls() -> usize {
    12
}
fn default_tool_iterations() -> usize {
    10
}
fn default_lease() -> u64 {
    300
}
fn default_poll() -> u64 {
    500
}
fn default_history_limit() -> usize {
    50
}
fn default_tick() -> u64 {
    30
}
fn default_timezone() -> String {
    "Europe/Rome".into()
}
fn default_brief_time() -> String {
    "08:00".into()
}
fn default_source_stagger() -> u64 {
    20
}
fn default_watch_times() -> Vec<String> {
    vec!["07:45".into(), "13:00".into(), "18:00".into()]
}
fn parse_hhmm(value: &str) -> Option<(u32, u32)> {
    let (hour, minute) = value.split_once(':')?;
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    (hour < 24 && minute < 60).then_some((hour, minute))
}
fn default_watch_dir() -> String {
    "data/inbox".into()
}
fn default_max_document_bytes() -> u64 {
    50 * 1024 * 1024
}
fn default_audio_provider() -> String {
    "default".into()
}
fn default_audio_transcription_model() -> String {
    "voxtral-mini-latest".into()
}
fn default_audio_understanding_model() -> String {
    "voxtral-small-latest".into()
}
fn default_audio_speech_model() -> String {
    "voxtral-mini-tts-2603".into()
}
fn default_reply_mode() -> String {
    "text".into()
}
fn default_max_speech_chars() -> usize {
    4000
}
fn default_ocr_model() -> String {
    "mistral-ocr-latest".into()
}
fn default_ocr_max_bytes() -> u64 {
    20_971_520
}
fn default_ocr_min_text_chars() -> usize {
    200
}
fn default_audio_max_bytes() -> u64 {
    52_428_800
}
fn default_telegram_audio_max_bytes() -> u64 {
    20_971_520
}
fn default_elevenlabs_api_base() -> String {
    "https://api.elevenlabs.io".into()
}
fn default_elevenlabs_mcp_url() -> String {
    crate::elevenlabs::ELEVENLABS_MCP_URL.into()
}
fn default_elevenlabs_api_key_secret() -> String {
    "elevenlabs_api_key".into()
}
fn default_elevenlabs_oauth_client_id() -> String {
    crate::elevenlabs::cimd_client_id()
}
fn default_elevenlabs_oauth_scope() -> String {
    crate::elevenlabs::DEFAULT_OAUTH_SCOPE.into()
}
fn default_elevenlabs_oauth_access_token_secret() -> String {
    "elevenlabs_access_token".into()
}
fn default_elevenlabs_oauth_refresh_token_secret() -> String {
    "elevenlabs_refresh_token".into()
}
fn default_elevenlabs_timeout_secs() -> u64 {
    120
}
fn default_elevenlabs_voice_id() -> String {
    String::new()
}
fn default_elevenlabs_tts_model() -> String {
    "eleven_multilingual_v2".into()
}
fn default_elevenlabs_stt_model() -> String {
    "scribe_v2".into()
}
fn default_elevenlabs_image_model() -> String {
    "gemini-2.5-flash-image".into()
}
fn default_elevenlabs_video_model() -> String {
    "bytedance-seedance-v2-mini".into()
}
fn default_elevenlabs_max_tts_chars() -> usize {
    5_000
}
fn default_elevenlabs_poll_interval_ms() -> u64 {
    2_000
}
fn default_elevenlabs_poll_max_attempts() -> u32 {
    60
}
fn default_chunk_chars() -> usize {
    1200
}
fn default_chunk_overlap() -> usize {
    150
}
fn default_retrieval_k() -> usize {
    8
}
fn default_trigger() -> String {
    "command".into()
}
fn default_enabled() -> bool {
    true
}
fn default_source_kind() -> String {
    "html".into()
}
fn default_source_selectors() -> String {
    "article".into()
}
fn default_title_selector() -> String {
    "h2, h3, a".into()
}
fn default_date_selector() -> String {
    "time".into()
}
fn default_link_selector() -> String {
    "a[href]".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_bots_only_name_known_tools() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bots");
        let known = crate::tools::known_tool_names();
        let mut checked = 0usize;
        for entry in std::fs::read_dir(&root).expect("bots directory") {
            let path = entry.expect("bots entry").path();
            if path.extension().and_then(|value| value.to_str()) != Some("toml") {
                continue;
            }
            let raw = std::fs::read_to_string(&path).expect("bot file");
            let config: BotConfig =
                toml::from_str(&raw).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            for tool in &config.tools {
                assert!(
                    known.contains(&tool.as_str()),
                    "{} names unknown tool '{tool}'",
                    path.display()
                );
            }
            for tool in &config.require_approval {
                assert!(
                    config.tools.contains(tool),
                    "{} requires approval for '{tool}' but does not list it",
                    path.display()
                );
            }
            checked += 1;
        }
        assert!(checked >= 5, "expected the shipped bot files");
    }

    /// A row stored before 0.23 has no title or description and still parses;
    /// a new template may set both, and an unset field is not written back.
    #[test]
    fn bot_config_parses_with_and_without_title() {
        let legacy: BotConfig =
            toml::from_str("name = \"legacy\"\nsystem_prompt = \"p\"\n").unwrap();
        assert!(legacy.title.is_none() && legacy.description.is_none());
        legacy.validate_metadata().unwrap();
        let value = serde_json::to_value(&legacy).unwrap();
        assert!(value.get("title").is_none());
        assert!(value.get("description").is_none());

        let titled: BotConfig = toml::from_str(
            "name = \"auditor\"\ntitle = \"Auditor\"\ndescription = \"Runs checks.\"\nsystem_prompt = \"p\"\n",
        )
        .unwrap();
        assert_eq!(titled.title.as_deref(), Some("Auditor"));
        assert_eq!(titled.description.as_deref(), Some("Runs checks."));
        titled.validate_metadata().unwrap();
        let round: BotConfig =
            serde_json::from_value(serde_json::to_value(&titled).unwrap()).unwrap();
        assert_eq!(round.title, titled.title);
        assert_eq!(round.description, titled.description);
    }

    #[test]
    fn bot_metadata_lengths_are_checked() {
        let with = |title: Option<String>, description: Option<String>| BotConfig {
            name: "synthetic".into(),
            title,
            description,
            trigger: "command".into(),
            model: None,
            system_prompt: "prompts/main.txt".into(),
            skills: Vec::new(),
            tools: Vec::new(),
            require_approval: Vec::new(),
            enabled: true,
            temperature: None,
            thinking: None,
            role: BotRole::default(),
            delegates: Vec::new(),
        };
        let long_title = "t".repeat(MAX_BOT_TITLE_CHARS + 1);
        let long_description = "d".repeat(MAX_BOT_TEMPLATE_DESCRIPTION_CHARS + 1);
        with(Some("t".repeat(MAX_BOT_TITLE_CHARS)), None)
            .validate_metadata()
            .unwrap();
        with(None, Some("d".repeat(MAX_BOT_TEMPLATE_DESCRIPTION_CHARS)))
            .validate_metadata()
            .unwrap();
        assert!(with(Some(long_title), None).validate_metadata().is_err());
        assert!(with(Some("   ".into()), None).validate_metadata().is_err());
        assert!(
            with(Some("Bad\u{7}".into()), None)
                .validate_metadata()
                .is_err()
        );
        assert!(
            with(None, Some(long_description))
                .validate_metadata()
                .is_err()
        );
        assert!(
            with(None, Some("Two\nlines".into()))
                .validate_metadata()
                .is_err()
        );
    }

    /// Every shipped template has a human title and a one-line description,
    /// so no bot falls back to its raw id in the console.
    #[test]
    fn shipped_bots_have_a_title_and_a_description() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bots");
        let mut checked = 0usize;
        for entry in std::fs::read_dir(&root).expect("bots directory") {
            let path = entry.expect("bots entry").path();
            if path.extension().and_then(|value| value.to_str()) != Some("toml") {
                continue;
            }
            let bot: BotConfig = toml::from_str(&std::fs::read_to_string(&path).expect("bot file"))
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            bot.validate_metadata()
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let title = bot.title.as_deref().unwrap_or_default();
            let description = bot.description.as_deref().unwrap_or_default();
            assert!(!title.trim().is_empty(), "{} needs a title", path.display());
            assert!(
                !description.trim().is_empty(),
                "{} needs a description",
                path.display()
            );
            checked += 1;
        }
        assert!(checked >= 12, "expected the twelve shipped bot files");
    }

    fn shipped_bot(name: &str) -> BotConfig {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bots");
        let raw = std::fs::read_to_string(root.join(format!("{name}.toml"))).expect("bot file");
        toml::from_str(&raw).unwrap_or_else(|error| panic!("{name}: {error}"))
    }

    /// A prompt may name a tool only when its bot is granted it, or when the
    /// runtime grants it on every turn (memory, plan, skill, and team tools).
    #[test]
    fn shipped_prompts_only_name_tools_their_bot_has() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let known = crate::tools::known_tool_names();
        let always = [
            "memory_save",
            "memory_list",
            "memory_delete",
            crate::plans::PLAN_TOOL,
            "skill_list",
            "skill_search",
            "skill_load",
            crate::teams::TEAM_ASK_TOOL,
            crate::teams::TEAM_REPLY_TOOL,
        ];
        let mut checked = 0usize;
        for entry in std::fs::read_dir(repo.join("bots")).expect("bots directory") {
            let path = entry.expect("bots entry").path();
            if path.extension().and_then(|value| value.to_str()) != Some("toml") {
                continue;
            }
            let raw = std::fs::read_to_string(&path).expect("bot file");
            let bot: BotConfig =
                toml::from_str(&raw).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let prompt_path = repo.join(&bot.system_prompt);
            let prompt = std::fs::read_to_string(&prompt_path).expect("prompt file");
            for word in
                prompt.split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
            {
                let is_tool = known.contains(&word);
                let always_granted = always.contains(&word);
                let granted = bot.tools.iter().any(|tool| tool == word);
                assert!(
                    !is_tool || always_granted || granted,
                    "{} names tool '{word}' but bot '{}' is not granted it",
                    prompt_path.display(),
                    bot.name
                );
            }
            checked += 1;
        }
        assert!(checked >= 5, "expected the shipped bot files");
    }

    /// The owner's main bot can reach the PC folders and the approval-gated
    /// document send. The specialists may read PC folders, never write them.
    #[test]
    fn local_folder_tools_are_granted_to_main_and_read_only_to_specialists() {
        let main = shipped_bot("main");
        for tool in [
            "local_list",
            "local_read",
            "local_write",
            "telegram_send_document",
        ] {
            assert!(main.tools.iter().any(|name| name == tool), "main: {tool}");
        }
        for name in ["analyst", "maker"] {
            let bot = shipped_bot(name);
            for tool in ["local_list", "local_read"] {
                assert!(
                    bot.tools.iter().any(|granted| granted == tool),
                    "{name}: {tool}"
                );
            }
            assert!(
                !bot.tools.iter().any(|granted| granted == "local_write"),
                "{name} must not write to the PC"
            );
        }
    }

    #[test]
    fn validate_bot_tools_rejects_an_unknown_name() {
        let bot = LoadedBot {
            config: BotConfig {
                name: "synthetic".into(),
                title: None,
                description: None,
                trigger: "command".into(),
                model: None,
                system_prompt: "prompts/main.txt".into(),
                skills: Vec::new(),
                tools: vec!["deck_build".into(), "docx_teleport".into()],
                require_approval: Vec::new(),
                enabled: true,
                temperature: None,
                thinking: None,
                role: BotRole::default(),
                delegates: Vec::new(),
            },
            prompt: String::new(),
        };
        let error = validate_bot_tools(&[bot], &["deck_build"]).unwrap_err();
        assert!(error.to_string().contains("docx_teleport"));
    }

    #[test]
    fn rejects_empty_llm_config() {
        let mut config = Config::default();
        config.llm.model.clear();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_api_hash() {
        let mut config = Config::default();
        config.api.enabled = true;
        config.api.token_hash = "not-a-hash".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn parses_minimal_config() {
        let raw = r#"
[llm]
model = "test-model"
[api]
enabled = false
"#;
        let config: Config = toml::from_str(raw).unwrap();
        assert_eq!(config.llm.model, "test-model");
        assert!(!config.api.enabled);
        assert_eq!(config.fleet.bots_dir, "bots");
        assert_eq!(config.fleet.prompts_dir, "prompts");
    }

    #[test]
    fn rejects_fleet_dir_that_escapes_the_configuration_directory() {
        let mut config = Config::default();
        config.fleet.bots_dir = "../bots".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn builds_a_disabled_specialist_from_the_fixed_template() {
        let policy = crate::policy::Policy::empty();
        let known = vec!["ask".to_owned()];
        let scaffolded = build_specialist_bot(
            "notes",
            "Synthetic notes specialist.",
            Some(&["rag_search".into(), "firecrawl_search".into()]),
            Some(&["ask".into()]),
            &known,
            &policy,
        )
        .unwrap();
        assert_eq!(scaffolded.id, "notes");
        assert_eq!(scaffolded.config.name, "notes");
        assert!(!scaffolded.config.enabled);
        assert_eq!(scaffolded.config.role, BotRole::Specialist);
        assert!(scaffolded.config.delegates.is_empty());
        assert!(scaffolded.config.system_prompt.is_empty());
        assert_eq!(
            scaffolded.config.tools,
            vec!["rag_search", "firecrawl_search"]
        );
        assert_eq!(scaffolded.config.skills, vec!["ask"]);
        assert!(scaffolded.config.require_approval.is_empty());
        assert!(
            !scaffolded
                .config
                .tools
                .iter()
                .any(|tool| tool == "delegate")
        );
        assert!(scaffolded.prompt.contains("do not delegate"));
        assert!(scaffolded.prompt.contains("Synthetic notes specialist."));
        assert!(build_specialist_bot("../outside", "", None, None, &known, &policy).is_err());
        assert!(build_specialist_bot("main", "", None, None, &known, &policy).is_err());
        assert!(
            build_specialist_bot(
                "other",
                "",
                Some(&["delegate".into()]),
                None,
                &known,
                &policy
            )
            .is_err()
        );
        assert!(
            build_specialist_bot(
                "other",
                "",
                Some(&["not_a_tool".into()]),
                None,
                &known,
                &policy
            )
            .is_err()
        );
        assert!(
            build_specialist_bot(
                "other",
                "",
                None,
                Some(&["missing-skill".into()]),
                &known,
                &policy
            )
            .is_err()
        );
        let blocked = crate::policy::Policy::from_text("synthetic blocked phrase");
        assert!(
            build_specialist_bot(
                "other",
                "notes about synthetic blocked phrase",
                None,
                None,
                &known,
                &blocked
            )
            .is_err()
        );
    }

    #[test]
    fn connector_defaults_name_secret_keys_and_reject_bad_redirects() {
        let config = ConnectorsConfig::default();
        assert!(!config.any_enabled());
        assert_eq!(
            config.google_drive.client_id_secret,
            "google_oauth_client_id"
        );
        assert_eq!(
            config.google_drive.client_secret_secret,
            "google_oauth_client_secret"
        );
        assert_eq!(
            config.google_drive.refresh_token_secret,
            "google_drive_refresh_token"
        );
        assert_eq!(
            config.google_sheets.refresh_token_secret,
            "google_sheets_refresh_token"
        );
        assert_eq!(config.gmail.refresh_token_secret, "gmail_refresh_token");
        assert_eq!(config.outlook.client_id_secret, "outlook_client_id");
        assert_eq!(
            config.outlook_calendar.refresh_token_secret,
            "outlook_calendar_refresh_token"
        );
        assert_eq!(
            config.notion.client_secret_secret,
            "notion_oauth_client_secret"
        );
        assert_eq!(config.notion.refresh_token_secret, "notion_refresh_token");
        assert_eq!(config.agentmail.api_base, "https://api.agentmail.to/v0");
        assert_eq!(config.agentmail.api_key_secret, "agentmail_api_key");

        let parsed: ConnectorsConfig = toml::from_str("[gmail]\nenabled = false\n").unwrap();
        assert_eq!(parsed.gmail.client_id_secret, "google_oauth_client_id");
        assert_eq!(parsed.gmail.refresh_token_secret, "gmail_refresh_token");

        let mut enabled = ConnectorsConfig::default();
        enabled.google_drive.enabled = true;
        enabled.google_drive.client_id_secret.clear();
        assert!(enabled.validate().is_err());

        let mut redirect = ConnectorsConfig::default();
        redirect.google_calendar.redirect_uri = "https://example.com/callback".into();
        assert!(redirect.validate().is_err());
        redirect.google_calendar.redirect_uri = "http://127.0.0.1:8765/callback".into();
        assert!(redirect.validate().is_ok());
    }

    /// A config whose non-auth sections pass validation, so `validate()`
    /// exercises — and can pass — the auth/users checks. The raw default
    /// leaves `[api] enabled = true` with an empty `token_hash`, which is
    /// invalid by design (the API demands a configured token).
    fn valid_config() -> Config {
        let mut config = Config::default();
        config.api.enabled = false;
        config
    }

    #[test]
    fn auth_defaults_to_disabled_with_documented_values() {
        let config = AuthConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.providers, vec!["microsoft".to_string()]);
        assert_eq!(config.tenant, "consumers");
        assert_eq!(config.client_id_secret, "auth_client_id");
        assert_eq!(config.session_ttl_hours, 720);
        assert_eq!(config.invite_ttl_hours, 48);
        assert_eq!(config.max_users, 10);
        assert!(config.admin_email.is_empty());
        let mut with_email = valid_config();
        with_email.auth.admin_email = "operator@example.example".into();
        with_email.validate().unwrap();
        for bad in ["not-an-email", "a b@example.example", &"a@".repeat(200)] {
            with_email.auth.admin_email = bad.into();
            assert!(with_email.validate().is_err(), "{bad}");
        }
        assert_eq!(UsersConfig::default().tasks_per_user_per_day, 500);
        valid_config().validate().unwrap();
    }

    #[test]
    fn auth_rejects_unknown_providers_and_bad_values() {
        let mut config = valid_config();
        config.auth.providers = vec!["microsoft".into(), "saml".into()];
        assert!(config.validate().is_err());

        let mut config = valid_config();
        config.auth.providers = vec!["microsoft".into(), "microsoft".into()];
        assert!(config.validate().is_err(), "duplicates are rejected");

        let mut config = valid_config();
        config.auth.tenant = "../escape".into();
        assert!(config.validate().is_err());

        let mut config = valid_config();
        config.auth.client_id_secret = "not a secret name".into();
        assert!(config.validate().is_err());

        let mut config = valid_config();
        config.auth.session_ttl_hours = 0;
        assert!(config.validate().is_err());
        config.auth.session_ttl_hours = 720;
        config.auth.invite_ttl_hours = 24 * 31;
        assert!(config.validate().is_err());
        config.auth.invite_ttl_hours = 48;
        config.auth.max_users = 0;
        assert!(config.validate().is_err());
        config.auth.max_users = 10;
        config.users.tasks_per_user_per_day = 0;
        assert!(config.validate().is_err());
        config.users.tasks_per_user_per_day = 500;
        config.validate().unwrap();
    }

    #[test]
    fn auth_requires_https_base_url_and_microsoft_when_enabled() {
        let mut config = valid_config();
        config.auth.enabled = true;
        assert!(
            config.validate().is_err(),
            "enabled auth requires a base_url"
        );

        config.auth.base_url = "http://bot.example.example".into();
        assert!(config.validate().is_err(), "base_url must be https");

        config.auth.base_url = "https://user:pw@example.example/?x=1".into();
        assert!(
            config.validate().is_err(),
            "no credentials, query, fragment"
        );

        config.auth.base_url = "https://bot.example.example".into();
        config.validate().unwrap();

        // google/github may be listed but cannot enable sign-in on their own.
        config.auth.providers = vec!["google".into(), "github".into()];
        assert!(
            config.validate().is_err(),
            "enabled auth must include microsoft"
        );
        assert!(config.auth.effective_providers().is_empty());

        config.auth.providers = vec!["microsoft".into(), "google".into(), "github".into()];
        config.validate().unwrap();
        assert_eq!(config.auth.effective_providers(), vec!["microsoft"]);

        // While disabled, an invalid base_url still fails fast.
        let mut config = valid_config();
        config.auth.base_url = "ftp://example.example".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn auth_section_parses_from_toml() {
        let raw = r#"
[api]
enabled = false

[auth]
enabled = false
providers = ["microsoft", "github"]
tenant = "consumers"
client_id_secret = "auth_client_id"
base_url = "https://bot.example.example"
session_ttl_hours = 720
invite_ttl_hours = 48
max_users = 10

[users]
tasks_per_user_per_day = 500
"#;
        let config: Config = toml::from_str(raw).unwrap();
        assert_eq!(config.auth.providers.len(), 2);
        assert_eq!(config.auth.base_url, "https://bot.example.example");
        assert_eq!(config.users.tasks_per_user_per_day, 500);
        config.validate().unwrap();
    }

    #[test]
    fn the_repository_example_config_parses() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("workspace root")
            .to_path_buf();
        let raw = std::fs::read_to_string(root.join("serabot.toml")).unwrap();
        let config: Config = toml::from_str(&raw).unwrap();
        assert_eq!(config.secrets.master_key_file, "private/master.key");
        assert_eq!(config.onedrive.folder, "SeraBot");
        assert!(config.onedrive.redirect_base_url.is_empty());
    }
}
