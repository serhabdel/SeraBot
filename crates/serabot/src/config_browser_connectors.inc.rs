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
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<()> {
        match id {
            "google_drive" => self.google_drive.enabled = enabled,
            "google_sheets" => self.google_sheets.enabled = enabled,
            "google_calendar" => self.google_calendar.enabled = enabled,
            "gmail" => self.gmail.enabled = enabled,
            "outlook" => self.outlook.enabled = enabled,
            "outlook_calendar" => self.outlook_calendar.enabled = enabled,
            "notion" => self.notion.enabled = enabled,
            "agentmail" => self.agentmail.enabled = enabled,
            _ => {
                return Err(SeraError::Config(format!("unknown connector '{id}'")));
            }
        }
        Ok(())
    }

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
        self.google_drive.enabled
            || self.google_sheets.enabled
            || self.google_calendar.enabled
            || self.gmail.enabled
            || self.outlook.enabled
            || self.outlook_calendar.enabled
            || self.notion.enabled
            || self.agentmail.enabled
    }
}

/// Secret-store key names for one OAuth connector. Values stay in the age store.
pub trait OAuthConnectorSettings {
    fn oauth_enabled(&self) -> bool;
    fn client_id_secret_name(&self) -> &str;
    fn client_secret_secret_name(&self) -> &str;
    fn refresh_token_secret_name(&self) -> &str;
    fn redirect_uri(&self) -> &str;
}

fn validate_oauth_connector(
    label: &str,
    enabled: bool,
    client_id_secret: &str,
    client_secret_secret: &str,
    refresh_token_secret: &str,
    redirect_uri: &str,
) -> Result<()> {
    if !redirect_uri.trim().is_empty() {
        validate_oauth_redirect_uri(redirect_uri)?;
    }
    if !enabled {
        return Ok(());
    }
    for (field, value) in [
        ("client_id_secret", client_id_secret),
        ("client_secret_secret", client_secret_secret),
        ("refresh_token_secret", refresh_token_secret),
    ] {
        if !valid_secret_name(value) {
            return Err(SeraError::Config(format!(
                "connectors.{label}.{field} must be a secret-store key name (letters, digits, '_' or '-')"
            )));
        }
    }
    Ok(())
}

/// Loopback redirect only. Empty is allowed at config time and rejected when building an authorize URL.
pub(crate) fn validate_oauth_redirect_uri(redirect: &str) -> Result<()> {
    let redirect = redirect.trim();
    let url = url::Url::parse(redirect).map_err(|_| {
        SeraError::Config(
            "connectors redirect_uri must be http://127.0.0.1:<port>/<path> or http://localhost:<port>/<path>"
                .into(),
        )
    })?;
    let host = url.host_str().unwrap_or("");
    let loopback = host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost");
    let path_ok = url.path().starts_with('/')
        && url.path() != "/"
        && !url.path().contains("//")
        && !url.path().contains("..");
    if url.scheme() != "http"
        || !loopback
        || url.port().is_none_or(|port| port == 0)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !path_ok
    {
        return Err(SeraError::Config(
            "connectors redirect_uri must be http://127.0.0.1:<port>/<path> or http://localhost:<port>/<path> with no query, fragment, or credentials"
                .into(),
        ));
    }
    Ok(())
}

pub(crate) fn valid_secret_name(name: &str) -> bool {
    let name = name.trim();
    (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

macro_rules! oauth_connector {
    (
        $name:ident,
        $label:literal,
        $id_fn:ident = $id_path:literal = $id:literal,
        $secret_fn:ident = $secret_path:literal = $secret:literal,
        $refresh_fn:ident = $refresh_path:literal = $refresh:literal
    ) => {
        fn $id_fn() -> String {
            $id.into()
        }
        fn $secret_fn() -> String {
            $secret.into()
        }
        fn $refresh_fn() -> String {
            $refresh.into()
        }

        #[derive(Debug, Clone, Serialize, Deserialize)]
        pub struct $name {
            #[serde(default)]
            pub enabled: bool,
            #[serde(default = $id_path)]
            pub client_id_secret: String,
            #[serde(default = $secret_path)]
            pub client_secret_secret: String,
            #[serde(default = $refresh_path)]
            pub refresh_token_secret: String,
            /// Loopback OAuth redirect. Empty until the owner registers one.
            #[serde(default)]
            pub redirect_uri: String,
        }

        impl Default for $name {
            fn default() -> Self {
                Self {
                    enabled: false,
                    client_id_secret: $id_fn(),
                    client_secret_secret: $secret_fn(),
                    refresh_token_secret: $refresh_fn(),
                    redirect_uri: String::new(),
                }
            }
        }

        impl $name {
            fn validate_enabled(&self) -> Result<()> {
                validate_oauth_connector(
                    $label,
                    self.enabled,
                    &self.client_id_secret,
                    &self.client_secret_secret,
                    &self.refresh_token_secret,
                    &self.redirect_uri,
                )
            }
        }

        impl OAuthConnectorSettings for $name {
            fn oauth_enabled(&self) -> bool {
                self.enabled
            }
            fn client_id_secret_name(&self) -> &str {
                &self.client_id_secret
            }
            fn client_secret_secret_name(&self) -> &str {
                &self.client_secret_secret
            }
            fn refresh_token_secret_name(&self) -> &str {
                &self.refresh_token_secret
            }
            fn redirect_uri(&self) -> &str {
                &self.redirect_uri
            }
        }
    };
}

oauth_connector!(
    GoogleDriveConnectorConfig,
    "google_drive",
    google_drive_client_id_secret = "google_drive_client_id_secret" = "google_oauth_client_id",
    google_drive_client_secret_secret =
        "google_drive_client_secret_secret" = "google_oauth_client_secret",
    google_drive_refresh_secret = "google_drive_refresh_secret" = "google_drive_refresh_token"
);
oauth_connector!(
    GoogleSheetsConnectorConfig,
    "google_sheets",
    google_sheets_client_id_secret = "google_sheets_client_id_secret" = "google_oauth_client_id",
    google_sheets_client_secret_secret =
        "google_sheets_client_secret_secret" = "google_oauth_client_secret",
    google_sheets_refresh_secret = "google_sheets_refresh_secret" = "google_sheets_refresh_token"
);
oauth_connector!(
    GoogleCalendarConnectorConfig,
    "google_calendar",
    google_calendar_client_id_secret =
        "google_calendar_client_id_secret" = "google_oauth_client_id",
    google_calendar_client_secret_secret =
        "google_calendar_client_secret_secret" = "google_oauth_client_secret",
    google_calendar_refresh_secret =
        "google_calendar_refresh_secret" = "google_calendar_refresh_token"
);
oauth_connector!(
    GmailConnectorConfig,
    "gmail",
    gmail_client_id_secret = "gmail_client_id_secret" = "google_oauth_client_id",
    gmail_client_secret_secret = "gmail_client_secret_secret" = "google_oauth_client_secret",
    gmail_refresh_secret = "gmail_refresh_secret" = "gmail_refresh_token"
);
oauth_connector!(
    OutlookConnectorConfig,
    "outlook",
    outlook_client_id_secret = "outlook_client_id_secret" = "outlook_client_id",
    outlook_client_secret_secret = "outlook_client_secret_secret" = "outlook_client_secret",
    outlook_refresh_secret = "outlook_refresh_secret" = "outlook_refresh_token"
);
oauth_connector!(
    OutlookCalendarConnectorConfig,
    "outlook_calendar",
    outlook_calendar_client_id_secret = "outlook_calendar_client_id_secret" = "outlook_client_id",
    outlook_calendar_client_secret_secret =
        "outlook_calendar_client_secret_secret" = "outlook_client_secret",
    outlook_calendar_refresh_secret =
        "outlook_calendar_refresh_secret" = "outlook_calendar_refresh_token"
);
oauth_connector!(
    NotionConnectorConfig,
    "notion",
    notion_client_id_secret = "notion_client_id_secret" = "notion_oauth_client_id",
    notion_client_secret_secret = "notion_client_secret_secret" = "notion_oauth_client_secret",
    notion_refresh_secret = "notion_refresh_secret" = "notion_refresh_token"
);

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
        if !valid_secret_name(&self.api_key_secret) {
            return Err(SeraError::Config(
                "connectors.agentmail.api_key_secret must be a secret-store key name (letters, digits, '_' or '-')"
                    .into(),
            ));
        }
        let base = self.api_base.trim();
        let url = url::Url::parse(base).map_err(|_| {
            SeraError::Config("connectors.agentmail.api_base must be an https URL".into())
        })?;
        if url.scheme() != "https"
            || url.host_str().is_none_or(str::is_empty)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(SeraError::Config(
                "connectors.agentmail.api_base must be https without credentials, a query, or a fragment"
                    .into(),
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
fn default_agentmail_api_base() -> String {
    "https://api.agentmail.to/v0".into()
}
fn default_agentmail_api_key_secret() -> String {
    "agentmail_api_key".into()
}
fn default_agentmail_timeout() -> u64 {
    30
}
