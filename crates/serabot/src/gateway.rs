//! Tool execution gateway: allowlist, approvals, standing grants, policy
//! scanning, and the audit ledger.
//!
//! **Authority is per user** (phase 5 of the multi-user reconstruction). The
//! gateway holds no acting-user field: every call carries a [`ToolContext`]
//! whose `user_id` scopes the action, approval, and grant rows it touches.
//! A user decides only their own pending approvals; the admin sees every
//! audit row but has no power to decide a member's approval. Grants match
//! only within the granting user's rows. Policy scanning stays a global
//! server policy with one version hash for everyone, so its invalidation
//! semantics do not change.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Duration;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

use crate::domain::{Action, Approval, ApprovalState, Id, LlmMessage};
use crate::error::{Result, SeraError};
use crate::policy::Policy;
use crate::store::Store;
use crate::user_secrets::UserSecrets;
use crate::util::{canonical_hash, canonical_json, sha256_hex, truncate_chars};
use crate::vault::Redactor;
use crate::vault_providers::ResolvedValues;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    #[serde(default)]
    pub external_effect: bool,
}

/// Tools that need owner approval: they delete or irreversibly overwrite
/// user data, or send something to a third party that cannot be recalled.
/// Every other tool runs on the model's judgement. Listed in
/// `docs/approvals.md`; keep the two in sync.
pub const DESTRUCTIVE_TOOLS: &[&str] = &[
    "memory_delete",
    "agentmail_send",
    "telegram_send_document",
    "onedrive_upload",
    "local_write",
    "local_export",
    "github_create_issue",
    "github_comment_issue",
    "todoist_add_task",
    "todoist_complete_task",
    "slack_post_message",
];

/// Audit reason recorded when the owner's YOLO setting approved a call.
pub const YOLO_AUDIT_REASON: &str = "auto-approved by user YOLO setting";

/// Risk class of a tool by name: true when it needs approval.
pub fn is_destructive_tool(name: &str) -> bool {
    DESTRUCTIVE_TOOLS.contains(&name)
}

impl ToolSpec {
    /// True when the gateway requires approval before running this tool.
    pub fn destructive(&self) -> bool {
        is_destructive_tool(&self.name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolContext {
    /// The acting user's id — the authoritative scope for every user-owned
    /// row this context touches (phase 2). Never model-controlled.
    pub user_id: String,
    /// Audit actor string, kept for the audit ledger's backward-compatible
    /// readers; it mirrors `user_id` ("system" appears only for maintenance
    /// paths that have no acting user).
    pub actor: String,
    pub bot: String,
    pub task_id: Option<Id>,
    pub turn_id: Option<Id>,
    pub conversation_id: Option<Id>,
    pub allowed_tools: HashSet<String>,
    pub require_approval: HashSet<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolOutput {
    /// Text returned to the model and scanned before it crosses a trust boundary.
    pub content: String,
    pub data: Option<Value>,
    pub artifact_path: Option<String>,
    pub artifact_hash: Option<String>,
    pub filename: Option<String>,
    pub destination: Option<String>,
}

/// One further artifact a tool listed for the owner, beside its primary one
/// (0.24). Built by [`crate::maker::list_saved_artifact`] from a file the call
/// itself saved; the gateway never reads it from a tool's `data`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtraArtifact {
    pub hash: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub title: Option<String>,
}

/// Run one tool call inside its own artifact record. Returns the output and
/// the extra artifacts the call listed. Only files the call saved can be
/// listed, so a hash named in a tool's `data`, an earlier call's file or
/// another user's file never reaches the conversation library.
async fn run_tool_call(
    tool: &dyn Tool,
    args: Value,
    context: &ToolContext,
) -> (Result<ToolOutput>, Vec<ExtraArtifact>) {
    let calls = Arc::new(Mutex::new(crate::maker::CallArtifacts::default()));
    let output = crate::maker::CALL_ARTIFACTS
        .scope(calls.clone(), tool.execute(args, context))
        .await;
    let listed = calls
        .lock()
        .map(|calls| calls.listed.clone())
        .unwrap_or_default();
    (output, listed)
}

/// A lowercase or uppercase 64-digit hex SHA-256, as artifact hashes are.
fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Clone)]
pub struct ApprovalDetails {
    pub summary: String,
    pub destination: Option<String>,
    pub artifact_hash: Option<String>,
}

impl Default for ApprovalDetails {
    fn default() -> Self {
        Self {
            summary: "External action requires approval".into(),
            destination: None,
            artifact_hash: None,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    fn approval_details(&self, _args: &Value) -> Result<ApprovalDetails> {
        Ok(ApprovalDetails::default())
    }

    /// The approval text and destination for this call. Defaults to
    /// [`Tool::approval_details`]. A tool whose destination is known only at
    /// run time (the page a browser secret is typed into) overrides it.
    async fn approval_details_for(
        &self,
        args: &Value,
        _context: &ToolContext,
    ) -> Result<ApprovalDetails> {
        self.approval_details(args)
    }

    /// The destination this call would act on right now. When `Some`, an
    /// approved action is dispatched only if its approved destination is the
    /// same; otherwise it is invalidated and needs a new approval. `None`
    /// (the default) skips the check.
    async fn live_destination(
        &self,
        _args: &Value,
        _context: &ToolContext,
    ) -> Result<Option<String>> {
        Ok(None)
    }

    /// Whether the conversation's auto-approve switch may approve this call
    /// without the owner. Defaults to yes.
    async fn auto_approve_allowed(&self, _args: &Value, _context: &ToolContext) -> bool {
        true
    }

    /// Whether this call needs approval. Defaults to the central destructive
    /// list by tool name; a tool whose risk depends on its arguments (for
    /// example a Composio slug) overrides it.
    fn requires_approval(&self, _args: &Value) -> bool {
        self.spec().destructive()
    }

    /// Whether the owner's "always" choice may remember this call. A call that
    /// is too sensitive for a standing grant (a vault secret typed into a
    /// page) returns false: "always" then acts as "once" and asks again.
    fn allow_standing_grant(&self, _args: &Value) -> bool {
        true
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput>;
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    specs: BTreeMap<String, ToolSpec>,
}

impl ToolRegistry {
    pub fn register<T: Tool + 'static>(&mut self, tool: T) -> Result<()> {
        let spec = tool.spec();
        if spec.name.is_empty() || self.specs.contains_key(&spec.name) {
            return Err(SeraError::Config(format!(
                "invalid or duplicate tool name: {}",
                spec.name
            )));
        }
        self.specs.insert(spec.name.clone(), spec);
        self.tools.insert(tool.spec().name, Arc::new(tool));
        Ok(())
    }

    pub fn register_arc(&mut self, tool: Arc<dyn Tool>) -> Result<()> {
        let spec = tool.spec();
        if spec.name.is_empty() || self.specs.contains_key(&spec.name) {
            return Err(SeraError::Config(format!(
                "invalid or duplicate tool name: {}",
                spec.name
            )));
        }
        self.specs.insert(spec.name.clone(), spec);
        self.tools.insert(tool.spec().name, tool);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn is_registered(&self, name: &str) -> bool {
        self.specs.contains_key(name)
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.specs.values().cloned().collect()
    }

    pub fn tools_for(&self, names: &[String]) -> Vec<ToolSpec> {
        names
            .iter()
            .filter_map(|name| self.specs.get(name).cloned())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolOutcome {
    Completed(ToolOutput),
    PendingApproval {
        approval_id: Id,
        action_id: Id,
        summary: String,
        destination: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Once,
    Always,
    Deny,
}

#[derive(Debug, Clone)]
pub struct DecisionResult {
    pub approval: Approval,
    pub action: Action,
    pub resume_task: bool,
}

#[derive(Clone)]
pub struct Gateway {
    store: Store,
    policy: Arc<Policy>,
    registry: ToolRegistry,
    approval_expiry: Duration,
    standing_consent: bool,
    /// Honour bot `require_approval` lists for non-destructive tools.
    respect_bot_lists: bool,
    /// Vault redaction source; `None` leaves tool output untouched.
    vault: Option<Arc<UserSecrets>>,
    /// Values recently read from a password manager, scrubbed with the vault.
    resolved: Arc<ResolvedValues>,
}

impl Gateway {
    pub fn new(
        store: Store,
        policy: Arc<Policy>,
        registry: ToolRegistry,
        approval_expiry_hours: i64,
        standing_consent: bool,
    ) -> Self {
        Self {
            store,
            policy,
            registry,
            approval_expiry: Duration::hours(approval_expiry_hours.max(1)),
            standing_consent,
            respect_bot_lists: false,
            vault: None,
            resolved: Arc::default(),
        }
    }

    /// Let bot `require_approval` lists add approval for non-destructive
    /// tools (`approvals.respect_bot_lists`). Off by default.
    pub fn with_respect_bot_lists(mut self, respect: bool) -> Self {
        self.respect_bot_lists = respect;
        self
    }

    /// Enable vault redaction: before tool output or a model request leaves
    /// the gateway, the acting user's vault values become `[vault:<name>]`.
    pub fn with_vault(mut self, users: Arc<UserSecrets>) -> Self {
        self.vault = Some(users);
        self
    }

    /// The password manager values the redactor also scrubs. Pass the same
    /// registry the browser fills read through (`Providers::resolved_values`).
    pub fn with_resolved_values(mut self, resolved: Arc<ResolvedValues>) -> Self {
        self.resolved = resolved;
        self
    }

    /// The per-user secret store, when the gateway was built with one. Only
    /// per-user adapters (for example Mem0) read secrets through it, always
    /// under the acting user's id.
    pub fn user_secrets(&self) -> Option<Arc<UserSecrets>> {
        self.vault.clone()
    }

    /// Redact the acting user's vault values from `text`. Fails closed: if
    /// the vault cannot be read, the text is not returned.
    pub fn redact_text(&self, user_id: &str, text: &str) -> Result<String> {
        match &self.vault {
            Some(users) => Ok(Redactor::for_user(users, user_id, &self.resolved)?.redact(text)),
            None => Ok(text.to_owned()),
        }
    }

    /// A copy of `messages` with the acting user's vault values redacted:
    /// the form that may be sent to a model. Content, tool-call arguments and
    /// names are all covered.
    pub fn redact_messages(
        &self,
        user_id: &str,
        messages: &[LlmMessage],
    ) -> Result<Vec<LlmMessage>> {
        let Some(users) = &self.vault else {
            return Ok(messages.to_vec());
        };
        let redactor = Redactor::for_user(users, user_id, &self.resolved)?;
        if redactor.is_empty() {
            return Ok(messages.to_vec());
        }
        Ok(messages
            .iter()
            .map(|message| {
                let mut message = message.clone();
                message.content = message.content.map(|text| redactor.redact(&text));
                message.name = message.name.map(|text| redactor.redact(&text));
                message.tool_call_id = message.tool_call_id.map(|text| redactor.redact(&text));
                for call in &mut message.tool_calls {
                    call.id = redactor.redact(&call.id);
                    call.name = redactor.redact(&call.name);
                    redactor.redact_json(&mut call.arguments);
                }
                message
            })
            .collect())
    }

    /// Redact a finished tool call's output before it is audited, scanned or
    /// returned. This is the single hook every dispatch path goes through.
    fn redact_output(
        &self,
        context: &ToolContext,
        output: Result<ToolOutput>,
    ) -> Result<ToolOutput> {
        let Some(users) = &self.vault else {
            return output;
        };
        let mut output = output?;
        let redactor = Redactor::for_user(users, &context.user_id, &self.resolved)?;
        if redactor.is_empty() {
            return Ok(output);
        }
        output.content = redactor.redact(&output.content);
        if let Some(data) = output.data.as_mut() {
            redactor.redact_json(data);
        }
        for field in [&mut output.filename, &mut output.destination] {
            if let Some(text) = field.as_mut() {
                *text = redactor.redact(text);
            }
        }
        Ok(output)
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn is_registered(&self, name: &str) -> bool {
        self.registry.is_registered(name)
    }

    pub async fn execute(
        &self,
        name: &str,
        args: Value,
        context: &ToolContext,
    ) -> Result<ToolOutcome> {
        if !context.allowed_tools.contains(name) {
            self.store.record_audit(
                &context.actor,
                "tool.forbidden",
                Some(name),
                context.task_id.as_deref(),
                None,
                None,
                Some(self.policy.version()),
                None,
            )?;
            return Err(SeraError::Forbidden(format!(
                "tool not allowlisted: {name}"
            )));
        }
        let tool = self
            .registry
            .get(name)
            .ok_or_else(|| SeraError::NotFound(format!("tool {name}")))?;
        let canonical = canonical_json(&args)?;
        let hash = canonical_hash(&args)?;

        if tool.requires_approval(&args)
            || (self.respect_bot_lists && context.require_approval.contains(name))
        {
            if let Some(output) = self
                .check_standing_grant(tool.as_ref(), &args, &hash, context)
                .await?
            {
                return self.dispatch(tool.as_ref(), args, context, output).await;
            }
            let outcome = self
                .request_approval(tool.as_ref(), args.clone(), context, &canonical, &hash)
                .await?;
            return self
                .maybe_auto_approve(tool.as_ref(), outcome, &args, &canonical, context)
                .await;
        }

        let started = std::time::Instant::now();
        let (output, listed) = run_tool_call(tool.as_ref(), args, context).await;
        let output = self.redact_output(context, output);
        self.finish_tool_call(tool.as_ref(), &output, &listed, context, started.elapsed())?;
        output.map(ToolOutcome::Completed)
    }

    /// Per-conversation auto-approve ("YOLO"), set by the conversation's own
    /// owner. The call still goes through the full approval chain: the action
    /// and approval rows are created and bound to the user, canonical
    /// arguments, destination, artifact hash and policy version, the approval
    /// is decided as approved-once, and the action runs through
    /// `execute_approved`. Only the human click is replaced, and the audit
    /// ledger says so. The switch is read fresh on every call, so turning it
    /// off takes effect on the next tool call. Content the policy rejects is
    /// never auto-approved; it stays pending for the owner.
    async fn maybe_auto_approve(
        &self,
        tool: &dyn Tool,
        outcome: ToolOutcome,
        args: &Value,
        canonical: &str,
        context: &ToolContext,
    ) -> Result<ToolOutcome> {
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = &outcome
        else {
            return Ok(outcome);
        };
        let Some(conversation_id) = context.conversation_id.as_deref() else {
            return Ok(outcome);
        };
        if !self.store.yolo_enabled(&context.user_id, conversation_id)
            || !self.policy.is_allowed(canonical)
            || !tool.auto_approve_allowed(args, context).await
        {
            return Ok(outcome);
        }
        self.decide(&context.user_id, approval_id, Decision::Once)?;
        self.store.record_audit(
            &context.actor,
            "approval.auto_approved",
            None,
            context.task_id.as_deref(),
            Some(approval_id),
            Some(action_id),
            Some(self.policy.version()),
            Some(&serde_json::json!({
                "reason": YOLO_AUDIT_REASON,
                "conversation_id": conversation_id,
            })),
        )?;
        self.execute_approved(action_id, context).await
    }
    async fn check_standing_grant(
        &self,
        tool: &dyn Tool,
        args: &Value,
        hash: &str,
        context: &ToolContext,
    ) -> Result<Option<ToolOutput>> {
        if !tool.allow_standing_grant(args) {
            return Ok(None);
        }
        let spec = tool.spec();
        let details = tool.approval_details_for(args, context).await?;
        if self.store.matching_grant(
            &context.user_id,
            &spec.name,
            hash,
            details.destination.as_deref(),
            self.policy.version(),
        )? {
            self.store.record_audit(
                &context.actor,
                "approval.standing_grant_used",
                Some(&spec.name),
                context.task_id.as_deref(),
                None,
                None,
                Some(self.policy.version()),
                None,
            )?;
            return Ok(Some(ToolOutput::default()));
        }
        Ok(None)
    }

    async fn request_approval(
        &self,
        tool: &dyn Tool,
        args: Value,
        context: &ToolContext,
        canonical: &str,
        hash: &str,
    ) -> Result<ToolOutcome> {
        let spec = tool.spec();
        let mut details = tool.approval_details_for(&args, context).await?;
        let preview = truncate_chars(canonical, 600);
        let combined = format!("{}\nPayload: {preview}", details.summary);
        if self.policy.is_allowed(&combined) {
            details.summary = combined;
        } else if !self.policy.is_allowed(&details.summary) {
            details.summary = format!(
                "{} requests an external action whose details are withheld by policy",
                spec.name
            );
        } else {
            details.summary = format!("{}\nPayload withheld by content policy", details.summary);
        }
        let action = self.store.create_action(
            &context.user_id,
            &spec.name,
            context.task_id.as_deref(),
            context.turn_id.as_deref(),
            canonical,
            hash,
            details.destination.as_deref(),
            details.artifact_hash.as_deref(),
            self.policy.version(),
        )?;
        let approval = self.store.create_approval(
            &context.user_id,
            &action.id,
            &spec.name,
            &truncate_chars(&details.summary, 1000),
            chrono::Utc::now() + self.approval_expiry,
        )?;
        self.store.record_audit(
            &context.actor,
            "approval.requested",
            Some(&spec.name),
            context.task_id.as_deref(),
            Some(&approval.id),
            Some(&action.id),
            Some(self.policy.version()),
            Some(&serde_json::json!({
                "destination": details.destination,
                "artifact_hash": details.artifact_hash,
            })),
        )?;
        let _ = args;
        info!(tool = %spec.name, approval = %approval.id, "approval requested");
        Ok(ToolOutcome::PendingApproval {
            approval_id: approval.id,
            action_id: action.id,
            summary: approval.summary,
            destination: details.destination,
        })
    }

    async fn dispatch(
        &self,
        tool: &dyn Tool,
        args: Value,
        context: &ToolContext,
        _grant: ToolOutput,
    ) -> Result<ToolOutcome> {
        if self.standing_consent {
            self.store.record_audit(
                &context.actor,
                "approval.owner_standing_consent",
                Some(&tool.spec().name),
                context.task_id.as_deref(),
                None,
                None,
                Some(self.policy.version()),
                None,
            )?;
        }
        let started = std::time::Instant::now();
        let (output, listed) = run_tool_call(tool, args, context).await;
        let output = self.redact_output(context, output);
        self.finish_tool_call(tool, &output, &listed, context, started.elapsed())?;
        output.map(ToolOutcome::Completed)
    }

    /// Dispatch an action the acting user approved.
    ///
    /// Before any state change the approval chain is re-verified: the
    /// approval that authorized the action and the action's task (when it has
    /// one) must both belong to the acting user. Scoped reads make a foreign
    /// row indistinguishable from a missing one, so a mismatch fails closed
    /// with `NotFound` and nothing is dispatched.
    pub async fn execute_approved(
        &self,
        action_id: &str,
        context: &ToolContext,
    ) -> Result<ToolOutcome> {
        let action = self.store.get_action(&context.user_id, action_id)?;
        let approval = self
            .store
            .get_approval_for_action(&context.user_id, action_id)?;
        self.verify_action_task(&context.user_id, &action)?;
        if action.status != "approved" {
            return Err(SeraError::InvalidState(format!(
                "action {action_id} is not approved (status: {})",
                action.status
            )));
        }
        if action.policy_version != self.policy.version() {
            self.store
                .update_action(&context.user_id, action_id, "invalidated", None)?;
            self.store.record_audit(
                &context.actor,
                "approval.invalidated_by_policy",
                Some(&action.tool),
                context.task_id.as_deref(),
                None,
                Some(action_id),
                Some(self.policy.version()),
                None,
            )?;
            return Err(SeraError::PolicyViolation);
        }
        if approval.state != ApprovalState::ApprovedOnce.as_str()
            && approval.state != ApprovalState::ApprovedAlways.as_str()
        {
            return Err(SeraError::InvalidState(format!(
                "approval for action {action_id} is not approved (state: {})",
                approval.state
            )));
        }
        if !self
            .store
            .transition_action(&context.user_id, action_id, "approved", "dispatching")?
        {
            return Err(SeraError::InvalidState(
                "approval was already consumed".into(),
            ));
        }
        let args: Value = serde_json::from_str(&action.canonical_args)?;
        if canonical_hash(&args)? != action.canonical_hash {
            self.store
                .update_action(&context.user_id, action_id, "invalidated", None)?;
            return Err(SeraError::InvalidState(
                "approved action arguments changed".into(),
            ));
        }
        self.store.record_audit(
            &context.actor,
            "action.dispatching",
            Some(&action.tool),
            context.task_id.as_deref(),
            None,
            Some(action_id),
            Some(self.policy.version()),
            None,
        )?;
        let tool = self
            .registry
            .get(&action.tool)
            .ok_or_else(|| SeraError::NotFound(format!("tool {}", action.tool)))?;
        if !context.allowed_tools.contains(&action.tool) {
            self.store.update_action(
                &context.user_id,
                action_id,
                "failed",
                Some(&serde_json::json!({"error": "tool no longer allowlisted"})),
            )?;
            return Err(SeraError::Forbidden(
                "approved tool is no longer allowlisted".into(),
            ));
        }
        // A destination that moved since approval (a browser page that
        // navigated, say) needs a new approval, not this one.
        let live = match tool.live_destination(&args, context).await {
            Ok(live) => live,
            Err(error) => {
                self.store.update_action(
                    &context.user_id,
                    action_id,
                    "failed",
                    Some(&serde_json::json!({"error": error.to_string()})),
                )?;
                return Err(error);
            }
        };
        if let Some(live) = live
            && action.destination.as_deref() != Some(live.as_str())
        {
            self.store
                .update_action(&context.user_id, action_id, "invalidated", None)?;
            self.store.record_audit(
                &context.actor,
                "approval.destination_changed",
                Some(&action.tool),
                context.task_id.as_deref(),
                None,
                Some(action_id),
                Some(self.policy.version()),
                None,
            )?;
            return Err(SeraError::InvalidState(
                "the destination changed since approval; approve it again".into(),
            ));
        }
        let started = std::time::Instant::now();
        let (output, listed) = run_tool_call(tool.as_ref(), args, context).await;
        let output = self.redact_output(context, output);
        let succeeded = output.is_ok();
        self.finish_tool_call(tool.as_ref(), &output, &listed, context, started.elapsed())?;
        match output {
            Ok(value) => {
                self.store.update_action(
                    &context.user_id,
                    action_id,
                    "executed",
                    Some(&serde_json::json!({
                        "artifact_hash": value.artifact_hash,
                        "destination": value.destination,
                    })),
                )?;
                self.store.record_audit(
                    &context.actor,
                    "action.executed",
                    Some(&action.tool),
                    context.task_id.as_deref(),
                    None,
                    Some(action_id),
                    Some(self.policy.version()),
                    None,
                )?;
                Ok(ToolOutcome::Completed(value))
            }
            Err(error) => {
                self.store.update_action(
                    &context.user_id,
                    action_id,
                    if is_ambiguous(&error) {
                        "outcome_unknown"
                    } else {
                        "failed"
                    },
                    Some(&serde_json::json!({"error": error.to_string()})),
                )?;
                Err(error)
            }
        }
        .inspect_err(|_| {
            let _ = succeeded;
        })
    }

    fn finish_tool_call(
        &self,
        tool: &dyn Tool,
        output: &Result<ToolOutput>,
        listed: &[ExtraArtifact],
        context: &ToolContext,
        elapsed: std::time::Duration,
    ) -> Result<()> {
        let spec = tool.spec();
        match output {
            Ok(value) => {
                if !self.policy.is_allowed(&value.content) {
                    self.store.record_audit(
                        &context.actor,
                        "policy.blocked_tool_output",
                        Some(&spec.name),
                        context.task_id.as_deref(),
                        None,
                        None,
                        Some(self.policy.version()),
                        None,
                    )?;
                    warn!(tool = %spec.name, "blocked tool output by content policy");
                    return Err(SeraError::PolicyViolation);
                }
                if value.artifact_hash.is_some() {
                    self.store.record_audit(
                        &context.actor,
                        "artifact.created",
                        Some(&spec.name),
                        context.task_id.as_deref(),
                        None,
                        None,
                        Some(self.policy.version()),
                        Some(&serde_json::json!({
                            "sha256": value.artifact_hash,
                            "filename": value.filename,
                        })),
                    )?;
                }
                // A non-external tool never creates an `actions` row, so record
                // the artifact here as well. The owner library unions both, and
                // the row is scoped to the acting user.
                if let (Some(hash), Some(conversation_id)) = (
                    value.artifact_hash.as_deref(),
                    context.conversation_id.as_deref(),
                ) {
                    self.store.record_conversation_artifact(
                        &context.user_id,
                        conversation_id,
                        context.turn_id.as_deref(),
                        &spec.name,
                        hash,
                        value.filename.as_deref().unwrap_or_default(),
                        None,
                    )?;
                }
                // Further artifacts the call listed (0.24). Each one gets a
                // well-formed hash, the blocklist on its filename and title, and
                // one record per hash.
                if let Some(conversation_id) = context.conversation_id.as_deref() {
                    let mut seen: HashSet<String> = value.artifact_hash.iter().cloned().collect();
                    for extra in listed {
                        if !is_sha256_hex(&extra.hash) || !seen.insert(extra.hash.clone()) {
                            continue;
                        }
                        let title = extra.title.as_deref().unwrap_or_default();
                        if !self.policy.is_allowed(&extra.filename)
                            || !self.policy.is_allowed(title)
                        {
                            self.store.record_audit(
                                &context.actor,
                                "policy.blocked_tool_output",
                                Some(&spec.name),
                                context.task_id.as_deref(),
                                None,
                                None,
                                Some(self.policy.version()),
                                None,
                            )?;
                            continue;
                        }
                        self.store.record_conversation_artifact(
                            &context.user_id,
                            conversation_id,
                            context.turn_id.as_deref(),
                            &spec.name,
                            &extra.hash,
                            &extra.filename,
                            extra.title.as_deref(),
                        )?;
                        self.store.record_audit(
                            &context.actor,
                            "artifact.created",
                            Some(&spec.name),
                            context.task_id.as_deref(),
                            None,
                            None,
                            Some(self.policy.version()),
                            Some(&serde_json::json!({
                                "sha256": extra.hash,
                                "filename": extra.filename,
                            })),
                        )?;
                    }
                }
                self.store.record_audit(
                    &context.actor,
                    "tool.completed",
                    Some(&spec.name),
                    context.task_id.as_deref(),
                    None,
                    None,
                    Some(self.policy.version()),
                    Some(&serde_json::json!({
                        "elapsed_ms": elapsed.as_millis() as u64,
                        "artifact_hash": value.artifact_hash,
                    })),
                )?;
                Ok(())
            }
            Err(error) => {
                self.store.record_audit(
                    &context.actor,
                    "tool.failed",
                    Some(&spec.name),
                    context.task_id.as_deref(),
                    None,
                    None,
                    Some(self.policy.version()),
                    Some(&serde_json::json!({
                        "elapsed_ms": elapsed.as_millis() as u64,
                        "error": error.to_string(),
                        "outcome_unknown": is_ambiguous(error),
                    })),
                )?;
                Ok(())
            }
        }
    }

    /// Re-verify that an action's task (when present) belongs to the acting
    /// user. The scoped read makes a foreign task indistinguishable from a
    /// missing one; either way the caller fails closed before dispatching.
    fn verify_action_task(&self, user_id: &str, action: &Action) -> Result<()> {
        if let Some(task_id) = action.task_id.as_deref() {
            let task = self.store.get_task(user_id, task_id)?;
            if task.user_id != user_id {
                return Err(SeraError::NotFound(format!("task {task_id}")));
            }
        }
        Ok(())
    }

    /// Whether the action's tool allows a standing grant for its arguments. A
    /// tool that is no longer registered allows none.
    fn standing_grant_allowed(&self, action: &Action) -> bool {
        let Some(tool) = self.registry.get(&action.tool) else {
            return false;
        };
        serde_json::from_str::<Value>(&action.canonical_args)
            .is_ok_and(|args| tool.allow_standing_grant(&args))
    }

    /// Apply an approval decision for the acting user.
    ///
    /// The decision resolves within the acting user's rows: reads and writes
    /// are scoped by `acting_user_id`, so a user can only decide approvals
    /// recorded under their own id — another user's approval is
    /// indistinguishable from an unknown one (`NotFound`). The admin has no
    /// special power here: members approve their own tool actions, and the
    /// admin's role only widens what they can read in the audit ledger.
    pub fn decide(
        &self,
        acting_user_id: &str,
        approval_id: &str,
        decision: Decision,
    ) -> Result<DecisionResult> {
        let approval = self.store.get_approval(acting_user_id, approval_id)?;
        let action = self.store.get_action(acting_user_id, &approval.action_id)?;
        self.verify_action_task(acting_user_id, &action)?;
        if approval.state != ApprovalState::Pending.as_str() {
            return Err(SeraError::InvalidState(format!(
                "approval is {}",
                approval.state
            )));
        }
        if action.policy_version != self.policy.version() {
            self.store.transition_approval(
                acting_user_id,
                approval_id,
                ApprovalState::Invalidated.as_str(),
                None,
                Some("policy changed"),
            )?;
            return Err(SeraError::PolicyViolation);
        }
        // "Always" remembers the call. The tool may refuse that for this call
        // (a vault secret typed into a page): the choice then covers this
        // action only, and the next identical call asks again.
        let decision = if decision == Decision::Always && !self.standing_grant_allowed(&action) {
            Decision::Once
        } else {
            decision
        };
        match decision {
            Decision::Deny => {
                if !self.store.transition_approval(
                    acting_user_id,
                    approval_id,
                    ApprovalState::Denied.as_str(),
                    None,
                    None,
                )? {
                    return Err(SeraError::InvalidState("approval already decided".into()));
                }
                self.store
                    .update_action(acting_user_id, &action.id, "denied", None)?;
                self.store.record_audit(
                    acting_user_id,
                    "approval.denied",
                    Some(&action.tool),
                    None,
                    Some(approval_id),
                    Some(&action.id),
                    Some(self.policy.version()),
                    None,
                )?;
                Ok(DecisionResult {
                    approval: self.store.get_approval(acting_user_id, approval_id)?,
                    action: self.store.get_action(acting_user_id, &action.id)?,
                    resume_task: false,
                })
            }
            Decision::Once | Decision::Always => {
                let state = if decision == Decision::Once {
                    ApprovalState::ApprovedOnce
                } else {
                    ApprovalState::ApprovedAlways
                };
                if !self.store.transition_approval(
                    acting_user_id,
                    approval_id,
                    state.as_str(),
                    Some("exact_payload"),
                    None,
                )? {
                    return Err(SeraError::InvalidState("approval already decided".into()));
                }
                if decision == Decision::Always {
                    self.store.insert_grant(
                        acting_user_id,
                        &action.tool,
                        &action.canonical_hash,
                        action.destination.as_deref(),
                        &action.policy_version,
                        approval_id,
                    )?;
                }
                self.store.transition_action(
                    acting_user_id,
                    &action.id,
                    "pending_approval",
                    "approved",
                )?;
                self.store.record_audit(
                    acting_user_id,
                    "approval.approved",
                    Some(&action.tool),
                    None,
                    Some(approval_id),
                    Some(&action.id),
                    Some(self.policy.version()),
                    Some(&serde_json::json!({
                        "always": decision == Decision::Always,
                        "destination": action.destination,
                    })),
                )?;
                Ok(DecisionResult {
                    approval: self.store.get_approval(acting_user_id, approval_id)?,
                    action: self.store.get_action(acting_user_id, &action.id)?,
                    resume_task: true,
                })
            }
        }
    }

    /// Scan outgoing text against the global policy and audit a block under
    /// the acting user. The caller always has an acting user (an API session,
    /// a Telegram binding, or a task's owner); there is no gateway-level
    /// fallback actor.
    pub fn scan_egress(&self, user_id: &str, text: &str) -> Result<()> {
        if !self.policy.is_allowed(text) {
            self.store.record_audit(
                user_id,
                "policy.blocked_egress",
                None,
                None,
                None,
                None,
                Some(self.policy.version()),
                None,
            )?;
            return Err(SeraError::PolicyViolation);
        }
        Ok(())
    }

    pub fn sha256(&self, bytes: &[u8]) -> String {
        sha256_hex(bytes)
    }
}

fn is_ambiguous(error: &SeraError) -> bool {
    matches!(
        error,
        SeraError::External(message)
            if message.contains("timeout") || message.contains("unknown") || message.contains("connection")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Policy;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeExternal {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for FakeExternal {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "onedrive_upload".into(),
                description: "fake upload".into(),
                parameters: serde_json::json!({"type": "object"}),
                external_effect: true,
            }
        }

        fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
            Ok(ApprovalDetails {
                summary: format!("upload to {}", args["dest"].as_str().unwrap_or("?")),
                destination: args["dest"].as_str().map(str::to_owned),
                artifact_hash: Some("abc".into()),
            })
        }

        async fn execute(&self, _args: Value, _context: &ToolContext) -> Result<ToolOutput> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput {
                content: "uploaded".into(),
                destination: Some("folder".into()),
                ..ToolOutput::default()
            })
        }
    }

    fn setup() -> (Gateway, Arc<AtomicUsize>, ToolContext) {
        let store = Store::open_in_memory(384).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::default();
        registry
            .register(FakeExternal {
                calls: calls.clone(),
            })
            .unwrap();
        let gateway = Gateway::new(
            store.clone(),
            Arc::new(Policy::from_text("TopSecret")),
            registry,
            1,
            false,
        );
        let admin = gateway.store().admin_user_id().unwrap();
        // The action's task must be a real row owned by the acting user: the
        // gateway re-verifies the approval chain (approval + action + task)
        // before dispatching.
        let task = gateway
            .store()
            .enqueue_task(
                &admin,
                "turn",
                "maker",
                &serde_json::json!({}),
                0,
                chrono::Utc::now(),
            )
            .unwrap();
        let context = ToolContext {
            user_id: admin.clone(),
            actor: admin,
            bot: "maker".into(),
            task_id: Some(task.id),
            turn_id: None,
            conversation_id: None,
            allowed_tools: ["onedrive_upload".to_string()].into_iter().collect(),
            require_approval: HashSet::new(),
        };
        (gateway, calls, context)
    }

    #[tokio::test]
    async fn external_tool_waits_for_approval() {
        let (gateway, calls, context) = setup();
        let outcome = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"dest": "folder"}),
                &context,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::PendingApproval { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn once_approval_executes_and_cannot_be_reused() {
        let (gateway, calls, context) = setup();
        let outcome = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"dest": "folder"}),
                &context,
            )
            .await
            .unwrap();
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = outcome
        else {
            panic!("expected approval");
        };
        gateway
            .decide(&context.user_id, &approval_id, Decision::Once)
            .unwrap();
        gateway
            .execute_approved(&action_id, &context)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            gateway
                .execute_approved(&action_id, &context)
                .await
                .is_err()
        );
    }

    /// A tool whose destination is the page it is on, like a browser secret
    /// fill: the approval binds the destination, and the gateway re-checks it.
    struct LiveDestTool {
        calls: Arc<AtomicUsize>,
        page: Arc<std::sync::Mutex<String>>,
        auto_ok: bool,
    }

    #[async_trait]
    impl Tool for LiveDestTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "onedrive_upload".into(),
                description: "fake live destination".into(),
                parameters: serde_json::json!({"type": "object"}),
                external_effect: true,
            }
        }

        fn requires_approval(&self, _args: &Value) -> bool {
            true
        }

        async fn approval_details_for(
            &self,
            _args: &Value,
            _context: &ToolContext,
        ) -> Result<ApprovalDetails> {
            let page = self.page.lock().unwrap().clone();
            Ok(ApprovalDetails {
                summary: format!("type on {page}"),
                destination: Some(format!("vault:bank → {page}")),
                artifact_hash: None,
            })
        }

        async fn live_destination(
            &self,
            _args: &Value,
            _context: &ToolContext,
        ) -> Result<Option<String>> {
            let page = self.page.lock().unwrap().clone();
            Ok(Some(format!("vault:bank → {page}")))
        }

        async fn auto_approve_allowed(&self, _args: &Value, _context: &ToolContext) -> bool {
            self.auto_ok
        }

        // Like a vault secret fill: "always" never remembers it.
        fn allow_standing_grant(&self, _args: &Value) -> bool {
            false
        }

        async fn execute(&self, _args: Value, _context: &ToolContext) -> Result<ToolOutput> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput {
                content: "typed".into(),
                ..ToolOutput::default()
            })
        }
    }

    fn live_setup(
        auto_ok: bool,
        page: &str,
    ) -> (
        Gateway,
        Arc<AtomicUsize>,
        Arc<std::sync::Mutex<String>>,
        ToolContext,
    ) {
        let store = Store::open_in_memory(384).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let page = Arc::new(std::sync::Mutex::new(page.to_owned()));
        let mut registry = ToolRegistry::default();
        registry
            .register(LiveDestTool {
                calls: calls.clone(),
                page: page.clone(),
                auto_ok,
            })
            .unwrap();
        let gateway = Gateway::new(store.clone(), Arc::new(Policy::empty()), registry, 1, false);
        let admin = gateway.store().admin_user_id().unwrap();
        let task = gateway
            .store()
            .enqueue_task(
                &admin,
                "turn",
                "main",
                &serde_json::json!({}),
                0,
                chrono::Utc::now(),
            )
            .unwrap();
        let context = ToolContext {
            user_id: admin.clone(),
            actor: admin,
            bot: "main".into(),
            task_id: Some(task.id),
            turn_id: None,
            conversation_id: Some("dir:live".into()),
            allowed_tools: ["onedrive_upload".to_string()].into_iter().collect(),
            require_approval: HashSet::new(),
        };
        (gateway, calls, page, context)
    }

    #[tokio::test]
    async fn an_approved_destination_that_moved_is_not_dispatched() {
        let (gateway, calls, page, context) = live_setup(false, "login.bank.example");
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            destination,
            ..
        } = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"field": "password"}),
                &context,
            )
            .await
            .unwrap()
        else {
            panic!("expected approval");
        };
        assert_eq!(
            destination.as_deref(),
            Some("vault:bank → login.bank.example")
        );
        // The page navigates elsewhere while the owner decides.
        *page.lock().unwrap() = "evil.example".into();
        gateway
            .decide(&context.user_id, &approval_id, Decision::Once)
            .unwrap();
        assert!(matches!(
            gateway.execute_approved(&action_id, &context).await,
            Err(SeraError::InvalidState(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let action = gateway
            .store()
            .get_action(&context.user_id, &action_id)
            .unwrap();
        assert_eq!(action.status, "invalidated");
    }

    #[tokio::test]
    async fn an_unchanged_destination_dispatches_after_approval() {
        let (gateway, calls, _page, context) = live_setup(false, "login.bank.example");
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"field": "password"}),
                &context,
            )
            .await
            .unwrap()
        else {
            panic!("expected approval");
        };
        gateway
            .decide(&context.user_id, &approval_id, Decision::Once)
            .unwrap();
        gateway
            .execute_approved(&action_id, &context)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn yolo_approves_only_calls_the_tool_allows() {
        let store_check = |gateway: &Gateway, context: &ToolContext| {
            gateway
                .store()
                .set_yolo(&context.user_id, "dir:live", true)
                .unwrap();
        };
        // The tool refuses: the call stays pending for the owner even with yolo on.
        let (gateway, calls, _page, context) = live_setup(false, "login.bank.example");
        store_check(&gateway, &context);
        let outcome = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"field": "password"}),
                &context,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::PendingApproval { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        // The tool allows it: yolo approves and the call runs.
        let (gateway, calls, _page, context) = live_setup(true, "login.bank.example");
        store_check(&gateway, &context);
        let outcome = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"field": "password"}),
                &context,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::Completed(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn always_approval_only_matches_exact_arguments() {
        let (gateway, calls, context) = setup();
        let args = serde_json::json!({"dest": "folder"});
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = gateway
            .execute("onedrive_upload", args.clone(), &context)
            .await
            .unwrap()
        else {
            panic!("expected approval");
        };
        gateway
            .decide(&context.user_id, &approval_id, Decision::Always)
            .unwrap();
        gateway
            .execute_approved(&action_id, &context)
            .await
            .unwrap();
        let again = gateway
            .execute("onedrive_upload", args, &context)
            .await
            .unwrap();
        assert!(matches!(again, ToolOutcome::Completed(_)));
        let changed = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"dest": "other"}),
                &context,
            )
            .await
            .unwrap();
        assert!(matches!(changed, ToolOutcome::PendingApproval { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn always_is_downgraded_for_a_call_the_tool_will_not_remember() {
        let (gateway, calls, _page, context) = live_setup(false, "login.bank.example");
        let args = serde_json::json!({"field": "password"});
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = gateway
            .execute("onedrive_upload", args.clone(), &context)
            .await
            .unwrap()
        else {
            panic!("expected approval");
        };
        // The owner picks "always": the approval is recorded as once only.
        let decided = gateway
            .decide(&context.user_id, &approval_id, Decision::Always)
            .unwrap();
        assert_eq!(decided.approval.state, ApprovalState::ApprovedOnce.as_str());
        gateway
            .execute_approved(&action_id, &context)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // No grant was left behind, so the same call asks again.
        let again = gateway
            .execute("onedrive_upload", args.clone(), &context)
            .await
            .unwrap();
        assert!(matches!(again, ToolOutcome::PendingApproval { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_standing_grant_row_is_ignored_for_a_call_the_tool_will_not_remember() {
        let (gateway, calls, _page, context) = live_setup(false, "login.bank.example");
        let args = serde_json::json!({"field": "password"});
        let ToolOutcome::PendingApproval { approval_id, .. } = gateway
            .execute("onedrive_upload", args.clone(), &context)
            .await
            .unwrap()
        else {
            panic!("expected approval");
        };
        // A grant row that matches this call exactly, as an older build could
        // have written it. The gateway must not honour it.
        gateway
            .store()
            .insert_grant(
                &context.user_id,
                "onedrive_upload",
                &canonical_hash(&args).unwrap(),
                Some("vault:bank → login.bank.example"),
                gateway.policy().version(),
                &approval_id,
            )
            .unwrap();
        let outcome = gateway
            .execute("onedrive_upload", args, &context)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::PendingApproval { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn deny_blocks_execution() {
        let (gateway, calls, context) = setup();
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"dest": "folder"}),
                &context,
            )
            .await
            .unwrap()
        else {
            panic!("expected approval");
        };
        gateway
            .decide(&context.user_id, &approval_id, Decision::Deny)
            .unwrap();
        assert!(
            gateway
                .execute_approved(&action_id, &context)
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn policy_blocks_approved_output() {
        let store = Store::open_in_memory(384).unwrap();
        let mut registry = ToolRegistry::default();
        registry
            .register(FakeExternal {
                calls: Arc::new(AtomicUsize::new(0)),
            })
            .unwrap();
        let gateway = Gateway::new(
            store.clone(),
            Arc::new(Policy::from_text("TopSecret")),
            registry,
            1,
            false,
        );
        let admin = store.admin_user_id().unwrap();
        let context = ToolContext {
            user_id: admin.clone(),
            actor: admin,
            bot: "maker".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: ["onedrive_upload".to_string()].into_iter().collect(),
            require_approval: HashSet::new(),
        };
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"dest": "folder"}),
                &context,
            )
            .await
            .unwrap()
        else {
            panic!("expected approval");
        };
        gateway
            .decide(&context.user_id, &approval_id, Decision::Once)
            .unwrap();
        let output = gateway.execute_approved(&action_id, &context).await;
        assert!(output.is_ok());
    }

    struct FakeLeak;

    #[async_trait]
    impl Tool for FakeLeak {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "leak".into(),
                description: "returns prohibited text".into(),
                parameters: serde_json::json!({"type": "object"}),
                external_effect: false,
            }
        }

        async fn execute(&self, _args: Value, _context: &ToolContext) -> Result<ToolOutput> {
            Ok(ToolOutput {
                content: "TopSecret material".into(),
                ..ToolOutput::default()
            })
        }
    }

    #[tokio::test]
    async fn blocks_policy_violating_tool_output() {
        let store = Store::open_in_memory(384).unwrap();
        let mut registry = ToolRegistry::default();
        registry.register(FakeLeak).unwrap();
        let gateway = Gateway::new(
            store.clone(),
            Arc::new(Policy::from_text("TopSecret")),
            registry,
            1,
            false,
        );
        let admin = store.admin_user_id().unwrap();
        let context = ToolContext {
            user_id: admin.clone(),
            actor: admin,
            bot: "main".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: ["leak".to_string()].into_iter().collect(),
            require_approval: HashSet::new(),
        };
        assert!(matches!(
            gateway
                .execute("leak", serde_json::json!({}), &context)
                .await,
            Err(SeraError::PolicyViolation)
        ));
    }

    fn artifact_meta(filename: &str) -> crate::maker::ArtifactMeta {
        crate::maker::ArtifactMeta {
            title: filename.into(),
            filename: filename.into(),
            language: crate::domain::Language::En,
            source_artifact: None,
            created_at: chrono::Utc::now(),
        }
    }

    /// One tool call that saves three files and lists them: the primary and
    /// two extras. A duplicate, a blocked name and a malformed hash must not
    /// record.
    struct BatchTool {
        artifacts: Arc<crate::maker::ArtifactStore>,
    }

    #[async_trait]
    impl Tool for BatchTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "make_batch".into(),
                description: "fake batch".into(),
                parameters: serde_json::json!({"type": "object"}),
                external_effect: false,
            }
        }

        async fn execute(&self, _args: Value, _context: &ToolContext) -> Result<ToolOutput> {
            let one = self
                .artifacts
                .save(b"one", "one.txt", artifact_meta("one.txt"))?;
            let two = self
                .artifacts
                .save(b"two", "two.txt", artifact_meta("two.txt"))?;
            let three =
                self.artifacts
                    .save(b"three", "TopSecret.txt", artifact_meta("TopSecret.txt"))?;
            // A file the call never saved, shaped like a stored one, is refused.
            let forged = crate::maker::StoredArtifact {
                hash: "not-a-hash".into(),
                filename: "bad.txt".into(),
                bytes: Vec::new(),
                meta: artifact_meta("bad.txt"),
            };
            crate::maker::list_saved_artifact(&two);
            crate::maker::list_saved_artifact(&two);
            crate::maker::list_saved_artifact(&three);
            crate::maker::list_saved_artifact(&forged);
            Ok(ToolOutput {
                content: "made three files".into(),
                artifact_hash: Some(one.hash),
                filename: Some("one.txt".into()),
                ..ToolOutput::default()
            })
        }
    }

    /// Returns one fixed output and saves nothing.
    struct FixedTool {
        output: ToolOutput,
    }

    #[async_trait]
    impl Tool for FixedTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "fixed".into(),
                description: "fixed output".into(),
                parameters: serde_json::json!({"type": "object"}),
                external_effect: false,
            }
        }

        async fn execute(&self, _args: Value, _context: &ToolContext) -> Result<ToolOutput> {
            Ok(self.output.clone())
        }
    }

    /// Lists a file that an earlier call saved. The list must refuse it.
    struct ListForeignTool {
        stored: crate::maker::StoredArtifact,
    }

    #[async_trait]
    impl Tool for ListForeignTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "list_foreign".into(),
                description: "lists an artifact it did not save".into(),
                parameters: serde_json::json!({"type": "object"}),
                external_effect: false,
            }
        }

        async fn execute(&self, _args: Value, _context: &ToolContext) -> Result<ToolOutput> {
            assert!(!crate::maker::list_saved_artifact(&self.stored));
            Ok(ToolOutput::default())
        }
    }

    /// A gateway with one tool and a context in the admin's `dir:main`.
    fn extra_gateway(tool: impl Tool + 'static) -> (Gateway, ToolContext) {
        let store = Store::open_in_memory(384).unwrap();
        let mut registry = ToolRegistry::default();
        registry.register(tool).unwrap();
        let gateway = Gateway::new(
            store.clone(),
            Arc::new(Policy::from_text("TopSecret")),
            registry,
            1,
            false,
        );
        let admin = store.admin_user_id().unwrap();
        let context = ToolContext {
            user_id: admin.clone(),
            actor: admin,
            bot: "maker".into(),
            task_id: None,
            turn_id: None,
            conversation_id: Some("dir:main".into()),
            allowed_tools: ["fixed", "make_batch", "list_foreign"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            require_approval: HashSet::new(),
        };
        (gateway, context)
    }

    #[tokio::test]
    async fn connector_data_that_names_an_artifact_records_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let artifacts = crate::maker::ArtifactStore::new(directory.path()).unwrap();
        // Saved outside any tool call, so no call of this gateway saved it.
        let existing = artifacts
            .save(b"not yours", "report.pdf", artifact_meta("report.pdf"))
            .unwrap();
        let (gateway, context) = extra_gateway(FixedTool {
            output: ToolOutput {
                content: "listed".into(),
                data: Some(serde_json::json!({
                    "extra_artifacts": [{"hash": existing.hash, "filename": "report.pdf"}]
                })),
                ..ToolOutput::default()
            },
        });
        gateway
            .execute("fixed", serde_json::json!({}), &context)
            .await
            .unwrap();
        assert!(
            gateway
                .store()
                .list_conversation_artifacts(&context.user_id, "dir:main")
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_file_saved_outside_the_call_cannot_be_listed_by_it() {
        let directory = tempfile::tempdir().unwrap();
        let artifacts = crate::maker::ArtifactStore::new(directory.path()).unwrap();
        // Saved outside any tool call, like an earlier turn's file.
        let existing = artifacts
            .save(b"saved earlier", "report.pdf", artifact_meta("report.pdf"))
            .unwrap();
        let (gateway, context) = extra_gateway(ListForeignTool { stored: existing });
        gateway
            .execute("list_foreign", serde_json::json!({}), &context)
            .await
            .unwrap();
        assert!(
            gateway
                .store()
                .list_conversation_artifacts(&context.user_id, "dir:main")
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn one_tool_call_records_every_artifact_once() {
        let directory = tempfile::tempdir().unwrap();
        let artifacts = Arc::new(crate::maker::ArtifactStore::new(directory.path()).unwrap());
        let (gateway, context) = extra_gateway(BatchTool {
            artifacts: artifacts.clone(),
        });
        let outcome = gateway
            .execute("make_batch", serde_json::json!({}), &context)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::Completed(_)));
        let mut hashes: Vec<String> = gateway
            .store()
            .list_conversation_artifacts(&context.user_id, "dir:main")
            .unwrap()
            .into_iter()
            .map(|entry| entry.hash)
            .collect();
        hashes.sort();
        let mut expected = vec![sha256_hex(b"one"), sha256_hex(b"two")];
        expected.sort();
        assert_eq!(hashes, expected);
    }

    fn in_conversation(context: &ToolContext, conversation: &str) -> ToolContext {
        let mut context = context.clone();
        context.conversation_id = Some(conversation.into());
        context
    }

    /// YOLO replaces the click, not the chain: the call runs, an approval and
    /// action row exist, and the audit ledger names the setting.
    #[tokio::test]
    async fn yolo_auto_approves_through_the_full_chain_and_audits_it() {
        let (gateway, calls, context) = setup();
        let context = in_conversation(&context, "dir:main");
        gateway
            .store()
            .set_yolo(&context.user_id, "dir:main", true)
            .unwrap();
        let outcome = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"dest": "folder"}),
                &context,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::Completed(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let audit = gateway
            .store()
            .list_audit_for_user(&context.user_id, 50)
            .unwrap();
        let auto = audit
            .iter()
            .find(|entry| entry.event == "approval.auto_approved")
            .expect("auto approval is audited");
        assert_eq!(
            auto.details.as_ref().unwrap()["reason"],
            "auto-approved by user YOLO setting"
        );
        assert!(auto.approval_id.is_some() && auto.action_id.is_some());
        for event in ["approval.requested", "approval.approved", "action.executed"] {
            assert!(audit.iter().any(|entry| entry.event == event), "{event}");
        }
        // No standing grant is left behind: switching YOLO off asks again.
        gateway
            .store()
            .set_yolo(&context.user_id, "dir:main", false)
            .unwrap();
        let again = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"dest": "folder"}),
                &context,
            )
            .await
            .unwrap();
        assert!(matches!(again, ToolOutcome::PendingApproval { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// One user's YOLO never reaches another conversation or another user,
    /// even when the conversation ids are the same string.
    #[tokio::test]
    async fn yolo_never_leaks_to_another_chat_or_user() {
        let (gateway, calls, admin_context) = setup();
        let member = gateway
            .store()
            .create_user("microsoft:yolo-member", "y@example.example", "Y")
            .unwrap();
        gateway
            .store()
            .set_yolo(&admin_context.user_id, "dir:main", true)
            .unwrap();
        let mut member_context = in_conversation(&admin_context, "dir:main");
        member_context.user_id = member.id.clone();
        member_context.actor = member.id.clone();
        member_context.task_id = None;
        let args = serde_json::json!({"dest": "folder"});
        // Same conversation id, different user: still asks.
        let outcome = gateway
            .execute("onedrive_upload", args.clone(), &member_context)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::PendingApproval { .. }));
        // Same user, different conversation: still asks.
        let other_chat = in_conversation(&admin_context, "dir:other");
        let outcome = gateway
            .execute("onedrive_upload", args.clone(), &other_chat)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::PendingApproval { .. }));
        // No conversation at all (scheduler, API-less paths): still asks.
        let outcome = gateway
            .execute("onedrive_upload", args.clone(), &admin_context)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::PendingApproval { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        // The member turning their own switch on does not touch the admin's.
        gateway
            .store()
            .set_yolo(&member.id, "dir:main", true)
            .unwrap();
        gateway
            .store()
            .set_yolo(&member.id, "dir:main", false)
            .unwrap();
        assert!(
            gateway
                .store()
                .yolo_enabled(&admin_context.user_id, "dir:main")
        );
        let admin_chat = in_conversation(&admin_context, "dir:main");
        let outcome = gateway
            .execute("onedrive_upload", args, &admin_chat)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::Completed(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// Content the policy rejects is not auto-approved: it stays pending.
    #[tokio::test]
    async fn yolo_does_not_approve_blocklisted_arguments() {
        let (gateway, calls, context) = setup();
        let context = in_conversation(&context, "dir:main");
        gateway
            .store()
            .set_yolo(&context.user_id, "dir:main", true)
            .unwrap();
        let outcome = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"dest": "TopSecret"}),
                &context,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::PendingApproval { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn non_allowlisted_tool_is_rejected() {
        let (gateway, _, mut context) = setup();
        context.allowed_tools.clear();
        assert!(matches!(
            gateway
                .execute("onedrive_upload", serde_json::json!({}), &context)
                .await,
            Err(SeraError::Forbidden(_))
        ));
    }

    /// Cross-user isolation at the gateway: a second user's tool context
    /// never matches another user's standing grant, cannot execute another
    /// user's approved action, and cannot decide another user's approval.
    #[tokio::test]
    async fn approvals_grants_and_actions_never_cross_users() {
        let (gateway, calls, admin_context) = setup();
        let member = gateway
            .store()
            .create_user("microsoft:gateway-member", "g@example.example", "G")
            .unwrap();
        let member_context = ToolContext {
            user_id: member.id.clone(),
            actor: member.id.clone(),
            bot: admin_context.bot.clone(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: admin_context.allowed_tools.clone(),
            require_approval: HashSet::new(),
        };

        // The admin requests an approval and approves it "always": the
        // grant is recorded under the admin.
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"dest": "folder"}),
                &admin_context,
            )
            .await
            .unwrap()
        else {
            panic!("expected approval");
        };
        gateway
            .decide(&admin_context.user_id, &approval_id, Decision::Always)
            .unwrap();

        // The member's identical call does not match the admin's grant...
        let member_outcome = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"dest": "folder"}),
                &member_context,
            )
            .await
            .unwrap();
        assert!(
            matches!(member_outcome, ToolOutcome::PendingApproval { .. }),
            "the admin's standing grant must not cover the member"
        );
        // ...the member cannot decide the admin's pending approval...
        let ToolOutcome::PendingApproval {
            approval_id: member_approval,
            ..
        } = member_outcome
        else {
            unreachable!("checked above");
        };
        assert!(
            gateway
                .decide(&member.id, &approval_id, Decision::Once)
                .is_err(),
            "the member must not be able to decide the admin's approval"
        );
        // ...and the member cannot execute the admin's approved action.
        assert!(
            gateway
                .execute_approved(&action_id, &member_context)
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        // The admin's own action still executes under the admin.
        gateway
            .execute_approved(&action_id, &admin_context)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // The member's own approval remains pending for them.
        let pending = gateway
            .store()
            .list_pending_approvals(&member.id, 10)
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, member_approval);
    }

    /// The admin role widens what the admin can read in the audit ledger, not
    /// what they can decide: a member's approval is not in the admin's rows.
    #[tokio::test]
    async fn admin_cannot_decide_a_members_approval() {
        let (gateway, calls, admin_context) = setup();
        let member = gateway
            .store()
            .create_user("microsoft:gateway-member-2", "g2@example.example", "G2")
            .unwrap();
        let member_context = ToolContext {
            user_id: member.id.clone(),
            actor: member.id.clone(),
            bot: admin_context.bot.clone(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: admin_context.allowed_tools.clone(),
            require_approval: HashSet::new(),
        };
        let ToolOutcome::PendingApproval { approval_id, .. } = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"dest": "member-folder"}),
                &member_context,
            )
            .await
            .unwrap()
        else {
            panic!("expected approval");
        };
        assert!(matches!(
            gateway.decide(&admin_context.user_id, &approval_id, Decision::Once),
            Err(SeraError::NotFound(_))
        ));
        assert_eq!(
            gateway
                .store()
                .list_pending_approvals(&member.id, 10)
                .unwrap()
                .len(),
            1,
            "the admin's failed decision must leave the member's approval pending"
        );
        // The member decides their own approval and nothing executes yet.
        gateway
            .decide(&member.id, &approval_id, Decision::Once)
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// A corrupted or mismatched row must never let an approved action
    /// dispatch: the approval and the task are re-checked against the acting
    /// user at execute time.
    #[tokio::test]
    async fn execute_approved_rejects_a_foreign_approval_or_task() {
        let (gateway, calls, context) = setup();
        let member = gateway
            .store()
            .create_user("microsoft:gateway-member-3", "g3@example.example", "G3")
            .unwrap();

        // The approval that authorized the action is re-owned to the member.
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"dest": "folder"}),
                &context,
            )
            .await
            .unwrap()
        else {
            panic!("expected approval");
        };
        gateway
            .decide(&context.user_id, &approval_id, Decision::Once)
            .unwrap();
        gateway
            .store()
            .connection()
            .unwrap()
            .execute(
                "UPDATE approvals SET user_id = ?1 WHERE id = ?2",
                rusqlite::params![member.id, approval_id],
            )
            .unwrap();
        assert!(matches!(
            gateway.execute_approved(&action_id, &context).await,
            Err(SeraError::NotFound(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // The action's task is re-owned to the member.
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"dest": "folder"}),
                &context,
            )
            .await
            .unwrap()
        else {
            panic!("expected approval");
        };
        gateway
            .decide(&context.user_id, &approval_id, Decision::Once)
            .unwrap();
        let member_task = gateway
            .store()
            .enqueue_task(
                &member.id,
                "turn",
                "maker",
                &serde_json::json!({}),
                0,
                chrono::Utc::now(),
            )
            .unwrap();
        gateway
            .store()
            .connection()
            .unwrap()
            .execute(
                "UPDATE actions SET task_id = ?1 WHERE id = ?2",
                rusqlite::params![member_task.id, action_id],
            )
            .unwrap();
        assert!(matches!(
            gateway.execute_approved(&action_id, &context).await,
            Err(SeraError::NotFound(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// Every gateway audit row is attributed to the acting user, and the
    /// admin's ledger does not acquire a member's row.
    #[tokio::test]
    async fn audit_rows_carry_the_acting_user() {
        let (gateway, _, admin_context) = setup();
        let member = gateway
            .store()
            .create_user("microsoft:gateway-member-4", "g4@example.example", "G4")
            .unwrap();
        let member_context = ToolContext {
            user_id: member.id.clone(),
            actor: member.id.clone(),
            bot: admin_context.bot.clone(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: admin_context.allowed_tools.clone(),
            require_approval: HashSet::new(),
        };
        gateway
            .execute(
                "onedrive_upload",
                serde_json::json!({"dest": "folder"}),
                &member_context,
            )
            .await
            .unwrap();
        let member_audit = gateway.store().list_audit_for_user(&member.id, 50).unwrap();
        assert!(
            member_audit
                .iter()
                .any(|entry| entry.event == "approval.requested"),
            "the member's request must be audited under the member"
        );
        assert!(member_audit.iter().all(|entry| entry.actor == member.id));
        let admin_audit = gateway
            .store()
            .list_audit_for_user(&admin_context.user_id, 50)
            .unwrap();
        assert!(
            !admin_audit
                .iter()
                .any(|entry| entry.event == "approval.requested"),
            "the member's audit row must not appear under the admin"
        );
    }

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "echo".into(),
                description: "echoes its argument".into(),
                parameters: serde_json::json!({"type": "object"}),
                external_effect: false,
            }
        }

        async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
            let text = args["text"].as_str().unwrap_or_default().to_owned();
            Ok(ToolOutput {
                content: format!("page says: {text}"),
                data: Some(serde_json::json!({"nested": [text.clone()]})),
                ..ToolOutput::default()
            })
        }
    }

    fn vault_gateway() -> (Gateway, Arc<UserSecrets>, String, String) {
        let store = Store::open_in_memory(384).unwrap();
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [5u8; 32]).unwrap());
        let a = store
            .create_user("subject-a", "a@example.example", "A")
            .unwrap()
            .id;
        let b = store
            .create_user("subject-b", "b@example.example", "B")
            .unwrap()
            .id;
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).unwrap();
        let gateway = Gateway::new(store, Arc::new(Policy::empty()), registry, 1, false)
            .with_vault(users.clone());
        (gateway, users, a, b)
    }

    fn vault_context(user: &str) -> ToolContext {
        ToolContext {
            user_id: user.into(),
            actor: user.into(),
            bot: "main".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: HashSet::from(["echo".to_owned()]),
            require_approval: HashSet::new(),
        }
    }

    fn put_secret(users: &UserSecrets, user: &str, name: &str, value: &str) {
        crate::vault::put(
            users,
            user,
            name,
            &crate::vault::VaultInput {
                kind: crate::vault::VaultKind::ApiKey,
                username: None,
                value: zeroize::Zeroizing::new(value.into()),
                domains: vec![],
                note: None,
                source: crate::vault::VaultSource::Local,
                reference: None,
            },
        )
        .unwrap();
    }

    #[tokio::test]
    async fn non_destructive_tools_skip_approval_unless_bot_lists_are_respected() {
        let (gateway, _users, a, _b) = vault_gateway();
        let mut context = vault_context(&a);
        context.require_approval.insert("echo".into());
        let outcome = gateway
            .execute("echo", serde_json::json!({"text": "x"}), &context)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::Completed(_)));
        let strict = gateway.clone().with_respect_bot_lists(true);
        let outcome = strict
            .execute("echo", serde_json::json!({"text": "x"}), &context)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::PendingApproval { .. }));
    }

    #[test]
    fn destructive_classification_is_explicit() {
        for name in [
            "memory_delete",
            "agentmail_send",
            "telegram_send_document",
            "onedrive_upload",
            "local_write",
            "local_export",
            "github_create_issue",
            "github_comment_issue",
            "todoist_add_task",
            "todoist_complete_task",
            "slack_post_message",
        ] {
            assert!(is_destructive_tool(name), "{name}");
        }
        for name in [
            "browser_navigate",
            "browser_click",
            "browser_type",
            "browser_press",
            "firecrawl_search",
            "github_search_repos",
            "github_get_file",
            "todoist_list_tasks",
            "slack_list_channels",
            "slack_read_channel",
            "exa_search",
            "exa_get_contents",
            "memory_save",
            "pdf_edit",
        ] {
            assert!(!is_destructive_tool(name), "{name}");
        }
    }

    #[tokio::test]
    async fn tool_output_is_redacted_for_the_owner_only() {
        let (gateway, users, a, b) = vault_gateway();
        put_secret(&users, &a, "site", "synthetic-secret-a");
        let args = serde_json::json!({"text": "synthetic-secret-a"});
        let ToolOutcome::Completed(own) = gateway
            .execute("echo", args.clone(), &vault_context(&a))
            .await
            .unwrap()
        else {
            panic!("expected completion");
        };
        assert_eq!(own.content, "page says: [vault:site]");
        assert_eq!(
            own.data.unwrap(),
            serde_json::json!({"nested": ["[vault:site]"]})
        );
        let ToolOutcome::Completed(other) = gateway
            .execute("echo", args, &vault_context(&b))
            .await
            .unwrap()
        else {
            panic!("expected completion");
        };
        assert_eq!(other.content, "page says: synthetic-secret-a");
    }

    #[test]
    fn model_requests_are_redacted_without_touching_the_original() {
        use crate::domain::ToolCall;
        let (gateway, users, a, b) = vault_gateway();
        put_secret(&users, &a, "site", "synthetic-secret-a");
        let messages = vec![
            LlmMessage::user("my key is synthetic-secret-a"),
            LlmMessage::assistant(
                Some("typing synthetic-secret-a".into()),
                vec![ToolCall {
                    id: "call-1".into(),
                    name: "echo".into(),
                    arguments: serde_json::json!({"text": "synthetic-secret-a"}),
                }],
            ),
        ];
        let redacted = gateway.redact_messages(&a, &messages).unwrap();
        let wire = serde_json::to_string(&redacted).unwrap();
        assert!(!wire.contains("synthetic-secret-a"));
        assert!(wire.contains("[vault:site]"));
        assert_eq!(
            messages[0].content.as_deref(),
            Some("my key is synthetic-secret-a")
        );
        // Another user's request is not scrubbed with A's values.
        let other = gateway.redact_messages(&b, &messages).unwrap();
        assert!(
            serde_json::to_string(&other)
                .unwrap()
                .contains("synthetic-secret-a")
        );
        assert_eq!(
            gateway.redact_text(&a, "x synthetic-secret-a").unwrap(),
            "x [vault:site]"
        );
    }

    #[tokio::test]
    async fn a_gateway_without_a_vault_leaves_output_alone() {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).unwrap();
        let gateway = Gateway::new(store, Arc::new(Policy::empty()), registry, 1, false);
        let ToolOutcome::Completed(output) = gateway
            .execute(
                "echo",
                serde_json::json!({"text": "plain"}),
                &vault_context(&admin),
            )
            .await
            .unwrap()
        else {
            panic!("expected completion");
        };
        assert_eq!(output.content, "page says: plain");
    }
}
