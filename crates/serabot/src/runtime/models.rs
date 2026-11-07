//! Per-conversation model catalog and selection.

use super::*;

impl Runtime {
    pub async fn refresh_model_catalogs(&self, user_id: &str) -> Result<()> {
        for report in self.router.refresh_catalogs(user_id).await? {
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

    /// The acting user's routable provider catalog. A provider they hold no
    /// credential for is absent, never shown with another account's status.
    pub fn model_catalog(&self, user_id: &str) -> Result<Vec<ProviderView>> {
        let catalog = self.router.catalog(user_id);
        self.gateway
            .scan_egress(user_id, &serde_json::to_string(&catalog)?)?;
        Ok(catalog)
    }

    pub fn model_catalog_entries(
        &self,
        user_id: &str,
    ) -> Result<Vec<crate::model_router::CatalogEntry>> {
        let entries = self.router.catalog_entries(user_id);
        self.gateway
            .scan_egress(user_id, &serde_json::to_string(&entries)?)?;
        Ok(entries)
    }

    /// Drop cached per-user provider clients after a credential or grant
    /// change. `None` clears every account (an admin credential or grant can
    /// affect more than one user).
    pub fn invalidate_model_credentials(&self, user_id: Option<&str>) {
        match user_id {
            Some(user_id) => self.router.invalidate_user(user_id),
            None => self.router.invalidate_all(),
        }
    }

    fn stored_model_selection(&self, conversation_id: &str) -> Result<Option<ModelSelection>> {
        match self
            .store
            .kv_get(&format!("model_selection:{conversation_id}"))?
        {
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
        let selection = match self.stored_model_selection(conversation_id)? {
            Some(stored) => stored,
            None => self
                .router
                .default_selection(user_id, self.bot_model_hint(bot).as_deref()),
        };
        if selection.is_auto_mode() {
            self.gateway
                .scan_egress(user_id, &serde_json::to_string(&selection)?)?;
            return Ok(selection);
        }
        self.router.validate_selection(user_id, &selection)?;
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
        let stored = match self.stored_model_selection(conversation_id)? {
            Some(stored) => stored,
            None => self
                .router
                .default_selection(user_id, self.bot_model_hint(bot).as_deref()),
        };
        let resolved = self.router.resolve_selection(
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
            if !self.router.autoselect_available(user_id) {
                return Err(SeraError::Forbidden(
                    "no provider with data-egress approval is configured for autoselect".into(),
                ));
            }
            ModelSelection::auto_mode()
        } else {
            self.router.pin(&selection)?;
            self.router.catalog_gate(user_id, &selection)?;
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
            .kv_set(&format!("model_selection:{conversation_id}"), &value)?;
        Ok(stored)
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
        self.bounded_completion_for("onboarding", user_id, bot, system, input)
            .await
    }

    /// One tool-less model call under the acting user, on the bot's default
    /// route, for an auxiliary task. `purpose` is recorded on the `model.used`
    /// audit row. Model requests pass the same blocklist and vault redaction
    /// as turns.
    pub async fn bounded_completion_for(
        &self,
        purpose: &str,
        user_id: &str,
        bot: &str,
        system: &str,
        input: &str,
    ) -> Result<String> {
        let selection = self.effective_model_selection(user_id, &format!("dir:{bot}"), bot)?;
        let route = Some(self.router.pin(&selection)?);
        let route = self.checked_route(user_id, &route)?;
        let messages = vec![LlmMessage::system(system), LlmMessage::user(input)];
        self.scan_model_messages(user_id, &messages)?;
        let outgoing = self.gateway.redact_messages(user_id, &messages)?;
        let response = self
            .router
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
        self.router.validate_pinned(route)?;
        self.gateway
            .scan_egress(user_id, &serde_json::to_string(&route.selection)?)?;
        Ok(route)
    }
}
