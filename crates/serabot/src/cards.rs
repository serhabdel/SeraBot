//! Interactive chat cards (0.23).
//!
//! A card is either a question a bot puts to its owner, or a request for a
//! secret that the vault resolves. Cards are rows in `chat_cards`, scoped by
//! the acting user's id: a card id alone never selects another user's card.
//! Creating a card appends `card.created` to the owner's event stream and
//! resolving it appends `card.resolved`; the desktop re-reads the transcript,
//! which carries the cards.
//!
//! `ask_user` asks the owner instead of guessing. In autonomy mode (the
//! conversation's auto-approve switch is on, or it has a goal plan) the tool
//! refuses to ask and the bot decides. At most one question is pending per
//! conversation; a second call returns the pending one. Card text is
//! model-written and untrusted: it is bounded, stripped of control characters,
//! and checked against the content policy before it is stored.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::domain::new_id;
use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolRegistry, ToolSpec};
use crate::orchestration::validate_conversation_id;
use crate::plans::clean_text;
use crate::policy::Policy;
use crate::store::{Store, now_string, to_string};

/// The bot tool that puts a question to the owner.
pub const ASK_USER_TOOL: &str = "ask_user";
/// Event kinds streamed to the desktop when a card is created or resolved.
pub const CARD_CREATED_EVENT: &str = "card.created";
pub const CARD_RESOLVED_EVENT: &str = "card.resolved";

pub const MAX_QUESTION_CHARS: usize = 500;
pub const MAX_OPTIONS: usize = 6;
pub const MAX_OPTION_CHARS: usize = 80;
pub const MAX_CONTEXT_CHARS: usize = 500;
pub const MAX_ANSWER_CHARS: usize = 1_000;
/// A pending question older than this is expired the next time one is asked.
pub const QUESTION_TTL_HOURS: i64 = 24;
/// Most cards one conversation listing returns (the newest ones).
pub const MAX_LISTED_CARDS: usize = 100;
const MAX_PAYLOAD_BYTES: usize = 8 * 1024;
const MAX_RESPONSE_BYTES: usize = 4 * 1024;

const AUTONOMY_NOTE: &str =
    "Autonomy is on for this chat; decide yourself and state the assumption you made.";
const NO_CARD_VIEW_NOTE: &str =
    "Questions cannot be shown in this chat; decide yourself and state the assumption you made.";
const ASKED_NOTE: &str = "Question shown to the user. End your turn now and wait for their answer.";
const PENDING_NOTE: &str =
    "A question is already waiting for the user. End your turn now and wait for their answer.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    Question,
    SecretRequest,
}

impl CardKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::SecretRequest => "secret_request",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "question" => Some(Self::Question),
            "secret_request" => Some(Self::SecretRequest),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardStatus {
    Pending,
    Answered,
    Declined,
    Expired,
}

impl CardStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Answered => "answered",
            Self::Declined => "declined",
            Self::Expired => "expired",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "answered" => Some(Self::Answered),
            "declined" => Some(Self::Declined),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

/// One card as the API and the desktop see it. `response` never holds a
/// secret value: a secret request records only that it was saved or declined.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    pub conversation_id: String,
    pub bot: String,
    pub kind: CardKind,
    pub payload: Value,
    pub status: CardStatus,
    pub response: Option<Value>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

/// A row as read from SQLite, before the enum columns are checked.
struct RawCard {
    id: String,
    conversation_id: String,
    bot: String,
    kind: String,
    payload: String,
    status: String,
    response: Option<String>,
    created_at: String,
    resolved_at: Option<String>,
}

fn raw_card(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawCard> {
    Ok(RawCard {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        bot: row.get(2)?,
        kind: row.get(3)?,
        payload: row.get(4)?,
        status: row.get(5)?,
        response: row.get(6)?,
        created_at: row.get(7)?,
        resolved_at: row.get(8)?,
    })
}

fn into_card(raw: RawCard) -> Result<Card> {
    let kind = CardKind::parse(&raw.kind)
        .ok_or_else(|| SeraError::Internal("stored card has an unknown kind".into()))?;
    let status = CardStatus::parse(&raw.status)
        .ok_or_else(|| SeraError::Internal("stored card has an unknown status".into()))?;
    Ok(Card {
        id: raw.id,
        conversation_id: raw.conversation_id,
        bot: raw.bot,
        kind,
        // A corrupt payload reads as empty content, never as an error.
        payload: serde_json::from_str(&raw.payload).unwrap_or(Value::Null),
        status,
        response: raw
            .response
            .and_then(|text| serde_json::from_str(&text).ok()),
        created_at: raw.created_at,
        resolved_at: raw.resolved_at,
    })
}

const CARD_COLUMNS: &str =
    "id, conversation_id, bot, kind, payload, status, response, created_at, resolved_at";

fn invalid(message: &str) -> SeraError {
    SeraError::Validation(message.into())
}

/// Check a card payload before it is stored. The store is the only writer,
/// so these bounds hold whichever tool or route produced the card.
pub fn validate_card_payload(kind: CardKind, payload: &Value) -> Result<()> {
    let object = payload
        .as_object()
        .ok_or_else(|| invalid("card payload must be an object"))?;
    if serde_json::to_string(payload)?.len() > MAX_PAYLOAD_BYTES {
        return Err(invalid("card payload is too large"));
    }
    match kind {
        CardKind::Question => {
            let question = object.get("question").and_then(Value::as_str);
            if !question.is_some_and(|text| text_within(text, MAX_QUESTION_CHARS)) {
                return Err(invalid("question must be 1 to 500 characters"));
            }
            let options = object
                .get("options")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("options must be a list"))?;
            if options.len() > MAX_OPTIONS {
                return Err(invalid("a question holds at most 6 options"));
            }
            for option in options {
                if !option
                    .as_str()
                    .is_some_and(|text| text_within(text, MAX_OPTION_CHARS))
                {
                    return Err(invalid("each option must be 1 to 80 characters"));
                }
            }
            let allow_free_text = object
                .get("allow_free_text")
                .and_then(Value::as_bool)
                .ok_or_else(|| invalid("allow_free_text must be true or false"))?;
            if options.is_empty() && !allow_free_text {
                return Err(invalid("a question needs options or free text"));
            }
            if let Some(context) = object.get("context")
                && !context
                    .as_str()
                    .is_some_and(|text| text.chars().count() <= MAX_CONTEXT_CHARS)
            {
                return Err(invalid("context must be at most 500 characters"));
            }
        }
        CardKind::SecretRequest => {
            if !matches!(
                object.get("purpose").and_then(Value::as_str),
                Some("save" | "provide")
            ) {
                return Err(invalid("purpose must be save or provide"));
            }
            if !matches!(
                object.get("kind").and_then(Value::as_str),
                Some("password" | "api_key" | "note")
            ) {
                return Err(invalid("kind must be password, api_key or note"));
            }
            let name = object.get("name").and_then(Value::as_str);
            if !name.is_some_and(|text| text_within(text, 128)) {
                return Err(invalid("name must be 1 to 128 characters"));
            }
            let reason = object.get("reason").and_then(Value::as_str);
            if !reason.is_some_and(|text| text.chars().count() <= MAX_CONTEXT_CHARS) {
                return Err(invalid("reason must be at most 500 characters"));
            }
            let domains = object
                .get("domains")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("domains must be a list"))?;
            if domains.len() > 20
                || !domains
                    .iter()
                    .all(|domain| domain.as_str().is_some_and(|text| text_within(text, 253)))
            {
                return Err(invalid("domains must be at most 20 short names"));
            }
        }
    }
    Ok(())
}

fn text_within(text: &str, max_chars: usize) -> bool {
    !text.trim().is_empty() && text.chars().count() <= max_chars
}

/// Validate and clean a model-supplied question into its card payload.
pub fn parse_question_args(args: &Value) -> Result<Value> {
    let question = clean_text(
        args.get("question")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        MAX_QUESTION_CHARS,
    );
    if question.is_empty() {
        return Err(invalid("question is required"));
    }
    let mut options: Vec<String> = Vec::new();
    match args.get("options") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            if items.len() > MAX_OPTIONS {
                return Err(invalid("a question holds at most 6 options"));
            }
            for item in items {
                let text = clean_text(
                    item.as_str()
                        .ok_or_else(|| invalid("options must be text"))?,
                    MAX_OPTION_CHARS,
                );
                if text.is_empty() {
                    return Err(invalid("options cannot be empty"));
                }
                if !options.contains(&text) {
                    options.push(text);
                }
            }
        }
        Some(_) => return Err(invalid("options must be a list")),
    }
    let allow_free_text = match args.get("allow_free_text") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(invalid("allow_free_text must be true or false")),
    };
    if options.is_empty() && !allow_free_text {
        return Err(invalid("give options or allow free text"));
    }
    let context = match args.get("context") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => {
            Some(clean_text(text, MAX_CONTEXT_CHARS)).filter(|text| !text.is_empty())
        }
        Some(_) => return Err(invalid("context must be text")),
    };
    let mut payload = json!({
        "question": question,
        "options": options,
        "allow_free_text": allow_free_text,
    });
    if let Some(context) = context {
        payload["context"] = json!(context);
    }
    Ok(payload)
}

fn read_card(connection: &Connection, user_id: &str, id: &str) -> Result<Option<Card>> {
    let raw = connection
        .query_row(
            &format!("SELECT {CARD_COLUMNS} FROM chat_cards WHERE id = ?1 AND user_id = ?2"),
            params![id, user_id],
            raw_card,
        )
        .optional()?;
    raw.map(into_card).transpose()
}

impl Store {
    /// Create a pending card in the acting user's conversation and append
    /// `card.created`. The payload is validated for its kind first.
    pub fn create_card(
        &self,
        user_id: &str,
        conversation_id: &str,
        bot: &str,
        kind: CardKind,
        payload: &Value,
    ) -> Result<Card> {
        validate_conversation_id(conversation_id)?;
        if bot.is_empty() || bot.chars().count() > 128 {
            return Err(invalid("card bot is required"));
        }
        validate_card_payload(kind, payload)?;
        let card = Card {
            id: new_id(),
            conversation_id: conversation_id.to_owned(),
            bot: bot.to_owned(),
            kind,
            payload: payload.clone(),
            status: CardStatus::Pending,
            response: None,
            created_at: now_string(),
            resolved_at: None,
        };
        let event = serde_json::to_string(&json!({
            "card_id": card.id,
            "conversation_id": conversation_id,
            "kind": kind.as_str(),
        }))?;
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO chat_cards
                 (id, user_id, conversation_id, bot, kind, payload, status, response, created_at, resolved_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', NULL, ?7, NULL)",
            params![
                card.id,
                user_id,
                conversation_id,
                bot,
                kind.as_str(),
                serde_json::to_string(payload)?,
                card.created_at,
            ],
        )?;
        transaction.execute(
            "INSERT INTO events(at, kind, payload_json, user_id) VALUES (?1, ?2, ?3, ?4)",
            params![card.created_at, CARD_CREATED_EVENT, event, user_id],
        )?;
        transaction.commit()?;
        Ok(card)
    }

    /// The acting user's cards in one conversation, oldest first. Only the
    /// newest `MAX_LISTED_CARDS` are returned.
    pub fn list_cards(&self, user_id: &str, conversation_id: &str) -> Result<Vec<Card>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT {CARD_COLUMNS} FROM (
                 SELECT {CARD_COLUMNS} FROM chat_cards
                 WHERE user_id = ?1 AND conversation_id = ?2
                 ORDER BY created_at DESC, id DESC LIMIT ?3
             ) ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = statement.query_map(
            params![user_id, conversation_id, MAX_LISTED_CARDS as i64],
            raw_card,
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(into_card)
            .collect()
    }

    /// One of the acting user's cards. Another user's card is indistinguishable
    /// from a missing one.
    pub fn get_card(&self, user_id: &str, id: &str) -> Result<Card> {
        let connection = self.connection()?;
        read_card(&connection, user_id, id)?
            .ok_or_else(|| SeraError::NotFound(format!("card {id}")))
    }

    /// The acting user's newest pending question in one conversation, if any.
    pub fn pending_question(&self, user_id: &str, conversation_id: &str) -> Result<Option<Card>> {
        let connection = self.connection()?;
        let raw = connection
            .query_row(
                &format!(
                    "SELECT {CARD_COLUMNS} FROM chat_cards
                     WHERE user_id = ?1 AND conversation_id = ?2
                       AND kind = 'question' AND status = 'pending'
                     ORDER BY created_at DESC, id DESC LIMIT 1"
                ),
                params![user_id, conversation_id],
                raw_card,
            )
            .optional()?;
        raw.map(into_card).transpose()
    }

    /// Expire the acting user's pending questions in one conversation that
    /// were created before `before`. Returns how many changed.
    pub fn expire_stale_questions(
        &self,
        user_id: &str,
        conversation_id: &str,
        before: DateTime<Utc>,
    ) -> Result<usize> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "UPDATE chat_cards SET status = 'expired', resolved_at = ?1
             WHERE user_id = ?2 AND conversation_id = ?3
               AND kind = 'question' AND status = 'pending' AND created_at < ?4",
            params![now_string(), user_id, conversation_id, to_string(before)],
        )?)
    }

    /// Move one pending card to answered, declined or expired, and append
    /// `card.resolved`. A card resolves once: a second resolve, or a resolve of
    /// a card that is not pending, is a validation error.
    pub fn resolve_card(
        &self,
        user_id: &str,
        id: &str,
        status: CardStatus,
        response: &Value,
    ) -> Result<Card> {
        if status == CardStatus::Pending {
            return Err(invalid("a card resolves to answered, declined or expired"));
        }
        let stored_response = serde_json::to_string(response)?;
        if stored_response.len() > MAX_RESPONSE_BYTES {
            return Err(invalid("card response is too large"));
        }
        let resolved_at = now_string();
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction()?;
        let conversation_id: Option<String> = transaction
            .query_row(
                "SELECT conversation_id FROM chat_cards WHERE id = ?1 AND user_id = ?2",
                params![id, user_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(conversation_id) = conversation_id else {
            return Err(SeraError::NotFound(format!("card {id}")));
        };
        let changed = transaction.execute(
            "UPDATE chat_cards SET status = ?1, response = ?2, resolved_at = ?3
             WHERE id = ?4 AND user_id = ?5 AND status = 'pending'",
            params![status.as_str(), stored_response, resolved_at, id, user_id],
        )?;
        if changed != 1 {
            return Err(invalid("card is no longer pending"));
        }
        let event = serde_json::to_string(&json!({
            "card_id": id,
            "conversation_id": conversation_id,
            "status": status.as_str(),
        }))?;
        transaction.execute(
            "INSERT INTO events(at, kind, payload_json, user_id) VALUES (?1, ?2, ?3, ?4)",
            params![resolved_at, CARD_RESOLVED_EVENT, event, user_id],
        )?;
        let card = read_card(&transaction, user_id, id)?
            .ok_or_else(|| SeraError::NotFound(format!("card {id}")))?;
        transaction.commit()?;
        Ok(card)
    }
}

/// `ask_user`: put one question to the owner as a card, or decide alone.
pub struct AskUserTool {
    store: Store,
    policy: Arc<Policy>,
}

impl AskUserTool {
    pub fn new(store: Store, policy: Arc<Policy>) -> Self {
        Self { store, policy }
    }

    /// Auto-approve on, or a goal plan set: the bot decides and says so. A
    /// read error counts as no goal, so the bot may still ask.
    fn autonomous(&self, user_id: &str, conversation_id: &str) -> bool {
        self.store.yolo_enabled(user_id, conversation_id)
            || matches!(
                self.store.get_plan(user_id, conversation_id),
                Ok(Some(plan)) if !plan.goal.trim().is_empty()
            )
    }

    /// Whether the conversation was started from Telegram. A read error counts
    /// as not Telegram.
    fn telegram_chat(&self, user_id: &str, conversation_id: &str) -> bool {
        matches!(
            self.store.conversation_origin(user_id, conversation_id),
            Ok(Some(origin)) if origin == crate::telegram::ORIGIN_TELEGRAM
        )
    }

    /// The owner-visible text of a question must pass the content policy.
    fn policy_allows(&self, payload: &Value) -> bool {
        let mut texts = vec![payload["question"].as_str().unwrap_or_default().to_owned()];
        texts.extend(
            payload["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
        texts.extend(payload["context"].as_str().map(str::to_owned));
        texts.iter().all(|text| self.policy.is_allowed(text))
    }
}

fn reply(value: Value) -> Result<ToolOutput> {
    Ok(ToolOutput {
        content: serde_json::to_string(&value)?,
        data: Some(value),
        ..ToolOutput::default()
    })
}

#[async_trait]
impl Tool for AskUserTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ASK_USER_TOOL.into(),
            description: "Ask the owner one question as a card in this chat, with up to 6 short options and optional free text. Use it only when the request is ambiguous and a wrong guess would waste work. It returns at once: end your turn and wait for the answer. If it returns asked=false (autonomy is on), decide yourself and state the assumption. Never ask for passwords or API keys here.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "question": {"type": "string", "description": "One clear question, at most 500 characters"},
                    "options": {
                        "type": "array",
                        "maxItems": MAX_OPTIONS,
                        "items": {"type": "string", "maxLength": MAX_OPTION_CHARS},
                        "description": "Two to four concrete answers"
                    },
                    "allow_free_text": {"type": "boolean", "description": "Let the owner type another answer. Defaults to true."},
                    "context": {"type": "string", "description": "Optional short reason, at most 500 characters"}
                },
                "required": ["question"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let conversation_id = context
            .conversation_id
            .as_deref()
            .ok_or_else(|| invalid("ask_user needs a conversation"))?;
        let user = context.user_id.as_str();
        if self.autonomous(user, conversation_id) {
            return reply(json!({"asked": false, "autonomy": true, "note": AUTONOMY_NOTE}));
        }
        // A Telegram chat has no card view and no answer route: asking would
        // leave the turn waiting for an answer that cannot arrive.
        if self.telegram_chat(user, conversation_id) {
            return reply(json!({"asked": false, "autonomy": false, "note": NO_CARD_VIEW_NOTE}));
        }
        let payload = parse_question_args(&args)?;
        if !self.policy_allows(&payload) {
            return Err(invalid("question is withheld by content policy"));
        }
        let stale_before = Utc::now() - Duration::hours(QUESTION_TTL_HOURS);
        self.store
            .expire_stale_questions(user, conversation_id, stale_before)?;
        if let Some(pending) = self.store.pending_question(user, conversation_id)? {
            return reply(json!({
                "asked": true,
                "card_id": pending.id,
                "pending": true,
                "note": PENDING_NOTE,
            }));
        }
        let card = self.store.create_card(
            user,
            conversation_id,
            &context.bot,
            CardKind::Question,
            &payload,
        )?;
        reply(json!({"asked": true, "card_id": card.id, "note": ASKED_NOTE}))
    }
}

/// Register the card tools. `ask_user` is allowlisted per bot in `bots/*.toml`.
pub fn register_card_tools(
    registry: &mut ToolRegistry,
    store: Store,
    policy: Arc<Policy>,
) -> Result<()> {
    registry.register(AskUserTool::new(store, policy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn store_with_users() -> (Store, String, String) {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let other = store
            .create_user("microsoft:card-other", "card@example.example", "Card")
            .unwrap()
            .id;
        (store, admin, other)
    }

    fn question(text: &str) -> Value {
        json!({
            "question": text,
            "options": ["Blue", "Green"],
            "allow_free_text": true,
        })
    }

    fn secret_payload() -> Value {
        json!({
            "purpose": "save",
            "name": "vendor-portal",
            "kind": "password",
            "username": "ada",
            "domains": ["vendor.example"],
            "reason": "Log in to the vendor portal for the invoice.",
        })
    }

    fn context(user: &str, conversation: &str) -> ToolContext {
        ToolContext {
            user_id: user.into(),
            actor: user.into(),
            bot: "main".into(),
            task_id: None,
            turn_id: None,
            conversation_id: Some(conversation.into()),
            allowed_tools: HashSet::new(),
            require_approval: HashSet::new(),
        }
    }

    #[test]
    fn card_round_trips_and_appends_created_and_resolved_events() {
        let (store, admin, _) = store_with_users();
        let created = store
            .create_card(
                &admin,
                "dir:main",
                "main",
                CardKind::Question,
                &question("Which colour?"),
            )
            .unwrap();
        assert_eq!(created.status, CardStatus::Pending);
        assert_eq!(created.kind, CardKind::Question);
        assert!(created.response.is_none());

        let listed = store.list_cards(&admin, "dir:main").unwrap();
        assert_eq!(listed, vec![created.clone()]);
        assert!(store.list_cards(&admin, "dir:other").unwrap().is_empty());

        let resolved = store
            .resolve_card(
                &admin,
                &created.id,
                CardStatus::Answered,
                &json!({"answer": "Blue"}),
            )
            .unwrap();
        assert_eq!(resolved.status, CardStatus::Answered);
        assert_eq!(resolved.response, Some(json!({"answer": "Blue"})));
        assert!(resolved.resolved_at.is_some());
        assert_eq!(
            store.get_card(&admin, &created.id).unwrap().status,
            CardStatus::Answered
        );

        let events = store.events_after(&admin, 0, 100).unwrap();
        let kinds: Vec<&str> = events.iter().map(|event| event.kind.as_str()).collect();
        assert_eq!(kinds, vec![CARD_CREATED_EVENT, CARD_RESOLVED_EVENT]);
        assert_eq!(events[0].payload["card_id"], created.id);
        assert_eq!(events[0].payload["kind"], "question");
    }

    #[test]
    fn cards_are_isolated_per_user() {
        let (store, admin, other) = store_with_users();
        let card = store
            .create_card(
                &admin,
                "dir:main",
                "main",
                CardKind::Question,
                &question("Private?"),
            )
            .unwrap();
        assert!(matches!(
            store.get_card(&other, &card.id),
            Err(SeraError::NotFound(_))
        ));
        assert!(store.list_cards(&other, "dir:main").unwrap().is_empty());
        assert!(
            store
                .pending_question(&other, "dir:main")
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            store.resolve_card(&other, &card.id, CardStatus::Declined, &json!({})),
            Err(SeraError::NotFound(_))
        ));
        // The owner's card is untouched by the other user's attempt.
        assert_eq!(
            store.get_card(&admin, &card.id).unwrap().status,
            CardStatus::Pending
        );
        let other_events = store.events_after(&other, 0, 100).unwrap();
        assert!(other_events.is_empty());
    }

    #[test]
    fn a_card_resolves_once() {
        let (store, admin, _) = store_with_users();
        let card = store
            .create_card(
                &admin,
                "dir:main",
                "main",
                CardKind::Question,
                &question("Once?"),
            )
            .unwrap();
        store
            .resolve_card(&admin, &card.id, CardStatus::Declined, &json!({}))
            .unwrap();
        for status in [CardStatus::Answered, CardStatus::Declined] {
            assert!(matches!(
                store.resolve_card(&admin, &card.id, status, &json!({"answer": "late"})),
                Err(SeraError::Validation(_))
            ));
        }
        assert!(matches!(
            store.resolve_card(&admin, &card.id, CardStatus::Pending, &json!({})),
            Err(SeraError::Validation(_))
        ));
        assert_eq!(
            store.get_card(&admin, &card.id).unwrap().status,
            CardStatus::Declined
        );
    }

    #[test]
    fn invalid_payloads_are_refused_before_anything_is_stored() {
        let (store, admin, _) = store_with_users();
        let too_many: Vec<String> = (0..7).map(|i| format!("option {i}")).collect();
        let mut steal = secret_payload();
        steal["purpose"] = json!("steal");
        let mut file_kind = secret_payload();
        file_kind["kind"] = json!("file");
        let bad = [
            (
                CardKind::Question,
                json!({"question": "", "options": [], "allow_free_text": true}),
            ),
            (
                CardKind::Question,
                json!({"question": "q", "options": too_many, "allow_free_text": true}),
            ),
            (
                CardKind::Question,
                json!({"question": "q", "options": [], "allow_free_text": false}),
            ),
            (
                CardKind::Question,
                json!({"question": "q", "options": ["x".repeat(81)], "allow_free_text": true}),
            ),
            (CardKind::SecretRequest, steal),
            (CardKind::SecretRequest, file_kind),
            (CardKind::SecretRequest, json!({"purpose": "save"})),
        ];
        for (kind, payload) in bad {
            assert!(
                matches!(
                    store.create_card(&admin, "dir:main", "main", kind, &payload),
                    Err(SeraError::Validation(_))
                ),
                "{payload}"
            );
        }
        assert!(store.list_cards(&admin, "dir:main").unwrap().is_empty());
        store
            .create_card(
                &admin,
                "dir:main",
                "main",
                CardKind::SecretRequest,
                &secret_payload(),
            )
            .unwrap();
    }

    #[test]
    fn pending_question_expires_when_stale() {
        let (store, admin, _) = store_with_users();
        let card = store
            .create_card(
                &admin,
                "dir:main",
                "main",
                CardKind::Question,
                &question("Stale?"),
            )
            .unwrap();
        assert_eq!(
            store
                .pending_question(&admin, "dir:main")
                .unwrap()
                .unwrap()
                .id,
            card.id
        );
        // Not stale yet: a cutoff in the past expires nothing.
        let past = Utc::now() - Duration::hours(1);
        assert_eq!(
            store
                .expire_stale_questions(&admin, "dir:main", past)
                .unwrap(),
            0
        );
        let future = Utc::now() + Duration::hours(1);
        assert_eq!(
            store
                .expire_stale_questions(&admin, "dir:main", future)
                .unwrap(),
            1
        );
        assert!(
            store
                .pending_question(&admin, "dir:main")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store.get_card(&admin, &card.id).unwrap().status,
            CardStatus::Expired
        );
    }

    #[test]
    fn question_arguments_are_cleaned_and_bounded() {
        let payload = parse_question_args(&json!({
            "question": "  Which\u{7} file \n now? ",
            "options": ["A", "A", " B "],
            "context": "why\u{1b}",
        }))
        .unwrap();
        assert_eq!(payload["question"], "Which file now?");
        assert_eq!(payload["options"], json!(["A", "B"]));
        assert_eq!(payload["allow_free_text"], true);
        assert_eq!(payload["context"], "why");
        for bad in [
            json!({"options": ["a"]}),
            json!({"question": "q", "options": "a"}),
            json!({"question": "q", "options": ["  "]}),
            json!({"question": "q", "options": vec!["a"; 7]}),
            json!({"question": "q", "options": [], "allow_free_text": false}),
            json!({"question": "q", "allow_free_text": "yes"}),
        ] {
            assert!(parse_question_args(&bad).is_err(), "{bad}");
        }
        let long = parse_question_args(&json!({"question": "q".repeat(900)})).unwrap();
        assert_eq!(
            long["question"].as_str().unwrap().chars().count(),
            MAX_QUESTION_CHARS
        );
    }

    #[tokio::test]
    async fn autonomy_short_circuits_the_question_in_yolo_and_goal_chats() {
        let (store, admin, _) = store_with_users();
        let tool = AskUserTool::new(store.clone(), Arc::new(Policy::empty()));
        let ctx = context(&admin, "dir:main");

        store.set_yolo(&admin, "dir:main", true).unwrap();
        let out = tool
            .execute(json!({"question": "Which?"}), &ctx)
            .await
            .unwrap();
        assert_eq!(out.data.unwrap()["asked"], false);
        store.set_yolo(&admin, "dir:main", false).unwrap();

        store
            .set_plan(&admin, "dir:main", "Ship the report", &[])
            .unwrap();
        let out = tool
            .execute(json!({"question": "Which?"}), &ctx)
            .await
            .unwrap();
        let data = out.data.unwrap();
        assert_eq!(data["asked"], false);
        assert_eq!(data["autonomy"], true);
        assert!(store.list_cards(&admin, "dir:main").unwrap().is_empty());
    }

    #[tokio::test]
    async fn only_one_question_is_pending_per_conversation() {
        let (store, admin, _) = store_with_users();
        let tool = AskUserTool::new(store.clone(), Arc::new(Policy::empty()));
        let ctx = context(&admin, "dir:main");
        let first = tool
            .execute(
                json!({"question": "Which format?", "options": ["PDF", "DOCX"]}),
                &ctx,
            )
            .await
            .unwrap()
            .data
            .unwrap();
        assert_eq!(first["asked"], true);
        let card_id = first["card_id"].as_str().unwrap().to_owned();

        let second = tool
            .execute(json!({"question": "Another?"}), &ctx)
            .await
            .unwrap()
            .data
            .unwrap();
        assert_eq!(second["card_id"], card_id);
        assert_eq!(second["pending"], true);
        assert_eq!(store.list_cards(&admin, "dir:main").unwrap().len(), 1);

        // Another conversation has its own slot.
        tool.execute(
            json!({"question": "Elsewhere?"}),
            &context(&admin, "dir:other"),
        )
        .await
        .unwrap();
        assert_eq!(store.list_cards(&admin, "dir:other").unwrap().len(), 1);

        // Once answered, the next question is asked afresh.
        store
            .resolve_card(
                &admin,
                &card_id,
                CardStatus::Answered,
                &json!({"answer": "PDF"}),
            )
            .unwrap();
        let third = tool
            .execute(json!({"question": "Next?"}), &ctx)
            .await
            .unwrap()
            .data
            .unwrap();
        assert_ne!(third["card_id"], card_id);
        assert_eq!(store.list_cards(&admin, "dir:main").unwrap().len(), 2);
    }

    #[tokio::test]
    async fn policy_blocked_questions_are_not_stored() {
        let (store, admin, _) = store_with_users();
        let policy = Arc::new(Policy::from_text("forbiddenterm"));
        let tool = AskUserTool::new(store.clone(), policy);
        let ctx = context(&admin, "dir:main");
        let blocked = tool
            .execute(json!({"question": "Use forbiddenterm here?"}), &ctx)
            .await;
        assert!(matches!(blocked, Err(SeraError::Validation(_))));
        assert!(store.list_cards(&admin, "dir:main").unwrap().is_empty());
    }

    #[tokio::test]
    async fn telegram_chats_are_never_asked_because_no_card_can_reach_them() {
        let (store, admin, _) = store_with_users();
        store
            .set_conversation_origin(&admin, "20", crate::telegram::ORIGIN_TELEGRAM)
            .unwrap();
        let tool = AskUserTool::new(store.clone(), Arc::new(Policy::empty()));
        let out = tool
            .execute(json!({"question": "Which?"}), &context(&admin, "20"))
            .await
            .unwrap()
            .data
            .unwrap();
        assert_eq!(out["asked"], false);
        assert_eq!(out["autonomy"], false);
        assert!(store.list_cards(&admin, "20").unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_tool_needs_a_conversation() {
        let (store, admin, _) = store_with_users();
        let tool = AskUserTool::new(store, Arc::new(Policy::empty()));
        let mut ctx = context(&admin, "dir:main");
        ctx.conversation_id = None;
        assert!(
            tool.execute(json!({"question": "Where?"}), &ctx)
                .await
                .is_err()
        );
    }
}
