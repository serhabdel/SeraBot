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
    #[serde(default)]
    pub policy: PolicyConfig,
    #[serde(default)]
    pub watcher: WatcherConfig,
    #[serde(default)]
    pub onedrive: OneDriveConfig,
    #[serde(default)]
    pub firecrawl: FirecrawlConfig,
    #[serde(default)]
    pub elevenlabs: ElevenLabsConfig,
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
    pub skills: SkillsConfig,
    #[serde(default)]
    pub browser: BrowserConfig,
    #[serde(default)]
    pub connectors: ConnectorsConfig,
    /// Fleet file locations. Relative to `base_dir`; the owner API never accepts paths.
    #[serde(default)]
    pub fleet: FleetConfig,
    #[serde(skip)]
    pub base_dir: PathBuf,
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
            policy: PolicyConfig::default(),
            watcher: WatcherConfig::default(),
            onedrive: OneDriveConfig::default(),
            firecrawl: FirecrawlConfig::default(),
            elevenlabs: ElevenLabsConfig::default(),
            render: RenderConfig::default(),
            runtime: RuntimeConfig::default(),
            schedule: ScheduleConfig::default(),
            documents: DocumentsConfig::default(),
            audio: AudioConfig::default(),
            skills: SkillsConfig::default(),
            browser: BrowserConfig::default(),
            connectors: ConnectorsConfig::default(),
            fleet: FleetConfig::default(),
            base_dir: PathBuf::from("."),
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
        }
        self.browser.validate()?;
        self.connectors.validate()?;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_body_limit")]
    pub body_limit_bytes: usize,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_minute: u32,
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
}

impl Default for SecretsConfig {
    fn default() -> Self {
        Self {
            encrypted_file: default_secrets_file(),
            identity_file: default_identity_file(),
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
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            sources_file: default_sources_file(),
            user_agent: default_user_agent(),
            timeout_secs: default_fetch_timeout(),
            max_response_bytes: default_fetch_bytes(),
            morning_brief_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OneDriveConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_graph_base")]
    pub graph_base: String,
    #[serde(default = "default_tenant")]
    pub tenant: String,
    #[serde(default = "default_graph_client_secret")]
    pub client_id_secret: String,
    #[serde(default = "default_graph_refresh_secret")]
    pub refresh_token_secret: String,
    #[serde(default = "default_onedrive_folder")]
    pub folder: String,
    #[serde(default = "default_conflict_behavior")]
    pub conflict_behavior: String,
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
        }
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElevenLabsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_elevenlabs_api_base")]
    pub api_base: String,
    #[serde(default = "default_elevenlabs_api_key_secret")]
    pub api_key_secret: String,
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
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            brand_file: default_brand_file(),
            artifacts_dir: default_artifacts_dir(),
            max_pages: default_max_deck_pages(),
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
            max_bytes: default_audio_max_bytes(),
            telegram_intake: false,
            telegram_max_bytes: default_telegram_audio_max_bytes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotConfig {
    pub name: String,
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
    #[serde(default)]
    pub role: BotRole,
    #[serde(default)]
    pub delegates: Vec<String>,
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

/// Specialist tools the owner API may write into a scaffolded bot file.
/// `delegate` is orchestrator-only and is intentionally absent.
const SPECIALIST_TOOL_ALLOWLIST: &[&str] = &[
    "skill_list",
    "skill_search",
    "skill_load",
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
    "elevenlabs_list_voices",
    "elevenlabs_text_to_speech",
    "elevenlabs_sound_effect",
    "elevenlabs_transcribe",
    "elevenlabs_generate_image",
    "elevenlabs_generate_video",
    "deck_build",
    "telegram_send_document",
    "onedrive_upload",
    "onedrive_list",
    "browser_session_start",
    "browser_session_stop",
    "browser_navigate",
    "browser_snapshot",
    "google_drive_list",
    "google_sheets_read",
    "google_calendar_list_events",
    "gmail_list_messages",
    "outlook_list_messages",
    "outlook_calendar_list_events",
    "notion_query_database",
    "agentmail_inboxes_list",
    "agentmail_send",
];

const DEFAULT_SPECIALIST_TOOLS: &[&str] = &["skill_list", "skill_search", "skill_load"];

/// Tools with an external effect. A scaffold lists these in `require_approval`.
const EXTERNAL_EFFECT_TOOLS: &[&str] = &[
    "audio_transcribe",
    "audio_understand",
    "firecrawl_scrape",
    "firecrawl_map",
    "firecrawl_search",
    "firecrawl_crawl",
    "elevenlabs_text_to_speech",
    "elevenlabs_sound_effect",
    "elevenlabs_transcribe",
    "elevenlabs_generate_image",
    "elevenlabs_generate_video",
    "telegram_send_document",
    "onedrive_upload",
    "browser_session_start",
    "browser_session_stop",
    "browser_navigate",
    "google_drive_list",
    "google_sheets_read",
    "google_calendar_list_events",
    "gmail_list_messages",
    "outlook_list_messages",
    "outlook_calendar_list_events",
    "notion_query_database",
    "agentmail_inboxes_list",
    "agentmail_send",
];

const MAX_BOT_DESCRIPTION_CHARS: usize = 2000;
const MAX_SCAFFOLD_TOOLS: usize = 32;
const MAX_SCAFFOLD_SKILLS: usize = 16;

#[derive(Debug, Clone)]
pub struct ScaffoldedBot {
    pub id: String,
    pub bot_file: String,
    pub prompt_file: String,
    pub bot_path: PathBuf,
    pub prompt_path: PathBuf,
    pub tools: Vec<String>,
    pub skills: Vec<String>,
    pub require_approval: Vec<String>,
}

/// Write `bots/<id>.toml` and `prompts/<id>.txt` from the fixed specialist template.
/// Paths come from `[fleet]`, never from the caller. The bot stays disabled.
pub fn scaffold_specialist_bot(
    config: &Config,
    id: &str,
    description: &str,
    tools: Option<&[String]>,
    skills: Option<&[String]>,
    policy: &crate::policy::Policy,
) -> Result<ScaffoldedBot> {
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
    let skills = normalize_skill_list(config, skills.unwrap_or(&[]))?;
    let prompt = specialist_prompt(description);
    if !policy.is_allowed(description) || !policy.is_allowed(&prompt) {
        return Err(SeraError::PolicyViolation);
    }
    let require_approval: Vec<String> = tools
        .iter()
        .filter(|tool| EXTERNAL_EFFECT_TOOLS.contains(&tool.as_str()))
        .cloned()
        .collect();

    let bots_dir = fleet_dir(config, &config.fleet.bots_dir)?;
    let prompts_dir = fleet_dir(config, &config.fleet.prompts_dir)?;
    let bot_path = bots_dir.join(format!("{id}.toml"));
    let prompt_path = prompts_dir.join(format!("{id}.txt"));
    if bot_path.exists() || prompt_path.exists() {
        return Err(SeraError::InvalidState("bot already exists".into()));
    }
    let bot_file = relative_fleet_file(&config.fleet.bots_dir, &format!("{id}.toml"));
    let prompt_file = relative_fleet_file(&config.fleet.prompts_dir, &format!("{id}.txt"));
    let toml_body = render_bot_toml(id, &prompt_file, &skills, &tools, &require_approval);
    write_new_file(&prompt_path, &prompt)?;
    if let Err(error) = write_new_file(&bot_path, &toml_body) {
        let _ = std::fs::remove_file(&prompt_path);
        return Err(error);
    }
    Ok(ScaffoldedBot {
        id: id.to_owned(),
        bot_file,
        prompt_file,
        bot_path,
        prompt_path,
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

fn normalize_skill_list(config: &Config, skills: &[String]) -> Result<Vec<String>> {
    if skills.len() > MAX_SCAFFOLD_SKILLS {
        return Err(SeraError::Validation("too many skills".into()));
    }
    if skills.is_empty() {
        return Ok(Vec::new());
    }
    let catalog = crate::skills::SkillCatalog::load(config)?;
    let known = catalog.skill_ids();
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

fn render_bot_toml(
    id: &str,
    prompt_file: &str,
    skills: &[String],
    tools: &[String],
    require_approval: &[String],
) -> String {
    format!(
        "name = {id}\nrole = \"specialist\"\ndelegates = []\ntrigger = \"command\"\nsystem_prompt = {prompt}\nskills = {skills}\ntools = {tools}\nrequire_approval = {approvals}\nenabled = false\n",
        id = toml_basic_string(id),
        prompt = toml_basic_string(prompt_file),
        skills = toml_string_array(skills),
        tools = toml_string_array(tools),
        approvals = toml_string_array(require_approval),
    )
}

fn toml_basic_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn toml_string_array(values: &[String]) -> String {
    if values.is_empty() {
        return "[]".into();
    }
    let items = values
        .iter()
        .map(|value| toml_basic_string(value))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{items}]")
}

fn fleet_dir(config: &Config, relative: &str) -> Result<PathBuf> {
    validate_relative_dir("fleet directory", relative)?;
    let dir = config.base_dir.join(relative);
    std::fs::create_dir_all(&dir)?;
    let base = config.base_dir.canonicalize().map_err(|error| {
        SeraError::Config(format!(
            "configuration directory cannot be resolved: {error}"
        ))
    })?;
    let dir = dir.canonicalize().map_err(|error| {
        SeraError::Config(format!("fleet directory cannot be resolved: {error}"))
    })?;
    if !dir.starts_with(&base) {
        return Err(SeraError::Config(
            "fleet directory escapes the configuration directory".into(),
        ));
    }
    Ok(dir)
}

fn relative_fleet_file(dir: &str, file_name: &str) -> String {
    let mut parts = Vec::new();
    for component in Path::new(dir).components() {
        if let std::path::Component::Normal(part) = component {
            parts.push(part.to_string_lossy().into_owned());
        }
    }
    parts.push(file_name.to_owned());
    parts.join("/")
}

fn write_new_file(path: &Path, contents: &str) -> Result<()> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(SeraError::InvalidState("bot already exists".into()))
        }
        Err(error) => Err(error.into()),
    }
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
fn default_audio_max_bytes() -> u64 {
    52_428_800
}
fn default_telegram_audio_max_bytes() -> u64 {
    20_971_520
}
fn default_elevenlabs_api_base() -> String {
    "https://api.elevenlabs.io".into()
}
fn default_elevenlabs_api_key_secret() -> String {
    "elevenlabs_api_key".into()
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
    fn scaffolds_disabled_specialist_under_configured_dirs() {
        let root = std::env::temp_dir().join(format!("serabot-scaffold-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let skill_dir = root.join("skills").join("ask");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: \"Ask\"\ndescription: \"Synthetic ask skill\"\n---\n\n# Ask\n",
        )
        .unwrap();
        let config = Config {
            base_dir: root.clone(),
            skills: SkillsConfig {
                dir: "skills".into(),
            },
            ..Config::default()
        };
        let policy = crate::policy::Policy::empty();
        let scaffolded = scaffold_specialist_bot(
            &config,
            "notes",
            "Synthetic notes specialist.",
            Some(&["rag_search".into(), "firecrawl_search".into()]),
            Some(&["ask".into()]),
            &policy,
        )
        .unwrap();
        assert_eq!(scaffolded.bot_file, "bots/notes.toml");
        assert_eq!(scaffolded.prompt_file, "prompts/notes.txt");
        assert!(scaffolded.bot_path.starts_with(&root));
        assert!(scaffolded.prompt_path.starts_with(&root));
        let raw = std::fs::read_to_string(&scaffolded.bot_path).unwrap();
        let bot: BotConfig = toml::from_str(&raw).unwrap();
        assert_eq!(bot.name, "notes");
        assert!(!bot.enabled);
        assert_eq!(bot.role, BotRole::Specialist);
        assert!(bot.delegates.is_empty());
        assert_eq!(bot.system_prompt, "prompts/notes.txt");
        assert_eq!(bot.tools, vec!["rag_search", "firecrawl_search"]);
        assert_eq!(bot.skills, vec!["ask"]);
        assert_eq!(bot.require_approval, vec!["firecrawl_search"]);
        assert!(!bot.tools.iter().any(|tool| tool == "delegate"));
        let prompt = std::fs::read_to_string(&scaffolded.prompt_path).unwrap();
        assert!(prompt.contains("do not delegate"));
        assert!(prompt.contains("Synthetic notes specialist."));
        assert!(scaffold_specialist_bot(&config, "notes", "", None, None, &policy).is_err());
        assert!(scaffold_specialist_bot(&config, "../outside", "", None, None, &policy).is_err());
        assert!(scaffold_specialist_bot(&config, "main", "", None, None, &policy).is_err());
        assert!(
            scaffold_specialist_bot(
                &config,
                "other",
                "",
                Some(&["delegate".into()]),
                None,
                &policy
            )
            .is_err()
        );
        assert!(
            scaffold_specialist_bot(
                &config,
                "other",
                "",
                Some(&["not_a_tool".into()]),
                None,
                &policy
            )
            .is_err()
        );
        assert!(
            scaffold_specialist_bot(
                &config,
                "other",
                "",
                None,
                Some(&["missing-skill".into()]),
                &policy
            )
            .is_err()
        );
        let blocked = crate::policy::Policy::from_text("synthetic blocked phrase");
        assert!(
            scaffold_specialist_bot(
                &config,
                "other",
                "notes about synthetic blocked phrase",
                None,
                None,
                &blocked
            )
            .is_err()
        );
        assert!(!root.join("bots").join("other.toml").exists());
        let _ = std::fs::remove_dir_all(&root);
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
}
