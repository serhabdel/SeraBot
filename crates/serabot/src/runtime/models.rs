//! Per-conversation model catalog and selection.

use super::*;

/// Longest the first message after a restart waits for a provider's live
/// model list before falling back to the configured allowlist.
const ENSURE_CATALOG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

impl Runtime {
    pub async fn refresh_model_catalogs(&self, user_id: &str) -> Result<()> {
        for report in self.router().refresh_catalogs(user_id).await? {
            if !report.fetched {
                continue;
            }
            self.store.record_audit(
                user_id,
                "model.catalog_refreshed",
                None,
                None,
                None,
                None,
                Some(self.gateway.policy().version()),
                Some(&json!({
                    "provider": report.provider,
                    "verified": report.verified,
                    "model_count": report.model_count,
                })),
            )?;
        }
        Ok(())
    }

    /// Load the acting user's live model lists when they have none yet (the
    /// first message after a restart), so validation compares against what
    /// the provider really offers and not the short configured allowlist.
    /// Bounded and infallible: on any problem the allowlist stays in force.
    pub async fn ensure_model_catalogs(&self, user_id: &str) {
        self.router()
            .ensure_catalogs(user_id, ENSURE_CATALOG_TIMEOUT)
            .await;
    }

    /// Whether `error` means the chosen model is simply not on offer (as
    /// opposed to a missing credential or a policy block).
    fn is_model_gone(error: &SeraError) -> bool {
        matches!(
            error,
            SeraError::Rejected {
                code: "model_unavailable" | "unknown_provider",
                ..
            }
        )
    }

    /// A route the account can use right now when its chosen one is gone:
    /// its default route, then the bot's or server default, then the first
    /// routable model of any provider they hold.
    fn fallback_selection(&self, user_id: &str, bot_hint: Option<&str>) -> Option<ModelSelection> {
        let candidates = [
            self.default_route(user_id).ok().flatten(),
            Some(self.router().default_selection(user_id, bot_hint)),
            Some(self.router().default_selection(user_id, None)),
            self.router().first_approved(user_id),
        ];
        candidates.into_iter().flatten().find(|candidate| {
            !candidate.is_auto_mode()
                && self.router().validate_selection(user_id, candidate).is_ok()
                && self.router().pin(candidate).is_ok()
        })
    }

    /// Resolve the conversation's chosen route, falling back when a stored
    /// model is no longer valid. The bool is true when a fallback was used.
    fn selection_or_fallback(
        &self,
        user_id: &str,
        intended: ModelSelection,
        bot_hint: Option<&str>,
    ) -> Result<(ModelSelection, bool)> {
        if intended.is_auto_mode() {
            return Ok((intended, false));
        }
        match self.router().validate_selection(user_id, &intended) {
            Ok(()) => Ok((intended, false)),
            Err(error) if Self::is_model_gone(&error) => {
                match self.fallback_selection(user_id, bot_hint) {
                    Some(fallback) => Ok((fallback, true)),
                    None => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Before a message is queued: when the conversation's stored (or
    /// default) model is gone, switch the conversation to a working route and
    /// tell the user once in the chat. A conversation already on a valid
    /// route is untouched.
    pub(super) fn heal_conversation_model(
        &self,
        user_id: &str,
        conversation_id: &str,
        bot: &crate::config::LoadedBot,
    ) -> Result<()> {
        let hint = self.bot_model_hint(bot);
        let intended = match self.stored_model_selection(user_id, conversation_id)? {
            Some(stored) => stored,
            None => self.bot_default_selection(user_id, bot),
        };
        let (resolved, fell_back) =
            self.selection_or_fallback(user_id, intended.clone(), hint.as_deref())?;
        if !fell_back {
            return Ok(());
        }
        warn!(
            from_provider = %intended.provider,
            from_model = %intended.model,
            to_provider = %resolved.provider,
            to_model = %resolved.model,
            "stored model is no longer available; fell back"
        );
        self.store.kv_set(
            &Self::model_selection_key(user_id, conversation_id),
            &serde_json::to_string(&resolved)?,
        )?;
        self.store.append_message(
            user_id,
            conversation_id,
            None,
            Role::Assistant,
            &format!(
                "The model {} is no longer available, using {}",
                intended.model, resolved.model
            ),
        )?;
        Ok(())
    }

    fn thinking_key(user_id: &str, conversation_id: &str) -> String {
        format!("thinking:{user_id}:{conversation_id}")
    }

    fn legacy_thinking_key(conversation_id: &str) -> String {
        format!("thinking:{conversation_id}")
    }

    fn model_selection_key(user_id: &str, conversation_id: &str) -> String {
        format!("model_selection:{user_id}:{conversation_id}")
    }

    fn legacy_model_selection_key(conversation_id: &str) -> String {
        format!("model_selection:{conversation_id}")
    }

    fn can_read_legacy_conversation_kv(
        &self,
        user_id: &str,
        conversation_id: &str,
    ) -> Result<bool> {
        if self
            .store
            .get_conversation(user_id, conversation_id)?
            .is_some()
        {
            return Ok(true);
        }
        Ok(!self.store.conversation_exists(conversation_id)?
            && self.store.admin_user_id().ok().as_deref() == Some(user_id))
    }

    /// The conversation's own thinking level, when the owner picked one.
    pub fn conversation_thinking(
        &self,
        user_id: &str,
        conversation_id: &str,
    ) -> Result<Option<String>> {
        validate_conversation_id(conversation_id)?;
        self.guard_conversation_scope(user_id, conversation_id)?;
        let value = match self
            .store
            .kv_get(&Self::thinking_key(user_id, conversation_id))?
        {
            Some(value) => Some(value),
            None if self.can_read_legacy_conversation_kv(user_id, conversation_id)? => {
                self.store
                    .kv_get(&Self::legacy_thinking_key(conversation_id))?
            }
            None => None,
        };
        Ok(value.filter(|level| crate::thinking::is_known_level(level)))
    }

    /// The thinking levels the conversation's current model accepts.
    pub fn conversation_thinking_levels(
        &self,
        user_id: &str,
        conversation_id: &str,
        bot: &str,
    ) -> Result<Vec<&'static str>> {
        let selection = self.effective_model_selection(user_id, conversation_id, bot)?;
        Ok(self.router().thinking_levels(user_id, &selection))
    }

    /// Set (or with `None`, clear) the conversation's thinking level. The
    /// level must be one the current model accepts; a later model change that
    /// does not accept it simply stops applying it.
    pub fn set_conversation_thinking(
        &self,
        user_id: &str,
        conversation_id: &str,
        bot: &str,
        level: Option<&str>,
    ) -> Result<Option<String>> {
        validate_conversation_id(conversation_id)?;
        self.guard_conversation_scope(user_id, conversation_id)?;
        let key = Self::thinking_key(user_id, conversation_id);
        let Some(level) = level else {
            self.store.kv_set(&key, "")?;
            return Ok(None);
        };
        let levels = self.conversation_thinking_levels(user_id, conversation_id, bot)?;
        if !levels.contains(&level) {
            return Err(SeraError::Rejected {
                code: "thinking_unavailable",
                message: if levels.is_empty() {
                    "This model has no thinking levels".into()
                } else {
                    "That thinking level isn't available for this model".into()
                },
            });
        }
        self.store.kv_set(&key, level)?;
        self.store.record_audit(
            user_id,
            "model.thinking_set",
            None,
            None,
            None,
            None,
            Some(self.gateway.policy().version()),
            Some(&json!({"level": level})),
        )?;
        Ok(Some(level.to_owned()))
    }

    /// The level a turn applies: the conversation's, else the bot's. The
    /// router ignores it when the model does not support it.
    pub(super) fn turn_thinking(
        &self,
        user_id: &str,
        conversation_id: &str,
        bot: &crate::config::LoadedBot,
    ) -> Option<String> {
        self.conversation_thinking(user_id, conversation_id)
            .ok()
            .flatten()
            .or_else(|| {
                bot.config
                    .thinking
                    .clone()
                    .or_else(|| {
                        bot.config
                            .route
                            .as_ref()
                            .and_then(|route| route.thinking.clone())
                    })
                    .filter(|level| crate::thinking::is_known_level(level))
            })
    }

    /// The acting user's routable provider catalog. A provider they hold no
    /// credential for is absent, never shown with another account's status.
    pub fn model_catalog(&self, user_id: &str) -> Result<Vec<ProviderView>> {
        let catalog = self.router().catalog(user_id);
        self.gateway
            .scan_egress(user_id, &serde_json::to_string(&catalog)?)?;
        Ok(catalog)
    }

    pub fn model_catalog_entries(
        &self,
        user_id: &str,
    ) -> Result<Vec<crate::model_router::CatalogEntry>> {
        let mut entries = self.router().catalog_entries(user_id);
        let mut accounts: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for entry in &mut entries {
            if !accounts.contains_key(&entry.provider) {
                let ids = self.store.usable_account_ids(user_id, &entry.provider)?;
                accounts.insert(entry.provider.clone(), ids);
            }
            entry.account_ids = accounts[&entry.provider].clone();
        }
        self.gateway
            .scan_egress(user_id, &serde_json::to_string(&entries)?)?;
        Ok(entries)
    }

    fn default_route_key(user_id: &str) -> String {
        format!("default_route:{user_id}")
    }

    /// The route used for a conversation that has not picked one: the
    /// account's own default when set and still valid, else the bot's hint or
    /// the server default.
    fn user_default_selection(&self, user_id: &str, bot_hint: Option<&str>) -> ModelSelection {
        if bot_hint.is_none()
            && let Ok(Some(chosen)) = self.default_route(user_id)
        {
            return chosen;
        }
        self.router().default_selection(user_id, bot_hint)
    }

    /// The route a conversation with no choice of its own starts on: the
    /// bot's own route when the user can run it, otherwise the user's default
    /// route, the bot's legacy `model` and the server default, in that order.
    fn bot_default_selection(
        &self,
        user_id: &str,
        bot: &crate::config::LoadedBot,
    ) -> ModelSelection {
        match self.router().bot_chain(user_id, &bot.config).head {
            Some(head) => head,
            None => self.user_default_selection(user_id, self.bot_model_hint(bot).as_deref()),
        }
    }

    /// Whether a pinned chain still matches what the bot is configured with
    /// now. Editing a bot's route, fallbacks or failover policy makes the
    /// pins of queued and suspended turns stale, so the owner resubmits.
    pub(super) fn chain_is_current(
        &self,
        user_id: &str,
        bot: &crate::config::LoadedBot,
        route: &Option<PinnedRoute>,
    ) -> Result<()> {
        let Some(route) = route else {
            return Ok(());
        };
        let chain = self.router().bot_chain(user_id, &bot.config);
        let expected: Vec<_> = chain
            .hops
            .into_iter()
            .filter(|(hop, _)| *hop != route.selection)
            .collect();
        let pinned: Vec<_> = route
            .fallbacks
            .iter()
            .map(|hop| (hop.selection.clone(), hop.thinking.clone()))
            .collect();
        if pinned != expected || route.failover != chain.failover {
            return Err(SeraError::Forbidden(
                "the bot's model route changed; owner must resubmit the request".into(),
            ));
        }
        Ok(())
    }

    /// The account's chosen default route, if it is still routable for them.
    pub fn default_route(&self, user_id: &str) -> Result<Option<ModelSelection>> {
        let Some(value) = self.store.kv_get(&Self::default_route_key(user_id))? else {
            return Ok(None);
        };
        let Ok(stored) = serde_json::from_str::<ModelSelection>(&value) else {
            return Ok(None);
        };
        Ok(self
            .router()
            .validate_selection(user_id, &stored)
            .ok()
            .map(|_| stored))
    }

    /// Make `selection` the account's default route. Same gates as picking a
    /// model for one conversation.
    pub fn set_default_route(
        &self,
        user_id: &str,
        selection: ModelSelection,
    ) -> Result<ModelSelection> {
        if selection.is_auto_mode() {
            return Err(SeraError::Validation(
                "pick a provider and model for the default route".into(),
            ));
        }
        self.router().pin(&selection)?;
        self.router().catalog_gate(user_id, &selection)?;
        self.router().validate_selection(user_id, &selection)?;
        let value = serde_json::to_string(&selection)?;
        self.gateway.scan_egress(user_id, &value)?;
        self.store.record_audit(
            user_id,
            "model.default_set",
            None,
            None,
            None,
            None,
            Some(self.gateway.policy().version()),
            Some(&serde_json::to_value(&selection)?),
        )?;
        self.store
            .kv_set(&Self::default_route_key(user_id), &value)?;
        Ok(selection)
    }

    /// Ask one provider for its live model list with the account's own key.
    pub async fn test_provider(
        &self,
        user_id: &str,
        provider: &str,
    ) -> Result<crate::model_router::CatalogRefresh> {
        let report = self.router().test_provider(user_id, provider).await?;
        self.store.record_audit(
            user_id,
            "model.provider_tested",
            None,
            None,
            None,
            None,
            Some(self.gateway.policy().version()),
            Some(&json!({
                "provider": report.provider,
                "verified": report.verified,
                "model_count": report.model_count,
            })),
        )?;
        Ok(report)
    }

    /// Drop cached per-user provider clients after a credential or grant
    /// change. `None` clears every account (an admin credential or grant can
    /// affect more than one user).
    pub fn invalidate_model_credentials(&self, user_id: Option<&str>) {
        match user_id {
            Some(user_id) => self.router().invalidate_user(user_id),
            None => self.router().invalidate_all(),
        }
    }

    fn stored_model_selection(
        &self,
        user_id: &str,
        conversation_id: &str,
    ) -> Result<Option<ModelSelection>> {
        let value = match self
            .store
            .kv_get(&Self::model_selection_key(user_id, conversation_id))?
        {
            Some(value) => Some(value),
            None if self.can_read_legacy_conversation_kv(user_id, conversation_id)? => {
                self.store
                    .kv_get(&Self::legacy_model_selection_key(conversation_id))?
            }
            None => None,
        };
        match value {
            Some(value) => Ok(Some(serde_json::from_str(&value).map_err(|_| {
                SeraError::InvalidState("stored model selection is invalid".into())
            })?)),
            None => Ok(None),
        }
    }

    fn bot_model_hint(&self, bot: &crate::config::LoadedBot) -> Option<String> {
        bot.config
            .model
            .clone()
            .or_else(|| self.config.llm.bot_models.get(&bot.config.name).cloned())
    }

    /// Guard a conversation-scoped action for the acting user: a
    /// conversation that exists must belong to them (another user's
    /// conversation is indistinguishable from an unknown one); a
    /// not-yet-created id is allowed (lazy creation happens under the
    /// acting user).
    fn guard_conversation_scope(&self, user_id: &str, conversation_id: &str) -> Result<()> {
        if self
            .store
            .get_conversation(user_id, conversation_id)?
            .is_some()
        {
            return Ok(());
        }
        if self.store.conversation_exists(conversation_id)? {
            return Err(SeraError::NotFound(format!(
                "conversation {conversation_id}"
            )));
        }
        Ok(())
    }

    pub fn model_selection(
        &self,
        user_id: &str,
        conversation_id: &str,
        bot: &str,
    ) -> Result<ModelSelection> {
        validate_conversation_id(conversation_id)?;
        self.guard_conversation_scope(user_id, conversation_id)?;
        let fleet = self.fleet.load(user_id)?;
        let bot = fleet
            .get(bot)
            .ok_or_else(|| SeraError::NotFound("bot not found".into()))?;
        let selection = match self.stored_model_selection(user_id, conversation_id)? {
            Some(stored) => stored,
            None => self.bot_default_selection(user_id, bot),
        };
        let (selection, _) =
            self.selection_or_fallback(user_id, selection, self.bot_model_hint(bot).as_deref())?;
        self.gateway
            .scan_egress(user_id, &serde_json::to_string(&selection)?)?;
        Ok(selection)
    }

    pub fn effective_model_selection(
        &self,
        user_id: &str,
        conversation_id: &str,
        bot: &str,
    ) -> Result<ModelSelection> {
        validate_conversation_id(conversation_id)?;
        self.guard_conversation_scope(user_id, conversation_id)?;
        let fleet = self.fleet.load(user_id)?;
        let bot = fleet
            .get(bot)
            .ok_or_else(|| SeraError::NotFound("bot not found".into()))?;
        let stored = match self.stored_model_selection(user_id, conversation_id)? {
            Some(stored) => stored,
            None => self.bot_default_selection(user_id, bot),
        };
        let (stored, _) =
            self.selection_or_fallback(user_id, stored, self.bot_model_hint(bot).as_deref())?;
        let resolved = self.router().resolve_selection(
            user_id,
            &stored,
            bot.config.model.as_deref(),
            &bot.config.name,
            &self.config.llm.bot_models,
        )?;
        self.gateway
            .scan_egress(user_id, &serde_json::to_string(&resolved)?)?;
        Ok(resolved)
    }

    pub fn select_model(
        &self,
        user_id: &str,
        conversation_id: &str,
        selection: ModelSelection,
    ) -> Result<ModelSelection> {
        validate_conversation_id(conversation_id)?;
        self.guard_conversation_scope(user_id, conversation_id)?;
        let stored = if selection.is_auto_mode() {
            if !self.router().autoselect_available(user_id) {
                return Err(SeraError::Forbidden(
                    "no provider with data-egress approval is configured for autoselect".into(),
                ));
            }
            ModelSelection::auto_mode()
        } else {
            self.router().pin(&selection)?;
            self.router().catalog_gate(user_id, &selection)?;
            selection
        };
        let value = serde_json::to_string(&stored)?;
        self.gateway.scan_egress(user_id, &value)?;
        self.store.record_audit(
            user_id,
            "model.selected",
            None,
            None,
            None,
            None,
            Some(self.gateway.policy().version()),
            Some(&serde_json::to_value(&stored)?),
        )?;
        self.store
            .kv_set(&Self::model_selection_key(user_id, conversation_id), &value)?;
        if !stored.is_auto_mode() {
            self.carry_thinking_to_model(user_id, conversation_id, &stored)?;
        }
        Ok(stored)
    }

    /// After a model switch, keep the conversation's thinking level when the
    /// new model accepts it; otherwise use that model's default, or clear the
    /// choice when it has no levels. A conversation with no chosen level keeps
    /// following the bot's default.
    fn carry_thinking_to_model(
        &self,
        user_id: &str,
        conversation_id: &str,
        selection: &ModelSelection,
    ) -> Result<()> {
        let Some(current) = self.conversation_thinking(user_id, conversation_id)? else {
            return Ok(());
        };
        let levels = self.router().thinking_levels(user_id, selection);
        if levels.contains(&current.as_str()) {
            return Ok(());
        }
        let key = Self::thinking_key(user_id, conversation_id);
        match crate::thinking::level_after_switch(&levels, &current) {
            Some(level) => self.store.kv_set(&key, level)?,
            None => self.store.kv_set(&key, "")?,
        }
        Ok(())
    }

    /// One bounded model call with no tools, outside any turn: used by the
    /// onboarding interview. It goes through the same gates as a turn: the
    /// bot's effective route is pinned and re-checked, the request is policy
    /// scanned and vault-redacted, and the call is audited. The reply is
    /// returned as data; nothing here acts on it.
    pub async fn bounded_completion(
        &self,
        user_id: &str,
        bot: &str,
        system: &str,
        input: &str,
    ) -> Result<String> {
        self.bounded_completion_for("onboarding", user_id, bot, None, system, input)
            .await
    }

    /// One tool-less model call under the acting user, for an auxiliary task.
    /// The route is `model` when given (a configured provider must serve it),
    /// otherwise the bot's default route. `purpose` is recorded on the
    /// `model.used` audit row. Model requests pass the same blocklist and vault
    /// redaction as turns.
    pub async fn bounded_completion_for(
        &self,
        purpose: &str,
        user_id: &str,
        bot: &str,
        model: Option<&str>,
        system: &str,
        input: &str,
    ) -> Result<String> {
        let selection = match model {
            Some(model) => {
                let provider = self.router().provider_for_model(model).ok_or_else(|| {
                    SeraError::Config(format!("memory.extract_model {model} is not served"))
                })?;
                ModelSelection {
                    provider,
                    model: model.to_owned(),
                    account: None,
                }
            }
            None => {
                let candidate = format!("dir:{bot}");
                if self.store.get_conversation(user_id, &candidate)?.is_some()
                    || !self.store.conversation_exists(&candidate)?
                {
                    self.effective_model_selection(user_id, &candidate, bot)?
                } else {
                    let prefix: String = bot.chars().take(15).collect();
                    let scoped =
                        format!("dir:{prefix}-{}", &crate::util::sha256_hex(user_id)[..16]);
                    self.effective_model_selection(user_id, &scoped, bot)?
                }
            }
        };
        let route = Some(self.router().pin(&selection)?);
        let route = self.checked_route(user_id, &route)?;
        let messages = vec![LlmMessage::system(system), LlmMessage::user(input)];
        self.scan_model_messages(user_id, &messages)?;
        let outgoing = self.gateway.redact_messages(user_id, &messages)?;
        let response = self
            .router()
            .complete(user_id, route, &outgoing, &[], Some(0.2))
            .await?;
        self.store.record_audit(
            user_id,
            "model.used",
            None,
            None,
            None,
            None,
            Some(self.gateway.policy().version()),
            Some(&json!({
                "provider": route.selection.provider,
                "model": route.selection.model,
                "usage": response.usage,
                "purpose": purpose,
            })),
        )?;
        Ok(response.message.content.unwrap_or_default())
    }

    pub(super) fn checked_route<'a>(
        &self,
        user_id: &str,
        route: &'a Option<PinnedRoute>,
    ) -> Result<&'a PinnedRoute> {
        let route = route.as_ref().ok_or_else(|| {
            SeraError::Forbidden(
                "turn has no pinned model route; owner must resubmit the request".into(),
            )
        })?;
        self.router().validate_pinned(route)?;
        self.gateway
            .scan_egress(user_id, &serde_json::to_string(&route.selection)?)?;
        Ok(route)
    }
}
