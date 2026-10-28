use reqwest::{Method, blocking::Client, redirect::Policy};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{fmt, io::Read, net::SocketAddr, time::Duration};
use url::Url;
use zeroize::{Zeroize, Zeroizing};

const MAX_RESPONSE: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiError {
    Validation(&'static str),
    Http(u16),
    Transport,
    InvalidResponse,
    TooLarge,
}

impl ApiError {
    pub fn unknown_outcome(self) -> bool {
        // In particular, a gateway/request timeout does not prove rejection.
        !matches!(self, Self::Validation(_) | Self::Http(400..=407 | 409..=499))
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) => f.write_str(message),
            Self::Http(401) => {
                f.write_str("Unauthorized (401). Disconnect and check your API token.")
            }
            Self::Http(403) => f.write_str(
                "Forbidden (403). Server policy or data-egress approval blocks this request.",
            ),
            Self::Http(429) => {
                f.write_str("Rate limited (429). Wait at least one minute, then retry the read.")
            }
            Self::Http(300..=399) => f.write_str(
                "Server redirected the request. Redirects are disabled; check the server URL.",
            ),
            Self::Http(code) => write!(
                f,
                "Server returned HTTP {code}. Check server availability or API compatibility."
            ),
            Self::Transport => f.write_str(
                "Connection failed or timed out. Check the server URL, TLS certificate and tunnel.",
            ),
            Self::InvalidResponse => {
                f.write_str("Unexpected API response. Check server version and compatibility.")
            }
            Self::TooLarge => f.write_str("Server response exceeds the 2 MiB safety limit."),
        }
    }
}

impl std::error::Error for ApiError {}

/// Accept an origin, not a user-selected route. Inspect the original authority
/// before URL normalization can turn shorthand or hexadecimal IPs into loopback.
pub fn validate_endpoint(raw: &str) -> Result<Url, ApiError> {
    let invalid = ApiError::Validation(
        "Use an HTTPS server origin, or exact HTTP localhost / 127.0.0.1 / [::1]. No credentials, path, query or fragment.",
    );
    if raw
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return Err(invalid);
    }
    let (scheme, rest) = raw.split_once("://").ok_or(invalid)?;
    if !matches!(scheme, "http" | "https") {
        return Err(invalid);
    }
    let authority = rest.split('/').next().ok_or(invalid)?;
    if authority.contains('@') {
        return Err(invalid);
    }
    let url = Url::parse(raw).map_err(|_| invalid)?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(invalid);
    }
    if scheme == "http" {
        let host = if authority.starts_with('[') {
            let end = authority.find(']').ok_or(invalid)?;
            &authority[..=end]
        } else {
            authority.split(':').next().ok_or(invalid)?
        };
        if !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
            return Err(invalid);
        }
    }
    Ok(url)
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_:-".contains(&b))
}

pub fn valid_token(token: &str) -> bool {
    (32..=512).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic())
}

pub fn valid_text(text: &str) -> bool {
    !text.trim().is_empty() && text.chars().count() <= 8000
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Selection {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Deserialize)]
pub struct Provider {
    pub id: String,
    pub models: Vec<String>,
    // Fail closed if an older server omits approval.
    #[serde(default)]
    pub data_egress_approved: bool,
}

#[derive(Debug, Deserialize)]
pub struct Catalog {
    pub providers: Vec<Provider>,
    pub selection: Selection,
}

impl Catalog {
    pub fn allows(&self, selection: &Selection) -> bool {
        self.providers.iter().any(|p| {
            p.id == selection.provider
                && p.data_egress_approved
                && p.models.contains(&selection.model)
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct Health {
    pub ok: bool,
    pub version: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Deserialize)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub content: String,
    pub created_at: String,
}

impl Drop for Message {
    fn drop(&mut self) {
        self.content.zeroize();
        self.id.zeroize();
        self.created_at.zeroize();
    }
}

#[derive(Deserialize)]
pub struct History {
    pub messages: Vec<Message>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    AwaitingApproval,
    Done,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Working",
            Self::AwaitingApproval => "Awaiting approval — Review in Telegram",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Task {
    pub id: String,
    pub kind: String,
    pub bot: String,
    pub status: TaskStatus,
}

#[derive(Deserialize)]
struct TaskEnvelope {
    task: Task,
}
#[derive(Deserialize)]
struct SelectionEnvelope {
    selection: Selection,
}

/// Construct and use only on a worker thread. No retries, cookies, redirects,
/// environment proxies or persistent storage. Errors never retain raw bodies/URLs.
pub struct ApiClient {
    client: Client,
    endpoint: Url,
    token: SecretString,
}

impl ApiClient {
    pub fn new(endpoint: Url, token: SecretString) -> Result<Self, ApiError> {
        validate_endpoint(endpoint.as_str())?;
        if !valid_token(token.expose_secret()) {
            return Err(ApiError::Validation(
                "API token must contain 32–512 visible ASCII characters, without spaces.",
            ));
        }
        let mut builder = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20));
        if endpoint.host_str() == Some("localhost") {
            // Never send a plaintext token to a DNS-resolved non-loopback address.
            builder = builder.resolve("localhost", SocketAddr::from(([127, 0, 0, 1], 0)));
        }
        let client = builder.build().map_err(|_| ApiError::Transport)?;
        Ok(Self {
            client,
            endpoint,
            token,
        })
    }

    fn request<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        method: Method,
        route: &[&str],
        query: &[(&str, &str)],
        body: Option<&B>,
    ) -> Result<T, ApiError> {
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| ApiError::InvalidResponse)?
            .clear()
            .extend(route);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let mut request = self
            .client
            .request(method, url)
            .bearer_auth(self.token.expose_secret());
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().map_err(|_| ApiError::Transport)?;
        let status = response.status();
        if !status.is_success() {
            return Err(ApiError::Http(status.as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAX_RESPONSE)
        {
            return Err(ApiError::TooLarge);
        }
        let mut bytes = Zeroizing::new(Vec::new());
        response
            .take(MAX_RESPONSE + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ApiError::Transport)?;
        if bytes.len() as u64 > MAX_RESPONSE {
            return Err(ApiError::TooLarge);
        }
        serde_json::from_slice(&bytes).map_err(|_| ApiError::InvalidResponse)
    }

    fn get<T: DeserializeOwned>(
        &self,
        route: &[&str],
        query: &[(&str, &str)],
    ) -> Result<T, ApiError> {
        self.request::<T, ()>(Method::GET, route, query, None)
    }

    pub fn health(&self) -> Result<Health, ApiError> {
        self.get(&["v1", "health"], &[])
    }
    pub fn models(&self, conversation: &str) -> Result<Catalog, ApiError> {
        check_id(conversation)?;
        self.get(&["v1", "models"], &[("conversation_id", conversation)])
    }
    pub fn history(&self, conversation: &str) -> Result<History, ApiError> {
        check_id(conversation)?;
        let history: History = self.get(
            &["v1", "conversations", conversation, "messages"],
            &[("limit", "100")],
        )?;
        if history.messages.len() > 100 {
            return Err(ApiError::InvalidResponse);
        }
        Ok(history)
    }
    pub fn select(&self, conversation: &str, selection: &Selection) -> Result<Selection, ApiError> {
        check_id(conversation)?;
        if selection.provider.is_empty()
            || selection.provider.len() > 128
            || selection.model.is_empty()
            || selection.model.len() > 256
        {
            return Err(ApiError::Validation(
                "Choose a provider and model from the server catalog.",
            ));
        }
        let response: SelectionEnvelope = self.request(
            Method::PUT,
            &["v1", "conversations", conversation, "model"],
            &[],
            Some(selection),
        )?;
        Ok(response.selection)
    }
    pub fn submit(&self, conversation: &str, text: &str) -> Result<Task, ApiError> {
        check_id(conversation)?;
        if !valid_text(text) {
            return Err(ApiError::Validation(
                "Enter 1–8,000 characters of message text.",
            ));
        }
        #[derive(Serialize)]
        struct Submit<'a> {
            conversation_id: &'a str,
            text: &'a str,
        }
        let response: TaskEnvelope = self.request(
            Method::POST,
            &["v1", "messages"],
            &[],
            Some(&Submit {
                conversation_id: conversation,
                text,
            }),
        )?;
        check_task(response.task)
    }
    pub fn task(&self, id: &str) -> Result<Task, ApiError> {
        check_id(id)?;
        let response: TaskEnvelope = self.get(&["v1", "tasks", id], &[])?;
        let task = check_task(response.task)?;
        if task.id != id {
            return Err(ApiError::InvalidResponse);
        }
        Ok(task)
    }
}

fn check_id(id: &str) -> Result<(), ApiError> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(ApiError::Validation(
            "IDs must be 1–128 ASCII letters, digits, underscores, colons or hyphens.",
        ))
    }
}

fn check_task(task: Task) -> Result<Task, ApiError> {
    if valid_id(&task.id) {
        Ok(task)
    } else {
        Err(ApiError::InvalidResponse)
    }
}
