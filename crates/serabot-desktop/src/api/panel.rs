//! Vault and live-browser client (contract: `docs/specs/vault-browser-contract.md`).
//!
//! Vault values are write-only: they travel in a `PUT` body and no response
//! is ever allowed to carry one (the entry type rejects unknown fields, so a
//! server that echoed a `value` would fail validation instead of leaking it
//! into the UI). Browser screenshots are size-capped and must be JPEG.

use super::*;

/// The server caps a screenshot at 512 KiB; anything larger is refused.
pub const MAX_SCREENSHOT_BYTES: usize = 512 * 1024;
pub const MAX_VAULT_ENTRIES: usize = 64;
pub const MAX_VAULT_VALUE_CHARS: usize = 8192;
pub const MAX_VAULT_DOMAINS: usize = 16;
pub const MAX_VAULT_NOTE_CHARS: usize = 500;
pub const MAX_VAULT_USERNAME_CHARS: usize = 256;
const MAX_VAULT_NAME_CHARS: usize = 48;
const MAX_URL_CHARS: usize = 2048;
const MAX_TITLE_CHARS: usize = 512;
const MAX_INPUT_TEXT_CHARS: usize = 2000;
const MAX_COORD: f64 = 4000.0;
const MAX_SCROLL: f64 = 5000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultKind {
    Password,
    ApiKey,
    Note,
}

impl VaultKind {
    pub const ALL: [Self; 3] = [Self::Password, Self::ApiKey, Self::Note];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::ApiKey => "api_key",
            Self::Note => "note",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Password => "Password",
            Self::ApiKey => "API key",
            Self::Note => "Note",
        }
    }
}

/// Where an entry's value comes from (server `source`). A reference names an
/// item in the owner's password manager; the value is read when it is typed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VaultSource {
    #[default]
    #[serde(rename = "local")]
    Local,
    #[serde(rename = "onepassword")]
    OnePassword,
    #[serde(rename = "protonpass")]
    ProtonPass,
}

impl VaultSource {
    pub const ALL: [Self; 3] = [Self::Local, Self::OnePassword, Self::ProtonPass];

    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Value",
            Self::OnePassword => "1Password",
            Self::ProtonPass => "Proton Pass",
        }
    }

    pub fn is_reference(self) -> bool {
        self != Self::Local
    }
}

/// One vault entry: metadata only, never the value.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VaultEntry {
    pub name: String,
    pub kind: VaultKind,
    #[serde(default)]
    pub source: VaultSource,
    /// The password manager item pointer for a reference. A pointer, not a value.
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub last_used_at: Option<String>,
}

/// A vault write. The value is wiped when this is dropped. A reference
/// carries an empty value.
pub struct VaultPut {
    pub kind: VaultKind,
    pub username: Option<String>,
    pub value: Zeroizing<String>,
    pub domains: Vec<String>,
    pub note: Option<String>,
    pub source: VaultSource,
    pub reference: Option<String>,
}

#[derive(Serialize)]
struct VaultPutBody<'a> {
    kind: VaultKind,
    source: VaultSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    username: Option<&'a str>,
    value: &'a str,
    domains: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
}

/// Longest password manager item pointer the server accepts.
pub const MAX_VAULT_REFERENCE_CHARS: usize = 512;

/// The shape the server accepts for a reference: `op://vault/item/field`
/// (or with a section) for 1Password, `pass://vault/item/field` for Proton
/// Pass. 1Password forbids spaces; Proton Pass titles may contain them. No
/// control characters either way.
pub fn valid_vault_reference(source: VaultSource, reference: &str) -> bool {
    let (prefix, counts, spaces_ok) = match source {
        VaultSource::Local => return false,
        VaultSource::OnePassword => ("op://", 3..=4, false),
        VaultSource::ProtonPass => ("pass://", 3..=3, true),
    };
    if reference.chars().count() > MAX_VAULT_REFERENCE_CHARS
        || reference
            .chars()
            .any(|c| c.is_control() || (!spaces_ok && c.is_whitespace()))
    {
        return false;
    }
    let Some(rest) = reference.strip_prefix(prefix) else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    counts.contains(&parts.len()) && parts.iter().all(|part| !part.is_empty())
}

/// One password manager as the server reports it. Never a token.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PasswordManager {
    pub id: String,
    pub name: String,
    /// The owner has saved an access token for it.
    pub configured: bool,
    /// The CLI is installed on the server.
    pub cli_found: bool,
    /// The server reads from this manager (Proton Pass is opt-in).
    pub enabled: bool,
    pub install_url: String,
}

/// Result of `POST /v1/vault/providers/{id}/test`. Never a token or CLI output.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PasswordManagerProbe {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub cli_found: bool,
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordManagerList {
    providers: Vec<PasswordManager>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VaultPendingSecret {
    pub card_id: String,
    pub conversation_id: String,
    pub name: String,
    pub kind: VaultKind,
    #[serde(default)]
    pub domains: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VaultAccounts {
    pub entries: Vec<VaultEntry>,
    #[serde(default)]
    pub pending_secrets: Vec<VaultPendingSecret>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserController {
    Bot,
    User,
}

/// `GET /v1/browser`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrowserInfo {
    pub active: bool,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    pub controller: BrowserController,
    pub updated_at: String,
}

/// `POST /v1/browser/input`. Coordinates are screenshot pixels.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BrowserInput {
    Click { x: f64, y: f64 },
    Text { text: String },
    Key { key: String },
    Scroll { dy: f64 },
    Navigate { url: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeletedAck {
    deleted: bool,
}

#[derive(Deserialize)]
struct OneDriveStartAck {
    url: String,
}

/// Reply of `PUT /v1/connectors/{id}/token` and `DELETE /v1/connectors/{id}`.
#[derive(Deserialize)]
struct ConnectorAck {
    #[serde(default)]
    connected: bool,
}

#[derive(Serialize)]
struct ConnectorTokenBody<'a> {
    token: &'a str,
}

/// Console ids of the per-user products (`outlook`, `google_drive`, ...).
fn valid_personal_id(id: &str) -> bool {
    (1..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

#[derive(Deserialize)]
struct OneDriveDisconnectAck {
    #[serde(default)]
    connected: bool,
}

/// The consent address must be a plain `https` URL of sane length.
pub(crate) fn check_consent_url(raw: &str) -> Result<(), ApiError> {
    let parsed = url::Url::parse(raw).map_err(|_| ApiError::InvalidResponse)?;
    if raw.len() > 4096 || parsed.scheme() != "https" || parsed.host_str().is_none() {
        return Err(ApiError::InvalidResponse);
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClosedAck {
    closed: bool,
}

/// `^[a-z0-9][a-z0-9-]{0,47}$`.
pub fn valid_vault_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_VAULT_NAME_CHARS
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// A domain chip: a bare lower-case host name or IPv4 address. A scheme,
/// path, port, wildcard or user-info is refused; `example.com` already
/// covers its sub-domains on the server.
pub fn normalize_vault_domain(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('.');
    if trimmed.is_empty()
        || trimmed.len() > 253
        || trimmed.chars().any(|c| {
            c.is_whitespace()
                || c.is_control()
                || matches!(c, '/' | '\\' | '@' | '?' | '#' | '*' | ':' | '[' | ']')
        })
    {
        return None;
    }
    match url::Host::parse(trimmed).ok()? {
        url::Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.').to_owned();
            (!domain.is_empty()).then_some(domain)
        }
        url::Host::Ipv4(ip) => Some(ip.to_string()),
        url::Host::Ipv6(_) => None,
    }
}

fn clean_text(value: &str, max: usize) -> bool {
    value.chars().count() <= max && !value.chars().any(char::is_control)
}

fn check_vault_entry(entry: &VaultEntry) -> Result<(), ApiError> {
    let ok = valid_vault_name(&entry.name)
        && entry
            .username
            .as_deref()
            .is_none_or(|v| clean_text(v, MAX_VAULT_USERNAME_CHARS))
        && entry.domains.len() <= MAX_VAULT_DOMAINS
        && entry
            .domains
            .iter()
            .all(|d| !d.is_empty() && clean_text(d, 253))
        && entry
            .note
            .as_deref()
            .is_none_or(|v| clean_text(v, MAX_VAULT_NOTE_CHARS))
        && bounded_text(&entry.created_at, 64)
        && bounded_text(&entry.updated_at, 64)
        && entry
            .last_used_at
            .as_deref()
            .is_none_or(|v| bounded_text(v, 64))
        && entry
            .reference
            .as_deref()
            .is_none_or(|v| bounded_text(v, MAX_VAULT_REFERENCE_CHARS));
    if ok {
        Ok(())
    } else {
        Err(ApiError::InvalidResponse)
    }
}

fn check_browser_info(info: BrowserInfo) -> Result<BrowserInfo, ApiError> {
    let ok = bounded_text(&info.updated_at, 64)
        && info
            .url
            .as_deref()
            .is_none_or(|v| bounded_text(v, MAX_URL_CHARS))
        && info
            .title
            .as_deref()
            .is_none_or(|v| clean_text(v, MAX_TITLE_CHARS));
    if ok {
        Ok(info)
    } else {
        Err(ApiError::InvalidResponse)
    }
}

/// Validate a vault write before it leaves the machine.
pub fn validate_vault_put(name: &str, input: &VaultPut) -> Result<(), ApiError> {
    if !valid_vault_name(name) {
        return Err(ApiError::Validation(
            "Name: lower-case letters, digits and dashes, 1-48 characters, starting with a letter or digit.",
        ));
    }
    if input.source.is_reference() {
        if input.kind == VaultKind::Note {
            return Err(ApiError::Validation(
                "A note cannot come from a password manager.",
            ));
        }
        if !input.value.is_empty() {
            return Err(ApiError::Validation("A reference holds no value."));
        }
        if !input
            .reference
            .as_deref()
            .is_some_and(|reference| valid_vault_reference(input.source, reference))
        {
            return Err(ApiError::Validation(
                "Reference: op://vault/item/field for 1Password, pass://vault/item/field for Proton Pass.",
            ));
        }
    } else {
        let value_chars = input.value.chars().count();
        if value_chars == 0 || value_chars > MAX_VAULT_VALUE_CHARS {
            return Err(ApiError::Validation("Value must be 1-8192 characters."));
        }
        if input
            .value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return Err(ApiError::Validation(
                "Value must not contain control characters.",
            ));
        }
    }
    if input.kind == VaultKind::Password
        && input
            .username
            .as_deref()
            .is_some_and(|v| !clean_text(v, MAX_VAULT_USERNAME_CHARS))
    {
        return Err(ApiError::Validation(
            "Username must be at most 256 characters without control characters.",
        ));
    }
    if input.domains.len() > MAX_VAULT_DOMAINS {
        return Err(ApiError::Validation("At most 16 domains per entry."));
    }
    if input
        .domains
        .iter()
        .any(|d| normalize_vault_domain(d).as_deref() != Some(d.as_str()))
    {
        return Err(ApiError::Validation("A domain is not a valid host name."));
    }
    if input
        .note
        .as_deref()
        .is_some_and(|v| !clean_text(v, MAX_VAULT_NOTE_CHARS))
    {
        return Err(ApiError::Validation(
            "Note must be at most 500 characters without control characters.",
        ));
    }
    Ok(())
}

pub fn validate_browser_input(input: &BrowserInput) -> Result<(), ApiError> {
    let ok = match input {
        BrowserInput::Click { x, y } => [x, y]
            .iter()
            .all(|v| v.is_finite() && (0.0..=MAX_COORD).contains(*v)),
        BrowserInput::Text { text } => {
            !text.is_empty() && text.chars().count() <= MAX_INPUT_TEXT_CHARS
        }
        BrowserInput::Key { key } => {
            !key.is_empty()
                && key.chars().count() <= 32
                && !key.chars().any(|c| c.is_control() || c.is_whitespace())
        }
        BrowserInput::Scroll { dy } => dy.is_finite() && dy.abs() <= MAX_SCROLL,
        BrowserInput::Navigate { url } => {
            (url.starts_with("https://") || url.starts_with("http://"))
                && url.chars().count() <= MAX_URL_CHARS
                && !url.chars().any(|c| c.is_control() || c.is_whitespace())
        }
    };
    if ok {
        Ok(())
    } else {
        Err(ApiError::Validation("That browser input is not valid."))
    }
}

impl ApiClient {
    /// One request with a bounded body. `400`/`409`/`422`/`404` keep the
    /// server's short message when it is safe to show; anything else stays a
    /// bare status so the session's token and back-off paths still work.
    pub(super) fn panel_exchange<B: Serialize + ?Sized>(
        &self,
        method: Method,
        route: &[&str],
        body: Option<&B>,
    ) -> Result<Zeroizing<Vec<u8>>, ApiError> {
        self.panel_exchange_query(method, route, &[], body)
    }

    /// Like [`Self::panel_exchange`], with query parameters (encoded here).
    pub(super) fn panel_exchange_query<B: Serialize + ?Sized>(
        &self,
        method: Method,
        route: &[&str],
        query: &[(&str, &str)],
        body: Option<&B>,
    ) -> Result<Zeroizing<Vec<u8>>, ApiError> {
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| ApiError::InvalidResponse)?
            .clear()
            .extend(route);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let response = send_with_policy(&method, route, || {
            let mut request = self
                .client
                .request(method.clone(), url.clone())
                .bearer_auth(self.token.expose_secret());
            if let Some(body) = body {
                request = request.json(body);
            }
            request
        })?;
        note_rate_limit(&response);
        let status = response.status();
        let mut bytes = Zeroizing::new(Vec::new());
        response
            .take(MAX_RESPONSE + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ApiError::Transport)?;
        if bytes.len() as u64 > MAX_RESPONSE {
            return Err(ApiError::TooLarge);
        }
        if !status.is_success() {
            if let Some(error) = rejection(status.as_u16(), &bytes) {
                return Err(error);
            }
            return Err(ApiError::Http(status.as_u16()));
        }
        Ok(bytes)
    }

    pub(super) fn panel_json<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        method: Method,
        route: &[&str],
        body: Option<&B>,
    ) -> Result<T, ApiError> {
        let bytes = self.panel_exchange(method, route, body)?;
        serde_json::from_slice(&bytes).map_err(|_| ApiError::InvalidResponse)
    }

    /// `GET /v1/vault`: names, kinds and metadata. Never a value.
    pub fn vault_accounts(&self, domain: Option<&str>) -> Result<VaultAccounts, ApiError> {
        let query = domain
            .filter(|value| !value.trim().is_empty())
            .map(|filter| ("domain", filter.trim()));
        let bytes = match query {
            Some((key, value)) => self.panel_exchange_query::<()>(
                Method::GET,
                &["v1", "vault"],
                &[(key, value)],
                None,
            )?,
            None => self.panel_exchange::<()>(Method::GET, &["v1", "vault"], None)?,
        };
        let list: VaultAccounts =
            serde_json::from_slice(&bytes).map_err(|_| ApiError::InvalidResponse)?;
        if list.entries.len() > MAX_VAULT_ENTRIES {
            return Err(ApiError::InvalidResponse);
        }
        for entry in &list.entries {
            check_vault_entry(entry)?;
        }
        Ok(list)
    }

    /// `GET /v1/vault`: names, kinds and metadata. Never a value.
    pub fn vault_list(&self) -> Result<Vec<VaultEntry>, ApiError> {
        Ok(self.vault_accounts(None)?.entries)
    }

    /// `PUT /v1/vault/{name}`: create or replace. A write: an unknown
    /// outcome is never replayed by the caller.
    pub fn vault_put(&self, name: &str, input: &VaultPut) -> Result<VaultEntry, ApiError> {
        validate_vault_put(name, input)?;
        let body = VaultPutBody {
            kind: input.kind,
            source: input.source,
            reference: input.reference.as_deref(),
            username: input
                .username
                .as_deref()
                .filter(|_| input.kind == VaultKind::Password)
                .map(str::trim)
                .filter(|v| !v.is_empty()),
            value: input.value.as_str(),
            domains: &input.domains,
            note: input
                .note
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty()),
        };
        let entry: VaultEntry =
            self.panel_json(Method::PUT, &["v1", "vault", name], Some(&body))?;
        check_vault_entry(&entry)?;
        if entry.name != name {
            return Err(ApiError::InvalidResponse);
        }
        Ok(entry)
    }

    /// `GET /v1/vault/providers`: the password managers, with token and CLI
    /// status. Never a token.
    pub fn password_managers(&self) -> Result<Vec<PasswordManager>, ApiError> {
        let list: PasswordManagerList =
            self.panel_json::<_, ()>(Method::GET, &["v1", "vault", "providers"], None)?;
        Ok(list.providers)
    }

    /// `PUT /v1/vault/providers/{id}`: save the owner's access token (write
    /// only). The response is the status, never the token.
    pub fn password_manager_save(
        &self,
        id: &str,
        token: &str,
    ) -> Result<PasswordManager, ApiError> {
        #[derive(Serialize)]
        struct Body<'a> {
            token: &'a str,
        }
        let manager: PasswordManager = self.panel_json(
            Method::PUT,
            &["v1", "vault", "providers", id],
            Some(&Body { token }),
        )?;
        if manager.id != id {
            return Err(ApiError::InvalidResponse);
        }
        Ok(manager)
    }

    /// `DELETE /v1/vault/providers/{id}`: forget the owner's token.
    pub fn password_manager_clear(&self, id: &str) -> Result<(), ApiError> {
        let ack: DeletedAck =
            self.panel_json::<_, ()>(Method::DELETE, &["v1", "vault", "providers", id], None)?;
        if ack.deleted {
            Ok(())
        } else {
            Err(ApiError::InvalidResponse)
        }
    }

    /// `POST /v1/vault/providers/{id}/test`: probe the saved token via the CLI.
    pub fn password_manager_test(&self, id: &str) -> Result<PasswordManagerProbe, ApiError> {
        self.panel_json::<PasswordManagerProbe, ()>(
            Method::POST,
            &["v1", "vault", "providers", id, "test"],
            None,
        )
    }

    /// `DELETE /v1/vault/{name}`.
    pub fn vault_delete(&self, name: &str) -> Result<(), ApiError> {
        if !valid_vault_name(name) {
            return Err(ApiError::Validation("Vault entry name is not valid."));
        }
        let ack: DeletedAck =
            self.panel_json::<_, ()>(Method::DELETE, &["v1", "vault", name], None)?;
        if ack.deleted {
            Ok(())
        } else {
            Err(ApiError::InvalidResponse)
        }
    }

    /// `GET /v1/browser`.
    pub fn browser_status(&self) -> Result<BrowserInfo, ApiError> {
        check_browser_info(self.panel_json::<_, ()>(Method::GET, &["v1", "browser"], None)?)
    }

    /// `GET /v1/browser/screenshot`: JPEG bytes, at most 512 KiB. `404`
    /// (no session) comes back as `ApiError::Http(404)`.
    pub fn browser_screenshot(&self) -> Result<Vec<u8>, ApiError> {
        self.fetch_screenshot(&["v1", "browser", "screenshot"])
    }

    pub(super) fn fetch_screenshot(&self, route: &[&str]) -> Result<Vec<u8>, ApiError> {
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| ApiError::InvalidResponse)?
            .clear()
            .extend(route);
        let response = self
            .client
            .get(url)
            .bearer_auth(self.token.expose_secret())
            .send()
            .map_err(|_| ApiError::Transport)?;
        note_rate_limit(&response);
        let status = response.status();
        if !status.is_success() {
            return Err(ApiError::Http(status.as_u16()));
        }
        let cap = MAX_SCREENSHOT_BYTES as u64;
        if response.content_length().is_some_and(|len| len > cap) {
            return Err(ApiError::TooLarge);
        }
        let is_jpeg = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("image/jpeg"));
        if !is_jpeg {
            return Err(ApiError::InvalidResponse);
        }
        let mut bytes = Vec::new();
        response
            .take(cap + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ApiError::Transport)?;
        if bytes.len() as u64 > cap {
            return Err(ApiError::TooLarge);
        }
        if bytes.len() < 4 || bytes[..3] != [0xFF, 0xD8, 0xFF] {
            return Err(ApiError::InvalidResponse);
        }
        Ok(bytes)
    }

    /// `POST /v1/browser/takeover`: the user takes control.
    pub fn browser_takeover(&self) -> Result<BrowserInfo, ApiError> {
        check_browser_info(self.panel_json::<_, ()>(
            Method::POST,
            &["v1", "browser", "takeover"],
            None,
        )?)
    }

    /// `POST /v1/browser/handback`: control returns to the bot.
    pub fn browser_handback(&self) -> Result<BrowserInfo, ApiError> {
        check_browser_info(self.panel_json::<_, ()>(
            Method::POST,
            &["v1", "browser", "handback"],
            None,
        )?)
    }

    /// `POST /v1/browser/input`, valid only while the user holds control.
    pub fn browser_input(&self, input: &BrowserInput) -> Result<BrowserInfo, ApiError> {
        validate_browser_input(input)?;
        check_browser_info(self.panel_json(
            Method::POST,
            &["v1", "browser", "input"],
            Some(input),
        )?)
    }

    /// `POST /v1/browser/close`: returns whether a session was closed.
    pub fn browser_close(&self) -> Result<bool, ApiError> {
        let ack: ClosedAck =
            self.panel_json::<_, ()>(Method::POST, &["v1", "browser", "close"], None)?;
        Ok(ack.closed)
    }

    /// `POST /v1/connectors/onedrive/start`: the Microsoft consent page for
    /// the acting user. Only an `https` address is accepted back; the caller
    /// opens it in the system browser.
    pub fn onedrive_consent_start(&self) -> Result<String, ApiError> {
        let ack: OneDriveStartAck = self.panel_json::<_, ()>(
            Method::POST,
            &["v1", "connectors", "onedrive", "start"],
            None,
        )?;
        check_consent_url(&ack.url)?;
        Ok(ack.url)
    }

    /// `POST /v1/connectors/{id}/start`: the provider's consent page for the
    /// acting user (Outlook, Google, Notion sign-in). Only an `https` address
    /// is accepted back. A conflict means the provider is not set up yet.
    pub fn connector_consent_start(&self, id: &str) -> Result<String, ApiError> {
        if !valid_personal_id(id) {
            return Err(ApiError::Validation("Connector id is not valid."));
        }
        let ack: OneDriveStartAck =
            self.panel_json::<_, ()>(Method::POST, &["v1", "connectors", id, "start"], None)?;
        check_consent_url(&ack.url)?;
        Ok(ack.url)
    }

    /// `PUT /v1/connectors/{id}/token`: connect with a pasted token (Notion).
    /// The token is sent once, sealed on the server, and never read back.
    pub fn connector_set_token(&self, id: &str, token: &str) -> Result<(), ApiError> {
        if !valid_personal_id(id) {
            return Err(ApiError::Validation("Connector id is not valid."));
        }
        if token.is_empty() || token.len() > 512 || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(ApiError::Validation("That does not look like a token."));
        }
        let ack: ConnectorAck = self.panel_json(
            Method::PUT,
            &["v1", "connectors", id, "token"],
            Some(&ConnectorTokenBody { token }),
        )?;
        if ack.connected {
            Ok(())
        } else {
            Err(ApiError::InvalidResponse)
        }
    }

    /// `DELETE /v1/connectors/{id}`: forget the acting user's connection.
    pub fn connector_disconnect(&self, id: &str) -> Result<(), ApiError> {
        if !valid_personal_id(id) {
            return Err(ApiError::Validation("Connector id is not valid."));
        }
        let ack: ConnectorAck =
            self.panel_json::<_, ()>(Method::DELETE, &["v1", "connectors", id], None)?;
        if ack.connected {
            Err(ApiError::InvalidResponse)
        } else {
            Ok(())
        }
    }

    /// `DELETE /v1/connectors/onedrive`: forget the acting user's OneDrive.
    pub fn onedrive_disconnect(&self) -> Result<(), ApiError> {
        let ack: OneDriveDisconnectAck =
            self.panel_json::<_, ()>(Method::DELETE, &["v1", "connectors", "onedrive"], None)?;
        if ack.connected {
            Err(ApiError::InvalidResponse)
        } else {
            Ok(())
        }
    }
}

/// The server's own words for a refused request: `{"error": "...", "code": "..."}`.
/// 400/404/409/422 keep the message; 422 and 502-504 keep the code too, so the
/// panel can tell an upstream outage from a rejected key. Anything else, or any
/// text that is not short and plain, is a bare status.
fn rejection(status: u16, bytes: &[u8]) -> Option<ApiError> {
    if !matches!(status, 400 | 404 | 409 | 422 | 502..=504) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let message = value.get("error").and_then(|v| v.as_str())?;
    if message.is_empty() || message.len() > 512 || message.chars().any(char::is_control) {
        return None;
    }
    let code = value.get("code").and_then(|v| v.as_str()).filter(|code| {
        (1..=64).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    });
    match (status, code) {
        (422 | 502..=504, Some(code)) => Some(ApiError::Coded {
            status,
            code: code.to_owned(),
            message: message.to_owned(),
        }),
        (400 | 404 | 409 | 422, _) => Some(ApiError::Rejected(message.to_owned())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_503_rate_limit_keeps_its_code() {
        let body =
            br#"{"error":"Composio is rate limiting requests.","code":"composio_rate_limited"}"#;
        assert_eq!(
            rejection(503, body),
            Some(ApiError::Coded {
                status: 503,
                code: "composio_rate_limited".into(),
                message: "Composio is rate limiting requests.".into(),
            })
        );
    }

    #[test]
    fn coded_rejections_keep_the_server_message_and_code() {
        let body = br#"{"error":"Composio could not be reached (HTTP 503).","code":"composio_unavailable"}"#;
        assert_eq!(
            rejection(502, body),
            Some(ApiError::Coded {
                status: 502,
                code: "composio_unavailable".into(),
                message: "Composio could not be reached (HTTP 503).".into(),
            })
        );
        let body = br#"{"error":"invalid request","code":"validation"}"#;
        assert_eq!(
            rejection(422, body),
            Some(ApiError::Coded {
                status: 422,
                code: "validation".into(),
                message: "invalid request".into(),
            })
        );
    }

    #[test]
    fn plain_rejections_keep_the_message_only() {
        assert_eq!(
            rejection(422, br#"{"error":"name is taken"}"#),
            Some(ApiError::Rejected("name is taken".into()))
        );
        assert_eq!(
            rejection(404, br#"{"error":"not found","code":"not_found"}"#),
            Some(ApiError::Rejected("not found".into()))
        );
    }

    #[test]
    fn other_statuses_and_unsafe_text_are_bare_statuses() {
        assert_eq!(rejection(502, b"<html>bad gateway</html>"), None);
        assert_eq!(rejection(500, br#"{"error":"boom"}"#), None);
        assert_eq!(rejection(401, br#"{"error":"expired"}"#), None);
        assert_eq!(rejection(422, br#"{"error":""}"#), None);
        assert_eq!(rejection(422, br#"{"error":"a\nb"}"#), None);
        let long = format!(r#"{{"error":"{}"}}"#, "x".repeat(513));
        assert_eq!(rejection(422, long.as_bytes()), None);
        // A 502 needs a plain code to count as a server message. Without one
        // it is a bare status, as before.
        assert_eq!(
            rejection(502, br#"{"error":"down","code":"Bad Code!"}"#),
            None
        );
    }
}
