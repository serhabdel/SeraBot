//! Provider accounts, their sign-ins and the bot route editor, on the console
//! session (v0.49).
//!
//! The Providers page lists the member's accounts and the Add account sheet
//! adds one by key, by browser sign-in or by device code. A sign-in runs on its
//! own thread (like the native sign-in) so the page stays alive and the member
//! can cancel it at once. Every write goes through the one worker, which keeps
//! them ordered. No key is kept past the write: the draft is zeroed on success,
//! on cancel and on drop.

use super::*;
use crate::provider_accounts::{self as accounts, AddMethod, Move};
use crate::provider_flow::{self, FlowEnd, FlowKind, FlowMsg, FlowPhase, ProviderFlow};
use crate::route_chip::FallbackOffer;
use crate::route_editor::{Options, RouteDraft};

/// What the last account write was, so a refusal can be read in context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountOp {
    Add,
    Patch,
    Remove {
        provider: String,
        account: String,
        force: bool,
    },
    Test,
    Quota,
}

/// The Providers page's account state.
#[derive(Default)]
pub struct AccountsUi {
    /// The provider the Add account sheet is open for.
    pub sheet_provider: Option<String>,
    /// How the member is adding this account.
    pub method: Option<AddMethod>,
    pub label: String,
    /// The pasted key. Zeroed on success, on cancel and on drop.
    pub key: String,
    /// The running or finished sign-in the sheet shows.
    pub flow: Option<ProviderFlow>,
    flow_since: Option<Instant>,
    flow_cancel: Option<Arc<AtomicBool>>,
    flow_rx: Option<std::sync::mpsc::Receiver<FlowMsg>>,
    /// An account whose Remove is waiting for its confirm step.
    pub confirm_remove: Option<(String, String)>,
    /// The server refused a Remove because bots use the account; the page
    /// asks whether to remove it anyway.
    pub force_prompt: Option<(String, String, String)>,
    /// `provider/account` of a Test or quota read in flight.
    pub busy: Option<String>,
    /// The last account write, for reading its refusal.
    last_op: Option<AccountOp>,
    /// Shown in the sheet; the page uses the shared error banner.
    pub sheet_error: Option<String>,
}

impl AccountsUi {
    pub fn flow_running(&self) -> bool {
        self.flow
            .as_ref()
            .is_some_and(|flow| !flow.phase.is_final())
    }

    /// Time since the provider's page was issued; zero before that.
    pub fn flow_elapsed(&self) -> Duration {
        self.flow_since.map(|at| at.elapsed()).unwrap_or_default()
    }

    fn stop_flow(&mut self) {
        if let Some(cancel) = self.flow_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        self.flow_rx = None;
        self.flow_since = None;
    }

    fn clear_secrets(&mut self) {
        self.key.zeroize();
        self.key.clear();
    }
}

impl Drop for AccountsUi {
    fn drop(&mut self) {
        self.key.zeroize();
        self.stop_flow();
    }
}

/// The bot route editor's state.
#[derive(Default)]
pub struct RouteEditorUi {
    /// The bot the editor was filled for.
    pub bot: Option<String>,
    pub draft: RouteDraft,
    pub original: RouteDraft,
    pub hydrated: bool,
    pub saving: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
}

impl RouteEditorUi {
    pub fn dirty(&self) -> bool {
        self.hydrated && self.draft.dirty(&self.original)
    }
}

/// One edit of a route row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteChange {
    Provider(String),
    Account(String),
    Model(String),
    /// `""` is the model's default.
    Thinking(String),
}

/// The longest account label the sheet sends.
const LABEL_HINT: &str = "Optional. Helps you tell accounts apart, for example \"Work\".";

impl Session {
    /// What the route pickers can offer, from the live catalog and the
    /// Providers page. Empty slices until they have loaded.
    pub fn route_options(&self) -> Options<'_> {
        let (entries, catalog_providers): (&[crate::CatalogEntry], &[crate::api::Provider]) =
            match &self.state.catalog {
                Some(catalog) => (&catalog.entries, &catalog.providers),
                None => (&[], &[]),
            };
        Options {
            entries,
            catalog_providers,
            providers: &self.state.providers,
        }
    }

    /// The hint under the label field.
    pub fn account_label_hint() -> &'static str {
        LABEL_HINT
    }

    // ---- Add account sheet ----

    /// Open the Add account sheet for one provider with its first method.
    pub fn open_add_account(&mut self, provider: &str) {
        self.accounts_ui.stop_flow();
        self.accounts_ui.clear_secrets();
        self.accounts_ui.flow = None;
        self.accounts_ui.label.clear();
        self.accounts_ui.sheet_error = None;
        self.admin_error = None;
        let methods = self
            .state
            .providers
            .iter()
            .find(|candidate| candidate.id == provider)
            .map(|candidate| {
                let preset = crate::provider_presets::presets().iter().find(|preset| {
                    crate::provider_presets::provider_matches_preset(
                        &candidate.id,
                        &candidate.kind,
                        preset,
                    )
                });
                accounts::add_methods(candidate, preset)
            })
            .unwrap_or_default();
        self.accounts_ui.method = methods.first().copied();
        self.accounts_ui.sheet_provider = Some(provider.to_owned());
    }

    /// Close the sheet. A sign-in still waiting is cancelled first, so its
    /// loopback listener closes.
    pub fn close_add_account(&mut self) {
        self.accounts_ui.stop_flow();
        self.accounts_ui.clear_secrets();
        self.accounts_ui.flow = None;
        self.accounts_ui.label.clear();
        self.accounts_ui.sheet_error = None;
        self.accounts_ui.sheet_provider = None;
        self.accounts_ui.method = None;
    }

    /// Pick how to add the account. Changing it cancels a sign-in in flight.
    pub fn set_add_method(&mut self, method: AddMethod) {
        if self.accounts_ui.method == Some(method) {
            return;
        }
        self.accounts_ui.stop_flow();
        self.accounts_ui.flow = None;
        self.accounts_ui.sheet_error = None;
        self.accounts_ui.method = Some(method);
    }

    /// Add the pasted key as one more account of the sheet's provider.
    pub fn add_account_with_key(&mut self) {
        let Some(provider) = self.accounts_ui.sheet_provider.clone() else {
            return;
        };
        let key = self.accounts_ui.key.trim().to_owned();
        let label = self.accounts_ui.label.trim().to_owned();
        if key.is_empty() {
            self.accounts_ui.sheet_error = Some("Paste the key first.".into());
            return;
        }
        if !valid_credential(&key) {
            self.accounts_ui.sheet_error =
                Some("The key contains control characters or is too long.".into());
            return;
        }
        if !crate::api::valid_account_label(&label) {
            self.accounts_ui.sheet_error = Some(format!(
                "Keep the label to {} characters, without special characters.",
                crate::api::MAX_ACCOUNT_LABEL_CHARS
            ));
            return;
        }
        if !self.can_admin() {
            self.accounts_ui.sheet_error = Some("Wait a moment, then try again.".into());
            return;
        }
        self.accounts_ui.sheet_error = None;
        self.admin_error = None;
        self.accounts_ui.busy = Some(format!("{provider}/new"));
        self.accounts_ui.last_op = Some(AccountOp::Add);
        self.start_request(Operation::AddProviderAccount {
            provider,
            label,
            key: zeroize::Zeroizing::new(key),
        });
    }

    // ---- Sign-in ----

    /// Start the browser or device sign-in for the sheet's provider.
    pub fn start_provider_signin(&mut self, provider: String, kind: String, method: AddMethod) {
        if self.accounts_ui.flow_running() {
            return;
        }
        let flow_kind = match method {
            AddMethod::Browser => FlowKind::Browser,
            AddMethod::Device => FlowKind::Device,
            AddMethod::Key => return,
        };
        let Some(credentials) = self.credentials.clone() else {
            self.accounts_ui.sheet_error = Some("Connect to the server first.".into());
            return;
        };
        self.accounts_ui.stop_flow();
        self.accounts_ui.sheet_error = None;
        self.admin_error = None;
        let (sender, receiver) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        let thread_provider = provider.clone();
        let spawned = std::thread::Builder::new()
            .name("serabot-provider-signin".into())
            .spawn(move || {
                let report = |msg: FlowMsg| {
                    let _ = sender.send(msg);
                };
                let open = |url: &str| webbrowser::open(url).is_ok();
                let end =
                    match ApiClient::new(credentials.endpoint.clone(), credentials.token.clone()) {
                        Err(error) => FlowEnd::Failed(error.to_string()),
                        Ok(client) => match flow_kind {
                            FlowKind::Browser => provider_flow::run_browser(
                                &client,
                                &thread_provider,
                                &kind,
                                &thread_cancel,
                                &report,
                                &open,
                            ),
                            FlowKind::Device => provider_flow::run_device(
                                &client,
                                &thread_provider,
                                &thread_cancel,
                                &report,
                                &open,
                            ),
                        },
                    };
                let _ = sender.send(FlowMsg::Ended(end));
            });
        if spawned.is_err() {
            self.accounts_ui.sheet_error = Some("Could not start the sign-in.".into());
            return;
        }
        self.accounts_ui.flow = Some(ProviderFlow::new(&provider, flow_kind));
        self.accounts_ui.flow_since = None;
        self.accounts_ui.flow_cancel = Some(cancel);
        self.accounts_ui.flow_rx = Some(receiver);
    }

    /// Stop a sign-in the member backed out of.
    pub fn cancel_provider_signin(&mut self) {
        let was_running = self.accounts_ui.flow_running();
        if let Some(cancel) = self.accounts_ui.flow_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        self.accounts_ui.flow_rx = None;
        self.accounts_ui.flow_since = None;
        if was_running && let Some(flow) = self.accounts_ui.flow.as_mut() {
            flow.cancel();
        }
    }

    /// Drain the sign-in thread's messages. Called from the UI timer.
    pub(super) fn poll_provider_flow(&mut self) {
        let Some(receiver) = &self.accounts_ui.flow_rx else {
            return;
        };
        let mut messages = Vec::new();
        while let Ok(message) = receiver.try_recv() {
            messages.push(message);
        }
        let mut finished = false;
        for message in messages {
            if let FlowMsg::Started { .. } = &message {
                self.accounts_ui.flow_since = Some(Instant::now());
            }
            if let FlowMsg::Ended(end) = &message {
                finished = true;
                if matches!(end, FlowEnd::Done { .. }) {
                    // The new account shows on the page at once.
                    self.state.providers_loaded = false;
                    self.schedule(Operation::Catalog, Duration::ZERO);
                    self.admin_notice = Some("Account added.".into());
                }
            }
            if let Some(flow) = self.accounts_ui.flow.as_mut() {
                flow.apply(message);
            }
        }
        if finished {
            self.accounts_ui.flow_rx = None;
            self.accounts_ui.flow_cancel = None;
        }
    }

    // ---- Account rows ----

    fn account_write_allowed(&mut self) -> bool {
        if !self.state.connected {
            return false;
        }
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return false;
        }
        self.admin_error = None;
        self.admin_notice = None;
        true
    }

    /// Switch an account on or off. Bots skip a switched-off account.
    pub fn set_account_disabled(&mut self, provider: &str, account: &str, disabled: bool) {
        if !self.account_write_allowed() {
            return;
        }
        self.accounts_ui.last_op = Some(AccountOp::Patch);
        // Show the switch move at once; the reload that follows corrects it.
        if let Some(row) = self.account_row_mut(provider, account) {
            if disabled {
                row.status = "disabled".into();
            } else if row.status == "disabled" {
                row.status = "ok".into();
            }
        }
        self.start_request(Operation::PatchProviderAccount {
            provider: provider.to_owned(),
            account: account.to_owned(),
            patch: crate::api::AccountPatch {
                disabled: Some(disabled),
                ..Default::default()
            },
        });
    }

    /// Move an account up or down the order bots try them in. The writes go
    /// out together, in order, so the page never shows half a move.
    pub fn move_account(&mut self, provider: &str, account: &str, direction: Move) {
        if !self.state.connected {
            return;
        }
        let Some(status) = self.state.providers.iter().find(|p| p.id == provider) else {
            return;
        };
        let writes = accounts::priority_moves(&status.accounts, account, direction);
        if writes.is_empty() {
            return;
        }
        self.admin_error = None;
        self.accounts_ui.last_op = Some(AccountOp::Patch);
        for (id, priority) in writes {
            // The new order shows at once; the reload that follows corrects it.
            if let Some(row) = self.account_row_mut(provider, &id) {
                row.priority = priority;
            }
            self.start_request(Operation::PatchProviderAccount {
                provider: provider.to_owned(),
                account: id,
                patch: crate::api::AccountPatch {
                    priority: Some(priority),
                    ..Default::default()
                },
            });
        }
    }

    fn account_row_mut(
        &mut self,
        provider: &str,
        account: &str,
    ) -> Option<&mut crate::api::ProviderAccount> {
        self.state
            .providers
            .iter_mut()
            .find(|candidate| candidate.id == provider)?
            .accounts
            .iter_mut()
            .find(|row| row.id == account)
    }

    /// Ask for a confirm step before removing an account.
    pub fn ask_remove_account(&mut self, provider: &str, account: &str) {
        self.accounts_ui.force_prompt = None;
        self.accounts_ui.confirm_remove = Some((provider.to_owned(), account.to_owned()));
    }

    pub fn cancel_remove_account(&mut self) {
        self.accounts_ui.confirm_remove = None;
        self.accounts_ui.force_prompt = None;
    }

    /// Remove an account and its sealed credential. Without `force` the server
    /// refuses while a bot pins it; the page then offers "Remove anyway".
    pub fn remove_account(&mut self, provider: &str, account: &str, force: bool) {
        if !self.account_write_allowed() {
            return;
        }
        self.accounts_ui.confirm_remove = None;
        self.accounts_ui.force_prompt = None;
        self.accounts_ui.last_op = Some(AccountOp::Remove {
            provider: provider.to_owned(),
            account: account.to_owned(),
            force,
        });
        self.start_request(Operation::RemoveProviderAccount {
            provider: provider.to_owned(),
            account: account.to_owned(),
            force,
        });
    }

    /// Ask the provider for its model list with one account's credential.
    pub fn test_account(&mut self, provider: &str, account: &str) {
        if !self.account_write_allowed() {
            return;
        }
        self.accounts_ui.busy = Some(format!("{provider}/{account}"));
        self.accounts_ui.last_op = Some(AccountOp::Test);
        self.start_request(Operation::TestProviderAccount {
            provider: provider.to_owned(),
            account: account.to_owned(),
        });
    }

    /// Read the account's quota again (the server allows it once per 4 minutes).
    pub fn refresh_account_quota(&mut self, provider: &str, account: &str) {
        if !self.account_write_allowed() {
            return;
        }
        self.accounts_ui.busy = Some(format!("{provider}/{account}"));
        self.accounts_ui.last_op = Some(AccountOp::Quota);
        self.start_request(Operation::RefreshAccountQuota {
            provider: provider.to_owned(),
            account: account.to_owned(),
        });
    }

    /// An account write came back fine.
    pub(super) fn account_write_done(&mut self, added: Option<crate::api::AccountAdded>) {
        self.accounts_ui.busy = None;
        self.admin_error = None;
        self.accounts_ui.sheet_error = None;
        let last = self.accounts_ui.last_op.take();
        if let Some(added) = added {
            self.accounts_ui.clear_secrets();
            self.accounts_ui.label.clear();
            self.accounts_ui.sheet_provider = None;
            self.accounts_ui.method = None;
            self.admin_notice = Some(match (added.created, added.verified) {
                (false, _) => "You already had that key. Nothing was added.".to_owned(),
                (true, true) => "Account added. The provider accepted the key.".to_owned(),
                (true, false) => {
                    "Account added. It could not be checked yet; use Test to check it.".to_owned()
                }
            });
        } else if matches!(last, Some(AccountOp::Remove { .. })) {
            self.admin_notice = Some("Account removed.".into());
        }
        self.state.providers_loaded = false;
        self.schedule(Operation::Catalog, Duration::ZERO);
    }

    /// An account write failed: say why, in the right place.
    pub(super) fn account_write_failed(&mut self, error: &ApiError) {
        self.accounts_ui.busy = None;
        self.scheduled = None;
        let message = match error {
            ApiError::Http(404) => {
                "That account is gone, or this server has no accounts yet. Refresh the page."
                    .to_owned()
            }
            other => other.to_string(),
        };
        let last = self.accounts_ui.last_op.take();
        if let Some(AccountOp::Remove {
            provider,
            account,
            force: false,
        }) = &last
            && accounts::is_pinned_refusal(&message)
        {
            self.accounts_ui.force_prompt = Some((provider.clone(), account.clone(), message));
            return;
        }
        if self.accounts_ui.sheet_provider.is_some() {
            self.accounts_ui.sheet_error = Some(message);
        } else {
            self.admin_error = Some(message);
        }
    }

    // ---- Bot route editor ----

    /// Fill the editor from the bot's record once it has arrived. The pump
    /// calls this on every tick; it does nothing after the first fill.
    pub(super) fn hydrate_route_editor(&mut self) {
        if !self.bot_settings.open {
            return;
        }
        let Some(id) = self.bot_settings.bot.clone() else {
            return;
        };
        if self.route_editor.bot.as_deref() == Some(id.as_str()) && self.route_editor.hydrated {
            return;
        }
        let Some(detail) = self.state.bot_details.get(&id) else {
            // A different bot than the one the editor shows: start clean.
            if self.route_editor.bot.as_deref() != Some(id.as_str()) {
                self.route_editor = RouteEditorUi::default();
            }
            return;
        };
        let draft = RouteDraft::from_view(detail.route.as_deref());
        self.route_editor = RouteEditorUi {
            bot: Some(id),
            original: draft.clone(),
            draft,
            hydrated: true,
            ..RouteEditorUi::default()
        };
    }

    /// Edit one row of the route editor. Row 0 is the primary, 1 and 2 are the
    /// fallbacks. A change that makes a ticked consent meaningless clears it.
    pub fn route_change(&mut self, row: usize, change: RouteChange) {
        let mut draft = std::mem::take(&mut self.route_editor.draft);
        {
            let options = self.route_options();
            if let Some(slot) = draft.slot_mut(row) {
                match change {
                    RouteChange::Provider(provider) => slot.set_provider(&provider, &options),
                    RouteChange::Account(account) => slot.account = account,
                    RouteChange::Model(model) => slot.set_model(&model, &options),
                    RouteChange::Thinking(level) => slot.thinking = level,
                }
            }
        }
        for index in 0..draft.fallbacks.len() {
            if !draft.is_cross_provider(index) {
                draft.consent[index] = false;
            }
        }
        self.route_editor.draft = draft;
        self.route_editor.error = None;
        self.route_editor.notice = None;
    }

    /// Put the editor back to what the server has.
    pub fn reset_route_editor(&mut self) {
        self.route_editor.draft = self.route_editor.original.clone();
        self.route_editor.error = None;
        self.route_editor.notice = None;
    }

    /// Check and save the bot's route chain.
    pub fn save_bot_route(&mut self) {
        let Some(id) = self.route_editor.bot.clone() else {
            return;
        };
        if self.route_editor.saving || !self.route_editor.hydrated {
            return;
        }
        let check = self.route_editor.draft.check(&self.route_options());
        if let Some(message) = check.first() {
            self.route_editor.error = Some(message.to_owned());
            return;
        }
        if !self.route_editor.dirty() {
            self.route_editor.notice = Some("Nothing to save.".into());
            return;
        }
        if !self.can_admin() {
            self.route_editor.error = Some("Wait a moment, then press Save again.".into());
            return;
        }
        self.route_editor.error = None;
        self.route_editor.notice = None;
        self.route_editor.saving = true;
        let update = self.route_editor.draft.to_update();
        self.start_request(Operation::SaveBotRoute { id, update });
        if self.pending.is_none() && self.backlog.is_empty() {
            self.route_editor.saving = false;
            self.route_editor.error = Some("Network worker unavailable. Reconnect.".into());
        }
    }

    pub(super) fn route_saved(&mut self) {
        self.route_editor.saving = false;
        self.route_editor.error = None;
        self.route_editor.notice = Some("Route saved. It applies to the next turn.".into());
        self.route_editor.original = self.route_editor.draft.clone();
        // The effective route and the last switch come from the record.
        if let Some(id) = self.route_editor.bot.clone() {
            self.state.bot_details.remove(&id);
            self.load_bot_detail(id);
        }
        self.state.route_events_stale = true;
    }

    pub(super) fn route_save_failed(&mut self, error: &ApiError) {
        self.route_editor.saving = false;
        self.route_editor.notice = None;
        self.route_editor.error = Some(match error {
            ApiError::Http(404) => {
                "This server has no route editor yet. Update the server.".to_owned()
            }
            other => other.to_string(),
        });
    }

    // ---- Composer route chip ----

    /// Read the open conversation's route switches and its bot's chain, once
    /// per conversation and again after the server records a switch.
    pub(super) fn poll_route_events(&mut self) {
        if !self.state.connected
            || !self.state.history_loaded
            || self.state.unknown_outcome
            || !self.ready()
            || !(self.state.route_events_stale || !self.state.route_events_asked)
        {
            return;
        }
        self.state.route_events_asked = true;
        self.state.route_events_stale = false;
        let bot = self.conversation_bot(&self.conversation);
        self.start_request(Operation::RouteEvents { bot });
    }

    /// "Retry on <fallback>": pick that route for the conversation, then send
    /// the last message again once the pick has landed.
    pub fn retry_on_fallback(&mut self, offer: FallbackOffer) {
        let Some(text) = self
            .state
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::User && !message.id.starts_with("opt-"))
            .map(|message| message.content.clone())
        else {
            return;
        };
        if self.pending_retry.is_some() {
            return;
        }
        self.state.error = None;
        self.state.chat_failure = None;
        self.pending_retry = Some(text);
        self.apply_model(offer.selection);
        if self.state.error.is_some() {
            self.pending_retry = None;
        }
    }

    /// A route pick landed: if a retry was waiting on it, send the message.
    pub(super) fn after_route_applied(&mut self) {
        if let Some(text) = self.pending_retry.take() {
            self.composer = text;
            self.submit();
        }
        self.state.route_events_stale = true;
    }

    /// A pick that failed ends a waiting retry; the text stays in the chat.
    pub(super) fn route_apply_failed(&mut self) {
        self.pending_retry = None;
    }

    /// Whether the account sheet's flow is in a state that shows a final line.
    pub fn flow_is_final(&self) -> bool {
        self.accounts_ui
            .flow
            .as_ref()
            .is_some_and(|flow| flow.phase.is_final())
    }

    /// Whether the sign-in finished with a new or renewed account.
    pub fn flow_succeeded(&self) -> bool {
        matches!(
            self.accounts_ui.flow.as_ref().map(|flow| &flow.phase),
            Some(FlowPhase::Done { .. })
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected_session() -> Session {
        let mut session = Session::new();
        session.state.connected = true;
        session
    }

    #[test]
    fn a_key_draft_is_validated_before_anything_is_sent() {
        let mut session = connected_session();
        session.accounts_ui.sheet_provider = Some("openai".into());
        session.add_account_with_key();
        assert_eq!(
            session.accounts_ui.sheet_error.as_deref(),
            Some("Paste the key first.")
        );
        session.accounts_ui.key = "sk-synthetic\u{7}".into();
        session.add_account_with_key();
        assert!(
            session
                .accounts_ui
                .sheet_error
                .as_deref()
                .unwrap()
                .contains("control characters")
        );
        session.accounts_ui.key = "sk-synthetic".into();
        session.accounts_ui.label = "x".repeat(200);
        session.add_account_with_key();
        assert!(
            session
                .accounts_ui
                .sheet_error
                .as_deref()
                .unwrap()
                .contains("Keep the label")
        );
        // No worker, nothing in flight.
        assert!(session.pending.is_none());
    }

    #[test]
    fn closing_the_sheet_wipes_the_key_and_cancels_the_flow() {
        let mut session = connected_session();
        session.accounts_ui.sheet_provider = Some("openai".into());
        session.accounts_ui.key = "sk-synthetic".into();
        let cancel = Arc::new(AtomicBool::new(false));
        session.accounts_ui.flow_cancel = Some(Arc::clone(&cancel));
        session.accounts_ui.flow = Some(ProviderFlow::new("openai", FlowKind::Device));
        session.close_add_account();
        assert!(session.accounts_ui.key.is_empty());
        assert!(cancel.load(Ordering::SeqCst), "the runner is told to stop");
        assert!(session.accounts_ui.sheet_provider.is_none());
        assert!(session.accounts_ui.flow.is_none());
    }

    #[test]
    fn a_finished_add_clears_the_draft_and_says_what_happened() {
        let mut session = connected_session();
        session.accounts_ui.sheet_provider = Some("openai".into());
        session.accounts_ui.key = "sk-synthetic".into();
        session.account_write_done(Some(crate::api::AccountAdded {
            id: "a1".into(),
            created: true,
            verified: true,
        }));
        assert!(session.accounts_ui.key.is_empty());
        assert!(session.accounts_ui.sheet_provider.is_none());
        assert!(
            session
                .admin_notice
                .as_deref()
                .unwrap()
                .contains("accepted the key")
        );
        assert!(!session.state.providers_loaded);
        session.account_write_done(Some(crate::api::AccountAdded {
            id: "a1".into(),
            created: false,
            verified: false,
        }));
        assert!(
            session
                .admin_notice
                .as_deref()
                .unwrap()
                .contains("already had")
        );
    }

    #[test]
    fn a_switch_and_a_move_show_at_once_before_the_server_answers() {
        use crate::api::{ProviderAccount, ProviderStatus};
        let mut session = connected_session();
        let account = |id: &str, priority: i64| ProviderAccount {
            id: id.into(),
            label: id.into(),
            status: "ok".into(),
            priority,
            ..ProviderAccount::default()
        };
        session.state.providers = vec![ProviderStatus {
            id: "openai".into(),
            kind: "openai".into(),
            accounts: vec![account("a", 2), account("b", 1)],
            ..ProviderStatus::default()
        }];
        session.set_account_disabled("openai", "a", true);
        assert_eq!(session.state.providers[0].accounts[0].status, "disabled");
        session.set_account_disabled("openai", "a", false);
        assert_eq!(session.state.providers[0].accounts[0].status, "ok");
        // Moving b above a swaps their priorities in the list straight away.
        session.move_account("openai", "b", Move::Up);
        let rows = &session.state.providers[0].accounts;
        assert_eq!((rows[0].priority, rows[1].priority), (1, 2));
        // A move at the top does nothing.
        session.move_account("openai", "b", Move::Up);
        let rows = &session.state.providers[0].accounts;
        assert_eq!((rows[0].priority, rows[1].priority), (1, 2));
    }

    #[test]
    fn a_pinned_account_refusal_becomes_a_remove_anyway_prompt() {
        let mut session = connected_session();
        session.accounts_ui.last_op = Some(AccountOp::Remove {
            provider: "openai".into(),
            account: "a1".into(),
            force: false,
        });
        session.account_write_failed(&ApiError::Rejected(
            "These bots use this account: notes. Remove it anyway to let them use any account."
                .into(),
        ));
        let prompt = session.accounts_ui.force_prompt.clone().unwrap();
        assert_eq!((prompt.0.as_str(), prompt.1.as_str()), ("openai", "a1"));
        assert!(
            session.admin_error.is_none(),
            "the prompt replaces the banner"
        );
        // The same refusal after a forced remove is an ordinary error.
        session.accounts_ui.force_prompt = None;
        session.accounts_ui.last_op = Some(AccountOp::Remove {
            provider: "openai".into(),
            account: "a1".into(),
            force: true,
        });
        session.account_write_failed(&ApiError::Rejected("Something else broke".into()));
        assert_eq!(session.admin_error.as_deref(), Some("Something else broke"));
    }

    #[test]
    fn a_failed_add_stays_in_the_open_sheet() {
        let mut session = connected_session();
        session.accounts_ui.sheet_provider = Some("openai".into());
        session.accounts_ui.last_op = Some(AccountOp::Add);
        session.account_write_failed(&ApiError::Rejected(
            "The provider did not accept this key".into(),
        ));
        assert_eq!(
            session.accounts_ui.sheet_error.as_deref(),
            Some("The provider did not accept this key")
        );
        assert!(session.admin_error.is_none());
    }

    #[test]
    fn the_route_editor_fills_from_the_record_and_saves_only_valid_changes() {
        let mut session = connected_session();
        session.bot_settings.open = true;
        session.bot_settings.bot = Some("notes".into());
        // Nothing to fill from yet.
        session.hydrate_route_editor();
        assert!(!session.route_editor.hydrated);
        let detail: crate::BotDetail = serde_json::from_str(
            r#"{"id":"notes","display_name":"Notes","role":"specialist","description":"",
                "enabled":true,"provider":"openai","model":"gpt-5",
                "route":{"route":{"provider":"openai","model":"gpt-5"},"fallbacks":[],
                         "failover":{"enabled":true,"switch_account_first":true,
                                     "max_switches":3,"allow_cross_provider":false}}}"#,
        )
        .unwrap();
        session.state.bot_details.insert("notes".into(), detail);
        session.hydrate_route_editor();
        assert!(session.route_editor.hydrated);
        assert_eq!(session.route_editor.draft.primary.model, "gpt-5");
        assert!(!session.route_editor.dirty());
        // A fallback naming a provider nobody can use is refused inline.
        session.route_editor.draft.fallbacks[0] = crate::route_editor::RouteSlot {
            provider: "nowhere".into(),
            model: "m".into(),
            ..Default::default()
        };
        session.save_bot_route();
        assert!(
            session
                .route_editor
                .error
                .as_deref()
                .unwrap()
                .contains("not available to you")
        );
        assert!(!session.route_editor.saving);
        // Cancel puts back what the server has.
        session.reset_route_editor();
        assert!(!session.route_editor.dirty());
        assert!(session.route_editor.error.is_none());
        // Another bot starts clean.
        session.bot_settings.bot = Some("other".into());
        session.hydrate_route_editor();
        assert!(session.route_editor.bot.is_none() && !session.route_editor.hydrated);
    }

    #[test]
    fn editing_a_row_keeps_the_draft_consistent() {
        let mut session = connected_session();
        session.route_editor.bot = Some("notes".into());
        session.route_editor.hydrated = true;
        session.route_editor.error = Some("old".into());
        // With no catalog the pickers are empty, but edits still land.
        session.route_change(0, RouteChange::Provider("openai".into()));
        assert_eq!(session.route_editor.draft.primary.provider, "openai");
        assert!(session.route_editor.error.is_none());
        session.route_change(1, RouteChange::Provider("anthropic".into()));
        session.route_editor.draft.consent[0] = true;
        assert!(session.route_editor.draft.is_cross_provider(0));
        // Moving the fallback onto the primary's provider drops the tick.
        session.route_change(1, RouteChange::Provider("openai".into()));
        assert!(!session.route_editor.draft.consent[0]);
        session.route_change(2, RouteChange::Account("a2".into()));
        assert_eq!(session.route_editor.draft.fallbacks[1].account, "a2");
        session.route_change(2, RouteChange::Thinking("high".into()));
        assert_eq!(session.route_editor.draft.fallbacks[1].thinking, "high");
        // A row that does not exist is ignored.
        session.route_change(7, RouteChange::Model("x".into()));
        // Clearing the primary clears its row.
        session.route_change(0, RouteChange::Provider(String::new()));
        assert!(session.route_editor.draft.primary.is_empty());
    }

    #[test]
    fn a_saved_route_becomes_the_new_baseline() {
        let mut session = connected_session();
        session.route_editor.bot = Some("notes".into());
        session.route_editor.hydrated = true;
        session.route_editor.saving = true;
        session.route_editor.draft.failover.enabled = false;
        session.route_saved();
        assert!(!session.route_editor.saving);
        assert!(!session.route_editor.dirty());
        assert!(
            session
                .route_editor
                .notice
                .as_deref()
                .unwrap()
                .contains("next turn")
        );
        assert!(session.state.route_events_stale);
        session.route_save_failed(&ApiError::Rejected("A route can appear only once".into()));
        assert_eq!(
            session.route_editor.error.as_deref(),
            Some("A route can appear only once")
        );
        assert!(session.route_editor.notice.is_none());
    }

    #[test]
    fn a_retry_waits_for_the_pick_and_then_resends_the_last_message() {
        let mut session = connected_session();
        session.state.messages.push(Message {
            id: "m1".into(),
            role: Role::User,
            content: "hello".into(),
            created_at: "2026-11-16T10:00:00Z".into(),
            name: None,
            bot: None,
            attachments: Vec::new(),
            limit_reached: false,
        });
        // With no catalog the pick is refused, and nothing waits.
        session.retry_on_fallback(FallbackOffer {
            selection: crate::Selection::auto(),
            thinking: None,
            label: "Retry on X".into(),
        });
        assert!(session.pending_retry.is_none());
        // A waiting retry is dropped when the pick fails.
        session.pending_retry = Some("hello".into());
        session.route_apply_failed();
        assert!(session.pending_retry.is_none());
        // And a landed pick with nothing waiting changes nothing but the stale flag.
        session.after_route_applied();
        assert!(session.composer.is_empty());
        assert!(session.state.route_events_stale);
    }
}
