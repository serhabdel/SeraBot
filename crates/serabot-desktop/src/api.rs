use reqwest::{Method, blocking::Client, redirect::Policy};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    io::Read,
    net::SocketAddr,
    time::Duration,
};
use url::Url;
use zeroize::{Zeroize, Zeroizing};

const MAX_RESPONSE: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiError {
    Validation(&'static str),
    /// A safe, server-authored administration message. Never contains a secret.
    Rejected(String),
    Http(u16),
    Transport,
    InvalidResponse,
    TooLarge,
}

impl ApiError {
    pub fn unknown_outcome(&self) -> bool {
        // In particular, a gateway/request timeout does not prove rejection.
        !matches!(
            self,
            Self::Validation(_) | Self::Rejected(_) | Self::Http(400..=407 | 409..=499)
        )
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) => f.write_str(message),
            Self::Rejected(message) => f.write_str(message),
            Self::Http(401) => {
                f.write_str("Unauthorized (401). Disconnect and check your API token.")
            }
            Self::Http(403) => f.write_str(
                "Forbidden (403). Server policy or data-egress approval blocks this request.",
            ),
            Self::Http(409) => f.write_str(
                "Conflict (409). That id already exists, or the current state does not allow it.",
            ),
            Self::Http(422) => {
                f.write_str("Rejected (422). The server did not accept the request.")
            }
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
        return Err(invalid.clone());
    }
    let (scheme, rest) = raw.split_once("://").ok_or_else(|| invalid.clone())?;
    if !matches!(scheme, "http" | "https") {
        return Err(invalid.clone());
    }
    let authority = rest.split('/').next().ok_or_else(|| invalid.clone())?;
    if authority.contains('@') {
        return Err(invalid.clone());
    }
    let url = Url::parse(raw).map_err(|_| invalid.clone())?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(invalid.clone());
    }
    if scheme == "http" {
        let host = if authority.starts_with('[') {
            let end = authority.find(']').ok_or_else(|| invalid.clone())?;
            &authority[..=end]
        } else {
            authority.split(':').next().ok_or_else(|| invalid.clone())?
        };
        if !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
            return Err(invalid.clone());
        }
    }
    Ok(url)
}

/// GitHub Actions secret *name* for the owner API token.
/// Workflows document this name and do not compile the secret value.
pub const OWNER_TOKEN_SECRET_NAME: &str = "SERABOT_OWNER_TOKEN";

/// First valid server origin among a runtime value and a compile-time value.
/// Empty and invalid strings are skipped. Tokens are never accepted here.
pub fn resolve_server_hint(runtime: Option<&str>, compiled: Option<&str>) -> String {
    for candidate in [runtime, compiled].into_iter().flatten() {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() && validate_endpoint(trimmed).is_ok() {
            return trimmed.to_owned();
        }
    }
    String::new()
}

/// Prefill for the desktop server field.
/// `SERABOT_OWNER_API_URL` in the process environment wins; otherwise the
/// value present when this binary was compiled (CI secret of the same name).
pub fn default_server_url() -> String {
    resolve_server_hint(
        std::env::var("SERABOT_OWNER_API_URL").ok().as_deref(),
        option_env!("SERABOT_OWNER_API_URL"),
    )
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_:-".contains(&b))
}

pub fn valid_provider_id(id: &str) -> bool {
    id != "auto"
        && id != "default"
        && (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn valid_secret_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn valid_credential(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 8_192
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

pub fn valid_model_id(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 256
        && model.trim() == model
        && !model.chars().any(char::is_control)
}

pub fn valid_token(token: &str) -> bool {
    (32..=512).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic())
}

pub fn valid_text(text: &str) -> bool {
    !text.trim().is_empty() && text.chars().count() <= 8000
}

/// Specialist tools the owner may select. Mirrors the server allowlist.
/// `delegate` is orchestrator-only and is intentionally absent.
pub const SPECIALIST_TOOLS: &[&str] = &[
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

pub const DEFAULT_SPECIALIST_TOOLS: &[&str] = &["skill_list", "skill_search", "skill_load"];

const MAX_BOT_DESCRIPTION_CHARS: usize = 2000;
const MAX_SCAFFOLD_TOOLS: usize = 32;
const MAX_SCAFFOLD_SKILLS: usize = 16;
pub const MAX_SKILL_DESCRIPTION_CHARS: usize = 500;
pub const MAX_SKILL_BODY_BYTES: usize = 96_000;
const MAX_SKILL_LIST: usize = 256;
const MAX_BOT_LIST: usize = 64;
const MAX_BOT_SUMMARY_ITEMS: usize = 64;

/// Bot id safe to show in the fleet list or on a chat bubble.
pub fn valid_speaker(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name.len() <= 64
        && first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Lowercase slug shared by bot ids and conversation slugs (`^[a-z][a-z0-9-]{0,31}$`).
pub fn valid_slug(slug: &str) -> bool {
    let mut chars = slug.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    slug.len() <= 32
        && first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub fn reserved_bot_id(id: &str) -> bool {
    matches!(id, "main" | "con" | "prn" | "aux" | "nul")
        || (id.len() == 4
            && (id.starts_with("com") || id.starts_with("lpt"))
            && id.as_bytes()[3].is_ascii_digit())
}

pub fn valid_bot_id(id: &str) -> bool {
    valid_slug(id) && !reserved_bot_id(id)
}

pub fn valid_skill_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    id.len() <= 64
        && first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn reserved_skill_id(id: &str) -> bool {
    matches!(id, "con" | "prn" | "aux" | "nul")
        || (id.len() == 4
            && (id.starts_with("com") || id.starts_with("lpt"))
            && id.as_bytes()[3].is_ascii_digit())
}

/// Skill ids the owner may register. `main` is allowed; Windows device names are not.
pub fn valid_new_skill_id(id: &str) -> bool {
    valid_skill_id(id) && !reserved_skill_id(id)
}

pub fn valid_skill_description(text: &str) -> bool {
    let text = text.trim();
    text.chars().count() <= MAX_SKILL_DESCRIPTION_CHARS
        && !text
            .chars()
            .any(|c| c.is_control() || c == '"' || c == '\\')
}

pub fn valid_skill_body(text: &str) -> bool {
    text.len() <= MAX_SKILL_BODY_BYTES
        && !text
            .chars()
            .any(|c| c != '\n' && c != '\r' && c != '\t' && c.is_control())
}

pub fn valid_bot_description(text: &str) -> bool {
    text.chars().count() <= MAX_BOT_DESCRIPTION_CHARS
        && !text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
}

pub fn validate_tool_list(tools: &[String]) -> Result<(), ApiError> {
    if tools.len() > MAX_SCAFFOLD_TOOLS {
        return Err(ApiError::Validation("Choose at most 32 specialist tools."));
    }
    let mut seen = HashSet::with_capacity(tools.len());
    for tool in tools {
        if tool == "delegate" {
            return Err(ApiError::Validation(
                "Specialists cannot be granted delegate.",
            ));
        }
        if !SPECIALIST_TOOLS.contains(&tool.as_str()) {
            return Err(ApiError::Validation(
                "Choose tools from the specialist list.",
            ));
        }
        if !seen.insert(tool.as_str()) {
            return Err(ApiError::Validation("Each tool can be selected once."));
        }
    }
    Ok(())
}

/// Comma- or whitespace-separated skill ids. Empty input means no skills.
pub fn parse_skill_list(raw: &str) -> Result<Vec<String>, ApiError> {
    let mut out = Vec::new();
    for part in raw.split(|c: char| c == ',' || c.is_whitespace()) {
        if part.is_empty() {
            continue;
        }
        if out.len() >= MAX_SCAFFOLD_SKILLS {
            return Err(ApiError::Validation("Enter at most 16 skill ids."));
        }
        if !valid_skill_id(part) {
            return Err(ApiError::Validation(
                "Skill ids are lowercase slugs such as ask.",
            ));
        }
        if out.iter().any(|id| id == part) {
            return Err(ApiError::Validation("Each skill id can be listed once."));
        }
        out.push(part.to_owned());
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationKind {
    Section,
    Group,
    Direct,
    Room,
}

impl ConversationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Section => "section",
            Self::Group => "group",
            Self::Direct => "direct",
            Self::Room => "room",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Section => "Section",
            Self::Group => "Group",
            Self::Direct => "Direct",
            Self::Room => "Room",
        }
    }

    pub fn id_prefix(self) -> &'static str {
        match self {
            Self::Section => "sec",
            Self::Group => "grp",
            Self::Direct => "dir",
            Self::Room => "room",
        }
    }

    pub fn accepts_turns(self) -> bool {
        matches!(self, Self::Direct | Self::Room)
    }

    pub const ALL: [Self; 4] = [Self::Section, Self::Group, Self::Direct, Self::Room];
}

pub fn conversation_node_id(kind: ConversationKind, slug: &str) -> Option<String> {
    valid_slug(slug).then(|| format!("{}:{slug}", kind.id_prefix()))
}

pub fn valid_conversation_parent(
    kind: ConversationKind,
    parent: Option<&str>,
) -> Result<(), ApiError> {
    match kind {
        ConversationKind::Section => {
            if parent.is_some() {
                return Err(ApiError::Validation("Sections cannot have a parent."));
            }
        }
        ConversationKind::Group => {
            let Some(parent) = parent.filter(|parent| !parent.is_empty()) else {
                return Err(ApiError::Validation(
                    "Groups require a section parent such as sec:finance.",
                ));
            };
            if !scoped_parent(parent, "sec") {
                return Err(ApiError::Validation(
                    "A group parent must be a section id such as sec:finance.",
                ));
            }
        }
        ConversationKind::Direct | ConversationKind::Room => {
            if let Some(parent) = parent.filter(|parent| !parent.is_empty()) {
                if !scoped_parent(parent, "sec") && !scoped_parent(parent, "grp") {
                    return Err(ApiError::Validation(
                        "Direct and room nodes sit under a section (sec:) or group (grp:).",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn scoped_parent(parent: &str, prefix: &str) -> bool {
    parent
        .split_once(':')
        .is_some_and(|(got, slug)| got == prefix && valid_slug(slug) && parent.len() <= 128)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct CreatedBot {
    pub id: String,
    pub enabled: bool,
    pub role: String,
    pub live: bool,
    pub tools: Vec<String>,
    pub skills: Vec<String>,
    pub require_approval: Vec<String>,
    pub bot_file: String,
    pub prompt_file: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct CreatedConversation {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub accepts_turns: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct CreatedSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub file: String,
    pub live: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct SkillSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub pack: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct BotSummary {
    pub id: String,
    pub role: String,
    pub enabled: bool,
    pub live: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    pub tools: Vec<String>,
    pub skills: Vec<String>,
    #[serde(default)]
    pub require_approval: Vec<String>,
}

/// One conversation row from `GET /v1/conversations`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ConversationSummary {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub last_message_at: Option<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
    pub accepts_turns: bool,
}

impl ConversationSummary {
    pub fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.id)
    }

    /// Legacy rows are turn-capable direct threads.
    pub fn normalized_kind(&self) -> &str {
        if self.kind == "legacy" {
            "direct"
        } else {
            &self.kind
        }
    }
}

/// One page of conversations. The cursor is opaque to the UI.
#[derive(Debug, Deserialize)]
pub struct ConversationPage {
    pub conversations: Vec<ConversationSummary>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// Operator-reviewable fact with mandatory provenance.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct MemoryEntry {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Presence-only secret status. Values never appear in API responses.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct SecretStatus {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub configured: bool,
}

/// Read-only provider view from `GET /v1/providers`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ProviderStatus {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub routing_classes: Vec<String>,
    #[serde(default)]
    pub data_egress_approved: bool,
    #[serde(default)]
    pub catalog_fetch: bool,
    #[serde(default)]
    pub catalog_verified: bool,
    #[serde(default)]
    pub managed: bool,
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretStatus>,
}

/// Read-only connector view from `GET /v1/connectors`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ConnectorStatus {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub redirect_uri: Option<String>,
    #[serde(default)]
    pub api_base: Option<String>,
    #[serde(default)]
    pub default_inbox_id: Option<String>,
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretStatus>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NewProvider {
    pub id: String,
    pub kind: String,
    pub base_url: String,
    pub models: Vec<String>,
    pub api_key_secret: String,
    #[serde(default)]
    pub catalog_fetch: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ConnectorUpdate {
    pub enabled: bool,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AudioUpdate {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcription_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub understanding_model: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct BotProfileUpdate {
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub avatar: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AdminAck {
    #[serde(default)]
    pub saved: bool,
    #[serde(default)]
    pub restart_required: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AdminRestart {
    #[serde(default)]
    pub restarting: bool,
}

pub const MEMORY_KINDS: &[&str] = &["preference", "fact", "decision", "context", "note"];
pub const MAX_MEMORY_CONTENT_CHARS: usize = 4_000;
pub const MAX_MEMORY_SOURCE_CHARS: usize = 512;
pub const MAX_CONVERSATION_TITLE_CHARS: usize = 120;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Selection {
    pub provider: String,
    pub model: String,
}

impl Selection {
    pub fn auto() -> Self {
        Self {
            provider: "auto".into(),
            model: String::new(),
        }
    }

    pub fn is_auto(&self) -> bool {
        self.provider == "auto"
    }
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
pub struct CatalogEntry {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub data_egress_approved: bool,
}

#[derive(Debug, Deserialize)]
pub struct Catalog {
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub entries: Vec<CatalogEntry>,
    pub selection: Selection,
    #[serde(default)]
    pub effective_selection: Option<Selection>,
}

impl Catalog {
    pub fn allows(&self, selection: &Selection) -> bool {
        if selection.is_auto() {
            return self
                .effective_selection
                .as_ref()
                .is_some_and(|effective| self.allows_concrete(effective));
        }
        self.allows_concrete(selection)
    }

    fn allows_concrete(&self, selection: &Selection) -> bool {
        self.providers.iter().any(|p| {
            p.id == selection.provider
                && p.data_egress_approved
                && p.models.contains(&selection.model)
        })
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct Health {
    pub ok: bool,
    pub version: String,
    #[serde(default)]
    pub audio_enabled: Option<bool>,
    #[serde(default)]
    pub audio_transcription_model: Option<String>,
    #[serde(default)]
    pub audio_understanding_model: Option<String>,
    #[serde(default)]
    pub firecrawl_enabled: Option<bool>,
    #[serde(default)]
    pub telegram_enabled: Option<bool>,
    #[serde(default)]
    pub catalog_fetch: Option<bool>,
    #[serde(default)]
    pub bot_chat_binding: Option<bool>,
    #[serde(default)]
    pub bot_profiles: Option<bool>,
    #[serde(default)]
    pub conversation_titles: Option<bool>,
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
    /// Bot id on a room or handoff reply. Absent on ordinary assistant text.
    #[serde(default)]
    pub name: Option<String>,
    /// Bot that produced this assistant message, resolved by the server.
    #[serde(default)]
    pub bot: Option<String>,
}

impl Drop for Message {
    fn drop(&mut self) {
        self.content.zeroize();
        self.id.zeroize();
        self.created_at.zeroize();
        if let Some(name) = self.name.as_mut() {
            name.zeroize();
        }
        if let Some(bot) = self.bot.as_mut() {
            bot.zeroize();
        }
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
            Self::AwaitingApproval => "Awaiting approval",
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

#[derive(Clone, Debug, Deserialize)]
pub struct Approval {
    pub id: String,
    pub action_id: String,
    pub tool: String,
    pub summary: String,
    pub expires_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    Once,
    Always,
    Deny,
}

impl ApprovalDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Always => "always",
            Self::Deny => "deny",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Once => "Approve once",
            Self::Always => "Always allow",
            Self::Deny => "Deny",
        }
    }
}

#[derive(Deserialize)]
struct TaskEnvelope {
    task: Task,
}
#[derive(Deserialize)]
struct SelectionEnvelope {
    selection: Selection,
}
#[derive(Deserialize)]
struct ApprovalsEnvelope {
    approvals: Vec<Approval>,
}
#[derive(Deserialize)]
struct BotEnvelope {
    bot: CreatedBot,
}
#[derive(Deserialize)]
struct ConversationEnvelope {
    conversation: CreatedConversation,
}
#[derive(Deserialize)]
struct SkillEnvelope {
    skill: CreatedSkill,
}
#[derive(Deserialize)]
struct SkillsEnvelope {
    skills: Vec<SkillSummary>,
}
#[derive(Deserialize)]
struct ConversationSummaryEnvelope {
    conversation: ConversationSummary,
}
#[derive(Deserialize)]
struct MemoriesEnvelope {
    memories: Vec<MemoryEntry>,
}
#[derive(Deserialize)]
struct MemoryEnvelope {
    memory: MemoryEntry,
}
#[derive(Deserialize)]
struct ProvidersEnvelope {
    providers: Vec<ProviderStatus>,
}
#[derive(Deserialize)]
struct ConnectorsEnvelope {
    connectors: Vec<ConnectorStatus>,
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
        if history.messages.iter().any(|message| {
            message
                .name
                .as_ref()
                .is_some_and(|name| !valid_speaker(name))
                || message.bot.as_ref().is_some_and(|bot| !valid_speaker(bot))
        }) {
            return Err(ApiError::InvalidResponse);
        }
        Ok(history)
    }
    pub fn select(&self, conversation: &str, selection: &Selection) -> Result<Selection, ApiError> {
        check_id(conversation)?;
        if selection.is_auto() {
            // Server stores auto mode; model id is not used.
        } else if selection.provider.is_empty()
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

    pub fn approvals(&self) -> Result<Vec<Approval>, ApiError> {
        let response: ApprovalsEnvelope = self.get(&["v1", "approvals"], &[])?;
        if response.approvals.len() > 100 {
            return Err(ApiError::InvalidResponse);
        }
        for approval in &response.approvals {
            if !valid_id(&approval.id) || !valid_id(&approval.action_id) {
                return Err(ApiError::InvalidResponse);
            }
            if approval.tool.is_empty()
                || approval.tool.len() > 128
                || approval.summary.chars().count() > 4000
                || approval.expires_at.len() > 64
            {
                return Err(ApiError::InvalidResponse);
            }
        }
        Ok(response.approvals)
    }

    pub fn decide_approval(&self, id: &str, decision: ApprovalDecision) -> Result<(), ApiError> {
        check_id(id)?;
        #[derive(Serialize)]
        struct Body {
            decision: &'static str,
        }
        self.request::<serde_json::Value, Body>(
            Method::POST,
            &["v1", "approvals", id, "decide"],
            &[],
            Some(&Body {
                decision: decision.as_str(),
            }),
        )?;
        Ok(())
    }

    pub fn create_bot(
        &self,
        id: &str,
        description: &str,
        tools: &[String],
        skills: &[String],
    ) -> Result<CreatedBot, ApiError> {
        if !valid_bot_id(id) {
            return Err(ApiError::Validation(
                "Bot id must be a lowercase slug of at most 32 characters, and cannot be main or a Windows device name.",
            ));
        }
        if !valid_bot_description(description) {
            return Err(ApiError::Validation(
                "Description must be at most 2,000 characters, without control characters.",
            ));
        }
        validate_tool_list(tools)?;
        if skills.len() > MAX_SCAFFOLD_SKILLS {
            return Err(ApiError::Validation("Enter at most 16 skill ids."));
        }
        let mut seen_skills = HashSet::with_capacity(skills.len());
        for skill in skills {
            if !valid_skill_id(skill) {
                return Err(ApiError::Validation(
                    "Skill ids are lowercase slugs such as ask.",
                ));
            }
            if !seen_skills.insert(skill.as_str()) {
                return Err(ApiError::Validation("Each skill id can be listed once."));
            }
        }
        #[derive(Serialize)]
        struct Body<'a> {
            id: &'a str,
            description: &'a str,
            tools: &'a [String],
            skills: &'a [String],
        }
        let response: BotEnvelope = self.request(
            Method::POST,
            &["v1", "bots"],
            &[],
            Some(&Body {
                id,
                description,
                tools,
                skills,
            }),
        )?;
        check_created_bot(response.bot, id)
    }

    pub fn create_conversation(
        &self,
        kind: ConversationKind,
        slug: &str,
        parent_id: Option<&str>,
        title: Option<&str>,
    ) -> Result<CreatedConversation, ApiError> {
        if !valid_slug(slug) {
            return Err(ApiError::Validation(
                "Slug must be a lowercase slug of at most 32 characters.",
            ));
        }
        let parent_id = parent_id.filter(|parent| !parent.is_empty());
        valid_conversation_parent(kind, parent_id)?;
        let title = title.map(str::trim).filter(|title| !title.is_empty());
        if title.is_some_and(|title| !valid_conversation_title(title)) {
            return Err(ApiError::Validation(
                "Conversation names must be 1–120 visible characters.",
            ));
        }
        #[derive(Serialize)]
        struct Body<'a> {
            kind: &'a str,
            slug: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            parent_id: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            title: Option<&'a str>,
        }
        let response: ConversationEnvelope = self.request(
            Method::POST,
            &["v1", "conversations"],
            &[],
            Some(&Body {
                kind: kind.as_str(),
                slug,
                parent_id,
                title,
            }),
        )?;
        check_created_conversation(response.conversation, kind, slug, parent_id)
    }

    pub fn create_skill(
        &self,
        id: &str,
        description: &str,
        body: Option<&str>,
    ) -> Result<CreatedSkill, ApiError> {
        if !valid_new_skill_id(id) {
            return Err(ApiError::Validation(
                "Skill id must be a lowercase slug of at most 64 characters, and cannot be a Windows device name.",
            ));
        }
        let description = description.trim();
        if !valid_skill_description(description) {
            return Err(ApiError::Validation(
                "Description must be one line of at most 500 characters, without quotes or backslashes.",
            ));
        }
        let body = body.map(str::trim).filter(|text| !text.is_empty());
        if let Some(body) = body {
            if !valid_skill_body(body) {
                return Err(ApiError::Validation(
                    "Body must be markdown of at most 96,000 bytes, without control characters.",
                ));
            }
        }
        #[derive(Serialize)]
        struct Body<'a> {
            id: &'a str,
            description: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            body: Option<&'a str>,
        }
        let response: SkillEnvelope = self.request(
            Method::POST,
            &["v1", "skills"],
            &[],
            Some(&Body {
                id,
                description,
                body,
            }),
        )?;
        check_created_skill(response.skill, id)
    }

    pub fn list_skills(&self) -> Result<Vec<SkillSummary>, ApiError> {
        let response: SkillsEnvelope = self.get(&["v1", "skills"], &[])?;
        if response.skills.len() > MAX_SKILL_LIST {
            return Err(ApiError::InvalidResponse);
        }
        let mut seen = HashSet::with_capacity(response.skills.len());
        for skill in &response.skills {
            if !valid_skill_id(&skill.id) || !seen.insert(skill.id.as_str()) {
                return Err(ApiError::InvalidResponse);
            }
            if !bounded_skill_label(&skill.name)
                || skill.description.chars().count() > 2_000
                || skill.description.chars().any(|c| c.is_control())
            {
                return Err(ApiError::InvalidResponse);
            }
            if skill
                .pack
                .as_ref()
                .is_some_and(|pack| !valid_skill_id(pack))
            {
                return Err(ApiError::InvalidResponse);
            }
        }
        Ok(response.skills)
    }

    pub fn list_bots(&self) -> Result<Vec<BotSummary>, ApiError> {
        #[derive(Deserialize)]
        struct BotsEnvelope {
            bots: Vec<BotSummary>,
        }
        let response: BotsEnvelope = self.get(&["v1", "bots"], &[])?;
        if response.bots.len() > MAX_BOT_LIST {
            return Err(ApiError::InvalidResponse);
        }
        let mut seen = HashSet::with_capacity(response.bots.len());
        for bot in &response.bots {
            if !valid_speaker(&bot.id) || !seen.insert(bot.id.as_str()) {
                return Err(ApiError::InvalidResponse);
            }
            // Accept future roles; the surface only displays the label.
            if !valid_speaker(&bot.role) {
                return Err(ApiError::InvalidResponse);
            }
            if bot.tools.len() > MAX_BOT_SUMMARY_ITEMS || bot.skills.len() > MAX_BOT_SUMMARY_ITEMS {
                return Err(ApiError::InvalidResponse);
            }
            if bot.require_approval.len() > MAX_BOT_SUMMARY_ITEMS
                || bot.require_approval.iter().any(|tool| {
                    !valid_speaker(tool) || !bot.tools.iter().any(|listed| listed == tool)
                })
            {
                return Err(ApiError::InvalidResponse);
            }
            if bot.tools.iter().any(|tool| !valid_speaker(tool))
                || bot.skills.iter().any(|skill| !valid_speaker(skill))
            {
                return Err(ApiError::InvalidResponse);
            }
            if bot.display_name.as_deref().is_some_and(|name| {
                name.trim().is_empty()
                    || name.chars().count() > 64
                    || name.chars().any(char::is_control)
            }) || bot.description.as_deref().is_some_and(|text| {
                text.chars().count() > 2_000
                    || text
                        .chars()
                        .any(|c| c != '\n' && c != '\r' && c != '\t' && c.is_control())
            }) || bot.avatar.as_deref().is_some_and(|avatar| {
                avatar.chars().count() > 16 || avatar.chars().any(char::is_control)
            }) || bot
                .model
                .as_deref()
                .is_some_and(|model| !valid_model_id(model))
            {
                return Err(ApiError::InvalidResponse);
            }
        }
        Ok(response.bots)
    }

    /// One page of conversation metadata. `cursor` is opaque and must come
    /// from a previous `next_cursor`; it is never guessed from UI state.
    pub fn list_conversations(
        &self,
        limit: usize,
        cursor: Option<&str>,
        include_archived: bool,
    ) -> Result<ConversationPage, ApiError> {
        if !(1..=200).contains(&limit) {
            return Err(ApiError::Validation(
                "Conversation page size must be 1–200.",
            ));
        }
        let limit_value = limit.to_string();
        let include_archived = if include_archived { "true" } else { "false" };
        let mut query = vec![
            ("limit", limit_value.as_str()),
            ("include_archived", include_archived),
        ];
        if let Some(cursor) = cursor {
            if !valid_cursor(cursor) {
                return Err(ApiError::Validation("Invalid conversation cursor."));
            }
            query.push(("cursor", cursor));
        }
        let page: ConversationPage = self.get(&["v1", "conversations"], &query)?;
        if page.conversations.len() > limit {
            return Err(ApiError::InvalidResponse);
        }
        for conversation in &page.conversations {
            check_conversation_summary(conversation)?;
        }
        if let Some(next) = &page.next_cursor {
            if !valid_cursor(next) {
                return Err(ApiError::InvalidResponse);
            }
        }
        Ok(page)
    }

    pub fn rename_conversation(
        &self,
        id: &str,
        title: &str,
    ) -> Result<ConversationSummary, ApiError> {
        check_id(id)?;
        let title = title.trim();
        if !valid_conversation_title(title) {
            return Err(ApiError::Validation(
                "Title must be 1–120 characters, without control characters.",
            ));
        }
        #[derive(Serialize)]
        struct Body<'a> {
            title: &'a str,
        }
        let response: ConversationSummaryEnvelope = self.request(
            Method::PATCH,
            &["v1", "conversations", id],
            &[],
            Some(&Body { title }),
        )?;
        check_conversation_summary(&response.conversation)?;
        if response.conversation.id != id {
            return Err(ApiError::InvalidResponse);
        }
        Ok(response.conversation)
    }

    /// Soft archive. Messages stay on the server.
    pub fn archive_conversation(&self, id: &str) -> Result<(), ApiError> {
        check_id(id)?;
        #[derive(Deserialize)]
        struct Body {
            archived: bool,
        }
        let body: Body =
            self.request::<Body, ()>(Method::DELETE, &["v1", "conversations", id], &[], None)?;
        if !body.archived {
            return Err(ApiError::InvalidResponse);
        }
        Ok(())
    }

    pub fn list_memories(&self, query: Option<&str>) -> Result<Vec<MemoryEntry>, ApiError> {
        let mut params = vec![("limit", "200")];
        if let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) {
            if query.chars().count() > 200 || query.chars().any(|c| c.is_control()) {
                return Err(ApiError::Validation(
                    "Search text must be at most 200 characters.",
                ));
            }
            params.push(("query", query));
        }
        let response: MemoriesEnvelope = self.get(&["v1", "memories"], &params)?;
        if response.memories.len() > 200 {
            return Err(ApiError::InvalidResponse);
        }
        let mut seen = HashSet::with_capacity(response.memories.len());
        for memory in &response.memories {
            check_memory(memory)?;
            if !seen.insert(memory.id.as_str()) {
                return Err(ApiError::InvalidResponse);
            }
        }
        Ok(response.memories)
    }

    pub fn add_memory(
        &self,
        kind: &str,
        content: &str,
        source: &str,
    ) -> Result<MemoryEntry, ApiError> {
        if !valid_memory_kind(kind) {
            return Err(ApiError::Validation(
                "Choose a memory kind: preference, fact, decision, context or note.",
            ));
        }
        if !valid_memory_content(content) {
            return Err(ApiError::Validation(
                "Memory must be 1–4,000 characters, without control characters.",
            ));
        }
        let source = source.trim();
        if !valid_memory_source(source) {
            return Err(ApiError::Validation(
                "Source must be at most 512 characters, without control characters.",
            ));
        }
        #[derive(Serialize)]
        struct Body<'a> {
            kind: &'a str,
            content: &'a str,
            source: &'a str,
        }
        let response: MemoryEnvelope = self.request(
            Method::POST,
            &["v1", "memories"],
            &[],
            Some(&Body {
                kind,
                content,
                source,
            }),
        )?;
        check_memory(&response.memory)?;
        if response.memory.kind != kind {
            return Err(ApiError::InvalidResponse);
        }
        Ok(response.memory)
    }

    pub fn delete_memory(&self, id: &str) -> Result<(), ApiError> {
        if !valid_memory_id(id) {
            return Err(ApiError::Validation("Invalid memory id."));
        }
        #[derive(Deserialize)]
        struct Body {
            deleted: String,
        }
        let body: Body =
            self.request::<Body, ()>(Method::DELETE, &["v1", "memories", id], &[], None)?;
        if body.deleted != id {
            return Err(ApiError::InvalidResponse);
        }
        Ok(())
    }

    pub fn providers(&self) -> Result<Vec<ProviderStatus>, ApiError> {
        let response: ProvidersEnvelope = self.get(&["v1", "providers"], &[])?;
        if response.providers.len() > 64 {
            return Err(ApiError::InvalidResponse);
        }
        let mut seen = HashSet::with_capacity(response.providers.len());
        for provider in &response.providers {
            if !bounded_text(&provider.id, 128)
                || !bounded_text(&provider.kind, 64)
                || !seen.insert(provider.id.as_str())
                || provider.models.len() > 256
                || provider
                    .models
                    .iter()
                    .any(|model| !bounded_text(model, 256))
                || provider.routing_classes.len() > 32
                || provider
                    .routing_classes
                    .iter()
                    .any(|class| !bounded_text(class, 64))
                || !valid_secret_map(&provider.secrets)
            {
                return Err(ApiError::InvalidResponse);
            }
        }
        Ok(response.providers)
    }

    pub fn connectors(&self) -> Result<Vec<ConnectorStatus>, ApiError> {
        let response: ConnectorsEnvelope = self.get(&["v1", "connectors"], &[])?;
        if response.connectors.len() > 64 {
            return Err(ApiError::InvalidResponse);
        }
        let mut seen = HashSet::with_capacity(response.connectors.len());
        for connector in &response.connectors {
            if !bounded_text(&connector.id, 128)
                || !bounded_text(&connector.kind, 32)
                || !seen.insert(connector.id.as_str())
                || connector
                    .redirect_uri
                    .as_deref()
                    .is_some_and(|uri| !bounded_text(uri, 2_048))
                || connector
                    .api_base
                    .as_deref()
                    .is_some_and(|base| !bounded_text(base, 2_048))
                || connector
                    .default_inbox_id
                    .as_deref()
                    .is_some_and(|inbox| !bounded_text(inbox, 256))
                || !valid_secret_map(&connector.secrets)
            {
                return Err(ApiError::InvalidResponse);
            }
        }
        Ok(response.connectors)
    }

    /// Admin writes carry a safe server-authored message on rejection so the
    /// console can say what to fix instead of only "422".
    fn admin_request<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        method: Method,
        route: &[&str],
        body: &B,
    ) -> Result<T, ApiError> {
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| ApiError::InvalidResponse)?
            .clear()
            .extend(route);
        let response = self
            .client
            .request(method, url)
            .bearer_auth(self.token.expose_secret())
            .json(body)
            .send()
            .map_err(|_| ApiError::Transport)?;
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
            // Keep auth and rate-limit statuses intact for the session's
            // token-rejection and back-off paths. Other server errors may have
            // applied the write, so they must stay "unknown".
            if !matches!(status.as_u16(), 404 | 409 | 422) {
                return Err(ApiError::Http(status.as_u16()));
            }
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if let Some(message) = value.get("error").and_then(|value| value.as_str()) {
                    if !message.is_empty()
                        && message.len() <= 512
                        && !message.chars().any(char::is_control)
                    {
                        return Err(ApiError::Rejected(message.to_owned()));
                    }
                }
            }
            return Err(ApiError::Http(status.as_u16()));
        }
        serde_json::from_slice(&bytes).map_err(|_| ApiError::InvalidResponse)
    }

    pub fn admin_add_provider(&self, request: &NewProvider) -> Result<AdminAck, ApiError> {
        if !valid_provider_id(&request.id)
            || request.models.is_empty()
            || request.models.len() > 256
            || request.models.iter().any(|model| !valid_model_id(model))
            || !valid_secret_name(&request.api_key_secret)
            || request.kind.is_empty()
            || request.kind.len() > 32
            || !request
                .kind
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || request.base_url.is_empty()
            || request.base_url.len() > 2_048
            || request
                .api_key
                .as_deref()
                .is_some_and(|key| !valid_credential(key))
        {
            return Err(ApiError::Validation(
                "Check the provider id, kind, base URL, models, secret name and key.",
            ));
        }
        self.admin_request(Method::POST, &["v1", "admin", "providers"], request)
    }

    pub fn admin_remove_provider(&self, id: &str) -> Result<AdminAck, ApiError> {
        if !valid_provider_id(id) {
            return Err(ApiError::Validation("Provider id is not valid."));
        }
        self.admin_request(
            Method::DELETE,
            &["v1", "admin", "providers", id],
            &serde_json::json!({}),
        )
    }

    pub fn admin_set_connector(
        &self,
        id: &str,
        update: &ConnectorUpdate,
    ) -> Result<AdminAck, ApiError> {
        if !valid_id(id) {
            return Err(ApiError::Validation("Connector id is not valid."));
        }
        for (role, value) in &update.values {
            if !matches!(
                role.as_str(),
                "client_id" | "client_secret" | "refresh_token" | "api_key"
            ) || !valid_credential(value)
            {
                return Err(ApiError::Validation(
                    "Check the connector credential values.",
                ));
            }
        }
        self.admin_request(Method::PUT, &["v1", "admin", "connectors", id], update)
    }

    pub fn admin_set_audio(&self, update: &AudioUpdate) -> Result<AdminAck, ApiError> {
        if update
            .transcription_model
            .as_deref()
            .is_some_and(|model| !valid_model_id(model))
            || update
                .understanding_model
                .as_deref()
                .is_some_and(|model| !valid_model_id(model))
        {
            return Err(ApiError::Validation("Check the audio model names."));
        }
        self.admin_request(Method::PUT, &["v1", "admin", "audio"], update)
    }

    pub fn admin_set_bot_profile(
        &self,
        id: &str,
        update: &BotProfileUpdate,
    ) -> Result<AdminAck, ApiError> {
        if !valid_speaker(id) {
            return Err(ApiError::Validation("Bot id is not valid."));
        }
        let name = update.display_name.trim();
        if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
            return Err(ApiError::Validation(
                "Bot display name must be 1–64 visible characters.",
            ));
        }
        if update.description.chars().count() > 2_000
            || update
                .description
                .chars()
                .any(|c| c != '\n' && c != '\r' && c != '\t' && c.is_control())
        {
            return Err(ApiError::Validation(
                "Bot description is too long or contains control characters.",
            ));
        }
        if update.avatar.chars().count() > 16 || update.avatar.chars().any(char::is_control) {
            return Err(ApiError::Validation(
                "Bot avatar must be a short label or emoji.",
            ));
        }
        let mut update = update.clone();
        update.display_name = name.to_owned();
        self.admin_request(Method::PUT, &["v1", "admin", "bots", id], &update)
    }

    pub fn admin_restart(&self) -> Result<AdminRestart, ApiError> {
        self.admin_request(
            Method::POST,
            &["v1", "admin", "restart"],
            &serde_json::json!({"confirm": true}),
        )
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

fn bounded_text(value: &str, max_chars: usize) -> bool {
    !value.is_empty() && value.chars().count() <= max_chars && !value.chars().any(char::is_control)
}

fn valid_cursor(cursor: &str) -> bool {
    !cursor.is_empty() && cursor.len() <= 256 && cursor.bytes().all(|byte| byte.is_ascii_graphic())
}

pub fn valid_conversation_title(title: &str) -> bool {
    let title = title.trim();
    !title.is_empty()
        && title.chars().count() <= MAX_CONVERSATION_TITLE_CHARS
        && !title.chars().any(char::is_control)
}

fn check_conversation_summary(conversation: &ConversationSummary) -> Result<(), ApiError> {
    if !valid_id(&conversation.id)
        || conversation
            .title
            .as_deref()
            .is_some_and(|title| !valid_conversation_title(title))
        || !bounded_text(&conversation.created_at, 64)
        || !bounded_text(&conversation.updated_at, 64)
        || conversation
            .last_message_at
            .as_deref()
            .is_some_and(|stamp| !bounded_text(stamp, 64))
        || conversation
            .archived_at
            .as_deref()
            .is_some_and(|stamp| !bounded_text(stamp, 64))
    {
        return Err(ApiError::InvalidResponse);
    }
    let parent = conversation.parent_id.as_deref();
    let valid = match conversation.kind.as_str() {
        "section" => {
            valid_conversation_parent(ConversationKind::Section, parent).is_ok()
                && !conversation.accepts_turns
        }
        "group" => {
            valid_conversation_parent(ConversationKind::Group, parent).is_ok()
                && !conversation.accepts_turns
        }
        "direct" => {
            valid_conversation_parent(ConversationKind::Direct, parent).is_ok()
                && conversation.accepts_turns
        }
        "room" => {
            valid_conversation_parent(ConversationKind::Room, parent).is_ok()
                && conversation.accepts_turns
        }
        // Legacy rows predate kinds and are direct chats.
        "legacy" => parent.is_none() && conversation.accepts_turns,
        // A newer server may add kinds. Accept them as opaque threads instead
        // of failing the whole directory.
        _ => parent.is_none_or(valid_id),
    };
    if valid {
        Ok(())
    } else {
        Err(ApiError::InvalidResponse)
    }
}

pub fn valid_memory_kind(kind: &str) -> bool {
    MEMORY_KINDS.contains(&kind)
}

pub fn valid_memory_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub fn valid_memory_content(content: &str) -> bool {
    let content = content.trim();
    !content.is_empty()
        && content.chars().count() <= MAX_MEMORY_CONTENT_CHARS
        && !content
            .chars()
            .any(|c| c != '\n' && c != '\r' && c != '\t' && c.is_control())
}

pub fn valid_memory_source(source: &str) -> bool {
    let source = source.trim();
    !source.is_empty()
        && source.chars().count() <= MAX_MEMORY_SOURCE_CHARS
        && !source.chars().any(char::is_control)
}

fn check_memory(memory: &MemoryEntry) -> Result<(), ApiError> {
    if !valid_memory_id(&memory.id)
        || !valid_memory_kind(&memory.kind)
        || !valid_memory_content(&memory.content)
        || !valid_memory_source(&memory.source)
        || !bounded_text(&memory.created_at, 64)
        || !bounded_text(&memory.updated_at, 64)
    {
        return Err(ApiError::InvalidResponse);
    }
    Ok(())
}

fn valid_secret_map(secrets: &BTreeMap<String, SecretStatus>) -> bool {
    secrets.len() <= 8
        && secrets.iter().all(|(key, secret)| {
            bounded_text(key, 64)
                && secret.name.as_deref().is_none_or(|name| {
                    name.len() <= 128
                        && !name.is_empty()
                        && name.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
                        })
                })
        })
}

fn check_task(task: Task) -> Result<Task, ApiError> {
    if valid_id(&task.id) {
        Ok(task)
    } else {
        Err(ApiError::InvalidResponse)
    }
}

fn bounded_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn safe_fleet_file(path: &str, id: &str, ext: &str) -> bool {
    if path.len() > 256
        || path.contains('\\')
        || path.contains(':')
        || path.contains("..")
        || path.starts_with('/')
        || path.contains("//")
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
    {
        return false;
    }
    let Some((dir, file)) = path.rsplit_once('/') else {
        return false;
    };
    !dir.is_empty() && file == format!("{id}.{ext}")
}

fn check_created_bot(bot: CreatedBot, id: &str) -> Result<CreatedBot, ApiError> {
    if bot.id != id
        || bot.enabled
        || bot.live
        || bot.role != "specialist"
        || bot.tools.len() > MAX_SCAFFOLD_TOOLS
        || bot.skills.len() > MAX_SCAFFOLD_SKILLS
        || bot.require_approval.len() > MAX_SCAFFOLD_TOOLS
        || !safe_fleet_file(&bot.bot_file, id, "toml")
        || !safe_fleet_file(&bot.prompt_file, id, "txt")
        || bot
            .tools
            .iter()
            .any(|tool| tool == "delegate" || !bounded_name(tool))
        || bot.skills.iter().any(|skill| !valid_skill_id(skill))
        || bot
            .require_approval
            .iter()
            .any(|tool| !bounded_name(tool) || !bot.tools.iter().any(|listed| listed == tool))
    {
        return Err(ApiError::InvalidResponse);
    }
    Ok(bot)
}

fn check_created_conversation(
    node: CreatedConversation,
    kind: ConversationKind,
    slug: &str,
    parent_id: Option<&str>,
) -> Result<CreatedConversation, ApiError> {
    let Some(expected_id) = conversation_node_id(kind, slug) else {
        return Err(ApiError::InvalidResponse);
    };
    if node.id != expected_id
        || node.kind != kind.as_str()
        || node.accepts_turns != kind.accepts_turns()
        || node.parent_id.as_deref() != parent_id
    {
        return Err(ApiError::InvalidResponse);
    }
    Ok(node)
}

fn bounded_skill_label(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 128
        && !value
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
}

fn safe_skill_file(path: &str, id: &str) -> bool {
    let nested = format!("/{id}/SKILL.md");
    let flat = format!("{id}/SKILL.md");
    (path.ends_with(&nested) || path == flat)
        && path.len() <= 256
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && !path.contains("..")
        && !path.contains("//")
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
}

fn check_created_skill(skill: CreatedSkill, id: &str) -> Result<CreatedSkill, ApiError> {
    if skill.id != id
        || skill.live
        || !bounded_skill_label(&skill.name)
        || skill.description.chars().count() > MAX_SKILL_DESCRIPTION_CHARS
        || skill.description.chars().any(|c| c.is_control())
        || !safe_skill_file(&skill.file, id)
    {
        return Err(ApiError::InvalidResponse);
    }
    Ok(skill)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(kind: &str, accepts_turns: bool) -> ConversationSummary {
        ConversationSummary {
            id: "windows-owner".into(),
            kind: kind.into(),
            parent_id: None,
            title: Some("Owner".into()),
            created_at: "2026-10-29T10:00:00.000Z".into(),
            updated_at: "2026-10-29T10:00:00.000Z".into(),
            last_message_at: None,
            archived_at: None,
            accepts_turns,
        }
    }

    #[test]
    fn admin_inputs_reject_reserved_ids_and_unsafe_values() {
        assert!(valid_provider_id("work"));
        assert!(!valid_provider_id("auto"));
        assert!(!valid_provider_id("default"));
        assert!(!valid_provider_id("work/../etc"));
        assert!(valid_secret_name("WORK_API_KEY"));
        assert!(!valid_secret_name("bad name"));
        assert!(valid_credential("synthetic-key-123"));
        assert!(!valid_credential("bad\nvalue"));
        assert!(valid_model_id("mistral-large-latest"));
        assert!(!valid_model_id("bad\nmodel"));
    }

    #[test]
    fn legacy_and_future_kinds_never_break_the_directory() {
        assert!(check_conversation_summary(&summary("legacy", true)).is_ok());
        assert!(check_conversation_summary(&summary("future-kind", true)).is_ok());
        assert!(check_conversation_summary(&summary("section", true)).is_err());
        assert!(check_conversation_summary(&summary("direct", false)).is_err());
        assert_eq!(summary("legacy", true).normalized_kind(), "direct");
    }
}
