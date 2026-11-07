//! Bot and group profiles (0.15): avatars, one-step group create, the bot
//! onboarding interview and the plain-language tool toggles.
//!
//! Images travel as raw bytes in both directions. The console checks the size
//! and the magic bytes before an upload; the server decodes and re-encodes
//! every upload, so what comes back is always a 256x256 PNG. A reply that is
//! not a PNG within the size cap is refused.

use super::*;

/// The server accepts at most 1 MiB per upload.
pub const MAX_AVATAR_UPLOAD_BYTES: usize = 1024 * 1024;
/// The `accept` value of the avatar file input.
///
/// Extensions, not MIME types: dioxus-desktop opens the native dialog itself
/// and only maps `.ext` and `image/*` to dialog filters. A MIME list such as
/// `image/png,image/jpeg` becomes a filter with no extensions, the dialog
/// offers nothing to pick, no file reaches the page and no upload is sent.
pub const AVATAR_FILE_ACCEPT: &str = ".png,.jpg,.jpeg,.webp";
/// A stored avatar is a 256x256 PNG; anything past this is not one.
pub const MAX_AVATAR_DOWNLOAD_BYTES: usize = 512 * 1024;
const MAX_ONBOARDING_LINES: usize = 40;
const MAX_ONBOARDING_TEXT: usize = 8_000;
pub const MAX_GROUP_DESCRIPTION_CHARS: usize = 500;
pub const MAX_GROUP_RULES_CHARS: usize = 4_000;
pub const MAX_NAME_CHARS: usize = 60;

/// The format of an image by its first bytes, limited to what the server
/// accepts: png, jpeg or webp.
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Check a picked image before it is read into memory or uploaded.
pub fn check_avatar_upload(bytes: &[u8]) -> Result<&'static str, ApiError> {
    if bytes.is_empty() {
        return Err(ApiError::Validation("That image is empty."));
    }
    if bytes.len() > MAX_AVATAR_UPLOAD_BYTES {
        return Err(ApiError::Validation("Pick an image under 1 MB."));
    }
    sniff_image(bytes).ok_or(ApiError::Validation("Pick a png, jpeg or webp image."))
}

/// The id the server will derive from a name. Shown read-only under
/// "Advanced"; the server is the authority and may add a number if the id is
/// taken.
pub fn preview_id(name: &str, fallback: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(character.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    if slug.is_empty() {
        return String::new();
    }
    if !slug.starts_with(|c: char| c.is_ascii_lowercase()) {
        slug = format!("{fallback}-{slug}");
    }
    slug.truncate(32);
    slug.trim_end_matches('-').to_owned()
}

pub fn valid_display_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && !name.chars().any(char::is_control)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct OnboardingLine {
    pub from: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct OnboardingProposal {
    pub role: String,
    pub objective: String,
    pub prompt: String,
}

/// `GET /v1/bots/{id}/onboarding` and the replies that change it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Onboarding {
    pub bot: String,
    pub name: String,
    pub state: String,
    pub messages: Vec<OnboardingLine>,
    #[serde(default)]
    pub proposal: Option<OnboardingProposal>,
    #[serde(default)]
    pub can_confirm: bool,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub joined_group: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct CreatedGroup {
    pub id: String,
}

/// What the group sheet sends. Ids are never part of it.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct NewGroup {
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub members: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub rules: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<Selection>,
}

#[derive(Deserialize)]
struct GroupEnvelope {
    group: CreatedGroup,
}

fn check_onboarding(onboarding: Onboarding, bot: &str) -> Result<Onboarding, ApiError> {
    let proposal_ok = onboarding.proposal.as_ref().is_none_or(|proposal| {
        [&proposal.role, &proposal.objective, &proposal.prompt]
            .iter()
            .all(|text| text.chars().count() <= MAX_ONBOARDING_TEXT)
    });
    if onboarding.bot != bot
        || !matches!(
            onboarding.state.as_str(),
            "awaiting_goal" | "clarifying" | "proposed" | "confirmed"
        )
        || onboarding.name.chars().count() > 64
        || onboarding.messages.len() > MAX_ONBOARDING_LINES
        || onboarding.messages.iter().any(|line| {
            !matches!(line.from.as_str(), "bot" | "user")
                || line.text.chars().count() > MAX_ONBOARDING_TEXT
        })
        || !proposal_ok
    {
        return Err(ApiError::InvalidResponse);
    }
    Ok(onboarding)
}

impl ApiClient {
    /// `POST /v1/bots` with a name: the server derives the id, creates the bot
    /// disabled and opens its onboarding interview.
    pub fn create_named_bot(
        &self,
        name: &str,
        model: Option<&str>,
        group: Option<&str>,
    ) -> Result<CreatedBot, ApiError> {
        if !valid_display_name(name) {
            return Err(ApiError::Validation(
                "Give the bot a name of 1 to 60 characters.",
            ));
        }
        if model.is_some_and(|model| !valid_model_id(model)) {
            return Err(ApiError::Validation("Pick a model from the list."));
        }
        #[derive(Serialize)]
        struct Body<'a> {
            name: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            model: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            group: Option<&'a str>,
        }
        let response: BotEnvelope = self.panel_json(
            Method::POST,
            &["v1", "bots"],
            Some(&Body {
                name: name.trim(),
                model,
                group,
            }),
        )?;
        let id = response.bot.id.clone();
        if !valid_bot_id(&id) {
            return Err(ApiError::InvalidResponse);
        }
        check_created_bot(response.bot, &id)
    }

    fn avatar_get(&self, route: &[&str]) -> Result<Option<Vec<u8>>, ApiError> {
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
        let status = response.status();
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(ApiError::Http(status.as_u16()));
        }
        let cap = MAX_AVATAR_DOWNLOAD_BYTES as u64;
        if response.content_length().is_some_and(|len| len > cap) {
            return Err(ApiError::TooLarge);
        }
        let mut bytes = Vec::new();
        response
            .take(cap + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ApiError::Transport)?;
        if bytes.len() as u64 > cap {
            return Err(ApiError::TooLarge);
        }
        if sniff_image(&bytes) != Some("image/png") {
            return Err(ApiError::InvalidResponse);
        }
        Ok(Some(bytes))
    }

    fn avatar_put(&self, route: &[&str], bytes: &[u8]) -> Result<(), ApiError> {
        check_avatar_upload(bytes)?;
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| ApiError::InvalidResponse)?
            .clear()
            .extend(route);
        let response = self
            .client
            .put(url)
            .timeout(crate::net_policy::WRITE_TIMEOUT)
            .bearer_auth(self.token.expose_secret())
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(bytes.to_vec())
            .send()
            .map_err(|_| ApiError::Transport)?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let mut body = Vec::new();
        let _ = response.take(4096).read_to_end(&mut body);
        if matches!(status.as_u16(), 400 | 422)
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body)
            && let Some(message) = value.get("error").and_then(|value| value.as_str())
            && !message.is_empty()
            && message.len() <= 512
            && !message.chars().any(char::is_control)
        {
            return Err(ApiError::Rejected(message.to_owned()));
        }
        Err(ApiError::Http(status.as_u16()))
    }

    /// `GET /v1/bots/{id}/avatar`; `None` when the bot has no image.
    pub fn bot_avatar(&self, id: &str) -> Result<Option<Vec<u8>>, ApiError> {
        if !valid_speaker(id) {
            return Err(ApiError::Validation("Bot id is not valid."));
        }
        self.avatar_get(&["v1", "bots", id, "avatar"])
    }

    /// `PUT /v1/bots/{id}/avatar`: raw image bytes.
    pub fn set_bot_avatar(&self, id: &str, bytes: &[u8]) -> Result<(), ApiError> {
        if !valid_speaker(id) {
            return Err(ApiError::Validation("Bot id is not valid."));
        }
        self.avatar_put(&["v1", "bots", id, "avatar"], bytes)
    }

    /// `DELETE /v1/bots/{id}/avatar`.
    pub fn remove_bot_avatar(&self, id: &str) -> Result<(), ApiError> {
        if !valid_speaker(id) {
            return Err(ApiError::Validation("Bot id is not valid."));
        }
        self.panel_json::<serde_json::Value, ()>(
            Method::DELETE,
            &["v1", "bots", id, "avatar"],
            None,
        )?;
        Ok(())
    }

    /// `POST /v1/groups`: a group chat in one step.
    pub fn create_group(&self, group: &NewGroup) -> Result<CreatedGroup, ApiError> {
        if !valid_display_name(&group.name) {
            return Err(ApiError::Validation(
                "Give the group a name of 1 to 60 characters.",
            ));
        }
        if group.description.chars().count() > MAX_GROUP_DESCRIPTION_CHARS {
            return Err(ApiError::Validation(
                "The description must be at most 500 characters.",
            ));
        }
        if group.rules.chars().count() > MAX_GROUP_RULES_CHARS {
            return Err(ApiError::Validation(
                "The rules must be at most 4,000 characters.",
            ));
        }
        validate_group_members(&group.members)?;
        let response: GroupEnvelope =
            self.panel_json(Method::POST, &["v1", "groups"], Some(group))?;
        if !response.group.id.starts_with("room:") || !valid_conversation_id(&response.group.id) {
            return Err(ApiError::InvalidResponse);
        }
        Ok(response.group)
    }

    /// `GET /v1/conversations/{id}/avatar`.
    pub fn group_avatar(&self, id: &str) -> Result<Option<Vec<u8>>, ApiError> {
        check_id(id)?;
        self.avatar_get(&["v1", "conversations", id, "avatar"])
    }

    /// `PUT /v1/conversations/{id}/avatar`.
    pub fn set_group_avatar(&self, id: &str, bytes: &[u8]) -> Result<(), ApiError> {
        check_id(id)?;
        self.avatar_put(&["v1", "conversations", id, "avatar"], bytes)
    }

    /// `GET /v1/bots/{id}/onboarding`.
    pub fn onboarding(&self, id: &str) -> Result<Onboarding, ApiError> {
        if !valid_speaker(id) {
            return Err(ApiError::Validation("Bot id is not valid."));
        }
        let state: Onboarding =
            self.panel_json::<_, ()>(Method::GET, &["v1", "bots", id, "onboarding"], None)?;
        check_onboarding(state, id)
    }

    /// `POST /v1/bots/{id}/onboarding`: start (or resume) the interview.
    pub fn onboarding_start(&self, id: &str) -> Result<Onboarding, ApiError> {
        if !valid_speaker(id) {
            return Err(ApiError::Validation("Bot id is not valid."));
        }
        let state: Onboarding =
            self.panel_json::<_, ()>(Method::POST, &["v1", "bots", id, "onboarding"], None)?;
        check_onboarding(state, id)
    }

    /// `POST /v1/bots/{id}/onboarding/reply`. A write: an unknown outcome is
    /// never replayed by the caller.
    pub fn onboarding_reply(&self, id: &str, text: &str) -> Result<Onboarding, ApiError> {
        if !valid_speaker(id) {
            return Err(ApiError::Validation("Bot id is not valid."));
        }
        let text = text.trim();
        if text.is_empty() || text.chars().count() > 2_000 {
            return Err(ApiError::Validation(
                "Write 1 to 2,000 characters before sending.",
            ));
        }
        #[derive(Serialize)]
        struct Body<'a> {
            text: &'a str,
        }
        let state: Onboarding = self.panel_json(
            Method::POST,
            &["v1", "bots", id, "onboarding", "reply"],
            Some(&Body { text }),
        )?;
        check_onboarding(state, id)
    }

    /// `POST /v1/bots/{id}/onboarding/confirm`: save the proposal into the bot.
    pub fn onboarding_confirm(&self, id: &str) -> Result<Onboarding, ApiError> {
        if !valid_speaker(id) {
            return Err(ApiError::Validation("Bot id is not valid."));
        }
        let state: Onboarding = self.panel_json::<_, ()>(
            Method::POST,
            &["v1", "bots", id, "onboarding", "confirm"],
            None,
        )?;
        check_onboarding(state, id)
    }

    /// `PUT /v1/bots/{id}` with only the tool list: the capability toggles.
    pub fn set_bot_tools(&self, id: &str, tools: &[String]) -> Result<(), ApiError> {
        if !valid_speaker(id) {
            return Err(ApiError::Validation("Bot id is not valid."));
        }
        validate_tool_list(tools)?;
        #[derive(Serialize)]
        struct Body<'a> {
            tools: &'a [String],
        }
        self.panel_json::<serde_json::Value, _>(
            Method::PUT,
            &["v1", "bots", id],
            Some(&Body { tools }),
        )?;
        Ok(())
    }
}

fn validate_group_members(members: &[String]) -> Result<(), ApiError> {
    if members.is_empty() {
        return Ok(());
    }
    validate_team_members(members)
}

fn valid_conversation_id(id: &str) -> bool {
    check_id(id).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatar_picker_filter_is_extensions_the_native_dialog_understands() {
        // dioxus-desktop turns only `.ext` and `image/*` into dialog filters;
        // any other token (a MIME type) yields an empty filter and an empty
        // dialog, so the upload never starts.
        for token in AVATAR_FILE_ACCEPT.split(',') {
            let ext = token.strip_prefix('.').expect("extension token");
            assert!(!ext.is_empty() && ext.chars().all(|c| c.is_ascii_lowercase()));
        }
        for needed in ["png", "jpg", "jpeg", "webp"] {
            assert!(
                AVATAR_FILE_ACCEPT
                    .split(',')
                    .any(|t| t == format!(".{needed}"))
            );
        }
    }

    #[test]
    fn images_are_sniffed_by_magic_bytes_and_capped() {
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
        assert_eq!(check_avatar_upload(&png), Ok("image/png"));
        assert_eq!(
            check_avatar_upload(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]),
            Ok("image/jpeg")
        );
        let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        webp.push(0);
        assert_eq!(check_avatar_upload(&webp), Ok("image/webp"));
        assert!(check_avatar_upload(&[]).is_err());
        assert!(check_avatar_upload(b"GIF89a....").is_err());
        assert!(check_avatar_upload(b"<svg></svg>").is_err());
        let mut big = png.to_vec();
        big.resize(MAX_AVATAR_UPLOAD_BYTES + 1, 0);
        assert!(check_avatar_upload(&big).is_err());
    }

    #[test]
    fn the_id_preview_mirrors_the_server_rules() {
        assert_eq!(preview_id("Marketing Lead", "bot"), "marketing-lead");
        assert_eq!(preview_id("  Q3 -- Budget!  ", "bot"), "q3-budget");
        assert_eq!(preview_id("42 Things", "bot"), "bot-42-things");
        assert_eq!(preview_id("???", "bot"), "");
        assert!(preview_id(&"a".repeat(80), "bot").len() <= 32);
    }

    #[test]
    fn names_are_bounded() {
        assert!(valid_display_name("Recruiter"));
        assert!(!valid_display_name("   "));
        assert!(!valid_display_name(&"x".repeat(61)));
        assert!(!valid_display_name("bad\u{7}name"));
    }

    #[test]
    fn onboarding_reads_the_server_shape_and_rejects_bad_rows() {
        let good = r#"{"bot":"recruiter","name":"Recruiter","state":"proposed",
            "messages":[{"from":"bot","text":"Hi"},{"from":"user","text":"Help"}],
            "proposal":{"role":"R","objective":"O","prompt":"P"},
            "can_confirm":true,"done":false}"#;
        let parsed: Onboarding = serde_json::from_str(good).unwrap();
        assert!(check_onboarding(parsed.clone(), "recruiter").is_ok());
        assert!(check_onboarding(parsed.clone(), "other").is_err());
        let mut bad = parsed.clone();
        bad.state = "mystery".into();
        assert!(check_onboarding(bad, "recruiter").is_err());
        let mut bad = parsed;
        bad.messages[0].from = "system".into();
        assert!(check_onboarding(bad, "recruiter").is_err());
    }

    #[test]
    fn group_requests_leave_ids_out_and_skip_empty_fields() {
        let group = NewGroup {
            name: "Hiring".into(),
            members: vec!["recruiter".into()],
            ..NewGroup::default()
        };
        let value = serde_json::to_value(&group).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"name": "Hiring", "members": ["recruiter"]})
        );
    }
}
