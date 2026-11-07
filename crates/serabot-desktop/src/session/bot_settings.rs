//! The bot settings panel (0.16): one editable form for the bot's picture,
//! name, description, instructions, model and browser profile.
//!
//! The draft is plain data. [`BotDraft::changes`] builds the smallest
//! `PUT /v1/bots/{id}` body from what the owner edited, and the `*_error`
//! helpers give the inline messages, so all of it is unit-tested without a
//! window. Only the picture saves on its own; the capability switches are
//! part of the draft, so Save and Cancel cover them too (0.20).

use super::*;
use crate::{
    BotSettingsUpdate, MAX_BOT_NAME_CHARS, MAX_BOT_SETTINGS_DESCRIPTION_CHARS,
    MAX_BOT_SETTINGS_INSTRUCTIONS_CHARS, ModelPick, valid_profile_id,
};

/// Which pane of the settings panel is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsTab {
    /// The bot's form (or the members, for a group).
    #[default]
    Settings,
    /// Files the bots made in this chat.
    Files,
}

/// The editable fields. `model` is empty for "Default", else
/// `provider|model`; `browser_profile` is empty for the default profile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BotDraft {
    pub display_name: String,
    pub description: String,
    pub instructions: String,
    pub model: String,
    pub browser_profile: String,
    /// The tool ids the capability switches add up to.
    pub tools: Vec<String>,
}

/// `provider|model` for the model picker's option values.
pub fn model_value(provider: &str, model: &str) -> String {
    format!("{provider}|{model}")
}

/// Split a picker value back into a model choice; empty means "Default".
pub fn model_pick(value: &str) -> Option<ModelPick> {
    let (provider, model) = value.split_once('|')?;
    Some(ModelPick {
        provider: provider.to_owned(),
        model: model.to_owned(),
    })
}

impl BotDraft {
    pub fn name_error(&self) -> Option<String> {
        let name = self.display_name.trim();
        if name.is_empty() {
            Some("Give the bot a name.".into())
        } else if name.chars().count() > MAX_BOT_NAME_CHARS {
            Some(format!(
                "Keep the name to {MAX_BOT_NAME_CHARS} characters or fewer."
            ))
        } else if name.chars().any(char::is_control) {
            Some("The name can't contain special characters.".into())
        } else {
            None
        }
    }

    pub fn description_error(&self) -> Option<String> {
        (self.description.chars().count() > MAX_BOT_SETTINGS_DESCRIPTION_CHARS).then(|| {
            format!(
                "Keep the description to {MAX_BOT_SETTINGS_DESCRIPTION_CHARS} characters or fewer."
            )
        })
    }

    pub fn instructions_error(&self) -> Option<String> {
        (self.instructions.chars().count() > MAX_BOT_SETTINGS_INSTRUCTIONS_CHARS)
            .then(|| "Keep the instructions under 96,000 characters.".to_owned())
    }

    /// The first inline problem, if any. Save stays off while there is one.
    pub fn first_error(&self) -> Option<String> {
        self.name_error()
            .or_else(|| self.description_error())
            .or_else(|| self.instructions_error())
    }

    /// Only what differs from `original` goes to the server.
    pub fn changes(&self, original: &Self) -> BotSettingsUpdate {
        let mut update = BotSettingsUpdate::default();
        if self.display_name.trim() != original.display_name.trim() {
            update.display_name = Some(self.display_name.trim().to_owned());
        }
        if self.description.trim() != original.description.trim() {
            update.description = Some(self.description.trim().to_owned());
        }
        if self.instructions.trim() != original.instructions.trim() {
            update.instructions = Some(self.instructions.trim().to_owned());
        }
        if self.model != original.model {
            update.model = Some(model_pick(&self.model));
        }
        if self.browser_profile != original.browser_profile {
            update.browser_profile = Some(
                (!self.browser_profile.is_empty()
                    && self.browser_profile != crate::DEFAULT_PROFILE)
                    .then(|| self.browser_profile.clone()),
            );
        }
        if !same_tools(&self.tools, &original.tools) {
            update.tools = Some(self.tools.clone());
        }
        update
    }

    pub fn dirty(&self, original: &Self) -> bool {
        !self.changes(original).is_empty()
    }
}

/// Tool lists are sets: order and duplicates do not count as a change.
fn same_tools(a: &[String], b: &[String]) -> bool {
    let mut a: Vec<&String> = a.iter().collect();
    let mut b: Vec<&String> = b.iter().collect();
    a.sort();
    a.dedup();
    b.sort();
    b.dedup();
    a == b
}

#[derive(Default)]
pub struct BotSettings {
    pub open: bool,
    /// The bot being edited; `None` while the panel shows a group.
    pub bot: Option<String>,
    pub tab: SettingsTab,
    pub draft: BotDraft,
    pub original: BotDraft,
    /// The draft was filled from the full bot record.
    pub hydrated: bool,
    pub saving: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
    /// Closing was asked for with unsaved edits: the panel is asking
    /// "Save changes?".
    pub confirm_close: bool,
    /// Close the panel once the save in flight succeeds.
    close_after_save: bool,
    /// The bot whose Delete is waiting for its confirm step.
    pub confirm_delete: Option<String>,
    /// A switch-off or delete is on its way; the buttons wait.
    pub lifecycle_busy: bool,
    deleted: Option<String>,
}

impl BotSettings {
    pub fn dirty(&self) -> bool {
        self.hydrated && self.draft.dirty(&self.original)
    }
}

impl Session {
    /// Show the settings panel for one bot.
    pub fn open_bot_settings(&mut self, id: String) {
        if !valid_speaker(&id) {
            return;
        }
        let first = self.bot_settings.bot.as_deref() != Some(id.as_str());
        self.bot_settings.open = true;
        self.bot_settings.bot = Some(id.clone());
        self.bot_settings.tab = SettingsTab::Settings;
        self.bot_settings.error = None;
        self.bot_settings.notice = None;
        self.bot_settings.confirm_close = false;
        self.bot_settings.close_after_save = false;
        if first || !self.bot_settings.hydrated {
            let draft = BotDraft {
                display_name: self.bot_display_name(&id),
                description: self
                    .state
                    .listed_bots
                    .iter()
                    .find(|bot| bot.id == id)
                    .and_then(|bot| bot.description.clone())
                    .unwrap_or_default(),
                tools: self.bot_tools(&id),
                ..BotDraft::default()
            };
            self.bot_settings.original = draft.clone();
            self.bot_settings.draft = draft;
            self.bot_settings.hydrated = false;
        }
        self.panel.ask_profiles();
        if !self.state.skills_loaded && self.state.connected {
            self.schedule(Operation::ListSkills, Duration::ZERO);
        }
        // The switches need to know which connectors are connected.
        if !self.state.connectors_loaded && self.state.connected {
            self.schedule(Operation::Connectors, Duration::ZERO);
        }
        if !self.telegram.loaded && self.state.connected {
            self.refresh_telegram();
        }
        // Always read the record fresh: the cached one may predate an edit.
        self.state.bot_details.remove(&id);
        self.load_bot_detail(id);
    }

    /// Show the panel for the open conversation (a bot chat, or a group).
    pub fn open_settings_for_conversation(&mut self) {
        match self.conversation.strip_prefix("dir:").map(str::to_owned) {
            Some(bot) => self.open_bot_settings(bot),
            None => {
                self.bot_settings.open = true;
                self.bot_settings.bot = None;
                self.bot_settings.tab = SettingsTab::Settings;
            }
        }
    }

    pub fn close_bot_settings(&mut self) {
        self.bot_settings.open = false;
        self.bot_settings.error = None;
        self.bot_settings.confirm_close = false;
        self.bot_settings.close_after_save = false;
    }

    /// The X button: close at once, or ask "Save changes?" when there are
    /// unsaved edits.
    pub fn request_close_bot_settings(&mut self) {
        if self.bot_settings.dirty() && !self.bot_settings.saving {
            self.bot_settings.confirm_close = true;
        } else {
            self.close_bot_settings();
        }
    }

    /// "Keep editing" on the close question.
    pub fn keep_editing_bot_settings(&mut self) {
        self.bot_settings.confirm_close = false;
    }

    /// "Save" on the close question: save, and close once it is written.
    pub fn save_and_close_bot_settings(&mut self) {
        self.bot_settings.confirm_close = false;
        self.bot_settings.close_after_save = true;
        self.save_bot_settings();
        if !self.bot_settings.saving {
            // Nothing was sent (invalid, or nothing to save): stay open so
            // the message is seen.
            self.bot_settings.close_after_save = false;
        }
    }

    /// Switch one ability on or off in the draft. Nothing is sent until Save.
    pub fn toggle_draft_capability(&mut self, index: usize, on: bool) {
        let Some(capability) = crate::capabilities::CAPABILITIES.get(index) else {
            return;
        };
        if !self.bot_settings.hydrated {
            return;
        }
        let tools = crate::capabilities::toggled(capability, &self.bot_settings.draft.tools, on);
        if let Err(error) = crate::validate_tool_list(&tools) {
            self.bot_settings.error = Some(error.to_string());
            return;
        }
        self.bot_settings.draft.tools = tools;
        self.bot_settings.error = None;
        self.bot_settings.notice = None;
    }

    /// What the ability at `index` still needs, from what the desktop knows
    /// about connectors and Telegram.
    pub fn capability_missing(&self, index: usize) -> Option<crate::capabilities::Missing> {
        let capability = crate::capabilities::CAPABILITIES.get(index)?;
        let connectors = self
            .state
            .connectors_loaded
            .then_some(self.state.connectors.as_slice());
        let telegram = self
            .telegram
            .status
            .as_ref()
            .map(|status| status.telegram_chat_id.is_some());
        crate::capabilities::missing(capability.needs, connectors, telegram)
    }

    /// Throw the edits away and close the panel.
    pub fn cancel_bot_settings(&mut self) {
        self.bot_settings.draft = self.bot_settings.original.clone();
        self.bot_settings.saving = false;
        self.close_bot_settings();
    }

    /// Fill the draft from the full record once it has arrived. The pump
    /// calls this on every tick; it does nothing after the first fill.
    pub(super) fn hydrate_bot_settings(&mut self) {
        if !self.bot_settings.open || self.bot_settings.hydrated {
            return;
        }
        let Some(id) = self.bot_settings.bot.clone() else {
            return;
        };
        let Some(detail) = self.state.bot_details.get(&id) else {
            return;
        };
        let pinned = self
            .state
            .listed_bots
            .iter()
            .find(|bot| bot.id == id)
            .is_some_and(|bot| bot.model.is_some());
        let draft = BotDraft {
            display_name: detail.display_name.clone(),
            description: detail.description.clone(),
            instructions: detail.instructions.clone().unwrap_or_default(),
            model: if pinned {
                model_value(&detail.provider, &detail.model)
            } else {
                String::new()
            },
            browser_profile: detail
                .browser_profile
                .clone()
                .filter(|profile| valid_profile_id(profile))
                .unwrap_or_default(),
            tools: detail.tools.clone(),
        };
        self.bot_settings.original = draft.clone();
        self.bot_settings.draft = draft;
        self.bot_settings.hydrated = true;
    }

    /// Validate and send the form. Nothing is sent when nothing changed.
    pub fn save_bot_settings(&mut self) {
        let Some(id) = self.bot_settings.bot.clone() else {
            return;
        };
        if self.bot_settings.saving || !self.bot_settings.hydrated {
            return;
        }
        if let Some(message) = self.bot_settings.draft.first_error() {
            self.bot_settings.error = Some(message);
            return;
        }
        let update = self.bot_settings.draft.changes(&self.bot_settings.original);
        if update.is_empty() {
            self.bot_settings.notice = Some("Nothing to save.".into());
            return;
        }
        if let Err(error) = crate::validate_bot_settings(&update) {
            self.bot_settings.error = Some(error.to_string());
            return;
        }
        if !self.can_admin() {
            self.bot_settings.error = Some("Wait a moment, then press Save again.".into());
            return;
        }
        self.bot_settings.error = None;
        self.bot_settings.notice = None;
        self.bot_settings.saving = true;
        self.start_request(Operation::UpdateBot { id, update });
    }

    /// The write came back: the draft is the new baseline, and the lists are
    /// read again so the sidebar and the header show the new name.
    pub(super) fn bot_settings_saved(&mut self) {
        self.bot_settings.saving = false;
        self.bot_settings.error = None;
        self.bot_settings.notice = Some("Saved".into());
        self.bot_settings.original = self.bot_settings.draft.clone();
        if std::mem::take(&mut self.bot_settings.close_after_save) {
            self.bot_settings.open = false;
        }
        self.panel.ask_profiles();
        if let Some(id) = self.bot_settings.bot.clone() {
            self.state.bot_details.remove(&id);
            self.schedule(Operation::ListBots, Duration::ZERO);
        }
    }

    fn can_remove(&self, id: &str) -> bool {
        self.state
            .listed_bots
            .iter()
            .find(|bot| bot.id == id)
            .is_some_and(|bot| crate::can_remove_bot(&bot.id, &bot.role))
    }

    /// Switch a bot on or off. Never the orchestrator.
    pub fn set_bot_enabled(&mut self, id: String, enabled: bool) {
        if !self.can_remove(&id) || self.bot_settings.lifecycle_busy || !self.can_admin() {
            return;
        }
        self.bot_settings.lifecycle_busy = true;
        self.start_request(Operation::SetBotEnabled { id, enabled });
    }

    /// Ask for the confirm step of a delete (from the row or the panel).
    pub fn ask_delete_bot(&mut self, id: String) {
        if self.can_remove(&id) {
            self.open_bot_settings(id.clone());
            self.bot_settings.confirm_delete = Some(id);
        }
    }

    pub fn cancel_delete_bot(&mut self) {
        self.bot_settings.confirm_delete = None;
    }

    /// Delete the bot whose confirm step is showing.
    pub fn confirm_delete_bot(&mut self) {
        let Some(id) = self.bot_settings.confirm_delete.take() else {
            return;
        };
        if !self.can_remove(&id) || self.bot_settings.lifecycle_busy || !self.can_admin() {
            return;
        }
        self.bot_settings.lifecycle_busy = true;
        self.bot_settings.deleted = Some(id.clone());
        self.start_request(Operation::DeleteBot { id });
    }

    /// A switch or a delete came back: reread the lists, and leave a deleted
    /// bot's panel and chat behind.
    pub(super) fn bot_lifecycle_done(&mut self) {
        self.bot_settings.lifecycle_busy = false;
        if let Some(id) = self.bot_settings.deleted.take() {
            self.state.bot_details.remove(&id);
            if self.bot_settings.bot.as_deref() == Some(id.as_str()) {
                self.bot_settings = BotSettings::default();
            }
            if self.conversation == format!("dir:{id}") {
                self.open_conversation("windows-owner".into());
            }
            self.toast = Some("Bot deleted.".into());
        }
        self.schedule(Operation::ListBots, Duration::ZERO);
        self.panel.ask_profiles();
    }

    pub(super) fn bot_settings_failed(&mut self, error: &ApiError) {
        self.bot_settings.saving = false;
        self.bot_settings.close_after_save = false;
        self.bot_settings.error = Some(if error.unknown_outcome() {
            "Not sure that was saved. Close this panel, open it again to see what the bot has now, then retry if needed."
                .to_owned()
        } else {
            error.to_string()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> BotDraft {
        BotDraft {
            display_name: "Mia".into(),
            description: "Books venues".into(),
            instructions: "Be brief.".into(),
            model: String::new(),
            browser_profile: String::new(),
            tools: vec!["vault_list".into(), "mystery_tool".into()],
        }
    }

    #[test]
    fn tools_are_part_of_the_draft() {
        let original = base();
        let mut draft = original.clone();
        // Order and duplicates are not a change.
        draft.tools = vec![
            "mystery_tool".into(),
            "vault_list".into(),
            "vault_list".into(),
        ];
        assert!(!draft.dirty(&original));
        draft.tools.push("memory_save".into());
        assert!(draft.dirty(&original));
        let update = draft.changes(&original);
        assert!(update.tools.as_ref().is_some_and(|t| t.len() == 4));
        assert!(update.display_name.is_none());
    }

    #[test]
    fn only_edited_fields_are_sent() {
        let original = base();
        let mut draft = original.clone();
        assert!(!draft.dirty(&original));
        assert!(draft.changes(&original).is_empty());
        draft.description = "  Books venues and dates ".into();
        let update = draft.changes(&original);
        assert_eq!(
            update.description.as_deref(),
            Some("Books venues and dates")
        );
        assert!(update.display_name.is_none() && update.model.is_none());
        assert!(draft.dirty(&original));
    }

    #[test]
    fn default_model_and_profile_are_sent_as_null() {
        let mut original = base();
        original.model = model_value("openai", "gpt-x");
        original.browser_profile = "work".into();
        let mut draft = original.clone();
        draft.model = String::new();
        draft.browser_profile = String::new();
        let update = draft.changes(&original);
        assert_eq!(update.model, Some(None));
        assert_eq!(update.browser_profile, Some(None));
        // Picking "default" in the list is the same as the empty choice.
        draft.browser_profile = "default".into();
        assert_eq!(draft.changes(&original).browser_profile, Some(None));
        draft.model = model_value("anthropic", "big");
        assert_eq!(
            draft.changes(&original).model,
            Some(Some(ModelPick {
                provider: "anthropic".into(),
                model: "big".into()
            }))
        );
    }

    #[test]
    fn inline_errors_are_plain_and_block_saving() {
        let mut draft = base();
        assert!(draft.first_error().is_none());
        draft.display_name = "   ".into();
        assert_eq!(draft.name_error().as_deref(), Some("Give the bot a name."));
        draft.display_name = "x".repeat(49);
        assert!(draft.name_error().unwrap().contains("48"));
        draft = base();
        draft.description = "x".repeat(281);
        assert!(draft.description_error().unwrap().contains("280"));
        draft = base();
        draft.instructions = "x".repeat(96_001);
        assert!(draft.instructions_error().unwrap().contains("96,000"));
        assert!(model_pick("").is_none());
        assert!(model_pick("noseparator").is_none());
    }
}
