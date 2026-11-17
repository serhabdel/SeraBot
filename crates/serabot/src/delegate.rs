use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde_json::{Value, json};
use tracing::warn;

use crate::config::{BotConfig, BotRoute, LoadedBot};
use crate::domain::new_id;
use crate::error::{Result, SeraError};
use crate::fleet::{Fleet, FleetRegistry};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolSpec};
use crate::memory;
use crate::model_router::{ModelRouter, ModelSelection, PinnedRoute, RouterHandle};
use crate::orchestration::HandoffContext;
use crate::orchestration::{BotRole, Handoff, ModelClass, ORCHESTRATOR_BOT, validate_handoff};
use crate::route_chain::NewRouteEvent;
use crate::store::{Store, insert_task_row, now_string, start_of_utc_day, to_string};
use crate::util::sha256_hex;

pub const DELEGATE_TOOL: &str = "delegate";
pub const HANDOFF_QUEUED_KEY: &str = "handoff_queued";
/// How many handoffs the orchestrator may queue in one turn (0.27, raised to
/// 16 in 0.28). Several `delegate` calls in the same round fan out to
/// parallel specialist subagents.
pub const MAX_HANDOFFS_PER_TURN: usize = 16;
/// Default for `[users] child_daily_limit`.
pub const DEFAULT_CHILD_DAILY_LIMIT: u64 = 500;
/// Most times one handoff may be resumed by a bot (the owner is not capped).
pub const MAX_RESUMES: i64 = 2;

#[derive(Clone)]
pub struct DelegateTool {
    store: Store,
    fleet: Arc<FleetRegistry>,
    router: RouterHandle,
    child_daily_limit: u64,
}

impl DelegateTool {
    pub fn new(store: Store, fleet: Arc<FleetRegistry>, router: impl Into<RouterHandle>) -> Self {
        Self {
            store,
            fleet,
            router: router.into(),
            child_daily_limit: DEFAULT_CHILD_DAILY_LIMIT,
        }
    }

    /// Cap handoff and resume tasks per user per UTC day.
    pub fn with_child_daily_limit(mut self, limit: u64) -> Self {
        self.child_daily_limit = limit;
        self
    }

    fn orchestrator<'a>(&self, fleet: &'a Fleet) -> Result<&'a LoadedBot> {
        fleet
            .get(ORCHESTRATOR_BOT)
            .filter(|bot| bot.config.enabled && bot.config.role == BotRole::Orchestrator)
            .ok_or_else(|| SeraError::Config("orchestrator bot is not configured".into()))
    }
}

#[async_trait]
impl Tool for DelegateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: DELEGATE_TOOL.into(),
            description: "Queue a bounded handoff from the orchestrator to an enabled specialist. Only main may call this; up to 16 handoffs per turn, so call it several times in one round to fan out parallel subagents.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to_bot": {"type": "string", "description": "Enabled specialist listed in main.delegates"},
                    "objective": {"type": "string", "description": "Non-empty specialist objective (max 2000 characters)"},
                    "skill": {"type": "string", "description": "Optional skill id from the target bot allowlist"},
                    "memory_ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "maxItems": 5,
                        "description": "Existing memory ids to attach (max 5)"
                    },
                    "model_class": {
                        "type": "string",
                        "enum": ["routine", "synthesis"],
                        "description": "Logical model class for the specialist turn"
                    }
                },
                "required": ["to_bot", "objective", "model_class"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        if context.bot != ORCHESTRATOR_BOT {
            return Err(SeraError::Forbidden(
                "only the orchestrator may delegate".into(),
            ));
        }
        let fleet = self.fleet.load(&context.user_id)?;
        let orchestrator = self.orchestrator(&fleet)?;
        let task_id = context
            .task_id
            .as_deref()
            .ok_or_else(|| SeraError::InvalidState("delegate requires an active task".into()))?;
        let turn_id = context
            .turn_id
            .as_deref()
            .ok_or_else(|| SeraError::InvalidState("delegate requires an active turn".into()))?;
        let conversation_id = context
            .conversation_id
            .as_deref()
            .ok_or_else(|| SeraError::InvalidState("delegate requires a conversation id".into()))?;
        self.store
            .ensure_conversation_accepts_turns(&context.user_id, conversation_id)?;

        let to_bot = args
            .get("to_bot")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if !orchestrator.config.delegates.iter().any(|d| d == &to_bot) {
            return Err(SeraError::Validation(
                "handoff to_bot is not delegated by main".into(),
            ));
        }
        let specialist = fleet
            .get(&to_bot)
            .ok_or_else(|| SeraError::NotFound(format!("bot {to_bot}")))?;
        if !specialist.config.enabled || specialist.config.role != BotRole::Specialist {
            return Err(SeraError::Validation("invalid handoff to_bot".into()));
        }

        let objective = args
            .get("objective")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let skill = args
            .get("skill")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let memory_ids: Vec<String> = args
            .get("memory_ids")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        for memory_id in &memory_ids {
            memory::get(&self.store, &context.user_id, memory_id)?;
        }
        let model_class = match args
            .get("model_class")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
        {
            "routine" => ModelClass::Routine,
            "synthesis" => ModelClass::Synthesis,
            _ => {
                return Err(SeraError::Validation(
                    "model_class must be routine or synthesis".into(),
                ));
            }
        };

        let handoff = Handoff {
            id: new_id(),
            parent_task_id: task_id.to_owned(),
            conversation_id: conversation_id.to_owned(),
            from_bot: ORCHESTRATOR_BOT.into(),
            to_bot: to_bot.clone(),
            skill,
            objective: objective.clone(),
            memory_ids: memory_ids.clone(),
            model_class,
        };
        validate_handoff(&handoff, &specialist.config.skills)?;

        // A repeated call (same parent turn, specialist and objective) returns
        // the child it already queued instead of queuing a second one.
        let key_hash = handoff_key_hash(turn_id, &to_bot, &objective);
        if let Some(existing) =
            self.store
                .find_handoff_by_key(&context.user_id, task_id, &key_hash)?
        {
            return Ok(duplicate_output(&existing));
        }

        let class_name = match model_class {
            ModelClass::Routine => "routine",
            ModelClass::Synthesis => "synthesis",
        };
        // The specialist's own route head wins over the class: its owner chose
        // that model for it. Without one, the class picks.
        let selection = match self
            .router
            .get()
            .bot_chain(&context.user_id, &specialist.config)
            .head
        {
            Some(head) => head,
            None => self.router.get().select_class(
                &context.user_id,
                Some(class_name),
                specialist.config.model.as_deref(),
                &specialist.config.name,
                &Default::default(),
            )?,
        };
        let route =
            self.router
                .get()
                .pin_for_bot(&context.user_id, &selection, &specialist.config)?;

        let parent_task = self.store.get_task(&context.user_id, task_id)?;
        let help_depth = parent_task
            .payload
            .get("help_depth")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let mut payload = json!({
            "conversation_id": conversation_id,
            "text": handoff.objective,
            "bot": to_bot,
            "source": "handoff",
            "ask_mode": false,
            "route": route,
            "help_depth": help_depth,
            "handoff": HandoffContext {
                handoff_id: handoff.id.clone(),
                parent_task_id: task_id.to_owned(),
                parent_turn_id: turn_id.to_owned(),
                memory_ids: memory_ids.clone(),
                skill: handoff.skill.clone(),
            },
        });
        if let Some(from) = parent_task.payload.get("help_from") {
            payload["help_from"] = from.clone();
        }
        // The handoff is audited before the specialist turn is queued: a failed
        // audit must never leave an untracked queued turn behind. The audit row
        // needs the child task id first and `Store::enqueue_task` allocates that
        // id itself, so the id is allocated here and the turn is queued under it.
        let child_task_id = new_id();
        self.store.record_audit(
            &context.actor,
            "handoff.queued",
            Some(DELEGATE_TOOL),
            Some(task_id),
            None,
            None,
            None,
            Some(&json!({
                "handoff_id": handoff.id,
                "child_task_id": child_task_id,
                "conversation_id": conversation_id,
                "from_bot": ORCHESTRATOR_BOT,
                "to_bot": to_bot,
                "skill": handoff.skill,
                "memory_ids": memory_ids,
                "model_class": class_name,
            })),
        )?;
        self.store.append_event(
            &context.user_id,
            "handoff.queued",
            &json!({
                "handoff_id": handoff.id,
                "parent_task_id": task_id,
                "child_task_id": child_task_id,
                "to_bot": to_bot,
            }),
        )?;
        // The ledger row and the child task are written in one transaction, so
        // a handoff never exists without its child or the reverse.
        let new = NewHandoff {
            id: &handoff.id,
            parent_task_id: task_id,
            parent_turn_id: Some(turn_id),
            child_task_id: &child_task_id,
            to_bot: &to_bot,
            key_hash: &key_hash,
            route: Some(&json!(route)),
        };
        let (row, created) = match self.store.create_subagent_handoff(
            &context.user_id,
            &new,
            &payload,
            self.child_daily_limit,
        ) {
            Ok(result) => result,
            Err(error) => {
                warn!(bot = %to_bot, %error, "audited handoff was not queued");
                return Err(error);
            }
        };
        if !created {
            // A concurrent identical call won the race.
            return Ok(duplicate_output(&row));
        }

        Ok(ToolOutput {
            content: format!("Queued handoff {id} to {to_bot}.", id = handoff.id),
            data: Some(json!({
                HANDOFF_QUEUED_KEY: true,
                "handoff_id": handoff.id,
                "child_task_id": child_task_id,
                "to_bot": to_bot,
            })),
            ..ToolOutput::default()
        })
    }
}

/// The idempotency key of a handoff: the parent turn, the specialist and the
/// objective. The same call from the same turn always hashes the same.
pub fn handoff_key_hash(parent_turn_id: &str, to_bot: &str, objective: &str) -> String {
    sha256_hex(format!("{parent_turn_id}\0{to_bot}\0{objective}"))
}

fn duplicate_output(existing: &HandoffRow) -> ToolOutput {
    ToolOutput {
        content: format!(
            "Handoff {id} to {to_bot} was already queued for this request; not queued again.",
            id = existing.id,
            to_bot = existing.to_bot
        ),
        data: Some(json!({
            "duplicate": true,
            "handoff_id": existing.id,
            "child_task_id": existing.child_task_id,
            "to_bot": existing.to_bot,
        })),
        ..ToolOutput::default()
    }
}

/// One ledger row, as stored.
#[derive(Debug, Clone, Serialize)]
pub struct HandoffRow {
    pub id: String,
    pub parent_task_id: String,
    pub parent_turn_id: Option<String>,
    pub child_task_id: String,
    pub to_bot: String,
    pub key_hash: String,
    pub status: String,
    pub outcome_code: Option<String>,
    pub effects_unknown: bool,
    pub resume_count: i64,
    pub route: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

/// A ledger row with the live state of its child task folded in. This is what
/// the API and `handoff_status` show.
#[derive(Debug, Clone, Serialize)]
pub struct HandoffView {
    pub id: String,
    pub parent_task_id: String,
    pub parent_turn_id: Option<String>,
    pub child_task_id: String,
    pub to_bot: String,
    /// `queued`, `running`, `completed`, `failed_resumable`, `failed` or
    /// `resumed`, from the stored row and the child task's own status.
    pub status: &'static str,
    pub outcome_code: Option<String>,
    pub effects_unknown: bool,
    pub resume_count: i64,
    pub route: Option<Value>,
    pub conversation_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// What the ledger needs to record one handoff.
pub struct NewHandoff<'a> {
    pub id: &'a str,
    pub parent_task_id: &'a str,
    pub parent_turn_id: Option<&'a str>,
    pub child_task_id: &'a str,
    pub to_bot: &'a str,
    pub key_hash: &'a str,
    pub route: Option<&'a Value>,
}

/// The status a handoff shows now. A child that failed or was cancelled can
/// be resumed while resumes remain and no effect has an unknown outcome;
/// otherwise it is plainly `failed`.
pub fn effective_status(
    row: &HandoffRow,
    task_status: Option<&str>,
    task_uncertain: bool,
) -> &'static str {
    let resumed = row.status == "resumed";
    match task_status {
        Some("queued") => {
            if resumed {
                "resumed"
            } else {
                "queued"
            }
        }
        Some("running" | "awaiting_approval" | "waiting_model") => {
            if resumed {
                "resumed"
            } else {
                "running"
            }
        }
        Some("done") => "completed",
        // An uncertain task is never resumed: its effect may have happened.
        Some("needs_review") => "failed",
        Some("failed" | "cancelled") => {
            if row.effects_unknown || task_uncertain || row.resume_count >= MAX_RESUMES {
                "failed"
            } else {
                "failed_resumable"
            }
        }
        // A child row that is gone, or a status this build does not know,
        // falls back to what the ledger stored.
        _ => match row.status.as_str() {
            "completed" => "completed",
            "failed_resumable" => "failed_resumable",
            "resumed" => "resumed",
            "running" => "running",
            "failed" => "failed",
            _ => "queued",
        },
    }
}

const HANDOFF_COLUMNS: &str = "id, parent_task_id, parent_turn_id, child_task_id, to_bot, key_hash,
    status, outcome_code, effects_unknown, resume_count, route_json, created_at, updated_at";

fn handoff_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HandoffRow> {
    Ok(HandoffRow {
        id: row.get(0)?,
        parent_task_id: row.get(1)?,
        parent_turn_id: row.get(2)?,
        child_task_id: row.get(3)?,
        to_bot: row.get(4)?,
        key_hash: row.get(5)?,
        status: row.get(6)?,
        outcome_code: row.get(7)?,
        effects_unknown: row.get::<_, i64>(8)? != 0,
        resume_count: row.get(9)?,
        route: row
            .get::<_, Option<String>>(10)?
            .and_then(|text| serde_json::from_str(&text).ok()),
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn find_by_key(
    connection: &rusqlite::Connection,
    user_id: &str,
    parent_task_id: &str,
    key_hash: &str,
) -> Result<Option<HandoffRow>> {
    Ok(connection
        .query_row(
            &format!(
                "SELECT {HANDOFF_COLUMNS} FROM handoffs
                 WHERE user_id = ?1 AND parent_task_id = ?2 AND key_hash = ?3"
            ),
            params![user_id, parent_task_id, key_hash],
            handoff_from_row,
        )
        .optional()?)
}

/// Handoff and resume turns the user queued since UTC midnight. They count
/// against `[users] child_daily_limit`.
fn child_tasks_today(connection: &rusqlite::Connection, user_id: &str) -> Result<i64> {
    let day_start = start_of_utc_day(chrono::Utc::now().date_naive())?;
    Ok(connection.query_row(
        "SELECT COUNT(*) FROM tasks
         WHERE user_id = ?1 AND created_at >= ?2
           AND json_extract(payload_json, '$.source') = 'handoff'",
        params![user_id, to_string(day_start)],
        |row| row.get(0),
    )?)
}

fn check_child_quota(connection: &rusqlite::Connection, user_id: &str, limit: u64) -> Result<()> {
    let used = child_tasks_today(connection, user_id)?;
    if used >= i64::try_from(limit).unwrap_or(i64::MAX) {
        return Err(SeraError::Validation(format!(
            "daily subagent quota exceeded: {limit} handoff tasks per user per UTC day"
        )));
    }
    Ok(())
}

impl Store {
    /// The acting user's handoff with this idempotency key under a parent
    /// task, if one was already queued.
    pub fn find_handoff_by_key(
        &self,
        user_id: &str,
        parent_task_id: &str,
        key_hash: &str,
    ) -> Result<Option<HandoffRow>> {
        let connection = self.connection()?;
        find_by_key(&connection, user_id, parent_task_id, key_hash)
    }

    /// Queue one child turn and write its ledger row in a single immediate
    /// transaction, after the daily child quota check. Returns the row and
    /// whether this call created it: when the key already exists nothing is
    /// written and the existing row comes back.
    pub fn create_subagent_handoff(
        &self,
        user_id: &str,
        new: &NewHandoff<'_>,
        payload: &Value,
        daily_limit: u64,
    ) -> Result<(HandoffRow, bool)> {
        let connection = self.connection()?;
        let transaction = Transaction::new_unchecked(&connection, TransactionBehavior::Immediate)?;
        if let Some(existing) =
            find_by_key(&transaction, user_id, new.parent_task_id, new.key_hash)?
        {
            return Ok((existing, false));
        }
        check_child_quota(&transaction, user_id, daily_limit)?;
        let now = chrono::Utc::now();
        insert_task_row(
            &transaction,
            new.child_task_id,
            user_id,
            "turn",
            new.to_bot,
            payload,
            0,
            now,
            now,
        )?;
        let stamp = to_string(now);
        let route_json = new.route.map(serde_json::to_string).transpose()?;
        transaction.execute(
            "INSERT INTO handoffs(id, user_id, parent_task_id, parent_turn_id, child_task_id,
                 to_bot, key_hash, status, outcome_code, effects_unknown, resume_count,
                 route_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'queued', NULL, 0, 0, ?8, ?9, ?9)",
            params![
                new.id,
                user_id,
                new.parent_task_id,
                new.parent_turn_id,
                new.child_task_id,
                new.to_bot,
                new.key_hash,
                route_json,
                stamp
            ],
        )?;
        let row = transaction.query_row(
            &format!("SELECT {HANDOFF_COLUMNS} FROM handoffs WHERE id = ?1 AND user_id = ?2"),
            params![new.id, user_id],
            handoff_from_row,
        )?;
        transaction.commit()?;
        Ok((row, true))
    }

    /// One of the acting user's handoffs. Another user's is a plain 404.
    pub fn get_handoff(&self, user_id: &str, id: &str) -> Result<HandoffRow> {
        let connection = self.connection()?;
        connection
            .query_row(
                &format!("SELECT {HANDOFF_COLUMNS} FROM handoffs WHERE id = ?1 AND user_id = ?2"),
                params![id, user_id],
                handoff_from_row,
            )
            .optional()?
            .ok_or_else(|| SeraError::NotFound(format!("handoff {id}")))
    }

    /// The acting user's handoffs, newest first, with each child's live state.
    /// With a conversation id, only that conversation's subagents.
    pub fn list_handoffs(
        &self,
        user_id: &str,
        conversation_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<HandoffView>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT h.id, h.parent_task_id, h.parent_turn_id, h.child_task_id, h.to_bot, h.key_hash,
                    h.status, h.outcome_code, h.effects_unknown, h.resume_count, h.route_json,
                    h.created_at, h.updated_at,
                    t.status, COALESCE(t.uncertain, 0),
                    json_extract(t.payload_json, '$.conversation_id')
             FROM handoffs h
             LEFT JOIN tasks t ON t.id = h.child_task_id AND t.user_id = h.user_id
             WHERE h.user_id = ?1
               AND (?2 IS NULL OR json_extract(t.payload_json, '$.conversation_id') = ?2)
             ORDER BY h.created_at DESC, h.id DESC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![user_id, conversation_id, limit.min(200) as i64],
            |row| {
                let handoff = handoff_from_row(row)?;
                let task_status: Option<String> = row.get(13)?;
                let uncertain: i64 = row.get(14)?;
                let conversation: Option<String> = row.get(15)?;
                Ok((handoff, task_status, uncertain != 0, conversation))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (handoff, task_status, uncertain, conversation_id) = row?;
            out.push(view_of(
                handoff,
                task_status.as_deref(),
                uncertain,
                conversation_id,
            ));
        }
        Ok(out)
    }

    /// One handoff with its child's live state.
    pub fn handoff_view(&self, user_id: &str, id: &str) -> Result<HandoffView> {
        let row = self.get_handoff(user_id, id)?;
        let task = self.get_task(user_id, &row.child_task_id).ok();
        let conversation = task
            .as_ref()
            .and_then(|task| task.payload.get("conversation_id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(view_of(
            row,
            task.as_ref().map(|task| task.status.as_str()),
            task.as_ref().is_some_and(|task| task.uncertain),
            conversation,
        ))
    }

    /// Queue a fresh child turn for an existing handoff and point the ledger
    /// at it, in one immediate transaction: the new task, `resume_count + 1`,
    /// status `resumed`. `expected_child` guards against two resumes racing;
    /// the loser changes nothing. Counts against the daily child quota.
    #[allow(clippy::too_many_arguments)]
    pub fn resume_handoff_task(
        &self,
        user_id: &str,
        handoff_id: &str,
        expected_child: &str,
        new_child_id: &str,
        bot: &str,
        payload: &Value,
        route: Option<&Value>,
        daily_limit: u64,
    ) -> Result<HandoffRow> {
        let connection = self.connection()?;
        let transaction = Transaction::new_unchecked(&connection, TransactionBehavior::Immediate)?;
        check_child_quota(&transaction, user_id, daily_limit)?;
        let now = chrono::Utc::now();
        let route_json = route.map(serde_json::to_string).transpose()?;
        let changed = transaction.execute(
            "UPDATE handoffs
             SET child_task_id = ?4, status = 'resumed', resume_count = resume_count + 1,
                 outcome_code = NULL, route_json = COALESCE(?5, route_json), updated_at = ?6
             WHERE id = ?1 AND user_id = ?2 AND child_task_id = ?3",
            params![
                handoff_id,
                user_id,
                expected_child,
                new_child_id,
                route_json,
                to_string(now)
            ],
        )?;
        if changed != 1 {
            return Err(SeraError::InvalidState(
                "this handoff was already resumed".into(),
            ));
        }
        insert_task_row(
            &transaction,
            new_child_id,
            user_id,
            "turn",
            bot,
            payload,
            0,
            now,
            now,
        )?;
        let row = transaction.query_row(
            &format!("SELECT {HANDOFF_COLUMNS} FROM handoffs WHERE id = ?1 AND user_id = ?2"),
            params![handoff_id, user_id],
            handoff_from_row,
        )?;
        transaction.commit()?;
        Ok(row)
    }

    /// Mark a handoff finished once its specialist has answered. Only the
    /// handoff's current child may write it, and not over a recorded failure,
    /// so a late completion of a replaced child changes nothing. Returns
    /// whether the row changed.
    pub fn mark_handoff_completed(
        &self,
        user_id: &str,
        handoff_id: &str,
        child_task_id: &str,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE handoffs SET status = 'completed', outcome_code = NULL, updated_at = ?4
             WHERE id = ?1 AND user_id = ?2 AND child_task_id = ?3
               AND status NOT IN ('failed', 'failed_resumable')",
            params![handoff_id, user_id, child_task_id, now_string()],
        )?;
        Ok(changed == 1)
    }

    /// Record that a handoff's child failed for good: `failed_resumable` when
    /// a retry is still allowed, else `failed`. Only the handoff's current
    /// child may write it, so a late failure of a replaced child changes
    /// nothing. Returns whether the row changed.
    pub fn mark_handoff_failed(
        &self,
        user_id: &str,
        handoff_id: &str,
        child_task_id: &str,
        status: &str,
        outcome_code: &str,
        effects_unknown: bool,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE handoffs
             SET status = ?4, outcome_code = ?5, effects_unknown = ?6, updated_at = ?7
             WHERE id = ?1 AND user_id = ?2 AND child_task_id = ?3
               AND status NOT IN ('completed', 'failed', 'failed_resumable')",
            params![
                handoff_id,
                user_id,
                child_task_id,
                status,
                outcome_code,
                i64::from(effects_unknown),
                now_string()
            ],
        )?;
        Ok(changed == 1)
    }

    /// Whether the acting user's bot has a turn running or waiting on an
    /// approval, other than `except_task`. A route change waits for this to
    /// be false.
    pub fn bot_is_busy(&self, user_id: &str, bot: &str, except_task: Option<&str>) -> Result<bool> {
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM tasks
             WHERE user_id = ?1 AND bot = ?2 AND status IN ('running','awaiting_approval','waiting_model')
               AND (?3 IS NULL OR id <> ?3)",
            params![user_id, bot, except_task],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }
}

fn view_of(
    row: HandoffRow,
    task_status: Option<&str>,
    uncertain: bool,
    conversation_id: Option<String>,
) -> HandoffView {
    let status = effective_status(&row, task_status, uncertain);
    HandoffView {
        effects_unknown: row.effects_unknown || uncertain,
        status,
        id: row.id,
        parent_task_id: row.parent_task_id,
        parent_turn_id: row.parent_turn_id,
        child_task_id: row.child_task_id,
        to_bot: row.to_bot,
        outcome_code: row.outcome_code,
        resume_count: row.resume_count,
        route: row.route,
        conversation_id,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

/// Pin the route a resumed child starts on: the explicit override when given
/// (checked against the live catalog and the provider's egress approval), else
/// the bot's own current head, else the route the child ran on. The bot's
/// failover chain rides along, like a first handoff.
pub fn resume_route(
    router: &ModelRouter,
    user_id: &str,
    bot: &BotConfig,
    override_route: Option<&BotRoute>,
    previous: Option<&Value>,
) -> Result<PinnedRoute> {
    let selection = match override_route {
        Some(route) => {
            router.validate_route_entry(user_id, route)?;
            ModelSelection {
                provider: route.provider.clone(),
                model: route.model.clone(),
                account: route.account.clone(),
            }
        }
        None => match router.bot_chain(user_id, bot).head {
            Some(head) => head,
            None => previous
                .and_then(|value| serde_json::from_value::<PinnedRoute>(value.clone()).ok())
                .map(|route| route.selection)
                .ok_or_else(|| SeraError::InvalidState("no route to resume on".into()))?,
        },
    };
    router.pin_for_bot(user_id, &selection, bot)
}

/// The chain slot after the one a failed child ran on: 0 is the primary route,
/// then the fallbacks. `None` when the child was on the last slot, or its slot
/// is not in the chain any more.
pub fn next_chain_slot(bot: &BotConfig, previous: Option<&Value>) -> Option<usize> {
    let ran = serde_json::from_value::<PinnedRoute>(previous?.clone())
        .ok()?
        .selection;
    let chain = bot.route.iter().chain(&bot.fallbacks).collect::<Vec<_>>();
    let slot = chain.iter().position(|entry| {
        entry.provider == ran.provider && entry.model == ran.model && entry.account == ran.account
    })?;
    (slot + 1 < chain.len()).then_some(slot + 1)
}

/// How a handoff is resumed.
pub struct ResumeOptions<'a> {
    pub actor: &'a str,
    /// The tool that asked, for the audit row; `None` for the owner's API call.
    pub tool: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub route: Option<&'a BotRoute>,
    /// Refuse past this many resumes (the bot path); the owner is not capped.
    pub max_resumes: Option<i64>,
    /// The owner confirmed the earlier attempt's effects are acceptable to
    /// repeat. Never set for a bot.
    pub accept_unknown_effects: bool,
    pub daily_limit: u64,
}

/// Queue a failed handoff's specialist turn again, for the acting user. The
/// original turn payload is reused with a freshly pinned route, so the
/// resumed child answers the same objective in the same conversation. A child
/// that may have had an external effect with an unknown outcome is never
/// replayed unless the owner confirms it.
pub fn resume_handoff(
    store: &Store,
    router: &ModelRouter,
    bot: &BotConfig,
    user_id: &str,
    handoff_id: &str,
    options: &ResumeOptions<'_>,
) -> Result<HandoffView> {
    let view = store.handoff_view(user_id, handoff_id)?;
    if view.status != "failed_resumable" && view.status != "failed" {
        return Err(SeraError::InvalidState(format!(
            "handoff {handoff_id} is {} and cannot be resumed",
            view.status
        )));
    }
    if view.effects_unknown && !options.accept_unknown_effects {
        return Err(SeraError::Rejected {
            code: "effects_unknown",
            message: "the earlier attempt may have had an effect with an unknown outcome; verify it first".into(),
        });
    }
    if let Some(max) = options.max_resumes
        && view.resume_count >= max
    {
        return Err(SeraError::Rejected {
            code: "resume_limit",
            message: format!("this handoff was already resumed {max} times"),
        });
    }
    let child = store.get_task(user_id, &view.child_task_id)?;
    let route = resume_route(router, user_id, bot, options.route, view.route.as_ref())?;
    let mut payload = child.payload.clone();
    payload["route"] = json!(route);
    payload["resume"] = json!(view.resume_count + 1);
    // A retry asked for by a follow-up turn carries that turn's depth, so
    // the follow-up depth bound holds across retries.
    if let Some(depth) = options
        .task_id
        .and_then(|id| store.get_task(user_id, id).ok())
        .and_then(|task| task.payload.get("followup_depth").and_then(Value::as_u64))
    {
        payload["followup_depth"] = json!(depth);
    }
    let new_child = new_id();
    store.resume_handoff_task(
        user_id,
        &view.id,
        &view.child_task_id,
        &new_child,
        &view.to_bot,
        &payload,
        Some(&json!(route)),
        options.daily_limit,
    )?;
    // Audited only once the transaction has committed.
    store.record_audit(
        options.actor,
        "handoff.resumed",
        options.tool,
        options.task_id,
        None,
        None,
        None,
        Some(&json!({
            "handoff_id": view.id,
            "old_child_task_id": view.child_task_id,
            "child_task_id": new_child,
            "to_bot": view.to_bot,
            "resume_count": view.resume_count + 1,
            "provider": route.selection.provider,
            "model": route.selection.model,
        })),
    )?;
    // Every resume is a route event, so the switch shows beside the failovers.
    let previous = view
        .route
        .as_ref()
        .and_then(|value| serde_json::from_value::<PinnedRoute>(value.clone()).ok())
        .map(|route| route.selection)
        .unwrap_or_else(|| route.selection.clone());
    if let Err(error) = store.record_route_event(
        user_id,
        &NewRouteEvent {
            conversation_id: view.conversation_id.as_deref(),
            task_id: Some(&new_child),
            bot: Some(&view.to_bot),
            from: &previous,
            to: &route.selection,
            reason: "handoff_resume",
        },
    ) {
        warn!(%error, "handoff resume was not recorded as a route event");
    }
    store.append_event(
        user_id,
        "handoff.resumed",
        &json!({
            "handoff_id": view.id,
            "child_task_id": new_child,
            "to_bot": view.to_bot,
        }),
    )?;
    store.handoff_view(user_id, handoff_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BotConfig, Config, LoadedBot};
    use crate::llm::LlmClient;
    use rusqlite::params;
    use std::collections::{BTreeMap, HashSet};

    const ROOM: &str = "room:review";

    /// The bootstrap admin's id â€” the acting user for the delegate fixtures.
    fn admin(store: &Store) -> String {
        store.admin_user_id().unwrap()
    }

    /// A room conversation with the orchestrator task that asked for a handoff.
    fn store() -> (Store, String) {
        let store = Store::open_in_memory(384).unwrap();
        let admin = admin(&store);
        store
            .create_conversation(&admin, ROOM, "room", None)
            .unwrap();
        let parent = store
            .enqueue_task(
                &admin,
                "turn",
                ORCHESTRATOR_BOT,
                &json!({"conversation_id": ROOM, "text": "synthetic", "bot": ORCHESTRATOR_BOT}),
                0,
                chrono::Utc::now(),
            )
            .unwrap();
        (store, parent.id)
    }

    fn bot(name: &str, role: BotRole, delegates: Vec<String>) -> LoadedBot {
        LoadedBot {
            config: BotConfig {
                title: None,
                description: None,
                name: name.into(),
                trigger: "command".into(),
                model: None,
                system_prompt: format!("prompts/{name}.txt"),
                skills: vec![],
                tools: vec![],
                require_approval: vec![],
                enabled: true,
                temperature: None,
                thinking: None,
                role,
                delegates,
                route: None,
                fallbacks: Vec::new(),
                failover: Default::default(),
                catalog_only: false,
            },
            prompt: name.into(),
        }
    }

    fn bots() -> Vec<LoadedBot> {
        vec![
            bot(
                ORCHESTRATOR_BOT,
                BotRole::Orchestrator,
                vec!["analyst".to_string()],
            ),
            bot("analyst", BotRole::Specialist, vec![]),
        ]
    }

    fn fleet(store: &Store) -> Arc<FleetRegistry> {
        let config = Arc::new(Config::default());
        let catalog = Arc::new(crate::skills::SkillCatalog::load(&config).unwrap());
        Arc::new(FleetRegistry::new(config, store.clone(), bots(), catalog).unwrap())
    }

    fn router() -> ModelRouter {
        let mut config = Config::default();
        config.llm.data_egress_approved = true;
        config.llm.model = "test".into();
        config.llm.base_url = "https://example.com/v1".into();
        config.llm.models = vec!["test-alternative".into()];
        ModelRouter::new(
            &config.llm,
            BTreeMap::from([(
                "default".into(),
                LlmClient::new("https://example.com/v1", "test", 5, 50)
                    .unwrap()
                    .with_model("test"),
            )]),
        )
        .unwrap()
    }

    fn context(store: &Store, task_id: &str, turn_id: &str) -> ToolContext {
        ToolContext {
            user_id: admin(store),
            actor: admin(store),
            bot: ORCHESTRATOR_BOT.into(),
            task_id: Some(task_id.into()),
            turn_id: Some(turn_id.into()),
            conversation_id: Some(ROOM.into()),
            allowed_tools: HashSet::from([DELEGATE_TOOL.to_string()]),
            require_approval: HashSet::new(),
        }
    }

    fn args() -> Value {
        json!({
            "to_bot": "analyst",
            "objective": "Summarize the synthetic cash position.",
            "model_class": "routine",
        })
    }

    /// Specialist turns that are queued or running.
    fn queued_specialist_tasks(store: &Store) -> i64 {
        let connection = store.connection().unwrap();
        connection
            .query_row(
                "SELECT COUNT(*) FROM tasks
                 WHERE bot='analyst' AND status IN ('queued','running','awaiting_approval')",
                params![],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[tokio::test]
    async fn delegate_audits_the_child_task_it_queues() {
        let (store, parent) = store();
        let tool = DelegateTool::new(store.clone(), fleet(&store), router());
        let turn = store
            .create_turn(&admin(&store), &parent, ORCHESTRATOR_BOT, ROOM, &json!({}))
            .unwrap();
        let context = context(&store, &parent, &turn.id);
        let output = tool.execute(args(), &context).await.unwrap();
        let child = output
            .data
            .as_ref()
            .and_then(|data| data.get("child_task_id"))
            .and_then(Value::as_str)
            .expect("child task id")
            .to_owned();
        // The audit row names the specialist turn that was queued.
        let entry = store
            .list_audit(20)
            .unwrap()
            .into_iter()
            .find(|entry| entry.event == "handoff.queued")
            .expect("handoff.queued audit row");
        let details = entry.details.expect("audit details");
        assert_eq!(
            details.get("child_task_id").and_then(Value::as_str),
            Some(child.as_str())
        );
        assert_eq!(
            details.get("to_bot").and_then(Value::as_str),
            Some("analyst")
        );
        assert_eq!(
            details.get("from_bot").and_then(Value::as_str),
            Some(ORCHESTRATOR_BOT)
        );
        assert_eq!(
            details.get("conversation_id").and_then(Value::as_str),
            Some(ROOM)
        );
        assert_eq!(
            details.get("model_class").and_then(Value::as_str),
            Some("routine")
        );
        // The queued task is the task the audit names.
        let task = store.get_task(&admin(&store), &child).unwrap();
        assert_eq!(task.bot, "analyst");
        assert_eq!(task.kind, "turn");
        assert_eq!(task.status, "queued");
        assert_eq!(queued_specialist_tasks(&store), 1);
    }

    #[tokio::test]
    async fn delegate_queues_no_child_task_when_the_audit_fails() {
        let (store, parent) = store();
        let tool = DelegateTool::new(store.clone(), fleet(&store), router());
        let turn = store
            .create_turn(&admin(&store), &parent, ORCHESTRATOR_BOT, ROOM, &json!({}))
            .unwrap();
        let context = context(&store, &parent, &turn.id);
        // The event stream write fails, so the handoff is not audited.
        {
            let connection = store.connection().unwrap();
            connection.execute_batch("DROP TABLE events").unwrap();
        }
        let error = tool.execute(args(), &context).await.unwrap_err();
        assert!(
            error.to_string().contains("no such table: events"),
            "{error}"
        );
        // No specialist turn was queued: the audit came first and failed.
        assert_eq!(queued_specialist_tasks(&store), 0);
        assert_eq!(store.pending_task_count(&admin(&store)).unwrap(), 1);
    }

    fn child_of(output: &ToolOutput) -> String {
        output
            .data
            .as_ref()
            .and_then(|data| data.get("child_task_id"))
            .and_then(Value::as_str)
            .expect("child task id")
            .to_owned()
    }

    fn handoff_of(output: &ToolOutput) -> String {
        output
            .data
            .as_ref()
            .and_then(|data| data.get("handoff_id"))
            .and_then(Value::as_str)
            .expect("handoff id")
            .to_owned()
    }

    fn set_task_status(store: &Store, task: &str, status: &str) {
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE tasks SET status = ?2 WHERE id = ?1",
                params![task, status],
            )
            .unwrap();
    }

    fn resume_options(limit: u64, max: Option<i64>, actor: &str) -> ResumeOptions<'_> {
        ResumeOptions {
            actor,
            tool: None,
            task_id: None,
            route: None,
            max_resumes: max,
            accept_unknown_effects: false,
            daily_limit: limit,
        }
    }

    #[tokio::test]
    async fn a_repeated_delegate_returns_the_existing_child() {
        let (store, parent) = store();
        let tool = DelegateTool::new(store.clone(), fleet(&store), router());
        let turn = store
            .create_turn(&admin(&store), &parent, ORCHESTRATOR_BOT, ROOM, &json!({}))
            .unwrap();
        let context = context(&store, &parent, &turn.id);
        let first = tool.execute(args(), &context).await.unwrap();
        let second = tool.execute(args(), &context).await.unwrap();
        assert_eq!(child_of(&first), child_of(&second));
        assert_eq!(handoff_of(&first), handoff_of(&second));
        assert_eq!(
            second.data.as_ref().and_then(|d| d.get("duplicate")),
            Some(&json!(true))
        );
        // One child task, one ledger row, one audit row.
        assert_eq!(queued_specialist_tasks(&store), 1);
        let connection = store.connection().unwrap();
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM handoffs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        drop(connection);
        let audited = store
            .list_audit(50)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.event == "handoff.queued")
            .count();
        assert_eq!(audited, 1);
        // A different objective is a different handoff.
        let mut other = args();
        other["objective"] = json!("Summarize the synthetic payables.");
        let third = tool.execute(other, &context).await.unwrap();
        assert_ne!(child_of(&first), child_of(&third));
        assert_eq!(queued_specialist_tasks(&store), 2);
    }

    #[tokio::test]
    async fn the_ledger_row_matches_the_child_and_stays_with_its_owner() {
        let (store, parent) = store();
        let tool = DelegateTool::new(store.clone(), fleet(&store), router());
        let turn = store
            .create_turn(&admin(&store), &parent, ORCHESTRATOR_BOT, ROOM, &json!({}))
            .unwrap();
        let context = context(&store, &parent, &turn.id);
        let output = tool.execute(args(), &context).await.unwrap();
        let admin = admin(&store);
        let row = store.get_handoff(&admin, &handoff_of(&output)).unwrap();
        assert_eq!(row.child_task_id, child_of(&output));
        assert_eq!(row.parent_task_id, parent);
        assert_eq!(row.parent_turn_id.as_deref(), Some(turn.id.as_str()));
        assert_eq!(row.to_bot, "analyst");
        assert_eq!(row.status, "queued");
        assert_eq!(
            row.key_hash,
            handoff_key_hash(
                &turn.id,
                "analyst",
                "Summarize the synthetic cash position."
            )
        );
        let listed = store.list_handoffs(&admin, Some(ROOM), 10).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, "queued");
        assert_eq!(listed[0].conversation_id.as_deref(), Some(ROOM));
        assert!(
            store
                .list_handoffs(&admin, Some("room:other"), 10)
                .unwrap()
                .is_empty()
        );
        // Another user sees nothing and cannot read the row.
        let member = store
            .create_user("microsoft:ledger", "ledger@example.example", "L")
            .unwrap();
        assert!(
            store
                .list_handoffs(&member.id, None, 10)
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            store.get_handoff(&member.id, &row.id),
            Err(SeraError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn the_child_daily_limit_counts_handoffs_and_resumes() {
        let (store, parent) = store();
        let fleet = fleet(&store);
        let router = router();
        let tool = DelegateTool::new(store.clone(), fleet.clone(), router.clone())
            .with_child_daily_limit(2);
        let turn = store
            .create_turn(&admin(&store), &parent, ORCHESTRATOR_BOT, ROOM, &json!({}))
            .unwrap();
        let context = context(&store, &parent, &turn.id);
        let first = tool.execute(args(), &context).await.unwrap();
        // The handoff bypasses the general task quota but not the child cap.
        let mut other = args();
        other["objective"] = json!("Second synthetic objective.");
        tool.execute(other, &context).await.unwrap();
        let mut third = args();
        third["objective"] = json!("Third synthetic objective.");
        let refused = tool.execute(third, &context).await.unwrap_err();
        assert!(
            refused.to_string().contains("daily subagent quota"),
            "{refused}"
        );
        assert_eq!(queued_specialist_tasks(&store), 2);

        // A resume is a child task too, so it is refused at the cap.
        set_task_status(&store, &child_of(&first), "failed");
        let admin = admin(&store);
        let bot = fleet.load(&admin).unwrap()["analyst"].config.clone();
        let error = resume_handoff(
            &store,
            &router,
            &bot,
            &admin,
            &handoff_of(&first),
            &resume_options(2, None, &admin),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("daily subagent quota"),
            "{error}"
        );
        // With room under the cap it queues a new child and counts.
        let view = resume_handoff(
            &store,
            &router,
            &bot,
            &admin,
            &handoff_of(&first),
            &resume_options(3, None, &admin),
        )
        .unwrap();
        assert_eq!(view.resume_count, 1);
        assert_eq!(view.status, "resumed");
        assert_ne!(view.child_task_id, child_of(&first));
        assert_eq!(
            child_tasks_today(&store.connection().unwrap(), &admin).unwrap(),
            3
        );
    }

    #[tokio::test]
    async fn resume_is_capped_and_never_replays_unknown_effects() {
        let (store, parent) = store();
        let fleet = fleet(&store);
        let router = router();
        let tool = DelegateTool::new(store.clone(), fleet.clone(), router.clone());
        let turn = store
            .create_turn(&admin(&store), &parent, ORCHESTRATOR_BOT, ROOM, &json!({}))
            .unwrap();
        let context = context(&store, &parent, &turn.id);
        let output = tool.execute(args(), &context).await.unwrap();
        let admin = admin(&store);
        let id = handoff_of(&output);
        let bot = fleet.load(&admin).unwrap()["analyst"].config.clone();
        // A running child is not resumable.
        let error = resume_handoff(
            &store,
            &router,
            &bot,
            &admin,
            &id,
            &resume_options(500, Some(MAX_RESUMES), &admin),
        )
        .unwrap_err();
        assert!(matches!(error, SeraError::InvalidState(_)), "{error}");

        // The cap holds: two resumes, then a plain failure.
        let mut child = child_of(&output);
        for expected in 1..=MAX_RESUMES {
            set_task_status(&store, &child, "failed");
            let view = resume_handoff(
                &store,
                &router,
                &bot,
                &admin,
                &id,
                &resume_options(500, Some(MAX_RESUMES), &admin),
            )
            .unwrap();
            assert_eq!(view.resume_count, expected);
            child = view.child_task_id;
        }
        set_task_status(&store, &child, "failed");
        let view = store.handoff_view(&admin, &id).unwrap();
        assert_eq!(view.status, "failed");
        let error = resume_handoff(
            &store,
            &router,
            &bot,
            &admin,
            &id,
            &resume_options(500, Some(MAX_RESUMES), &admin),
        )
        .unwrap_err();
        assert!(error.to_string().contains("resumed 2 times"), "{error}");
        // The resumed child's payload is the original objective on a fresh pin.
        let resumed = store.get_task(&admin, &child).unwrap();
        assert_eq!(
            resumed.payload["text"],
            "Summarize the synthetic cash position."
        );
        assert_eq!(resumed.payload["source"], "handoff");
        assert_eq!(resumed.payload["resume"], 2);

        // An unknown effect is never replayed without the owner's word.
        let mut second = args();
        second["objective"] = json!("Another synthetic objective.");
        let other = tool.execute(second, &context).await.unwrap();
        let other_child = child_of(&other);
        set_task_status(&store, &other_child, "failed");
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE tasks SET uncertain = 1 WHERE id = ?1",
                params![other_child],
            )
            .unwrap();
        let other_id = handoff_of(&other);
        assert_eq!(
            store.handoff_view(&admin, &other_id).unwrap().status,
            "failed"
        );
        let error = resume_handoff(
            &store,
            &router,
            &bot,
            &admin,
            &other_id,
            &resume_options(500, Some(MAX_RESUMES), &admin),
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                SeraError::Rejected {
                    code: "effects_unknown",
                    ..
                }
            ),
            "{error}"
        );
        let mut confirmed = resume_options(500, None, &admin);
        confirmed.accept_unknown_effects = true;
        resume_handoff(&store, &router, &bot, &admin, &other_id, &confirmed).unwrap();
    }

    #[tokio::test]
    async fn a_refused_resume_leaves_no_resumed_audit_row() {
        let (store, parent) = store();
        let fleet = fleet(&store);
        let router = router();
        let tool = DelegateTool::new(store.clone(), fleet.clone(), router.clone());
        let turn = store
            .create_turn(&admin(&store), &parent, ORCHESTRATOR_BOT, ROOM, &json!({}))
            .unwrap();
        let context = context(&store, &parent, &turn.id);
        let output = tool.execute(args(), &context).await.unwrap();
        let admin = admin(&store);
        let bot = fleet.load(&admin).unwrap()["analyst"].config.clone();
        set_task_status(&store, &child_of(&output), "failed");
        // The daily quota (1 child already) refuses the resume transaction.
        resume_handoff(
            &store,
            &router,
            &bot,
            &admin,
            &handoff_of(&output),
            &resume_options(1, None, &admin),
        )
        .unwrap_err();
        let resumed = |store: &Store| {
            store
                .list_audit_for_user(&admin, 100)
                .unwrap()
                .iter()
                .filter(|entry| entry.event == "handoff.resumed")
                .count()
        };
        assert_eq!(resumed(&store), 0);
        resume_handoff(
            &store,
            &router,
            &bot,
            &admin,
            &handoff_of(&output),
            &resume_options(500, None, &admin),
        )
        .unwrap();
        assert_eq!(resumed(&store), 1);
    }

    #[tokio::test]
    async fn only_the_current_child_completes_a_handoff() {
        let (store, parent) = store();
        let fleet = fleet(&store);
        let router = router();
        let tool = DelegateTool::new(store.clone(), fleet.clone(), router.clone());
        let turn = store
            .create_turn(&admin(&store), &parent, ORCHESTRATOR_BOT, ROOM, &json!({}))
            .unwrap();
        let context = context(&store, &parent, &turn.id);
        let output = tool.execute(args(), &context).await.unwrap();
        let admin = admin(&store);
        let id = handoff_of(&output);
        let old_child = child_of(&output);
        let bot = fleet.load(&admin).unwrap()["analyst"].config.clone();
        set_task_status(&store, &old_child, "failed");
        let view = resume_handoff(
            &store,
            &router,
            &bot,
            &admin,
            &id,
            &resume_options(500, None, &admin),
        )
        .unwrap();
        // A late completion from the replaced child changes nothing.
        assert!(
            !store
                .mark_handoff_completed(&admin, &id, &old_child)
                .unwrap()
        );
        assert_eq!(store.get_handoff(&admin, &id).unwrap().status, "resumed");
        // A failure already recorded for the current child is not overwritten.
        assert!(
            store
                .mark_handoff_failed(&admin, &id, &view.child_task_id, "failed", "error", false)
                .unwrap()
        );
        assert!(
            !store
                .mark_handoff_completed(&admin, &id, &view.child_task_id)
                .unwrap()
        );
        assert_eq!(store.get_handoff(&admin, &id).unwrap().status, "failed");
    }

    #[tokio::test]
    async fn a_retry_carries_the_follow_up_depth_of_the_asking_turn() {
        let (store, parent) = store();
        let fleet = fleet(&store);
        let router = router();
        let tool = DelegateTool::new(store.clone(), fleet.clone(), router.clone());
        let turn = store
            .create_turn(&admin(&store), &parent, ORCHESTRATOR_BOT, ROOM, &json!({}))
            .unwrap();
        let context = context(&store, &parent, &turn.id);
        let output = tool.execute(args(), &context).await.unwrap();
        let admin = admin(&store);
        let bot = fleet.load(&admin).unwrap()["analyst"].config.clone();
        set_task_status(&store, &child_of(&output), "failed");
        let asking = store
            .enqueue_task(
                &admin,
                "turn",
                ORCHESTRATOR_BOT,
                &json!({"followup_depth": 2}),
                0,
                chrono::Utc::now(),
            )
            .unwrap();
        let mut options = resume_options(500, None, &admin);
        options.task_id = Some(&asking.id);
        let view = resume_handoff(
            &store,
            &router,
            &bot,
            &admin,
            &handoff_of(&output),
            &options,
        )
        .unwrap();
        let child = store.get_task(&admin, &view.child_task_id).unwrap();
        assert_eq!(child.payload["followup_depth"], 2);
    }
}
