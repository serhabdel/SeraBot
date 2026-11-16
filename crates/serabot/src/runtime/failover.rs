//! The model call with failover: walk a turn's pinned route chain.
//!
//! [`crate::route_chain`] plans the candidates (accounts of the head provider,
//! then each fallback) and records holds. This module runs the attempts. It
//! never switches when switching cannot help or must not happen: a policy
//! block, a blocked egress, a cancelled call, a rejected request and a context
//! that is too long all end the call on the candidate that hit them. It never
//! switches while the turn has an outbound action whose outcome is unknown,
//! and every hop re-runs the blocklist and egress scans, so a fallback
//! provider sees only what the owner's policy lets out.

use chrono::Utc;

use super::*;
use crate::llm::LlmResponse;
use crate::llm_failure::{FailKind, classify_llm};
use crate::route_chain::{
    Candidate, NewRouteEvent, plan_candidates, reason_code, record_failure, record_success,
};

/// Where a model call belongs, for the switch record.
pub(super) struct RouteCall<'a> {
    pub user: &'a str,
    pub task_id: Option<&'a str>,
    pub conversation_id: &'a str,
    pub bot: &'a str,
}

/// A model reply and the route that produced it.
pub(super) struct RoutedResponse {
    pub response: LlmResponse,
    pub used: ModelSelection,
}

impl Runtime {
    /// Whether the turn may move to another route now. A pending outbound
    /// action that is dispatching or has an unknown outcome pins the turn to
    /// the route it was on.
    pub(super) fn turn_may_switch(&self, user: &str, state: &TurnState) -> bool {
        let Some(pending) = &state.pending else {
            return true;
        };
        match self.store.get_action(user, &pending.action_id) {
            Ok(action) => !matches!(action.status.as_str(), "dispatching" | "outcome_unknown"),
            Err(_) => false,
        }
    }

    /// One model call along the turn's route chain.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn complete_with_failover(
        &self,
        call: &RouteCall<'_>,
        route: &PinnedRoute,
        head_thinking: Option<&str>,
        source: &[LlmMessage],
        outgoing: &[LlmMessage],
        tools: &[ToolSpec],
        temperature: Option<f32>,
        may_switch: bool,
    ) -> Result<RoutedResponse> {
        let now = Utc::now();
        let plan = plan_candidates(
            &self.store,
            &self.router,
            call.user,
            route,
            head_thinking,
            call.conversation_id,
            now,
        )?;
        let mut candidates = plan.candidates;
        if !may_switch {
            candidates.truncate(1);
        }
        if candidates.is_empty() {
            // Every candidate is on hold or cooling and one of them comes
            // back: the turn waits for it instead of failing.
            if let Some(ready) = plan.earliest_ready {
                return Err(SeraError::ModelWaiting {
                    until: wait_until(ready, now),
                });
            }
            return Err(all_routes_failed(
                plan.skipped
                    .iter()
                    .map(|(label, reason)| format!("{label}: {reason}"))
                    .collect(),
            ));
        }
        let single = candidates.len() == 1;
        let mut reasons: Vec<String> = Vec::new();
        let mut last_failed: Option<(Candidate, &'static str)> = None;
        // Accounts a failure just took out whole: a later hop on the same
        // account (another model) would hit the same wall.
        let mut dead: Vec<(&str, &str)> = Vec::new();
        for (attempt, candidate) in candidates.iter().enumerate() {
            if let Some(account) = candidate.selection.account.as_deref()
                && dead.contains(&(candidate.selection.provider.as_str(), account))
            {
                continue;
            }
            let label = format!(
                "{}/{}{}",
                candidate.selection.provider,
                candidate.selection.model,
                candidate
                    .selection
                    .account
                    .as_deref()
                    .map(|account| format!("#{account}"))
                    .unwrap_or_default()
            );
            if attempt > 0 {
                // Every hop re-runs the scans: the same text and tool list
                // must clear the owner's policy for the new destination.
                self.scan_model_messages(call.user, source)?;
                self.gateway
                    .scan_egress(call.user, &serde_json::to_string(tools)?)?;
                self.gateway
                    .scan_egress(call.user, &serde_json::to_string(&candidate.selection)?)?;
            }
            let changed_model = candidate.selection.provider != route.selection.provider
                || candidate.selection.model != route.selection.model;
            let messages = if changed_model {
                strip_provider_reasoning(outgoing)
            } else {
                outgoing.to_vec()
            };
            let thinking = if candidate.hop > 0 {
                candidate.thinking.as_deref().filter(|level| {
                    self.router
                        .thinking_levels(call.user, &candidate.selection)
                        .contains(level)
                })
            } else {
                candidate.thinking.as_deref()
            };
            let result = crate::llm::with_conversation(
                call.conversation_id,
                self.router.complete_on(
                    call.user,
                    &candidate.selection,
                    &messages,
                    tools,
                    temperature,
                    thinking,
                ),
            )
            .await;
            let error = match result {
                Ok(response) => {
                    if let Err(error) =
                        record_success(&self.store, call.user, call.conversation_id, candidate)
                    {
                        warn!(%error, "account state was not updated after a model call");
                    }
                    if let Some((from, reason)) = last_failed {
                        self.record_switch(call, &from.selection, &candidate.selection, reason);
                    }
                    return Ok(RoutedResponse {
                        response,
                        used: candidate.selection.clone(),
                    });
                }
                Err(error) => error,
            };
            // A pool account whose credential cannot be opened is that
            // account's problem, not the turn's.
            let kind = match classify_llm(&error) {
                Ok(FailKind::ContextTooLong) => return Err(error),
                Ok(kind) => kind,
                Err(_) if is_unusable_account(&error, candidate) => FailKind::CredentialsRejected,
                Err(_) => return Err(error),
            };
            self.note_failure(call, candidate, &kind, now);
            if single {
                return Err(error);
            }
            let code = reason_code(&kind);
            reasons.push(format!("{label}: {code}"));
            last_failed = Some((candidate.clone(), code));
            if let Some(account) = candidate.selection.account.as_deref()
                && matches!(
                    kind,
                    FailKind::RateLimited { .. }
                        | FailKind::QuotaExhausted { .. }
                        | FailKind::AccountForbidden
                        | FailKind::CredentialsRejected
                )
            {
                dead.push((candidate.selection.provider.as_str(), account));
            }
        }
        Err(all_routes_failed(reasons))
    }

    fn note_failure(
        &self,
        call: &RouteCall<'_>,
        candidate: &Candidate,
        kind: &FailKind,
        now: chrono::DateTime<Utc>,
    ) {
        match record_failure(&self.store, call.user, candidate, kind, now) {
            Ok(Some(cooldown)) => {
                if let Err(error) = self.store.record_audit(
                    call.user,
                    "model.account_cooldown",
                    None,
                    call.task_id,
                    None,
                    None,
                    Some(self.gateway.policy().version()),
                    Some(&json!({
                        "provider": candidate.selection.provider,
                        "account": candidate.selection.account,
                        "model": candidate.selection.model,
                        "reason": reason_code(kind),
                        "hold": cooldown.kind,
                        "until": cooldown.until.to_rfc3339(),
                    })),
                ) {
                    warn!(%error, "account cooldown was not audited");
                }
            }
            Ok(None) => {}
            Err(error) => warn!(%error, "account failure was not recorded"),
        }
    }

    /// Record a switch: a `route_events` row, an audit row and a
    /// `route_switched` event. None of them can fail the turn.
    fn record_switch(
        &self,
        call: &RouteCall<'_>,
        from: &ModelSelection,
        to: &ModelSelection,
        reason: &str,
    ) {
        let outcome = self
            .store
            .record_route_event(
                call.user,
                &NewRouteEvent {
                    conversation_id: Some(call.conversation_id),
                    task_id: call.task_id,
                    bot: Some(call.bot),
                    from,
                    to,
                    reason,
                },
            )
            .and_then(|_| {
                let details = json!({
                    "conversation_id": call.conversation_id,
                    "bot": call.bot,
                    "from": {"provider": from.provider, "model": from.model, "account": from.account},
                    "to": {"provider": to.provider, "model": to.model, "account": to.account},
                    "reason": reason,
                });
                self.store.record_audit(
                    call.user,
                    "model.failover",
                    None,
                    call.task_id,
                    None,
                    None,
                    Some(self.gateway.policy().version()),
                    Some(&details),
                )?;
                self.store
                    .append_event(call.user, "route_switched", &details)?;
                Ok(())
            });
        if let Err(error) = outcome {
            warn!(%error, "route switch was not recorded");
        }
    }
}

/// Shortest and longest a turn parks before it looks again. The cap lets a
/// fresh sign-in or a cleared hold be noticed without waiting out a long hold.
const MIN_MODEL_WAIT_SECS: i64 = 5;
const MAX_MODEL_WAIT_SECS: i64 = 900;

/// When a parked turn should try again: the earliest hold expiry, kept
/// between [`MIN_MODEL_WAIT_SECS`] and [`MAX_MODEL_WAIT_SECS`] from now.
pub(super) fn wait_until(
    ready: chrono::DateTime<Utc>,
    now: chrono::DateTime<Utc>,
) -> chrono::DateTime<Utc> {
    ready.clamp(
        now + chrono::Duration::seconds(MIN_MODEL_WAIT_SECS),
        now + chrono::Duration::seconds(MAX_MODEL_WAIT_SECS),
    )
}

/// Whether `error` means this one account cannot be used (its credential is
/// missing or unreadable) while other candidates may work.
fn is_unusable_account(error: &SeraError, candidate: &Candidate) -> bool {
    candidate.selection.account.is_some() && matches!(error, SeraError::Forbidden(_))
}

/// Drop signed reasoning a provider attached to tool calls. It is bound to
/// the provider and model that produced it; another one rejects it.
fn strip_provider_reasoning(messages: &[LlmMessage]) -> Vec<LlmMessage> {
    messages
        .iter()
        .cloned()
        .map(|mut message| {
            for call in &mut message.tool_calls {
                call.reasoning_signature = None;
            }
            message
        })
        .collect()
}

/// Every candidate failed or none was ready. The message carries stable
/// reason words only, never provider text.
pub(super) fn all_routes_failed(reasons: Vec<String>) -> SeraError {
    let detail = if reasons.is_empty() {
        "no route is ready".to_owned()
    } else {
        reasons.join("; ")
    };
    SeraError::Rejected {
        code: "all_routes_failed",
        message: format!("No model route could answer ({detail})"),
    }
}
