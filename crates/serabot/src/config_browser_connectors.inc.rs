#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_browser_provider")]
    pub provider: String,
    #[serde(default)]
    pub chromium_executable: String,
    #[serde(default = "default_browser_debug_host")]
    pub remote_debugging_host: String,
    #[serde(default = "default_browser_debug_port")]
    pub remote_debugging_port: u16,
    #[serde(default = "default_browser_neo_base")]
    pub browseros_neo_base: String,
    #[serde(default)]
    pub user_data_dir: String,
    #[serde(default = "default_browser_timeout")]
    pub request_timeout_secs: u64,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_browser_provider(),
            chromium_executable: String::new(),
            remote_debugging_host: default_browser_debug_host(),
            remote_debugging_port: default_browser_debug_port(),
            browseros_neo_base: default_browser_neo_base(),
            user_data_dir: String::new(),
            request_timeout_secs: default_browser_timeout(),
        }
    }
}

impl BrowserConfig {
    pub fn validate(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        crate::browser::BrowserProvider::parse(&self.provider)?;
        if self.remote_debugging_port < 1024 {
            return Err(SeraError::Config(
                "browser.remote_debugging_port must be >= 1024".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectorsConfig {
    #[serde(default)]
    pub google_drive: GoogleDriveConnectorConfig,
    #[serde(default)]
    pub google_sheets: GoogleSheetsConnectorConfig,
    #[serde(default)]
    pub google_calendar: GoogleCalendarConnectorConfig,
    #[serde(default)]
    pub gmail: GmailConnectorConfig,
    #[serde(default)]
    pub outlook: OutlookConnectorConfig,
    #[serde(default)]
    pub outlook_calendar: OutlookCalendarConnectorConfig,
    #[serde(default)]
    pub notion: NotionConnectorConfig,
    #[serde(default)]
    pub agentmail: AgentMailConnectorConfig,
}

impl ConnectorsConfig {
    pub fn validate(&self) -> Result<()> {
        self.google_drive.validate_enabled()?;
        self.google_sheets.validate_enabled()?;
        self.google_calendar.validate_enabled()?;
        self.gmail.validate_enabled()?;
        self.outlook.validate_enabled()?;
        self.outlook_calendar.validate_enabled()?;
        self.notion.validate_enabled()?;
        self.agentmail.validate_enabled()?;
        Ok(())
    }

    pub fn any_enabled(&self) -> bool {
        self.google_drive.oauth.enabled
            || self.google_sheets.oauth.enabled
            || self.google_calendar.oauth.enabled
            || self.gmail.oauth.enabled
            || self.outlook.oauth.enabled
            || self.outlook_calendar.oauth.enabled
            || self.notion.oauth.enabled
            || self.agentmail.enabled
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthConnectorSecrets {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_oauth_client_id_secret")]
    pub client_id_secret: String,
    #[serde(default = "default_oauth_client_secret_secret")]
    pub client_secret_secret: String,
    #[serde(default = "default_oauth_refresh_secret")]
    pub refresh_token_secret: String,
}

impl Default for OAuthConnectorSecrets {
    fn default() -> Self {
        Self {
            enabled: false,
            client_id_secret: default_oauth_client_id_secret(),
            client_secret_secret: default_oauth_client_secret_secret(),
            refresh_token_secret: default_oauth_refresh_secret(),
        }
    }
}

impl OAuthConnectorSecrets {
    fn validate_enabled(&self, label: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        for (field, value) in [
            ("client_id_secret", self.client_id_secret.as_str()),
            ("client_secret_secret", self.client_secret_secret.as_str()),
            ("refresh_token_secret", self.refresh_token_secret.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(SeraError::Config(format!(
                    "connectors.{label}.{field} must not be empty when enabled"
                )));
            }
        }
        Ok(())
    }
}

macro_rules! oauth_connector {
    ($name:ident, $label:literal) => {
        #[derive(Debug, Clone, Serialize, Deserialize, Default)]
        pub struct $name {
            #[serde(flatten)]
            pub oauth: OAuthConnectorSecrets,
        }
        impl $name {
            fn validate_enabled(&self) -> Result<()> {
                self.oauth.validate_enabled($label)
            }
        }
    };
}

oauth_connector!(GoogleDriveConnectorConfig, "google_drive");
oauth_connector!(GoogleSheetsConnectorConfig, "google_sheets");
oauth_connector!(GoogleCalendarConnectorConfig, "google_calendar");
oauth_connector!(GmailConnectorConfig, "gmail");
oauth_connector!(OutlookConnectorConfig, "outlook");
oauth_connector!(OutlookCalendarConnectorConfig, "outlook_calendar");
oauth_connector!(NotionConnectorConfig, "notion");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMailConnectorConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_agentmail_api_base")]
    pub api_base: String,
    #[serde(default = "default_agentmail_api_key_secret")]
    pub api_key_secret: String,
    #[serde(default = "default_agentmail_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub default_inbox_id: String,
}

impl Default for AgentMailConnectorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_base: default_agentmail_api_base(),
            api_key_secret: default_agentmail_api_key_secret(),
            timeout_secs: default_agentmail_timeout(),
            default_inbox_id: String::new(),
        }
    }
}

impl AgentMailConnectorConfig {
    fn validate_enabled(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if self.api_key_secret.trim().is_empty() {
            return Err(SeraError::Config(
                "connectors.agentmail.api_key_secret must not be empty when enabled".into(),
            ));
        }
        if !self.api_base.starts_with("https://") {
            return Err(SeraError::Config(
                "connectors.agentmail.api_base must be HTTPS".into(),
            ));
        }
        Ok(())
    }
}

fn default_browser_provider() -> String {
    "remote_cdp".into()
}
fn default_browser_debug_host() -> String {
    "127.0.0.1".into()
}
fn default_browser_debug_port() -> u16 {
    9222
}
fn default_browser_neo_base() -> String {
    "http://127.0.0.1:9222".into()
}
fn default_browser_timeout() -> u64 {
    30
}
fn default_oauth_client_id_secret() -> String {
    "oauth_client_id".into()
}
fn default_oauth_client_secret_secret() -> String {
    "oauth_client_secret".into()
}
fn default_oauth_refresh_secret() -> String {
    "oauth_refresh_token".into()
}
fn default_agentmail_api_base() -> String {
    "https://api.agentmail.to".into()
}
fn default_agentmail_api_key_secret() -> String {
    "agentmail_api_key".into()
}
fn default_agentmail_timeout() -> u64 {
    30
}
