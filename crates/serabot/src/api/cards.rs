//! Chat card routes (0.23): the conversation's cards, the owner's answer to a
//! question, and a decline. A secret request is resolved by the vault route,
//! never here.
//!
//! An answer or decline becomes an ordinary owner message, so it starts a bot
//! turn through the same path as typed text. The message names the question
//! it answers, so the model knows what the owner is replying to.

use super::*;
use crate::cards::{CardKind, CardStatus, MAX_ANSWER_CHARS};
use crate::vault::{VaultInput, VaultKind, VaultSource};
use zeroize::Zeroizing;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AnswerRequest {
    answer: String,
}

/// Card ids with an answer or decline in flight. A second request for the same
/// card is refused while the first one posts its message, so one card posts at
/// most one message in this process.
pub(super) struct CardClaim {
    claims: Arc<std::sync::Mutex<HashSet<String>>>,
    id: String,
}

impl CardClaim {
    pub(super) fn take(claims: &Arc<std::sync::Mutex<HashSet<String>>>, id: &str) -> Result<Self> {
        let mut held = claims
            .lock()
            .map_err(|_| SeraError::Internal("card claims are unavailable".into()))?;
        if !held.insert(id.to_owned()) {
            return Err(SeraError::InvalidState(
                "card is already being answered".into(),
            ));
        }
        Ok(Self {
            claims: claims.clone(),
            id: id.to_owned(),
        })
    }
}

impl Drop for CardClaim {
    fn drop(&mut self) {
        if let Ok(mut held) = self.claims.lock() {
            held.remove(&self.id);
        }
    }
}

/// `GET /v1/conversations/{id}/cards`: the owner's cards in one conversation,
/// oldest first.
pub(super) async fn list_conversation_cards(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    validate_conversation_id(&id)?;
    let cards = state.store.list_cards(&user.id, &id)?;
    Ok(Json(json!({"cards": cards})))
}

/// `POST /v1/cards/{id}/answer`: `{"answer": "..."}` for a pending question.
/// The answer is posted as the owner's next message, prefixed with the
/// question, and the card is marked answered.
pub(super) async fn answer_card(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<AnswerRequest>,
) -> Result<Json<Value>> {
    let _claim = CardClaim::take(&state.card_claims, &id)?;
    let card = state.store.get_card(&user.id, &id)?;
    if card.kind != CardKind::Question {
        return Err(SeraError::Validation(
            "only a question takes an answer".into(),
        ));
    }
    if card.status != CardStatus::Pending {
        return Err(SeraError::Validation("card is no longer pending".into()));
    }
    let answer = answer_text(&request.answer)?;
    let free_text = card.payload["allow_free_text"].as_bool().unwrap_or(true);
    let listed = card.payload["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|option| option == answer);
    if !free_text && !listed {
        return Err(SeraError::Validation(
            "answer must be one of the options".into(),
        ));
    }
    let question = card.payload["question"].as_str().unwrap_or_default();
    let text = format!("Answer to “{question}”: {answer}");
    state.runtime.ensure_model_catalogs(&user.id).await;
    let task = state.runtime.submit_text_attached(
        &user.id,
        &card.conversation_id,
        &text,
        Some(&card.bot),
        &[],
    )?;
    let card = state.store.resolve_card(
        &user.id,
        &id,
        CardStatus::Answered,
        &json!({"answer": answer}),
    )?;
    Ok(Json(json!({"card": card, "task": task_view(&task)})))
}

/// `POST /v1/cards/{id}/decline`: dismiss a pending card. The bot is told in
/// the conversation, so it proceeds without the answer.
pub(super) async fn decline_card(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let _claim = CardClaim::take(&state.card_claims, &id)?;
    let card = state.store.get_card(&user.id, &id)?;
    if card.status != CardStatus::Pending {
        return Err(SeraError::Validation("card is no longer pending".into()));
    }
    let note = match card.kind {
        CardKind::Question => format!(
            "(The user dismissed the question “{}”; proceed with your best judgment.)",
            card.payload["question"].as_str().unwrap_or_default()
        ),
        CardKind::SecretRequest => format!(
            "(The user declined to share the secret “{}”; continue without it.)",
            card.payload["name"].as_str().unwrap_or_default()
        ),
    };
    state.runtime.ensure_model_catalogs(&user.id).await;
    let task = state.runtime.submit_text_attached(
        &user.id,
        &card.conversation_id,
        &note,
        Some(&card.bot),
        &[],
    )?;
    let card = state
        .store
        .resolve_card(&user.id, &id, CardStatus::Declined, &json!({}))?;
    Ok(Json(json!({"card": card, "task": task_view(&task)})))
}

/// The largest body the secret route reads: a value is at most 8 KiB, and the
/// JSON framing and escapes fit well inside 16 KiB.
pub(super) const SECRET_BODY_LIMIT_BYTES: usize = 16 * 1024;

/// `{"value": "..."}` for a secret request. Never logged: `Debug` redacts it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SecretRequest {
    value: String,
}

impl std::fmt::Debug for SecretRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretRequest")
            .field("value", &"<redacted>")
            .finish()
    }
}

/// `POST /v1/cards/{id}/secret`: seal the owner's value into the vault under
/// the card's name, kind, username and sites. The bot is told the entry was
/// saved (never the value), and the card is marked saved. A value for an
/// existing name replaces it only when the card asked to save; a card that
/// asked to provide a login never overwrites one.
pub(super) async fn submit_secret_card(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<SecretRequest>,
) -> Result<Json<Value>> {
    let value = Zeroizing::new(request.value);
    let _claim = CardClaim::take(&state.card_claims, &id)?;
    let card = state.store.get_card(&user.id, &id)?;
    if card.kind != CardKind::SecretRequest {
        return Err(SeraError::Validation(
            "only a secret request takes a value".into(),
        ));
    }
    if card.status != CardStatus::Pending {
        return Err(SeraError::Validation("card is no longer pending".into()));
    }
    crate::vault::validate_value(&value)?;
    let payload = &card.payload;
    let name = payload["name"].as_str().unwrap_or_default().to_owned();
    let kind: VaultKind = serde_json::from_value(payload["kind"].clone())
        .map_err(|_| SeraError::Internal("stored card has an unknown kind".into()))?;
    let purpose = payload["purpose"].as_str().unwrap_or_default();
    if purpose != "save" && crate::vault::get(&state.users, &user.id, &name)?.is_some() {
        return Err(SeraError::InvalidState(
            "a vault entry with this name already exists".into(),
        ));
    }
    let entry = crate::vault::put(
        &state.users,
        &user.id,
        &name,
        &VaultInput {
            kind,
            username: payload["username"].as_str().map(str::to_owned),
            value,
            domains: serde_json::from_value(payload["domains"].clone()).unwrap_or_default(),
            note: None,
            source: VaultSource::Local,
            reference: None,
        },
    )?;
    let sites = entry.domains.join(", ");
    let text = format!(
        "Secret “{name}” was saved to the vault for {sites}. Use it with browser_type secret={name}."
    );
    state.runtime.ensure_model_catalogs(&user.id).await;
    let task = state.runtime.submit_text_attached(
        &user.id,
        &card.conversation_id,
        &text,
        Some(&card.bot),
        &[],
    )?;
    state.store.record_audit(
        &user.id,
        "vault.saved_via_card",
        None,
        None,
        None,
        None,
        None,
        Some(&json!({"name": name, "card_id": id})),
    )?;
    let card = state.store.resolve_card(
        &user.id,
        &id,
        CardStatus::Answered,
        &json!({"saved": true, "name": name}),
    )?;
    Ok(Json(json!({"card": card, "task": task_view(&task)})))
}

/// Owner text for an answer: control characters become spaces, the ends are
/// trimmed, and the length is bounded.
fn answer_text(raw: &str) -> Result<String> {
    let text: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = text.trim().to_owned();
    if text.is_empty() {
        return Err(SeraError::Validation("answer is required".into()));
    }
    if text.chars().count() > MAX_ANSWER_CHARS {
        return Err(SeraError::Validation("answer is too long".into()));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_trimmed_and_bounded() {
        assert_eq!(answer_text("  Blue\n\tlight ").unwrap(), "Blue  light");
        assert!(answer_text("   ").is_err());
        assert!(answer_text(&"a".repeat(MAX_ANSWER_CHARS + 1)).is_err());
        assert!(answer_text(&"a".repeat(MAX_ANSWER_CHARS)).is_ok());
    }

    #[test]
    fn a_card_claim_is_exclusive_until_dropped() {
        let claims: Arc<std::sync::Mutex<HashSet<String>>> = Arc::default();
        let first = CardClaim::take(&claims, "card-1").unwrap();
        assert!(CardClaim::take(&claims, "card-1").is_err());
        assert!(CardClaim::take(&claims, "card-2").is_ok());
        drop(first);
        assert!(CardClaim::take(&claims, "card-1").is_ok());
    }
}
