//! Browser profiles, per-profile sessions and bot settings (0.16).
//!
//! Contract: `docs/specs/browser-profiles-contract.md`. A profile is a named
//! Chrome user-data directory; a session is the one Chrome running on a
//! profile. Everything a reply carries is bounded and checked before the UI
//! sees it, like the rest of the client.

use super::*;

pub const MAX_BROWSER_PROFILES: usize = 8;
pub const MAX_BROWSER_SESSIONS: usize = 8;
pub const MAX_PROFILE_NAME_CHARS: usize = 40;
pub const MAX_BOT_NAME_CHARS: usize = 48;
pub const MAX_BOT_SETTINGS_DESCRIPTION_CHARS: usize = 280;
pub const MAX_BOT_SETTINGS_INSTRUCTIONS_CHARS: usize = 96_000;
/// The profile every user has and nobody can delete.
pub const DEFAULT_PROFILE: &str = "default";
const MAX_BOTS_PER_PROFILE: usize = 64;

/// `^[a-z0-9][a-z0-9-]{0,31}$`.
pub fn valid_profile_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// A profile name as people type it: 1-40 characters, no control characters,
/// at least one letter or digit.
pub fn valid_profile_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name.chars().count() <= MAX_PROFILE_NAME_CHARS
        && !name.chars().any(char::is_control)
        && name.chars().any(char::is_alphanumeric)
}

/// One browser profile: `GET /v1/browser/profiles`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct BrowserProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub is_default: bool,
    /// Ids of the bots assigned to this profile.
    #[serde(default)]
    pub bots: Vec<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub last_used_at: Option<String>,
}

#[derive(Deserialize)]
struct ProfileList {
    profiles: Vec<BrowserProfile>,
}

/// One tab of a browser (`t1`, `t2`, ...).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct BrowserTab {
    pub id: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub active: bool,
}

/// The last pointer action of whoever drives the page, in viewport pixels
/// (`vw` x `vh`). `seq` grows with every action.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct BrowserPointer {
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub vw: u32,
    #[serde(default)]
    pub vh: u32,
}

pub const MAX_BROWSER_TABS: usize = 8;

/// One live (or idle) browser: `GET /v1/browser/sessions`. The session id is
/// the profile id.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct BrowserSessionInfo {
    pub id: String,
    pub profile_name: String,
    pub active: bool,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    pub controller: BrowserController,
    #[serde(default)]
    pub tabs: Vec<BrowserTab>,
    #[serde(default)]
    pub pointer: Option<BrowserPointer>,
    /// The bot that last drove this browser.
    #[serde(default)]
    pub bot: Option<String>,
    /// The conversation of that bot's last turn that used the browser.
    #[serde(default)]
    pub conversation_id: Option<String>,
    pub updated_at: String,
}

impl BrowserSessionInfo {
    /// The dock's single-session view of this session.
    pub fn as_info(&self) -> BrowserInfo {
        BrowserInfo {
            active: self.active,
            url: self.url.clone(),
            title: self.title.clone(),
            controller: self.controller,
            updated_at: self.updated_at.clone(),
        }
    }
}

#[derive(Deserialize)]
struct SessionList {
    sessions: Vec<BrowserSessionInfo>,
}

#[derive(Deserialize)]
struct SessionClosedAck {
    closed: bool,
}

#[derive(Serialize)]
struct ProfileNameBody<'a> {
    name: &'a str,
}

/// A model choice for one bot. `None` on the update means "use the default".
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ModelPick {
    pub provider: String,
    pub model: String,
}

/// Body of `PUT /v1/bots/{id}`. A field left `None` is not sent and the
/// server keeps the current value. `model` and `browser_profile` use a second
/// layer: `Some(None)` sends `null` ("use the default").
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct BotSettingsUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The bot's instructions: the server's `prompt` field.
    #[serde(rename = "prompt", skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<Option<ModelPick>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_profile: Option<Option<String>>,
    /// The bot's tool ids (the capability switches).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
}

impl BotSettingsUpdate {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

fn plain(value: &str, max: usize, allow_newlines: bool) -> bool {
    value.chars().count() <= max
        && !value
            .chars()
            .any(|c| c.is_control() && !(allow_newlines && matches!(c, '\n' | '\r' | '\t')))
}

/// Check an update before it leaves the machine. The messages are shown
/// inline, so they are plain sentences.
pub fn validate_bot_settings(update: &BotSettingsUpdate) -> Result<(), ApiError> {
    if let Some(name) = &update.display_name {
        let name = name.trim();
        if name.is_empty() || !plain(name, MAX_BOT_NAME_CHARS, false) {
            return Err(ApiError::Validation(
                "Give the bot a name of up to 48 characters.",
            ));
        }
    }
    if let Some(text) = &update.description
        && !plain(text, MAX_BOT_SETTINGS_DESCRIPTION_CHARS, true)
    {
        return Err(ApiError::Validation(
            "The description can be up to 280 characters.",
        ));
    }
    if let Some(text) = &update.instructions
        && !plain(text, MAX_BOT_SETTINGS_INSTRUCTIONS_CHARS, true)
    {
        return Err(ApiError::Validation(
            "The instructions can be up to 96,000 characters.",
        ));
    }
    if let Some(Some(pick)) = &update.model
        && (!valid_provider_id(&pick.provider) || !valid_model_id(&pick.model))
    {
        return Err(ApiError::Validation("Pick a model from the list."));
    }
    if let Some(Some(profile)) = &update.browser_profile
        && !valid_profile_id(profile)
    {
        return Err(ApiError::Validation(
            "Pick a browser profile from the list.",
        ));
    }
    if let Some(tools) = &update.tools {
        validate_tool_edit(tools)?;
    }
    Ok(())
}

fn check_profile(profile: &BrowserProfile) -> Result<(), ApiError> {
    let ok = valid_profile_id(&profile.id)
        && valid_profile_name(&profile.name)
        && profile.bots.len() <= MAX_BOTS_PER_PROFILE
        && profile.bots.iter().all(|bot| valid_speaker(bot))
        && profile
            .created_at
            .as_deref()
            .is_none_or(|v| plain(v, 64, false))
        && profile
            .last_used_at
            .as_deref()
            .is_none_or(|v| plain(v, 64, false));
    if ok {
        Ok(())
    } else {
        Err(ApiError::InvalidResponse)
    }
}

fn check_session(session: &BrowserSessionInfo) -> Result<(), ApiError> {
    let ok = valid_profile_id(&session.id)
        && plain(&session.profile_name, 128, false)
        && !session.profile_name.is_empty()
        && session.url.as_deref().is_none_or(|v| plain(v, 2048, false))
        && session
            .title
            .as_deref()
            .is_none_or(|v| plain(v, 512, false))
        && session.tabs.len() <= MAX_BROWSER_TABS
        && session.tabs.iter().all(|tab| {
            valid_tab_id(&tab.id) && plain(&tab.url, 2048, false) && plain(&tab.title, 512, false)
        })
        && session.pointer.as_ref().is_none_or(|p| {
            p.x.is_finite()
                && p.y.is_finite()
                && p.vw <= 8192
                && p.vh <= 8192
                && p.action.len() <= 16
        })
        && session.bot.as_deref().is_none_or(valid_speaker)
        && session.conversation_id.as_deref().is_none_or(valid_id)
        && plain(&session.updated_at, 64, false);
    if ok {
        Ok(())
    } else {
        Err(ApiError::InvalidResponse)
    }
}

/// `t` and 1-6 digits, like the server.
pub fn valid_tab_id(id: &str) -> bool {
    id.strip_prefix('t')
        .is_some_and(|n| !n.is_empty() && n.len() <= 6 && n.bytes().all(|b| b.is_ascii_digit()))
}

fn session_id(id: &str) -> Result<(), ApiError> {
    if valid_profile_id(id) {
        Ok(())
    } else {
        Err(ApiError::Validation("That browser is not valid."))
    }
}

fn profile_name_for_send(name: &str) -> Result<&str, ApiError> {
    let name = name.trim();
    if valid_profile_name(name) {
        Ok(name)
    } else {
        Err(ApiError::Validation(
            "Give the profile a name of up to 40 characters.",
        ))
    }
}

impl ApiClient {
    /// `GET /v1/browser/profiles`.
    pub fn browser_profiles(&self) -> Result<Vec<BrowserProfile>, ApiError> {
        let list: ProfileList =
            self.panel_json::<_, ()>(Method::GET, &["v1", "browser", "profiles"], None)?;
        if list.profiles.len() > MAX_BROWSER_PROFILES {
            return Err(ApiError::InvalidResponse);
        }
        list.profiles.iter().try_for_each(check_profile)?;
        Ok(list.profiles)
    }

    /// `POST /v1/browser/profiles`. The server derives the id from the name.
    pub fn browser_profile_create(&self, name: &str) -> Result<BrowserProfile, ApiError> {
        let name = profile_name_for_send(name)?;
        let profile: BrowserProfile = self.panel_json(
            Method::POST,
            &["v1", "browser", "profiles"],
            Some(&ProfileNameBody { name }),
        )?;
        check_profile(&profile)?;
        Ok(profile)
    }

    /// `PUT /v1/browser/profiles/{id}`: rename.
    pub fn browser_profile_rename(&self, id: &str, name: &str) -> Result<BrowserProfile, ApiError> {
        session_id(id)?;
        let name = profile_name_for_send(name)?;
        let profile: BrowserProfile = self.panel_json(
            Method::PUT,
            &["v1", "browser", "profiles", id],
            Some(&ProfileNameBody { name }),
        )?;
        check_profile(&profile)?;
        if profile.id != id {
            return Err(ApiError::InvalidResponse);
        }
        Ok(profile)
    }

    /// `DELETE /v1/browser/profiles/{id}`. The default profile is refused
    /// here and by the server (`409`).
    pub fn browser_profile_delete(&self, id: &str) -> Result<(), ApiError> {
        session_id(id)?;
        if id == DEFAULT_PROFILE {
            return Err(ApiError::Validation(
                "The default profile cannot be deleted.",
            ));
        }
        self.panel_json::<serde_json::Value, ()>(
            Method::DELETE,
            &["v1", "browser", "profiles", id],
            None,
        )?;
        Ok(())
    }

    /// `GET /v1/browser/sessions`.
    pub fn browser_sessions(&self) -> Result<Vec<BrowserSessionInfo>, ApiError> {
        let list: SessionList =
            self.panel_json::<_, ()>(Method::GET, &["v1", "browser", "sessions"], None)?;
        if list.sessions.len() > MAX_BROWSER_SESSIONS {
            return Err(ApiError::InvalidResponse);
        }
        list.sessions.iter().try_for_each(check_session)?;
        Ok(list.sessions)
    }

    /// `GET /v1/browser/sessions/{id}/screenshot`: JPEG, same caps as the
    /// single-session route.
    pub fn browser_session_screenshot(&self, id: &str) -> Result<Vec<u8>, ApiError> {
        session_id(id)?;
        self.fetch_screenshot(&["v1", "browser", "sessions", id, "screenshot"])
    }

    fn session_control(&self, id: &str, action: &str) -> Result<BrowserSessionInfo, ApiError> {
        session_id(id)?;
        let info: BrowserSessionInfo = self.panel_json::<_, ()>(
            Method::POST,
            &["v1", "browser", "sessions", id, action],
            None,
        )?;
        check_session(&info)?;
        Ok(info)
    }

    /// `POST /v1/browser/sessions/{id}/takeover`.
    pub fn browser_session_takeover(&self, id: &str) -> Result<BrowserSessionInfo, ApiError> {
        self.session_control(id, "takeover")
    }

    /// `POST /v1/browser/sessions/{id}/handback`.
    pub fn browser_session_handback(&self, id: &str) -> Result<BrowserSessionInfo, ApiError> {
        self.session_control(id, "handback")
    }

    /// `POST /v1/browser/sessions/{id}/tabs/{tab}/switch` (user in control).
    pub fn browser_tab_switch(&self, id: &str, tab: &str) -> Result<BrowserSessionInfo, ApiError> {
        self.tab_control(id, tab, "switch")
    }

    /// `POST /v1/browser/sessions/{id}/tabs/{tab}/close` (user in control).
    pub fn browser_tab_close(&self, id: &str, tab: &str) -> Result<BrowserSessionInfo, ApiError> {
        self.tab_control(id, tab, "close")
    }

    fn tab_control(
        &self,
        id: &str,
        tab: &str,
        action: &str,
    ) -> Result<BrowserSessionInfo, ApiError> {
        session_id(id)?;
        if !valid_tab_id(tab) {
            return Err(ApiError::Validation("That tab is not valid."));
        }
        let info: BrowserSessionInfo = self.panel_json::<_, ()>(
            Method::POST,
            &["v1", "browser", "sessions", id, "tabs", tab, action],
            None,
        )?;
        check_session(&info)?;
        Ok(info)
    }

    /// `POST /v1/browser/sessions/{id}/input`, valid only while the user
    /// holds control.
    pub fn browser_session_input(
        &self,
        id: &str,
        input: &BrowserInput,
    ) -> Result<BrowserSessionInfo, ApiError> {
        session_id(id)?;
        validate_browser_input(input)?;
        let info: BrowserSessionInfo = self.panel_json(
            Method::POST,
            &["v1", "browser", "sessions", id, "input"],
            Some(input),
        )?;
        check_session(&info)?;
        Ok(info)
    }

    /// `POST /v1/browser/sessions/{id}/close`: whether a session was closed.
    pub fn browser_session_close(&self, id: &str) -> Result<bool, ApiError> {
        session_id(id)?;
        let ack: SessionClosedAck = self.panel_json::<_, ()>(
            Method::POST,
            &["v1", "browser", "sessions", id, "close"],
            None,
        )?;
        Ok(ack.closed)
    }

    /// `PUT /v1/bots/{id}`: name, description, instructions, model and
    /// browser profile in one write. A write: an unknown outcome is never
    /// replayed by the caller.
    pub fn update_bot(&self, id: &str, update: &BotSettingsUpdate) -> Result<(), ApiError> {
        if !valid_speaker(id) {
            return Err(ApiError::Validation("Bot id is not valid."));
        }
        validate_bot_settings(update)?;
        let mut body = update.clone();
        if let Some(name) = &mut body.display_name {
            *name = name.trim().to_owned();
        }
        self.panel_json::<serde_json::Value, _>(Method::PUT, &["v1", "bots", id], Some(&body))?;
        Ok(())
    }

    /// `DELETE /v1/bots/{id}`. The orchestrator is refused here and by the
    /// server.
    pub fn delete_bot(&self, id: &str) -> Result<(), ApiError> {
        if !valid_speaker(id) || id == "main" {
            return Err(ApiError::Validation("That bot can't be deleted."));
        }
        self.panel_json::<serde_json::Value, ()>(Method::DELETE, &["v1", "bots", id], None)?;
        Ok(())
    }
}

/// Whether a bot may be switched off or deleted: never the orchestrator.
pub fn can_remove_bot(id: &str, role: &str) -> bool {
    id != "main" && role != "orchestrator"
}

/// The profile a bot browses with: the one that lists it, else `default`.
pub fn profile_for_bot(profiles: &[BrowserProfile], bot: &str) -> String {
    profiles
        .iter()
        .find(|profile| profile.bots.iter().any(|b| b == bot))
        .map(|profile| profile.id.clone())
        .unwrap_or_else(|| DEFAULT_PROFILE.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_ids_and_names_follow_the_contract() {
        for ok in ["default", "work", "a", "0x", "my-shop-2", &"a".repeat(32)] {
            assert!(valid_profile_id(ok), "{ok}");
        }
        for bad in ["", "-a", "A", "a_b", "a b", "a/b", "..", &"a".repeat(33)] {
            assert!(!valid_profile_id(bad), "{bad}");
        }
        assert!(valid_profile_name("  Work logins "));
        assert!(!valid_profile_name("   "));
        assert!(!valid_profile_name("---"));
        assert!(!valid_profile_name("bad\nname"));
        assert!(!valid_profile_name(&"x".repeat(41)));
    }

    #[test]
    fn bot_settings_validation_gives_plain_sentences() {
        let ok = BotSettingsUpdate {
            display_name: Some(" Mia ".into()),
            description: Some("Books venues.\nChecks dates.".into()),
            instructions: Some("Be brief.\n\nAsk first.".into()),
            model: Some(Some(ModelPick {
                provider: "openai".into(),
                model: "gpt-x".into(),
            })),
            browser_profile: Some(Some("work".into())),
            enabled: None,
            tools: Some(vec!["vault_list".into()]),
        };
        assert!(validate_bot_settings(&ok).is_ok());
        // The orchestrator keeps `delegate` and other server-owned tools when
        // its abilities are edited (0.22: "Choose tools from the specialist
        // list" blocked every Save on Main).
        let main_tools = BotSettingsUpdate {
            tools: Some(vec![
                "delegate".into(),
                "tavily_search".into(),
                "local_list".into(),
            ]),
            ..BotSettingsUpdate::default()
        };
        assert!(validate_bot_settings(&main_tools).is_ok());
        let bad_tools = BotSettingsUpdate {
            tools: Some(vec!["Bad Tool".into(), "vault_list".into()]),
            ..BotSettingsUpdate::default()
        };
        assert!(validate_bot_settings(&bad_tools).is_err());
        let doubled = BotSettingsUpdate {
            tools: Some(vec!["vault_list".into(), "vault_list".into()]),
            ..BotSettingsUpdate::default()
        };
        assert!(validate_bot_settings(&doubled).is_err());
        // Every ability on at once still fits.
        let every: Vec<String> = crate::SPECIALIST_TOOLS
            .iter()
            .map(|tool| (*tool).to_owned())
            .chain(["delegate".to_owned()])
            .collect();
        let all_on = BotSettingsUpdate {
            tools: Some(every),
            ..BotSettingsUpdate::default()
        };
        assert!(validate_bot_settings(&all_on).is_ok());
        let bad_name = BotSettingsUpdate {
            display_name: Some("  ".into()),
            ..Default::default()
        };
        assert!(validate_bot_settings(&bad_name).is_err());
        let long_name = BotSettingsUpdate {
            display_name: Some("x".repeat(49)),
            ..Default::default()
        };
        assert!(validate_bot_settings(&long_name).is_err());
        let long_description = BotSettingsUpdate {
            description: Some("x".repeat(281)),
            ..Default::default()
        };
        assert!(validate_bot_settings(&long_description).is_err());
        let ctrl = BotSettingsUpdate {
            instructions: Some("a\u{0}b".into()),
            ..Default::default()
        };
        assert!(validate_bot_settings(&ctrl).is_err());
        let bad_model = BotSettingsUpdate {
            model: Some(Some(ModelPick {
                provider: "auto".into(),
                model: "m".into(),
            })),
            ..Default::default()
        };
        assert!(validate_bot_settings(&bad_model).is_err());
        let bad_profile = BotSettingsUpdate {
            browser_profile: Some(Some("../x".into())),
            ..Default::default()
        };
        assert!(validate_bot_settings(&bad_profile).is_err());
    }

    #[test]
    fn default_choices_travel_as_null_and_unchanged_fields_are_omitted() {
        let update = BotSettingsUpdate {
            model: Some(None),
            browser_profile: Some(None),
            ..Default::default()
        };
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"model": null, "browser_profile": null})
        );
        assert_eq!(
            serde_json::to_value(BotSettingsUpdate::default()).unwrap(),
            serde_json::json!({})
        );
        assert!(BotSettingsUpdate::default().is_empty());
        assert!(!update.is_empty());
        let named = BotSettingsUpdate {
            instructions: Some("Be brief.".into()),
            enabled: Some(false),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&named).unwrap(),
            serde_json::json!({"prompt": "Be brief.", "enabled": false})
        );
        assert!(can_remove_bot("scribe", "specialist"));
        assert!(!can_remove_bot("main", "orchestrator"));
    }

    #[test]
    fn sessions_and_profiles_parse_the_contract_shapes() {
        let list: SessionList = serde_json::from_str(
            r#"{"sessions":[{"id":"default","profile_name":"Default","active":true,
            "url":"https://example.com/","title":"Example","controller":"bot","bot":"main",
            "conversation_id":"dir:main","updated_at":"2026-11-06T10:00:00Z","extra":1}]}"#,
        )
        .unwrap();
        assert!(check_session(&list.sessions[0]).is_ok());
        assert_eq!(
            list.sessions[0].as_info().controller,
            BrowserController::Bot
        );
        let profiles: ProfileList = serde_json::from_str(
            r#"{"profiles":[{"id":"default","name":"Default","is_default":true,"bots":["main"],
            "created_at":"2026-11-01T00:00:00Z"}]}"#,
        )
        .unwrap();
        assert!(check_profile(&profiles.profiles[0]).is_ok());
        let mut bad = list.sessions[0].clone();
        bad.id = "../x".into();
        assert!(check_session(&bad).is_err());
        let mut bad = list.sessions[0].clone();
        bad.title = Some("a\nb".into());
        assert!(check_session(&bad).is_err());
    }

    /// The takeover/handback/input reply is parsed as a session: it must carry
    /// `id` and `profile_name` (the server once left them out, so "Take
    /// control" failed with an API error although the switch had happened).
    #[test]
    fn control_replies_parse_as_sessions_and_carry_tabs_and_pointer() {
        let reply = r#"{"active":true,"url":"https://example.com/","title":"Example",
            "controller":"user","updated_at":"2026-11-07T10:00:00Z","id":"default",
            "profile_name":"Default","tabs":[{"id":"t1","url":"https://example.com/",
            "title":"Example","active":true},{"id":"t2","url":"https://example.org/",
            "title":"Other","active":false}],
            "pointer":{"x":10.5,"y":20.0,"action":"click","seq":3,"vw":1280,"vh":800}}"#;
        let info: BrowserSessionInfo = serde_json::from_str(reply).unwrap();
        assert!(check_session(&info).is_ok());
        assert_eq!(info.controller, BrowserController::User);
        assert_eq!(info.tabs.len(), 2);
        assert!(info.tabs[0].active && !info.tabs[1].active);
        assert_eq!(info.pointer.as_ref().map(|p| p.seq), Some(3));
        // Older servers send neither tabs nor pointer.
        let old: BrowserSessionInfo = serde_json::from_str(
            r#"{"active":true,"controller":"bot","updated_at":"x","id":"default","profile_name":"Default"}"#,
        )
        .unwrap();
        assert!(old.tabs.is_empty() && old.pointer.is_none());
        // A reply without the session identity is exactly what broke Take control.
        assert!(
            serde_json::from_str::<BrowserSessionInfo>(
                r#"{"active":true,"controller":"user","updated_at":"x"}"#
            )
            .is_err()
        );
        let mut bad = info.clone();
        bad.tabs[0].id = "../x".into();
        assert!(check_session(&bad).is_err());
        assert!(valid_tab_id("t12") && !valid_tab_id("t") && !valid_tab_id("x1"));
    }

    #[test]
    fn a_bot_browses_with_the_profile_that_lists_it() {
        let profile = |id: &str, bots: &[&str]| BrowserProfile {
            id: id.into(),
            name: id.into(),
            is_default: id == "default",
            bots: bots.iter().map(|b| (*b).to_owned()).collect(),
            created_at: None,
            last_used_at: None,
        };
        let profiles = vec![profile("default", &["main"]), profile("work", &["analyst"])];
        assert_eq!(profile_for_bot(&profiles, "analyst"), "work");
        assert_eq!(profile_for_bot(&profiles, "main"), "default");
        assert_eq!(profile_for_bot(&profiles, "unknown"), "default");
        assert_eq!(profile_for_bot(&[], "main"), "default");
    }
}
