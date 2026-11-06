//! Create-a-bot, create-a-group, avatars, the onboarding interview and the
//! capability toggles (0.15). These are multi-step flows on top of the
//! one-request-at-a-time worker, so each step schedules the next from the
//! completion handler in `session.rs`.

use super::*;
use crate::avatars::{self, AvatarTarget};
use crate::capabilities;

/// An image the owner picked but has not uploaded yet.
pub struct PickedImage {
    pub bytes: Vec<u8>,
    /// `data:` URL for the preview tile in the sheet.
    pub preview: String,
}

#[derive(Default)]
pub struct NewBotDraft {
    pub name: String,
    /// Model id, or empty for the account default.
    pub model: String,
    /// Room conversation id, or empty for no group.
    pub group: String,
    pub avatar: Option<PickedImage>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct NewGroupDraft {
    pub name: String,
    pub description: String,
    pub rules: String,
    /// `provider|model`, or empty for the account default.
    pub model: String,
    pub members: Vec<String>,
    pub avatar: Option<PickedImage>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct OnboardingUi {
    pub open: bool,
    pub bot: String,
    pub input: String,
    /// A reply or confirm is in flight; the composer waits.
    pub waiting: bool,
    pub error: Option<String>,
}

/// Where a picked image goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageSlot {
    NewBot,
    NewGroup,
    /// Upload straight away to an existing bot.
    Bot(String),
}

/// What the avatar request in flight is, so a failure can be handled quietly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AvatarOp {
    Fetch(AvatarTarget, Option<String>),
    Write,
}

impl Session {
    // ------------------------------------------------------------- images --

    /// Check and keep an image picked from disk.
    pub fn pick_image(&mut self, slot: ImageSlot, bytes: Vec<u8>) {
        let mime = match crate::check_avatar_upload(&bytes) {
            Ok(mime) => mime,
            Err(error) => {
                self.set_slot_error(&slot, error.to_string());
                return;
            }
        };
        match slot {
            ImageSlot::NewBot => {
                self.new_bot.error = None;
                self.new_bot.avatar = Some(PickedImage {
                    preview: avatars::preview_data_url(mime, &bytes),
                    bytes,
                });
            }
            ImageSlot::NewGroup => {
                self.new_group.error = None;
                self.new_group.avatar = Some(PickedImage {
                    preview: avatars::preview_data_url(mime, &bytes),
                    bytes,
                });
            }
            ImageSlot::Bot(id) => {
                if !self.ready() {
                    self.toast = Some("Wait for the current request, then retry.".into());
                    return;
                }
                self.avatar_upload = None;
                self.start_request(Operation::AvatarUpload {
                    target: AvatarTarget::Bot(id),
                    bytes,
                });
            }
        }
    }

    fn set_slot_error(&mut self, slot: &ImageSlot, message: String) {
        match slot {
            ImageSlot::NewBot => self.new_bot.error = Some(message),
            ImageSlot::NewGroup => self.new_group.error = Some(message),
            ImageSlot::Bot(_) => self.toast = Some(message),
        }
    }

    pub fn clear_image(&mut self, slot: ImageSlot) {
        match slot {
            ImageSlot::NewBot => self.new_bot.avatar = None,
            ImageSlot::NewGroup => self.new_group.avatar = None,
            ImageSlot::Bot(id) => {
                if self.ready() {
                    self.start_request(Operation::AvatarRemove {
                        target: AvatarTarget::Bot(id),
                    });
                } else {
                    self.toast = Some("Wait for the current request, then retry.".into());
                }
            }
        }
    }

    /// Fetch the next avatar the server has and the cache lacks. Called after
    /// every bot or conversation list and after every fetch, so the cache
    /// fills one request at a time.
    pub(super) fn schedule_next_avatar(&mut self) {
        if self.scheduled.is_some() || !self.state.connected {
            return;
        }
        // Forget images the server no longer has.
        let live: Vec<String> = self
            .state
            .listed_bots
            .iter()
            .filter(|bot| bot.avatar_version.is_some())
            .map(|bot| AvatarTarget::Bot(bot.id.clone()).key())
            .chain(
                self.state
                    .conversations
                    .iter()
                    .filter(|conversation| conversation.avatar_version.is_some())
                    .map(|conversation| AvatarTarget::Group(conversation.id.clone()).key()),
            )
            .collect();
        let before = self.state.avatars.len();
        if self.state.bots_loaded && self.state.conversations_loaded {
            self.state.avatars.retain(|key, _| live.contains(key));
        }
        if self.state.avatars.len() != before {
            avatars::publish(&self.state.avatars);
        }
        let wanted = self
            .state
            .listed_bots
            .iter()
            .filter_map(|bot| {
                bot.avatar_version
                    .clone()
                    .map(|version| (AvatarTarget::Bot(bot.id.clone()), version))
            })
            .chain(self.state.conversations.iter().filter_map(|conversation| {
                conversation
                    .avatar_version
                    .clone()
                    .map(|version| (AvatarTarget::Group(conversation.id.clone()), version))
            }))
            .find(|(target, version)| {
                self.state
                    .avatars
                    .get(&target.key())
                    .is_none_or(|have| have.version.as_deref() != Some(version.as_str()))
            });
        if let Some((target, version)) = wanted {
            self.schedule(
                Operation::AvatarFetch {
                    target,
                    version: Some(version),
                },
                Duration::ZERO,
            );
        }
    }

    // --------------------------------------------------------------- bots --

    /// Validate the create-bot sheet and send it. The sheet closes only on
    /// success, so a refused name is fixed in place.
    pub fn submit_new_bot(&mut self) -> bool {
        let name = self.new_bot.name.trim().to_owned();
        if !crate::valid_display_name(&name) {
            self.new_bot.error = Some("Give the bot a name of 1 to 60 characters.".into());
            return false;
        }
        if !self.ready() {
            self.new_bot.error = Some("Wait for the current request, then retry.".into());
            return false;
        }
        self.new_bot.error = None;
        let model = (!self.new_bot.model.is_empty()).then(|| self.new_bot.model.clone());
        let group = (!self.new_bot.group.is_empty()).then(|| self.new_bot.group.clone());
        self.creating_named_bot = true;
        self.start_request(Operation::CreateNamedBot { name, model, group });
        self.pending.is_some()
    }

    /// The server created the bot: queue its image, then open its interview.
    pub(super) fn bot_created(&mut self) {
        self.creating_named_bot = false;
        let Some(created) = self.state.created_bot.clone() else {
            return;
        };
        let draft = std::mem::take(&mut self.new_bot);
        self.avatar_upload = draft
            .avatar
            .map(|image| (AvatarTarget::Bot(created.id.clone()), image.bytes));
        self.selected_bot = None;
        self.section = Section::Chat;
        self.onboarding_ui = OnboardingUi {
            open: true,
            bot: created.id.clone(),
            waiting: true,
            ..OnboardingUi::default()
        };
        self.state.onboarding = None;
        self.schedule(Operation::ListBots, Duration::ZERO);
    }

    /// After a bot list: upload a queued image, load the interview, or keep
    /// filling the avatar cache.
    pub(super) fn continue_after_bots(&mut self) {
        if self.scheduled.is_some() {
            return;
        }
        if let Some((target, bytes)) = self.avatar_upload.take() {
            self.schedule(Operation::AvatarUpload { target, bytes }, Duration::ZERO);
            return;
        }
        if self.onboarding_ui.open
            && self
                .state
                .onboarding
                .as_ref()
                .is_none_or(|current| current.bot != self.onboarding_ui.bot)
        {
            let id = self.onboarding_ui.bot.clone();
            self.schedule(Operation::OnboardingLoad { id }, Duration::ZERO);
            return;
        }
        self.schedule_next_avatar();
    }

    // ------------------------------------------------------------- groups --

    /// Validate the create-group sheet and send it.
    pub fn submit_new_group(&mut self) -> bool {
        let name = self.new_group.name.trim().to_owned();
        if !crate::valid_display_name(&name) {
            self.new_group.error = Some("Give the group a name of 1 to 60 characters.".into());
            return false;
        }
        if !self.ready() {
            self.new_group.error = Some("Wait for the current request, then retry.".into());
            return false;
        }
        let model = self
            .new_group
            .model
            .split_once('|')
            .map(|(provider, model)| Selection {
                provider: provider.to_owned(),
                model: model.to_owned(),
            });
        let group = NewGroup {
            name,
            description: self.new_group.description.trim().to_owned(),
            members: self.new_group.members.clone(),
            rules: self.new_group.rules.trim().to_owned(),
            model,
        };
        self.new_group.error = None;
        self.start_request(Operation::CreateGroup(group));
        self.pending.is_some()
    }

    /// The server created the group: upload its image, open the room.
    pub(super) fn group_created(&mut self) {
        let Some(created) = self.state.created_group.clone() else {
            return;
        };
        let draft = std::mem::take(&mut self.new_group);
        if let Some(image) = draft.avatar {
            self.avatar_upload = Some((AvatarTarget::Group(created.id.clone()), image.bytes));
        }
        self.pending_open_group = Some(created.id);
        self.schedule(
            Operation::ListConversations {
                include_archived: false,
            },
            Duration::ZERO,
        );
    }

    /// After the conversation list that follows a group create.
    pub(super) fn continue_after_group_list(&mut self) {
        if let Some((target, bytes)) = self.avatar_upload.take() {
            self.schedule(Operation::AvatarUpload { target, bytes }, Duration::ZERO);
            return;
        }
        if let Some(id) = self.pending_open_group.take() {
            self.open_conversation(id);
        }
    }

    // --------------------------------------------------------- onboarding --

    /// Open the interview of an existing bot, starting it if needed.
    pub fn open_onboarding(&mut self, id: String) {
        if !valid_speaker(&id) || !self.ready() {
            return;
        }
        self.section = Section::Chat;
        self.selected_bot = None;
        self.onboarding_ui = OnboardingUi {
            open: true,
            bot: id.clone(),
            waiting: true,
            ..OnboardingUi::default()
        };
        self.state.onboarding = None;
        self.start_request(Operation::OnboardingStart { id });
    }

    pub fn close_onboarding(&mut self) {
        self.onboarding_ui = OnboardingUi::default();
    }

    pub fn send_onboarding_reply(&mut self) {
        let text = self.onboarding_ui.input.trim().to_owned();
        if text.is_empty() || self.onboarding_ui.waiting || !self.ready() {
            return;
        }
        let id = self.onboarding_ui.bot.clone();
        // Show the owner's words at once; the server's transcript replaces
        // this copy when the reply lands.
        if let Some(current) = self.state.onboarding.as_mut() {
            current.messages.push(crate::OnboardingLine {
                from: "user".into(),
                text: text.clone(),
            });
        }
        self.onboarding_ui.input.clear();
        self.onboarding_ui.waiting = true;
        self.onboarding_ui.error = None;
        self.start_request(Operation::OnboardingReply {
            id,
            text: zeroize::Zeroizing::new(text),
        });
    }

    pub fn confirm_onboarding(&mut self) {
        if self.onboarding_ui.waiting || !self.ready() {
            return;
        }
        let id = self.onboarding_ui.bot.clone();
        self.onboarding_ui.waiting = true;
        self.onboarding_ui.error = None;
        self.start_request(Operation::OnboardingConfirm { id });
    }

    /// An interview step failed. The error stays in the chat; a write is
    /// followed by a read so the screen shows what the server actually has.
    pub(super) fn onboarding_failed(&mut self, kind: OperationKind, message: String) {
        self.onboarding_ui.waiting = false;
        self.onboarding_ui.error = Some(message);
        if kind == OperationKind::OnboardingWrite && self.onboarding_ui.open {
            let id = self.onboarding_ui.bot.clone();
            self.schedule(Operation::OnboardingLoad { id }, Duration::from_secs(1));
        }
    }

    // ------------------------------------------------------- capabilities --

    /// The tool list of a listed bot.
    pub fn bot_tools(&self, id: &str) -> Vec<String> {
        self.state
            .listed_bots
            .iter()
            .find(|bot| bot.id == id)
            .map(|bot| bot.tools.clone())
            .unwrap_or_default()
    }

    /// Switch one plain-language capability on or off for a bot.
    pub fn toggle_capability(&mut self, id: String, index: usize, on: bool) {
        let Some(capability) = capabilities::CAPABILITIES.get(index) else {
            return;
        };
        if !valid_speaker(&id) {
            return;
        }
        if !self.ready() {
            self.toast = Some("Wait for the current request, then retry.".into());
            return;
        }
        let tools = capabilities::toggled(capability, &self.bot_tools(&id), on);
        if let Err(error) = validate_tool_list(&tools) {
            self.toast = Some(error.to_string());
            return;
        }
        self.start_request(Operation::SetBotTools { id, tools });
    }
}
