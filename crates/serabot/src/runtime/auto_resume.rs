//! Subagent auto-resume (0.50, B6).
//!
//! When a handoff's child turn fails for good (its route chain is exhausted, or
//! it is out of attempts) the ledger row becomes `failed_resumable`, and main
//! gets one synthetic follow-up turn naming the subagent and the reason codes.
//! Main may then call `subagent_retry` with the next fallback or account. The
//! retry cap (`MAX_RESUMES`) and the refusal on unknown effects live in
//! `delegate::resume_handoff`; this module only reports the failure.
//!
//! The follow-up text is written here from the bot id and stable reason words.
//! No provider text, tool output or specialist reply is copied into it, so it
//! cannot carry an injected instruction. A chain of failures cannot loop
//! forever: a follow-up whose own delegations fail may queue a further
//! follow-up only up to `MAX_FOLLOWUP_DEPTH` levels.

use super::activity::{self, HandoffRef};
use super::*;
use crate::delegate::{MAX_RESUMES, next_chain_slot};

/// `source` of a synthetic follow-up turn to main. The management tools treat
/// it as an owner-side turn (it holds no untrusted text), so `subagent_retry`
/// works from it.
pub(super) const FOLLOWUP_SOURCE: &str = "handoff_followup";
/// Follow-up levels: a follow-up's delegations may queue another, three deep.
const MAX_FOLLOWUP_DEPTH: u64 = 3;
/// Most reason codes one notice names.
const MAX_REASON_CODES: usize = 6;

/// Whether a task payload is a synthetic follow-up turn.
pub(super) fn is_followup(payload: &Value) -> bool {
    payload.get("source").and_then(Value::as_str) == Some(FOLLOWUP_SOURCE)
}

/// Follow-up levels already spent on a failing child. A retry keeps the
/// handoff's original parent, so the depth the retrying follow-up carried is
/// copied into the resumed child's own payload; the larger of the two counts.
fn followup_depth(parent: &Value, child: &Value) -> u64 {
    let read = |payload: &Value| payload.get("followup_depth").and_then(Value::as_u64);
    read(parent).max(read(child)).unwrap_or(0)
}

/// Stable reason words for a failure: the route-chain codes for an exhausted
/// chain, else one word for the error kind. Never error text.
pub(super) fn reason_codes(error: &SeraError) -> Vec<String> {
    let word = |text: &str| {
        let cleaned: String = text
            .chars()
            .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
            .take(40)
            .collect();
        (!cleaned.is_empty()).then_some(cleaned)
    };
    let mut codes: Vec<String> = Vec::new();
    match error {
        SeraError::Rejected { code, message } if *code == "all_routes_failed" => {
            // "No model route could answer (label: code; label: code)".
            if let (Some(open), Some(close)) = (message.find('('), message.rfind(')'))
                && open < close
            {
                for part in message[open + 1..close].split("; ") {
                    if let Some(code) = part.rsplit(": ").next().and_then(word)
                        && !codes.contains(&code)
                    {
                        codes.push(code);
                    }
                }
            }
            if codes.is_empty() {
                codes.push("all_routes_failed".into());
            }
        }
        SeraError::Rejected { code, .. } => codes.extend(word(code)),
        SeraError::Validation(_) => codes.push("validation".into()),
        SeraError::Forbidden(_) => codes.push("forbidden".into()),
        SeraError::Config(_) => codes.push("config".into()),
        SeraError::PolicyViolation => codes.push("policy".into()),
        SeraError::NotFound(_) => codes.push("not_found".into()),
        SeraError::InvalidState(_) => codes.push("invalid_state".into()),
        SeraError::External(_) => codes.push("external".into()),
        _ => codes.push("error".into()),
    }
    codes.truncate(MAX_REASON_CODES);
    codes
}

impl Runtime {
    /// A handoff child task has just failed. When it failed for good, record
    /// that on the ledger, emit `handoff.failed`, and queue the one follow-up
    /// turn to main. Returns whether a follow-up was queued. Never fails the
    /// caller: the task is already failed, this is reporting.
    pub(super) fn on_task_failed(&self, task: &Task, error: &SeraError) -> bool {
        match self.report_handoff_failure(task, &reason_codes(error)) {
            Ok(queued) => queued,
            Err(error) => {
                warn!(task = %task.id, %error, "handoff failure was not reported to main");
                false
            }
        }
    }

    fn report_handoff_failure(&self, task: &Task, codes: &[String]) -> Result<bool> {
        let user = task.user_id.as_str();
        let Ok(payload) = serde_json::from_value::<TurnPayload>(task.payload.clone()) else {
            return Ok(false);
        };
        let Some(handoff) = payload.handoff else {
            return Ok(false);
        };
        // Only a task that ended for good is reported; one that will retry is not.
        let stored = self.store.get_task(user, &task.id)?;
        if !matches!(stored.status.as_str(), "failed" | "needs_review") {
            return Ok(false);
        }
        let row = self.store.get_handoff(user, &handoff.handoff_id)?;
        if row.child_task_id != task.id {
            return Ok(false);
        }
        let effects_unknown = stored.uncertain || stored.status == crate::store::TASK_NEEDS_REVIEW;
        let resumable = !effects_unknown && row.resume_count < MAX_RESUMES;
        let status = if resumable {
            "failed_resumable"
        } else {
            "failed"
        };
        let outcome = codes.first().map_or("error", String::as_str);
        if !self.store.mark_handoff_failed(
            user,
            &row.id,
            &task.id,
            status,
            outcome,
            effects_unknown,
        )? {
            return Ok(false);
        }
        let reference = HandoffRef {
            handoff_id: &row.id,
            child_task_id: &task.id,
            parent_task_id: &row.parent_task_id,
            to_bot: &row.to_bot,
            conversation_id: &payload.conversation_id,
        };
        self.store.append_event(
            user,
            activity::EVENT_HANDOFF_FAILED,
            &activity::handoff_failed_payload(
                &reference,
                status,
                outcome,
                effects_unknown,
                row.resume_count,
            ),
        )?;
        self.store.record_audit(
            user,
            "handoff.failed",
            None,
            Some(&task.id),
            None,
            None,
            Some(self.gateway.policy().version()),
            Some(&json!({
                "handoff_id": row.id,
                "to_bot": row.to_bot,
                "status": status,
                "reason_codes": codes,
                "effects_unknown": effects_unknown,
                "resume_count": row.resume_count,
            })),
        )?;

        // One follow-up to main per failure, bounded in depth.
        let parent = self.store.get_task(user, &row.parent_task_id)?;
        let depth = followup_depth(&parent.payload, &task.payload) + 1;
        if depth > MAX_FOLLOWUP_DEPTH {
            return Ok(false);
        }
        let fleet = self.fleet.load(user)?;
        let Some(main) = fleet
            .get(ORCHESTRATOR_BOT)
            .filter(|bot| bot.config.enabled && bot.config.role == BotRole::Orchestrator)
        else {
            return Ok(false);
        };
        let next_slot = fleet
            .get(&row.to_bot)
            .and_then(|bot| next_chain_slot(&bot.config, row.route.as_ref()));
        let text = followup_text(
            &row.to_bot,
            &row.id,
            codes,
            effects_unknown,
            row.resume_count,
            next_slot,
        );
        self.scan_owner_output(user, &text)?;
        let selection =
            self.effective_model_selection(user, &payload.conversation_id, ORCHESTRATOR_BOT)?;
        let route = self.router().pin_for_bot(user, &selection, &main.config)?;
        let followup = self.store.enqueue_task(
            user,
            "turn",
            ORCHESTRATOR_BOT,
            &json!({
                "conversation_id": payload.conversation_id,
                "text": text,
                "bot": ORCHESTRATOR_BOT,
                "source": FOLLOWUP_SOURCE,
                "ask_mode": false,
                "route": route,
                "followup_depth": depth,
                "followup_for": row.id,
            }),
            0,
            Utc::now(),
        )?;
        self.store.append_event(
            user,
            "task.queued",
            &json!({"task_id": followup.id, "kind": "turn", "bot": ORCHESTRATOR_BOT}),
        )?;
        Ok(true)
    }
}

/// The notice main reads. Server-written from ids and reason words only.
fn followup_text(
    to_bot: &str,
    handoff_id: &str,
    codes: &[String],
    effects_unknown: bool,
    resume_count: i64,
    next_slot: Option<usize>,
) -> String {
    let mut text = format!(
        "System notice: subagent {to_bot} (handoff {handoff_id}) failed: {}. \
         effects_unknown: {effects_unknown}. Retries used: {resume_count} of {MAX_RESUMES}.\n",
        codes.join(", ")
    );
    if effects_unknown {
        text.push_str(
            "An outbound action may have had an effect, so subagent_retry will refuse. \
             Verify the result with a read tool or ask the owner before doing anything else.",
        );
    } else if resume_count >= MAX_RESUMES {
        text.push_str(
            "No retries remain for this handoff. Tell the owner what failed and offer next steps.",
        );
    } else {
        text.push_str("Options: call subagent_retry with this handoff_id, once, to run it again");
        match next_slot {
            Some(slot) => text.push_str(&format!(
                " on the next fallback (route_override {{\"chain_index\": {slot}}})"
            )),
            None => text.push_str(" on its own route"),
        }
        text.push_str(", or tell the owner. Do not retry in a loop.");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_up_depth_counts_the_retrying_turns_depth() {
        // The original parent has no depth; the resumed child carries 2.
        assert_eq!(followup_depth(&json!({}), &json!({})), 0);
        assert_eq!(followup_depth(&json!({}), &json!({"followup_depth": 2})), 2);
        assert_eq!(
            followup_depth(&json!({"followup_depth": 1}), &json!({"followup_depth": 2})),
            2
        );
    }

    #[test]
    fn reason_codes_keep_stable_words_only() {
        let error = crate::runtime::failover::all_routes_failed(vec![
            "default/test: rate_limited".into(),
            "default/other#acct1: rate_limited".into(),
            "second/model: quota_exhausted".into(),
        ]);
        assert_eq!(
            reason_codes(&error),
            vec!["rate_limited".to_owned(), "quota_exhausted".to_owned()]
        );
        // Free text never becomes a code.
        let leaked = SeraError::External("Bearer sk-secret leaked".into());
        assert_eq!(reason_codes(&leaked), vec!["external".to_owned()]);
        assert!(!reason_codes(&leaked).join(",").contains("secret"));
    }

    #[test]
    fn the_notice_names_options_by_state() {
        let codes = vec!["rate_limited".to_owned()];
        let open = followup_text("analyst", "h1", &codes, false, 0, Some(1));
        assert!(open.contains("subagent_retry") && open.contains("\"chain_index\": 1"));
        let unknown = followup_text("analyst", "h1", &codes, true, 0, Some(1));
        assert!(unknown.contains("will refuse") && !unknown.contains("chain_index"));
        let spent = followup_text("analyst", "h1", &codes, false, MAX_RESUMES, None);
        assert!(spent.contains("No retries remain"));
    }
}
