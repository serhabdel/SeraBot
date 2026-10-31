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
    "memory_save",
    "memory_list",
    "memory_delete",
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
/// Cap for every string array on a bot detail or bot config update.
pub const MAX_BOT_CONFIG_ITEMS: usize = 64;
/// Bounds for the optional instructions blob of a bot detail.
const MAX_BOT_INSTRUCTIONS_CHARS: usize = 96_000;
const MAX_BOT_INSTRUCTIONS_BYTES: usize = 512_000;
/// Usage window accepted by `GET /v1/usage`.
pub const MAX_USAGE_DAYS: u32 = 365;

/// Maximum team members a room may carry. Mirrors the server limit.
pub const MAX_TEAM_MEMBERS: usize = 5;

/// Maximum audio body accepted by `POST /v1/audio/transcribe` (20 MiB).
pub const MAX_AUDIO_BYTES: usize = 20 * 1024 * 1024;

/// Audio extensions the server accepts. Mirrors the server allowlist.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "mpeg", "ogg", "oga", "opus", "m4a", "aac", "flac", "webm",
];

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

/// A team member bot id: a lowercase slug that is never `main` (the
/// orchestrator) and never a Windows device name.
pub fn valid_team_member(bot: &str) -> bool {
    valid_slug(bot) && !reserved_bot_id(bot)
}

/// Validate a team member list before it leaves the console: 1–5 unique bot
/// ids, lowercase, never `main`.
pub fn validate_team_members(members: &[String]) -> Result<(), ApiError> {
    if members.is_empty() || members.len() > MAX_TEAM_MEMBERS {
        return Err(ApiError::Validation(
            "A team needs between 1 and 5 members.",
        ));
    }
    let mut seen = HashSet::with_capacity(members.len());
    for member in members {
        if !valid_team_member(member) {
            return Err(ApiError::Validation(
                "Team members are lowercase bot ids such as analyst. main is the orchestrator and cannot be a member.",
            ));
        }
        if !seen.insert(member.as_str()) {
            return Err(ApiError::Validation(
                "Each bot can be in the team only once.",
            ));
        }
    }
    Ok(())
}

/// A room conversation id, the only kind that carries a team.
pub fn is_room_conversation(conversation: &str) -> bool {
    conversation.starts_with("room:")
}

/// A safe recording name: no path separators, no control characters, and an
/// extension the server accepts.
pub fn valid_audio_filename(name: &str) -> bool {
    if name.is_empty() || name.len() > 128 {
        return false;
    }
    if name
        .chars()
        .any(|c| c.is_control() || c == '/' || c == '\\' || c == ':')
    {
        return false;
    }
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() {
        return false;
    }
    AUDIO_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
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

/// Validate a bot config update before it leaves the console: arrays are
/// capped at [`MAX_BOT_CONFIG_ITEMS`], unique, and use the same id shapes as
/// the fleet list. Approval tools must be part of the tools when both are
/// sent together.
pub fn validate_bot_config_update(update: &BotConfigUpdate) -> Result<(), ApiError> {
    for items in [
        update.tools.as_deref(),
        update.skills.as_deref(),
        update.require_approval.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if items.len() > MAX_BOT_CONFIG_ITEMS {
            return Err(ApiError::Validation(
                "Each list can hold at most 64 entries.",
            ));
        }
        let mut seen = HashSet::with_capacity(items.len());
        for item in items {
            if !seen.insert(item.as_str()) {
                return Err(ApiError::Validation("Each entry can be listed once."));
            }
        }
    }
    if let Some(tools) = &update.tools {
        for tool in tools {
            if tool == "delegate" || !SPECIALIST_TOOLS.contains(&tool.as_str()) {
                return Err(ApiError::Validation(
                    "Choose tools from the specialist list.",
                ));
            }
        }
    }
    if let Some(skills) = &update.skills {
        for skill in skills {
            if !valid_skill_id(skill) {
                return Err(ApiError::Validation(
                    "Skill ids are lowercase slugs such as ask.",
                ));
            }
        }
    }
    if let Some(approval) = &update.require_approval {
        for tool in approval {
            if !valid_speaker(tool) {
                return Err(ApiError::Validation(
                    "Approval tools are tool names from the specialist list.",
                ));
            }
        }
        if let Some(tools) = &update.tools
            && approval
                .iter()
                .any(|tool| !tools.iter().any(|listed| listed == tool))
        {
            return Err(ApiError::Validation(
                "Approval tools must be part of the bot's tools.",
            ));
        }
    }
    Ok(())
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
            Self::Group => "Folder",
            Self::Direct => "Chat",
            Self::Room => "Group chat",
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
            if let Some(parent) = parent.filter(|parent| !parent.is_empty())
                && !scoped_parent(parent, "sec")
                && !scoped_parent(parent, "grp")
            {
                return Err(ApiError::Validation(
                    "Direct and room nodes sit under a section (sec:) or group (grp:).",
                ));
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

/// Full bot record from `GET /v1/bots/{id}`: profile, routing, instructions
/// and counters. Nullable fields and arrays default so an older server that
/// omits them still parses.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct BotDetail {
    pub id: String,
    pub display_name: String,
    pub role: String,
    pub description: String,
    #[serde(default)]
    pub avatar: String,
    pub enabled: bool,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub fallback_provider: Option<String>,
    #[serde(default)]
    pub fallback_model: Option<String>,
    #[serde(default)]
    pub routing_classes: Vec<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub instructions_bytes: usize,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub require_approval: Vec<String>,
    #[serde(default)]
    pub memory_count: i64,
    #[serde(default)]
    pub artifact_count: i64,
}

/// Body of `PUT /v1/admin/bots/{id}/config`. Every field is optional; the
/// server keeps the current value when a field is absent.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct BotConfigUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_approval: Option<Vec<String>>,
}

/// Token and call counters of one usage slice.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct UsageTotals {
    #[serde(default)]
    pub calls: i64,
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub completion_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
}

/// One row of a usage breakdown: `day` on a `per_day` row, `bot` on a
/// `per_bot` row, plus the counters.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct UsageBucket {
    #[serde(default)]
    pub day: Option<String>,
    #[serde(default)]
    pub bot: Option<String>,
    #[serde(default)]
    pub calls: i64,
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub completion_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
}

/// Report of `GET /v1/usage?days=&bot=&conversation_id=`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct UsageReport {
    #[serde(default)]
    pub totals: UsageTotals,
    #[serde(default)]
    pub per_day: Vec<UsageBucket>,
    #[serde(default)]
    pub per_bot: Vec<UsageBucket>,
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
    #[serde(default)]
    pub unread_count: i64,
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
    /// `oauth` for providers that sign in through a browser, else `api_key`.
    #[serde(default)]
    pub auth: String,
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
    /// Current auth mode ("api_key" / "oauth"), when the connector has one.
    #[serde(default)]
    pub auth: Option<String>,
    #[serde(default)]
    pub auth_modes: Vec<String>,
    /// OAuth sign-in details when the server offers them.
    #[serde(default)]
    pub oauth: Option<ConnectorOauth>,
}

/// OAuth sign-in metadata for a connector (ElevenLabs CIMD flow).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ConnectorOauth {
    pub client_id: String,
    #[serde(default)]
    pub authorize_url: Option<String>,
    #[serde(default)]
    pub cimd_path: Option<String>,
    #[serde(default)]
    pub callback_uri: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
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
    /// ElevenLabs only: "api_key" or "oauth".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
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

/// First leg of `POST /v1/admin/providers/{id}/oauth/start`.
#[derive(Clone, Debug, Deserialize)]
pub struct OAuthStart {
    pub flow: String,
    pub url: String,
    /// One-time code the owner types on the provider page (device flow only).
    #[serde(default)]
    pub code: Option<String>,
}

/// Result of `POST /v1/admin/connectors/{id}/test`: a bounded, read-only probe.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ConnectorTestResult {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub message: String,
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

/// Live sandbox posture from `GET /v1/status`.
///
/// Every field is optional on purpose: an older server, or a count the
/// server could not read, must read as "unknown" in the console instead of
/// being invented.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ServerStatus {
    #[serde(default)]
    pub ok: bool,
    /// Pending task count. `-1` means the server could not read it.
    #[serde(default)]
    pub pending_tasks: Option<i64>,
    #[serde(default)]
    pub pending_approvals: Option<i64>,
    #[serde(default)]
    pub approvals: Vec<StatusApproval>,
    #[serde(default)]
    pub policy_version: Option<String>,
}

/// One pending approval as reported by `GET /v1/status`.
#[derive(Clone, Debug, Deserialize)]
pub struct StatusApproval {
    pub id: String,
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub action_id: Option<String>,
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

/// How a room picks the bots that answer. `selected` leaves the choice to the
/// orchestrator; `all` sends every owner message to every team member.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TeamMode {
    Selected,
    All,
}

impl TeamMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::All => "all",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Selected => "Selected",
            Self::All => "All members",
        }
    }
}

/// One member of a room team. `live` is the server's view of the bot.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct TeamMember {
    pub bot: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub live: bool,
}

/// The team of a `room:` conversation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Team {
    pub conversation_id: String,
    pub mode: TeamMode,
    #[serde(default)]
    pub members: Vec<TeamMember>,
    /// Group instructions written by the owner; injected into room turns.
    #[serde(default)]
    pub rules: String,
}

/// One artifact row from `GET /v1/conversations/{id}/library`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct LibraryEntry {
    pub action_id: String,
    pub tool: String,
    pub hash: String,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    pub created_at: String,
}

/// Transcript of one recorded audio note. The text is model output: untrusted
/// content that the owner reviews before it is sent.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Transcription {
    pub text: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub bytes: usize,
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
pub(crate) struct ConnectorsEnvelope {
    pub(crate) connectors: Vec<ConnectorStatus>,
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
    /// The live sandbox posture: pending work, pending approvals and the
    /// policy version the server enforces right now.
    pub fn status(&self) -> Result<ServerStatus, ApiError> {
        self.get(&["v1", "status"], &[])
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
        if let Some(body) = body
            && !valid_skill_body(body)
        {
            return Err(ApiError::Validation(
                "Body must be markdown of at most 96,000 bytes, without control characters.",
            ));
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

    /// Full record of one bot: `GET /v1/bots/{id}`.
    pub fn bot_detail(&self, id: &str) -> Result<BotDetail, ApiError> {
        if !valid_speaker(id) {
            return Err(ApiError::Validation("Bot id is not valid."));
        }
        let detail: BotDetail = self.get(&["v1", "bots", id], &[])?;
        let detail = check_bot_detail(detail)?;
        if detail.id != id {
            return Err(ApiError::InvalidResponse);
        }
        Ok(detail)
    }

    /// Replace tools, skills or approval requirements of one bot:
    /// `PUT /v1/admin/bots/{id}/config`. A write: an unknown outcome is
    /// never replayed.
    pub fn update_bot_config(
        &self,
        id: &str,
        update: &BotConfigUpdate,
    ) -> Result<AdminAck, ApiError> {
        if !valid_speaker(id) {
            return Err(ApiError::Validation("Bot id is not valid."));
        }
        validate_bot_config_update(update)?;
        self.admin_request(Method::PUT, &["v1", "admin", "bots", id, "config"], update)
    }

    /// Token usage report: `GET /v1/usage?days=&bot=&conversation_id=`.
    /// The filters are optional; `days` is the only required parameter.
    pub fn usage(
        &self,
        days: u32,
        bot: Option<&str>,
        conversation_id: Option<&str>,
    ) -> Result<UsageReport, ApiError> {
        if !(1..=MAX_USAGE_DAYS).contains(&days) {
            return Err(ApiError::Validation("Usage window must be 1–365 days."));
        }
        if bot.is_some_and(|bot| !valid_speaker(bot)) {
            return Err(ApiError::Validation("The bot filter must be a bot id."));
        }
        if conversation_id.is_some_and(|id| !valid_id(id)) {
            return Err(ApiError::Validation("Conversation id is not valid."));
        }
        let days_value = days.to_string();
        let mut query = vec![("days", days_value.as_str())];
        if let Some(bot) = bot {
            query.push(("bot", bot));
        }
        if let Some(conversation_id) = conversation_id {
            query.push(("conversation_id", conversation_id));
        }
        let report: UsageReport = self.get(&["v1", "usage"], &query)?;
        check_usage_report(report)
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
        if let Some(next) = &page.next_cursor
            && !valid_cursor(next)
        {
            return Err(ApiError::InvalidResponse);
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

    /// Restore an archived conversation to the rail.
    pub fn unarchive_conversation(&self, id: &str) -> Result<(), ApiError> {
        check_id(id)?;
        #[derive(Deserialize)]
        struct Body {
            archived: bool,
        }
        let body: Body = self.request::<Body, ()>(
            Method::POST,
            &["v1", "conversations", id, "unarchive"],
            &[],
            None,
        )?;
        if body.archived {
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
        Self::normalize_and_validate_connectors(response)
    }

    pub(crate) fn normalize_and_validate_connectors(
        mut response: ConnectorsEnvelope,
    ) -> Result<Vec<ConnectorStatus>, ApiError> {
        if response.connectors.len() > 64 {
            return Err(ApiError::InvalidResponse);
        }
        for connector in &mut response.connectors {
            if connector
                .redirect_uri
                .as_deref()
                .is_some_and(|s| s.trim().is_empty())
            {
                connector.redirect_uri = None;
            }
            if connector
                .api_base
                .as_deref()
                .is_some_and(|s| s.trim().is_empty())
            {
                connector.api_base = None;
            }
            if connector
                .default_inbox_id
                .as_deref()
                .is_some_and(|s| s.trim().is_empty())
            {
                connector.default_inbox_id = None;
            }
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

    /// Read the team of a room conversation.
    pub fn get_team(&self, conversation: &str) -> Result<Team, ApiError> {
        check_id(conversation)?;
        check_room(conversation)?;
        self.get(&["v1", "conversations", conversation, "team"], &[])
    }

    /// Replace the team of a room conversation. The call is a write: when the
    /// outcome is unknown it must never be replayed.
    pub fn set_team(
        &self,
        conversation: &str,
        members: &[String],
        mode: TeamMode,
        rules: &str,
    ) -> Result<Team, ApiError> {
        check_id(conversation)?;
        check_room(conversation)?;
        validate_team_members(members)?;
        if rules.chars().count() > 4_000
            || rules
                .chars()
                .any(|c| c != '\n' && c != '\r' && c != '\t' && c.is_control())
        {
            return Err(ApiError::Validation(
                "Group instructions must be at most 4,000 characters, without control characters.",
            ));
        }
        #[derive(Serialize)]
        struct Body<'a> {
            members: &'a [String],
            mode: TeamMode,
            rules: &'a str,
        }
        self.request(
            Method::PUT,
            &["v1", "conversations", conversation, "team"],
            &[],
            Some(&Body {
                members,
                mode,
                rules,
            }),
        )
    }

    /// The artifact library of any conversation: files the gateway produced
    /// inside its turns, newest first.
    pub fn library(&self, conversation: &str) -> Result<Vec<LibraryEntry>, ApiError> {
        check_id(conversation)?;
        #[derive(Deserialize)]
        struct Envelope {
            artifacts: Vec<LibraryEntry>,
        }
        let response: Envelope =
            self.get(&["v1", "conversations", conversation, "library"], &[])?;
        if response.artifacts.len() > 100 {
            return Err(ApiError::InvalidResponse);
        }
        for entry in &response.artifacts {
            let hash_ok =
                entry.hash.len() == 64 && entry.hash.bytes().all(|byte| byte.is_ascii_hexdigit());
            if !hash_ok
                || !bounded_text(&entry.tool, 128)
                || !bounded_text(&entry.created_at, 64)
                || entry
                    .filename
                    .as_deref()
                    .is_some_and(|name| !bounded_text(name, 256))
                || entry
                    .title
                    .as_deref()
                    .is_some_and(|title| !bounded_text(title, 256))
                || !valid_id(&entry.action_id)
            {
                return Err(ApiError::InvalidResponse);
            }
        }
        Ok(response.artifacts)
    }

    /// Transcribe one recorded audio note. The body is the raw recording; the
    /// transcript is model output and stays untrusted until the owner sends it.
    pub fn transcribe_audio(
        &self,
        filename: &str,
        bytes: &[u8],
    ) -> Result<Transcription, ApiError> {
        if bytes.is_empty() {
            return Err(ApiError::Validation("The recording is empty."));
        }
        if bytes.len() > MAX_AUDIO_BYTES {
            return Err(ApiError::Validation(
                "The recording is larger than the 20 MiB limit.",
            ));
        }
        if !valid_audio_filename(filename) {
            return Err(ApiError::Validation(
                "Recordings must be wav, mp3, mpeg, ogg, oga, opus, m4a, aac, flac or webm files.",
            ));
        }
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| ApiError::InvalidResponse)?
            .clear()
            .extend(["v1", "audio", "transcribe"]);
        let response = self
            .client
            .post(url)
            .bearer_auth(self.token.expose_secret())
            .header("X-Audio-Filename", filename)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(bytes.to_vec())
            .send()
            .map_err(|_| ApiError::Transport)?;
        let status = response.status();
        let mut body = Zeroizing::new(Vec::new());
        response
            .take(MAX_RESPONSE + 1)
            .read_to_end(&mut body)
            .map_err(|_| ApiError::Transport)?;
        if body.len() as u64 > MAX_RESPONSE {
            return Err(ApiError::TooLarge);
        }
        if !status.is_success() {
            // Keep a safe server-authored message for the owner-facing errors
            // (for example "audio is not configured"). Other statuses stay
            // numeric so the session keeps its token and back-off paths.
            if matches!(status.as_u16(), 400 | 404 | 409 | 422 | 503)
                && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body)
                && let Some(message) = value.get("error").and_then(|value| value.as_str())
                && !message.is_empty()
                && message.len() <= 512
                && !message.chars().any(char::is_control)
            {
                return Err(ApiError::Rejected(message.to_owned()));
            }
            return Err(ApiError::Http(status.as_u16()));
        }
        serde_json::from_slice(&body).map_err(|_| ApiError::InvalidResponse)
    }

    /// Download one artifact by content hash. Returns the server filename and
    /// the raw bytes (server cap: 100 MiB).
    pub fn artifact(&self, hash: &str) -> Result<(String, Vec<u8>), ApiError> {
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ApiError::Validation("Invalid artifact hash."));
        }
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| ApiError::InvalidResponse)?
            .clear()
            .extend(["v1", "artifacts", hash]);
        let response = self
            .client
            .get(url)
            .bearer_auth(self.token.expose_secret())
            .send()
            .map_err(|_| ApiError::Transport)?;
        let status = response.status();
        let filename = response
            .headers()
            .get("content-disposition")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split("filename=\"").nth(1))
            .and_then(|value| value.split('"').next())
            .map(str::trim)
            .filter(|name| !name.is_empty() && name.chars().count() <= 256)
            .map(str::to_owned);
        const MAX_ARTIFACT: u64 = 100 * 1024 * 1024;
        let mut bytes = Zeroizing::new(Vec::new());
        response
            .take(MAX_ARTIFACT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ApiError::Transport)?;
        if bytes.len() as u64 > MAX_ARTIFACT {
            return Err(ApiError::TooLarge);
        }
        if !status.is_success() {
            return Err(ApiError::Http(status.as_u16()));
        }
        let filename = filename.unwrap_or_else(|| format!("{hash}.pdf"));
        Ok((filename, bytes.to_vec()))
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
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && let Some(message) = value.get("error").and_then(|value| value.as_str())
                && !message.is_empty()
                && message.len() <= 512
                && !message.chars().any(char::is_control)
            {
                return Err(ApiError::Rejected(message.to_owned()));
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

    fn checked_oauth_start(
        &self,
        path: &[&str],
        body: &serde_json::Value,
    ) -> Result<OAuthStart, ApiError> {
        let start: OAuthStart = self.admin_request(Method::POST, path, body)?;
        // The URL comes from the server: open only a plain https page, and
        // keep the one-time code to printable text.
        let page = Url::parse(&start.url).map_err(|_| ApiError::InvalidResponse)?;
        if page.scheme() != "https"
            || page.host_str().is_none()
            || !page.username().is_empty()
            || page.password().is_some()
            || !matches!(start.flow.as_str(), "pkce" | "device")
            || start.code.as_deref().is_some_and(|code| {
                code.is_empty() || code.len() > 32 || code.chars().any(char::is_control)
            })
        {
            return Err(ApiError::InvalidResponse);
        }
        Ok(start)
    }

    fn checked_oauth_complete(
        &self,
        path: &[&str],
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<AdminAck, ApiError> {
        self.admin_request(
            Method::POST,
            path,
            &serde_json::json!({
                "code": code,
                "verifier": verifier,
                "redirect_uri": redirect_uri,
            }),
        )
    }

    pub fn admin_oauth_start(
        &self,
        id: &str,
        body: &serde_json::Value,
    ) -> Result<OAuthStart, ApiError> {
        if !valid_id(id) {
            return Err(ApiError::Validation("Provider id is not valid."));
        }
        self.checked_oauth_start(&["v1", "admin", "providers", id, "oauth", "start"], body)
    }

    pub fn admin_oauth_complete(
        &self,
        id: &str,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<AdminAck, ApiError> {
        if !valid_id(id) {
            return Err(ApiError::Validation("Provider id is not valid."));
        }
        self.checked_oauth_complete(
            &["v1", "admin", "providers", id, "oauth", "complete"],
            code,
            verifier,
            redirect_uri,
        )
    }

    pub fn admin_connector_oauth_start(
        &self,
        id: &str,
        body: &serde_json::Value,
    ) -> Result<OAuthStart, ApiError> {
        if !valid_id(id) {
            return Err(ApiError::Validation("Connector id is not valid."));
        }
        self.checked_oauth_start(&["v1", "admin", "connectors", id, "oauth", "start"], body)
    }

    pub fn admin_connector_oauth_complete(
        &self,
        id: &str,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<AdminAck, ApiError> {
        if !valid_id(id) {
            return Err(ApiError::Validation("Connector id is not valid."));
        }
        self.checked_oauth_complete(
            &["v1", "admin", "connectors", id, "oauth", "complete"],
            code,
            verifier,
            redirect_uri,
        )
    }

    pub fn admin_test_connector(&self, id: &str) -> Result<ConnectorTestResult, ApiError> {
        if !valid_id(id) {
            return Err(ApiError::Validation("Connector id is not valid."));
        }
        let mut result: ConnectorTestResult = self.admin_request(
            Method::POST,
            &["v1", "admin", "connectors", id, "test"],
            &serde_json::json!({}),
        )?;
        result.id = id.to_owned();
        result.message.truncate(256);
        result.message.retain(|c| !c.is_control());
        Ok(result)
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

fn check_room(conversation: &str) -> Result<(), ApiError> {
    if is_room_conversation(conversation) {
        Ok(())
    } else {
        Err(ApiError::Validation(
            "Only room conversations carry a team.",
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
        || conversation.unread_count < 0
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

fn check_bot_detail(detail: BotDetail) -> Result<BotDetail, ApiError> {
    let bad = !valid_speaker(&detail.id)
        || detail.display_name.is_empty()
        || detail.display_name.chars().count() > 64
        || detail.display_name.chars().any(char::is_control)
        || !valid_speaker(&detail.role)
        || detail.description.chars().count() > MAX_BOT_DESCRIPTION_CHARS
        || detail
            .description
            .chars()
            .any(|c| c != '\n' && c != '\r' && c != '\t' && c.is_control())
        || detail.avatar.chars().count() > 16
        || detail.avatar.chars().any(char::is_control)
        || !bounded_text(&detail.provider, 64)
        || (!detail.model.is_empty() && !valid_model_id(&detail.model))
        || detail
            .fallback_provider
            .as_deref()
            .is_some_and(|provider| !bounded_text(provider, 64))
        || detail
            .fallback_model
            .as_deref()
            .is_some_and(|model| !valid_model_id(model))
        || detail.routing_classes.len() > MAX_BOT_CONFIG_ITEMS
        || detail
            .routing_classes
            .iter()
            .any(|class| !bounded_text(class, 64))
        || detail.instructions.as_deref().is_some_and(|text| {
            text.chars().count() > MAX_BOT_INSTRUCTIONS_CHARS
                || text
                    .chars()
                    .any(|c| c != '\n' && c != '\r' && c != '\t' && c.is_control())
        })
        || detail.instructions_bytes > MAX_BOT_INSTRUCTIONS_BYTES
        || detail.skills.len() > MAX_BOT_CONFIG_ITEMS
        || detail.tools.len() > MAX_BOT_CONFIG_ITEMS
        || detail.require_approval.len() > MAX_BOT_CONFIG_ITEMS
        || detail.tools.iter().any(|tool| !valid_speaker(tool))
        || detail.skills.iter().any(|skill| !valid_speaker(skill))
        || detail
            .require_approval
            .iter()
            .any(|tool| !valid_speaker(tool) || !detail.tools.iter().any(|listed| listed == tool))
        || detail.memory_count < 0
        || detail.artifact_count < 0;
    if bad {
        return Err(ApiError::InvalidResponse);
    }
    Ok(detail)
}

fn usage_counts_ok(
    calls: i64,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
) -> bool {
    calls >= 0 && prompt_tokens >= 0 && completion_tokens >= 0 && total_tokens >= 0
}

fn check_usage_report(report: UsageReport) -> Result<UsageReport, ApiError> {
    let totals_ok = usage_counts_ok(
        report.totals.calls,
        report.totals.prompt_tokens,
        report.totals.completion_tokens,
        report.totals.total_tokens,
    );
    let bad = !totals_ok
        || report.per_day.len() > MAX_USAGE_DAYS as usize + 1
        || report.per_bot.len() > MAX_BOT_CONFIG_ITEMS
        || report.per_day.iter().any(|bucket| {
            !usage_counts_ok(
                bucket.calls,
                bucket.prompt_tokens,
                bucket.completion_tokens,
                bucket.total_tokens,
            ) || bucket
                .day
                .as_deref()
                .is_none_or(|day| !bounded_text(day, 16))
                || bucket.bot.is_some()
        })
        || report.per_bot.iter().any(|bucket| {
            !usage_counts_ok(
                bucket.calls,
                bucket.prompt_tokens,
                bucket.completion_tokens,
                bucket.total_tokens,
            ) || bucket.bot.as_deref().is_none_or(|bot| !valid_speaker(bot))
                || bucket.day.is_some()
        });
    if bad {
        return Err(ApiError::InvalidResponse);
    }
    Ok(report)
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
            unread_count: 0,
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

    #[test]
    fn team_members_stay_on_the_safe_subset() {
        assert!(valid_team_member("analyst"));
        assert!(valid_team_member("maker-2"));
        assert!(!valid_team_member("main"));
        assert!(!valid_team_member(""));
        assert!(!valid_team_member("Analyst"));
        assert!(!valid_team_member("../analyst"));
        assert!(!valid_team_member("con"));

        assert!(validate_team_members(&["analyst".into()]).is_ok());
        assert!(
            validate_team_members(&["analyst".into(), "maker".into(), "watcher".into()]).is_ok()
        );
        assert!(validate_team_members(&[]).is_err());
        assert!(validate_team_members(&["main".into()]).is_err());
        assert!(validate_team_members(&["analyst".into(), "analyst".into()]).is_err());
        assert!(validate_team_members(&["Analyst".into()]).is_err());
        assert!(
            validate_team_members(&[
                "a".into(),
                "b".into(),
                "c".into(),
                "d".into(),
                "e".into(),
                "f".into()
            ])
            .is_err()
        );
    }

    #[test]
    fn team_mode_round_trips_the_server_wire_values() {
        assert_eq!(
            serde_json::to_string(&TeamMode::Selected).unwrap(),
            "\"selected\""
        );
        assert_eq!(serde_json::to_string(&TeamMode::All).unwrap(), "\"all\"");
        assert_eq!(
            serde_json::from_str::<TeamMode>("\"selected\"").unwrap(),
            TeamMode::Selected
        );
        assert_eq!(
            serde_json::from_str::<TeamMode>("\"all\"").unwrap(),
            TeamMode::All
        );
        assert!(serde_json::from_str::<TeamMode>("\"everyone\"").is_err());
        assert_eq!(TeamMode::Selected.as_str(), "selected");
        assert_eq!(TeamMode::All.as_str(), "all");
    }

    #[test]
    fn team_reads_the_server_shape_including_a_member_that_is_not_live() {
        let team: Team = serde_json::from_str(
            r#"{"conversation_id":"room:review",
                "mode":"selected",
                "members":[{"bot":"analyst","role":"specialist","enabled":true,"live":true}],
                "max_members":5}"#,
        )
        .unwrap();
        assert_eq!(team.conversation_id, "room:review");
        assert_eq!(team.mode, TeamMode::Selected);
        assert_eq!(team.members.len(), 1);
        assert_eq!(team.members[0].bot, "analyst");
        assert_eq!(team.members[0].role, "specialist");
        assert!(team.members[0].enabled);
        assert!(team.members[0].live);

        let offline: Team = serde_json::from_str(
            r#"{"conversation_id":"room:review",
                "mode":"all",
                "members":[{"bot":"maker","role":"specialist","enabled":true,"live":false}]}"#,
        )
        .unwrap();
        assert_eq!(offline.mode, TeamMode::All);
        assert!(!offline.members[0].live);
        assert!(offline.members[0].enabled);
        assert_eq!(offline.rules, "");

        let with_rules: Team = serde_json::from_str(
            r#"{"conversation_id":"room:review",
                "mode":"all","rules":"Cite sources.",
                "members":[{"bot":"maker","role":"specialist","enabled":true,"live":false}]}"#,
        )
        .unwrap();
        assert_eq!(with_rules.rules, "Cite sources.");
    }

    #[test]
    fn library_reads_the_server_shape() {
        #[derive(Deserialize)]
        struct Env {
            artifacts: Vec<LibraryEntry>,
        }
        let parsed: Env = serde_json::from_str(
            r#"{"artifacts":[{"action_id":"act-1","tool":"deck_render",
                "hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "filename":"deck.pdf","title":"Q3 Deck","created_at":"2026-10-29T10:00:00.000Z"}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.artifacts.len(), 1);
        assert_eq!(parsed.artifacts[0].tool, "deck_render");
        assert_eq!(parsed.artifacts[0].filename.as_deref(), Some("deck.pdf"));
    }

    #[test]
    fn transcription_reads_the_server_shape() {
        let transcription: Transcription =
            serde_json::from_str(r#"{"text":"hello there","filename":"note.webm","bytes":1234}"#)
                .unwrap();
        assert_eq!(transcription.text, "hello there");
        assert_eq!(transcription.filename, "note.webm");
        assert_eq!(transcription.bytes, 1234);
    }

    #[test]
    fn recording_names_are_bounded_and_use_the_server_allowlist() {
        assert!(valid_audio_filename("note.webm"));
        assert!(valid_audio_filename("note.WEBM"));
        assert!(valid_audio_filename("note.m4a"));
        assert!(!valid_audio_filename("note.txt"));
        assert!(!valid_audio_filename("note"));
        assert!(!valid_audio_filename("../note.webm"));
        assert!(!valid_audio_filename("notes/note.webm"));
        assert!(!valid_audio_filename("note.webm\r\n"));
        assert!(!valid_audio_filename(""));
    }

    #[test]
    fn bot_detail_reads_the_server_shape_and_defaults_optional_fields() {
        let detail: BotDetail = serde_json::from_str(
            r#"{
                "id":"analyst",
                "display_name":"Analyst",
                "role":"specialist",
                "description":"Numbers and citations.",
                "avatar":"A",
                "enabled":true,
                "provider":"mistral",
                "model":"mistral-large-latest",
                "fallback_provider":"openai",
                "fallback_model":"gpt-5",
                "routing_classes":["code","finance"],
                "instructions":"Be precise.\nCite sources.",
                "instructions_bytes":24,
                "skills":["ask"],
                "tools":["rag_search","xlsx_analyze"],
                "require_approval":["xlsx_analyze"],
                "memory_count":3,
                "artifact_count":1
            }"#,
        )
        .unwrap();
        assert_eq!(detail.id, "analyst");
        assert_eq!(detail.fallback_model.as_deref(), Some("gpt-5"));
        assert_eq!(detail.routing_classes.len(), 2);
        assert_eq!(detail.instructions_bytes, 24);
        assert!(check_bot_detail(detail).is_ok());

        // An older server that omits the optional fields still parses.
        let minimal: BotDetail = serde_json::from_str(
            r#"{"id":"main","display_name":"Main","role":"orchestrator",
                "description":"","avatar":"","enabled":true,
                "provider":"auto","model":""}"#,
        )
        .unwrap();
        assert_eq!(minimal.fallback_provider, None);
        assert_eq!(minimal.instructions, None);
        assert!(minimal.routing_classes.is_empty());
        assert_eq!(minimal.memory_count, 0);
        assert!(check_bot_detail(minimal).is_ok());
    }

    #[test]
    fn bot_detail_rejects_unsafe_server_values() {
        let base = serde_json::json!({
            "id":"analyst","display_name":"Analyst","role":"specialist",
            "description":"ok","avatar":"A","enabled":true,
            "provider":"mistral","model":"mistral-large-latest"
        });
        let parse = |value: serde_json::Value| {
            check_bot_detail(serde_json::from_value::<BotDetail>(value).unwrap())
        };
        assert!(parse(base.clone()).is_ok());

        let mut wrong_id = base.clone();
        wrong_id["id"] = serde_json::json!("../analyst");
        assert!(parse(wrong_id).is_err());

        let mut control_name = base.clone();
        control_name["display_name"] = serde_json::json!("Bad\u{0}name");
        assert!(parse(control_name).is_err());

        let mut approval_outside = base.clone();
        approval_outside["tools"] = serde_json::json!(["rag_search"]);
        approval_outside["require_approval"] = serde_json::json!(["deck_build"]);
        assert!(parse(approval_outside).is_err());

        let mut too_many = base.clone();
        too_many["tools"] =
            serde_json::json!((0..65).map(|i| format!("tool{i}")).collect::<Vec<_>>());
        assert!(parse(too_many).is_err());

        let mut negative = base.clone();
        negative["memory_count"] = serde_json::json!(-1);
        assert!(parse(negative).is_err());
    }

    #[test]
    fn bot_config_update_validates_and_serializes_only_present_fields() {
        let update = BotConfigUpdate {
            tools: Some(vec!["rag_search".into(), "xlsx_analyze".into()]),
            skills: None,
            require_approval: Some(vec!["xlsx_analyze".into()]),
        };
        assert!(validate_bot_config_update(&update).is_ok());
        assert_eq!(
            serde_json::to_value(&update).unwrap(),
            serde_json::json!({
                "tools": ["rag_search", "xlsx_analyze"],
                "require_approval": ["xlsx_analyze"]
            })
        );

        // An empty update is valid and serializes to an empty object.
        let empty = BotConfigUpdate::default();
        assert!(validate_bot_config_update(&empty).is_ok());
        assert_eq!(serde_json::to_value(&empty).unwrap(), serde_json::json!({}));

        let delegate = BotConfigUpdate {
            tools: Some(vec!["delegate".into()]),
            ..BotConfigUpdate::default()
        };
        assert!(validate_bot_config_update(&delegate).is_err());

        let unknown_tool = BotConfigUpdate {
            tools: Some(vec!["not-a-tool".into()]),
            ..BotConfigUpdate::default()
        };
        assert!(validate_bot_config_update(&unknown_tool).is_err());

        let duplicate = BotConfigUpdate {
            skills: Some(vec!["ask".into(), "ask".into()]),
            ..BotConfigUpdate::default()
        };
        assert!(validate_bot_config_update(&duplicate).is_err());

        let too_many = BotConfigUpdate {
            skills: Some((0..65).map(|i| format!("skill-{i}")).collect()),
            ..BotConfigUpdate::default()
        };
        assert!(validate_bot_config_update(&too_many).is_err());

        let bad_skill = BotConfigUpdate {
            skills: Some(vec!["Not A Skill".into()]),
            ..BotConfigUpdate::default()
        };
        assert!(validate_bot_config_update(&bad_skill).is_err());

        // Approval tools must be a subset of the tools sent in the same update.
        let outside = BotConfigUpdate {
            tools: Some(vec!["rag_search".into()]),
            require_approval: Some(vec!["deck_build".into()]),
            ..BotConfigUpdate::default()
        };
        assert!(validate_bot_config_update(&outside).is_err());
    }

    #[test]
    fn usage_report_reads_the_server_shape_and_rejects_bad_rows() {
        let report: UsageReport = serde_json::from_str(
            r#"{
                "totals":{"calls":10,"prompt_tokens":1000,"completion_tokens":500,"total_tokens":1500},
                "per_day":[{"day":"2026-10-30","prompt_tokens":600,"completion_tokens":300,
                            "total_tokens":900,"calls":6}],
                "per_bot":[{"bot":"analyst","prompt_tokens":400,"completion_tokens":200,
                            "total_tokens":600,"calls":4}]
            }"#,
        )
        .unwrap();
        assert_eq!(report.totals.calls, 10);
        assert_eq!(report.totals.total_tokens, 1500);
        assert_eq!(report.per_day[0].day.as_deref(), Some("2026-10-30"));
        assert_eq!(report.per_bot[0].bot.as_deref(), Some("analyst"));
        assert!(check_usage_report(report).is_ok());

        // Missing optional sections default to empty.
        let totals_only: UsageReport = serde_json::from_str(
            r#"{"totals":{"calls":1,"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}"#,
        )
        .unwrap();
        assert!(totals_only.per_day.is_empty());
        assert!(check_usage_report(totals_only).is_ok());

        let negative = serde_json::json!({
            "totals":{"calls":-1,"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}
        });
        assert!(
            check_usage_report(serde_json::from_value::<UsageReport>(negative).unwrap()).is_err()
        );

        let bad_day = serde_json::json!({
            "totals":{"calls":0,"prompt_tokens":0,"completion_tokens":0,"total_tokens":0},
            "per_day":[{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2,"calls":1}]
        });
        assert!(
            check_usage_report(serde_json::from_value::<UsageReport>(bad_day).unwrap()).is_err()
        );

        let bad_bot = serde_json::json!({
            "totals":{"calls":0,"prompt_tokens":0,"completion_tokens":0,"total_tokens":0},
            "per_bot":[{"bot":"../analyst","prompt_tokens":1,"completion_tokens":1,"total_tokens":2,"calls":1}]
        });
        assert!(
            check_usage_report(serde_json::from_value::<UsageReport>(bad_bot).unwrap()).is_err()
        );
    }

    #[test]
    fn connectors_normalizes_empty_strings_and_succeeds() {
        let raw = r#"{
            "connectors": [{
                "id": "agentmail",
                "kind": "agentmail",
                "enabled": true,
                "redirect_uri": "",
                "api_base": "   ",
                "default_inbox_id": "",
                "secrets": {}
            }]
        }"#;
        let envelope: ConnectorsEnvelope = serde_json::from_str(raw).unwrap();
        let validated = ApiClient::normalize_and_validate_connectors(envelope)
            .expect("validation should succeed when optional fields are empty strings");
        assert_eq!(validated.len(), 1);
        assert_eq!(validated[0].id, "agentmail");
        assert_eq!(validated[0].default_inbox_id, None);
        assert_eq!(validated[0].redirect_uri, None);
        assert_eq!(validated[0].api_base, None);
    }
}
