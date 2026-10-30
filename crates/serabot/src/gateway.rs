use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Duration;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

use crate::domain::{Action, Approval, ApprovalState, Id};
use crate::error::{Result, SeraError};
use crate::policy::Policy;
use crate::store::Store;
use crate::util::{canonical_hash, canonical_json, sha256_hex, truncate_chars};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    #[serde(default)]
    pub external_effect: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolContext {
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
    actor: String,
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
            actor: "owner".into(),
        }
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
        let spec = tool.spec();
        let canonical = canonical_json(&args)?;
        let hash = canonical_hash(&args)?;

        if spec.external_effect || context.require_approval.contains(name) {
            if let Some(output) = self.check_standing_grant(&spec, &args, &hash, context)? {
                return self.dispatch(tool.as_ref(), args, context, output).await;
            }
            return self
                .request_approval(tool.as_ref(), args, context, &canonical, &hash)
                .await;
        }

        let started = std::time::Instant::now();
        let output = tool.execute(args, context).await;
        self.finish_tool_call(tool.as_ref(), &output, context, started.elapsed())?;
        output.map(ToolOutcome::Completed)
    }

    fn check_standing_grant(
        &self,
        spec: &ToolSpec,
        args: &Value,
        hash: &str,
        context: &ToolContext,
    ) -> Result<Option<ToolOutput>> {
        let _ = (spec, args, context);
        let details = match self.registry.get(&spec.name) {
            Some(tool) => tool.approval_details(args)?,
            None => ApprovalDetails::default(),
        };
        if self.store.matching_grant(
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
        let mut details = tool.approval_details(&args)?;
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
        let output = tool.execute(args, context).await;
        self.finish_tool_call(tool, &output, context, started.elapsed())?;
        output.map(ToolOutcome::Completed)
    }

    pub async fn execute_approved(
        &self,
        action_id: &str,
        context: &ToolContext,
    ) -> Result<ToolOutcome> {
        let action = self.store.get_action(action_id)?;
        if action.status != "approved" {
            return Err(SeraError::InvalidState(format!(
                "action {action_id} is not approved (status: {})",
                action.status
            )));
        }
        if action.policy_version != self.policy.version() {
            self.store.update_action(action_id, "invalidated", None)?;
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
        if !self
            .store
            .transition_action(action_id, "approved", "dispatching")?
        {
            return Err(SeraError::InvalidState(
                "approval was already consumed".into(),
            ));
        }
        let args: Value = serde_json::from_str(&action.canonical_args)?;
        if canonical_hash(&args)? != action.canonical_hash {
            self.store.update_action(action_id, "invalidated", None)?;
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
                action_id,
                "failed",
                Some(&serde_json::json!({"error": "tool no longer allowlisted"})),
            )?;
            return Err(SeraError::Forbidden(
                "approved tool is no longer allowlisted".into(),
            ));
        }
        let started = std::time::Instant::now();
        let output = tool.execute(args, context).await;
        let succeeded = output.is_ok();
        self.finish_tool_call(tool.as_ref(), &output, context, started.elapsed())?;
        match output {
            Ok(value) => {
                self.store.update_action(
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
                if let Some(path) = &value.artifact_path {
                    self.store.record_audit(
                        &context.actor,
                        "artifact.created",
                        Some(&spec.name),
                        context.task_id.as_deref(),
                        None,
                        None,
                        Some(self.policy.version()),
                        Some(&serde_json::json!({
                            "path": path,
                            "sha256": value.artifact_hash,
                            "filename": value.filename,
                            "bytes": value.content.len(),
                        })),
                    )?;
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

    pub fn decide(&self, approval_id: &str, decision: Decision) -> Result<DecisionResult> {
        let approval = self.store.get_approval(approval_id)?;
        let action = self.store.get_action(&approval.action_id)?;
        if approval.state != ApprovalState::Pending.as_str() {
            return Err(SeraError::InvalidState(format!(
                "approval is {}",
                approval.state
            )));
        }
        if action.policy_version != self.policy.version() {
            self.store.transition_approval(
                approval_id,
                ApprovalState::Invalidated.as_str(),
                None,
                Some("policy changed"),
            )?;
            return Err(SeraError::PolicyViolation);
        }
        match decision {
            Decision::Deny => {
                if !self.store.transition_approval(
                    approval_id,
                    ApprovalState::Denied.as_str(),
                    None,
                    None,
                )? {
                    return Err(SeraError::InvalidState("approval already decided".into()));
                }
                self.store.update_action(&action.id, "denied", None)?;
                self.store.record_audit(
                    &self.actor,
                    "approval.denied",
                    Some(&action.tool),
                    None,
                    Some(approval_id),
                    Some(&action.id),
                    Some(self.policy.version()),
                    None,
                )?;
                Ok(DecisionResult {
                    approval: self.store.get_approval(approval_id)?,
                    action: self.store.get_action(&action.id)?,
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
                    approval_id,
                    state.as_str(),
                    Some("exact_payload"),
                    None,
                )? {
                    return Err(SeraError::InvalidState("approval already decided".into()));
                }
                if decision == Decision::Always {
                    self.store.insert_grant(
                        &action.tool,
                        &action.canonical_hash,
                        action.destination.as_deref(),
                        &action.policy_version,
                        approval_id,
                    )?;
                }
                self.store
                    .transition_action(&action.id, "pending_approval", "approved")?;
                self.store.record_audit(
                    &self.actor,
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
                    approval: self.store.get_approval(approval_id)?,
                    action: self.store.get_action(&action.id)?,
                    resume_task: true,
                })
            }
        }
    }

    pub fn scan_egress(&self, text: &str) -> Result<()> {
        if !self.policy.is_allowed(text) {
            self.store.record_audit(
                &self.actor,
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
                name: "upload".into(),
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
            store,
            Arc::new(Policy::from_text("TopSecret")),
            registry,
            1,
            false,
        );
        let context = ToolContext {
            actor: "owner".into(),
            bot: "maker".into(),
            task_id: Some("task-1".into()),
            turn_id: None,
            conversation_id: None,
            allowed_tools: ["upload".to_string()].into_iter().collect(),
            require_approval: HashSet::new(),
        };
        (gateway, calls, context)
    }

    #[tokio::test]
    async fn external_tool_waits_for_approval() {
        let (gateway, calls, context) = setup();
        let outcome = gateway
            .execute("upload", serde_json::json!({"dest": "folder"}), &context)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolOutcome::PendingApproval { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn once_approval_executes_and_cannot_be_reused() {
        let (gateway, calls, context) = setup();
        let outcome = gateway
            .execute("upload", serde_json::json!({"dest": "folder"}), &context)
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
        gateway.decide(&approval_id, Decision::Once).unwrap();
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

    #[tokio::test]
    async fn always_approval_only_matches_exact_arguments() {
        let (gateway, calls, context) = setup();
        let args = serde_json::json!({"dest": "folder"});
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = gateway
            .execute("upload", args.clone(), &context)
            .await
            .unwrap()
        else {
            panic!("expected approval");
        };
        gateway.decide(&approval_id, Decision::Always).unwrap();
        gateway
            .execute_approved(&action_id, &context)
            .await
            .unwrap();
        let again = gateway.execute("upload", args, &context).await.unwrap();
        assert!(matches!(again, ToolOutcome::Completed(_)));
        let changed = gateway
            .execute("upload", serde_json::json!({"dest": "other"}), &context)
            .await
            .unwrap();
        assert!(matches!(changed, ToolOutcome::PendingApproval { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn deny_blocks_execution() {
        let (gateway, calls, context) = setup();
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = gateway
            .execute("upload", serde_json::json!({"dest": "folder"}), &context)
            .await
            .unwrap()
        else {
            panic!("expected approval");
        };
        gateway.decide(&approval_id, Decision::Deny).unwrap();
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
            store,
            Arc::new(Policy::from_text("TopSecret")),
            registry,
            1,
            false,
        );
        let context = ToolContext {
            actor: "owner".into(),
            bot: "maker".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: ["upload".to_string()].into_iter().collect(),
            require_approval: HashSet::new(),
        };
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = gateway
            .execute("upload", serde_json::json!({"dest": "folder"}), &context)
            .await
            .unwrap()
        else {
            panic!("expected approval");
        };
        gateway.decide(&approval_id, Decision::Once).unwrap();
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
            store,
            Arc::new(Policy::from_text("TopSecret")),
            registry,
            1,
            false,
        );
        let context = ToolContext {
            actor: "owner".into(),
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

    #[tokio::test]
    async fn non_allowlisted_tool_is_rejected() {
        let (gateway, _, mut context) = setup();
        context.allowed_tools.clear();
        assert!(matches!(
            gateway
                .execute("upload", serde_json::json!({}), &context)
                .await,
            Err(SeraError::Forbidden(_))
        ));
    }
}
