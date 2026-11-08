//! Chat cards in the open conversation (0.23): the questions a bot asks the
//! owner and the secret requests the vault raises.
//!
//! The server returns cards with the transcript. Card text is written by a
//! model or by the server, so it is untrusted display data here: bounded,
//! control characters dropped, and never sent anywhere except as the owner's
//! own answer. A card with an unknown kind, status or shape is left out rather
//! than failing the whole transcript. A secret request is shown with its
//! metadata only; no secret value ever reaches this module.

use crate::ChatCard;

pub const MAX_QUESTION: usize = 500;
pub const MAX_OPTIONS: usize = 6;
pub const MAX_OPTION: usize = 80;
pub const MAX_ANSWER: usize = 1_000;
const MAX_NAME: usize = 128;
const MAX_CONTEXT: usize = 500;
const MAX_DOMAINS: usize = 20;
const MAX_DOMAIN: usize = 253;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardState {
    Pending,
    Answered,
    Declined,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionCard {
    pub question: String,
    pub options: Vec<String>,
    pub allow_free_text: bool,
    pub context: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretPurpose {
    Save,
    Provide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretKind {
    Password,
    ApiKey,
    Note,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretCard {
    pub purpose: SecretPurpose,
    pub name: String,
    pub kind: SecretKind,
    pub username: Option<String>,
    pub domains: Vec<String>,
    pub reason: String,
    /// A save for a name that already has a vault entry: the value replaces it.
    pub replaces: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardBody {
    Question(QuestionCard),
    Secret(SecretCard),
}

/// One card, checked and cleaned for drawing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardView {
    pub id: String,
    pub bot: String,
    pub created_at: String,
    pub state: CardState,
    /// The owner's answer to a resolved question.
    pub answer: Option<String>,
    pub body: CardBody,
}

impl CardView {
    /// The drawable form of a server card, or `None` when its kind, status or
    /// payload is not one this console knows how to show.
    pub fn from_card(card: &ChatCard) -> Option<Self> {
        let state = match card.status.as_str() {
            "pending" => CardState::Pending,
            "answered" => CardState::Answered,
            "declined" => CardState::Declined,
            "expired" => CardState::Expired,
            _ => return None,
        };
        let body = match card.kind.as_str() {
            "question" => CardBody::Question(question_from(&card.payload)?),
            "secret_request" => CardBody::Secret(secret_from(&card.payload)?),
            _ => return None,
        };
        let answer = match (&body, state, &card.response) {
            (CardBody::Question(_), CardState::Answered, Some(response)) => response
                .get("answer")
                .and_then(serde_json::Value::as_str)
                .and_then(|text| clean(text, MAX_ANSWER)),
            _ => None,
        };
        Some(Self {
            id: card.id.clone(),
            bot: clean(&card.bot, MAX_NAME).unwrap_or_default(),
            created_at: card.created_at.clone(),
            state,
            answer,
            body,
        })
    }
}

fn question_from(payload: &serde_json::Value) -> Option<QuestionCard> {
    let question = clean(payload.get("question")?.as_str()?, MAX_QUESTION)?;
    let options = payload
        .get("options")?
        .as_array()?
        .iter()
        .map(|option| clean(option.as_str()?, MAX_OPTION))
        .collect::<Option<Vec<_>>>()?;
    if options.len() > MAX_OPTIONS {
        return None;
    }
    let allow_free_text = payload.get("allow_free_text")?.as_bool()?;
    if options.is_empty() && !allow_free_text {
        return None;
    }
    let context = match payload.get("context") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(clean(value.as_str()?, MAX_CONTEXT)?),
    };
    Some(QuestionCard {
        question,
        options,
        allow_free_text,
        context,
    })
}

fn secret_from(payload: &serde_json::Value) -> Option<SecretCard> {
    let purpose = match payload.get("purpose")?.as_str()? {
        "save" => SecretPurpose::Save,
        "provide" => SecretPurpose::Provide,
        _ => return None,
    };
    let kind = match payload.get("kind")?.as_str()? {
        "password" => SecretKind::Password,
        "api_key" => SecretKind::ApiKey,
        "note" => SecretKind::Note,
        _ => return None,
    };
    let name = clean(payload.get("name")?.as_str()?, MAX_NAME)?;
    let username = match payload.get("username") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => clean(value.as_str()?, MAX_NAME),
    };
    let domains = payload
        .get("domains")?
        .as_array()?
        .iter()
        .map(|domain| clean(domain.as_str()?, MAX_DOMAIN))
        .collect::<Option<Vec<_>>>()?;
    if domains.len() > MAX_DOMAINS {
        return None;
    }
    let reason = clean(payload.get("reason")?.as_str()?, MAX_CONTEXT).unwrap_or_default();
    let replaces = payload.get("replaces").and_then(serde_json::Value::as_bool) == Some(true);
    Some(SecretCard {
        purpose,
        name,
        kind,
        username,
        domains,
        reason,
        replaces,
    })
}

/// Display text: control characters become spaces, runs of whitespace collapse
/// to one space, and the length is bounded. Empty text is `None`. This matches
/// how the server cleans a card's text.
fn clean(raw: &str, max_chars: usize) -> Option<String> {
    let flat: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    let bounded = collapsed.chars().take(max_chars).collect::<String>();
    let bounded = bounded.trim_end();
    (!bounded.is_empty()).then(|| bounded.to_owned())
}

/// The owner's typed or chosen answer, cleaned the way the server cleans it.
/// `None` when it is empty or too long to send.
pub fn clean_answer(raw: &str) -> Option<String> {
    let text: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = text.trim();
    (!text.is_empty() && text.chars().count() <= MAX_ANSWER).then(|| text.to_owned())
}

/// Whether the answer may be sent for this question: any text when free text
/// is allowed, otherwise only one of the options.
pub fn answer_allowed(question: &QuestionCard, answer: &str) -> bool {
    if question.allow_free_text {
        clean_answer(answer).is_some()
    } else {
        question.options.iter().any(|option| option == answer)
    }
}

/// The title of a secret request card.
pub fn secret_title(purpose: SecretPurpose) -> &'static str {
    match purpose {
        SecretPurpose::Save => "Bot wants to save a secret",
        SecretPurpose::Provide => "Bot needs a secret",
    }
}

pub fn secret_kind_label(kind: SecretKind) -> &'static str {
    match kind {
        SecretKind::Password => "Password",
        SecretKind::ApiKey => "API key",
        SecretKind::Note => "Note",
    }
}

/// The line that stands in for a card once it is resolved.
pub fn resolved_line(view: &CardView) -> String {
    match (&view.body, view.state) {
        (CardBody::Question(_), CardState::Answered) => match &view.answer {
            Some(answer) => format!("You answered: {answer}"),
            None => "You answered.".into(),
        },
        (CardBody::Question(_), CardState::Declined) => "You skipped this question.".into(),
        (CardBody::Question(_), CardState::Expired) => "This question expired.".into(),
        (CardBody::Secret(_), CardState::Answered) => "Saved to your vault.".into(),
        (CardBody::Secret(_), CardState::Declined) => "You declined to share it.".into(),
        (CardBody::Secret(_), CardState::Expired) => "This request expired.".into(),
        (_, CardState::Pending) => "Waiting for you.".into(),
    }
}

/// Where a resolved card sits in the transcript: before the first message
/// created after it. `messages` holds the `created_at` stamps, oldest first.
/// Returns `messages.len()` when every message is older, so the card trails.
pub fn insert_index(messages: &[String], card_created_at: &str) -> usize {
    messages.partition_point(|at| at.as_str() <= card_created_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card(kind: &str, status: &str, payload: serde_json::Value) -> ChatCard {
        ChatCard {
            id: "card-1".into(),
            conversation_id: "dir:analyst".into(),
            bot: "analyst".into(),
            kind: kind.into(),
            payload,
            status: status.into(),
            response: None,
            created_at: "2026-11-08T10:00:00.000Z".into(),
            resolved_at: None,
        }
    }

    fn question(options: serde_json::Value, allow_free_text: bool) -> serde_json::Value {
        json!({
            "question": "Which format?",
            "options": options,
            "allow_free_text": allow_free_text,
        })
    }

    fn secret_payload() -> serde_json::Value {
        json!({
            "purpose": "save",
            "name": "vendor-portal",
            "kind": "password",
            "username": "ada",
            "domains": ["vendor.example"],
            "reason": "Log in to the vendor portal.",
        })
    }

    #[test]
    fn a_pending_question_keeps_its_options_and_text() {
        let view = CardView::from_card(&card(
            "question",
            "pending",
            json!({
                "question": "  Which\u{7} format?  ",
                "options": ["PDF", "DOCX"],
                "allow_free_text": true,
                "context": "The export step",
            }),
        ))
        .unwrap();
        assert_eq!(view.state, CardState::Pending);
        assert_eq!(view.bot, "analyst");
        let CardBody::Question(question) = view.body else {
            panic!("expected a question");
        };
        assert_eq!(question.question, "Which format?");
        assert_eq!(question.options, vec!["PDF", "DOCX"]);
        assert!(question.allow_free_text);
        assert_eq!(question.context.as_deref(), Some("The export step"));
    }

    #[test]
    fn malformed_or_unknown_cards_are_left_out() {
        let mut file_kind = secret_payload();
        file_kind["kind"] = json!("file");
        let too_many: Vec<&str> = vec!["a"; 7];
        for bad in [
            card(
                "question",
                "pending",
                json!({"question": "", "options": [], "allow_free_text": true}),
            ),
            card("question", "pending", question(json!(too_many), true)),
            card("question", "pending", question(json!([]), false)),
            card("question", "pending", question(json!([1]), true)),
            card("question", "waiting", question(json!(["a"]), true)),
            card("unknown_kind", "pending", question(json!(["a"]), true)),
            card("secret_request", "pending", file_kind),
            card("secret_request", "pending", json!({"purpose": "save"})),
        ] {
            assert!(CardView::from_card(&bad).is_none(), "{:?}", bad.kind);
        }
    }

    #[test]
    fn a_secret_request_shows_metadata_and_never_a_value() {
        let mut stored = card("secret_request", "pending", secret_payload());
        // Even if a value were ever sent, the console has no field for it.
        stored.payload["value"] = json!("hunter2");
        let view = CardView::from_card(&stored).unwrap();
        let CardBody::Secret(secret) = view.body.clone() else {
            panic!("expected a secret request");
        };
        assert_eq!(secret.purpose, SecretPurpose::Save);
        assert_eq!(secret.kind, SecretKind::Password);
        assert_eq!(secret.name, "vendor-portal");
        assert_eq!(secret.username.as_deref(), Some("ada"));
        assert_eq!(secret.domains, vec!["vendor.example"]);
        assert_eq!(secret_title(secret.purpose), "Bot wants to save a secret");
        assert_eq!(secret_kind_label(secret.kind), "Password");
        assert!(!secret.replaces);
        assert!(!format!("{view:?}").contains("hunter2"));
    }

    #[test]
    fn a_card_that_replaces_an_entry_says_so() {
        let mut payload = secret_payload();
        payload["replaces"] = json!(true);
        let view = CardView::from_card(&card("secret_request", "pending", payload)).unwrap();
        let CardBody::Secret(secret) = view.body else {
            panic!("expected a secret request");
        };
        assert!(secret.replaces);
        // Anything other than a true flag is not a replacement.
        let mut stringy = secret_payload();
        stringy["replaces"] = json!("true");
        let view = CardView::from_card(&card("secret_request", "pending", stringy)).unwrap();
        let CardBody::Secret(secret) = view.body else {
            panic!("expected a secret request");
        };
        assert!(!secret.replaces);
    }

    #[test]
    fn an_answered_question_reports_what_was_answered() {
        let mut stored = card("question", "answered", question(json!(["PDF"]), true));
        stored.response = Some(json!({"answer": "PDF"}));
        let view = CardView::from_card(&stored).unwrap();
        assert_eq!(view.answer.as_deref(), Some("PDF"));
        assert_eq!(resolved_line(&view), "You answered: PDF");

        let declined = CardView::from_card(&card(
            "question",
            "declined",
            question(json!(["PDF"]), true),
        ))
        .unwrap();
        assert_eq!(resolved_line(&declined), "You skipped this question.");
    }

    #[test]
    fn answers_are_cleaned_and_checked_against_the_options() {
        assert_eq!(
            clean_answer("  Blue\n\tlight ").as_deref(),
            Some("Blue  light")
        );
        assert_eq!(clean_answer(" \n "), None);
        assert_eq!(clean_answer(&"a".repeat(MAX_ANSWER + 1)), None);

        let CardBody::Question(closed) = CardView::from_card(&card(
            "question",
            "pending",
            question(json!(["PDF", "DOCX"]), false),
        ))
        .unwrap()
        .body
        else {
            panic!("expected a question");
        };
        assert!(answer_allowed(&closed, "DOCX"));
        assert!(!answer_allowed(&closed, "XLSX"));

        let CardBody::Question(open) =
            CardView::from_card(&card("question", "pending", question(json!([]), true)))
                .unwrap()
                .body
        else {
            panic!("expected a question");
        };
        assert!(answer_allowed(&open, "anything at all"));
        assert!(!answer_allowed(&open, "   "));
    }

    #[test]
    fn resolved_cards_sit_before_the_first_later_message() {
        let messages = vec![
            "2026-11-08T09:59:00.000Z".to_owned(),
            "2026-11-08T10:00:00.000Z".to_owned(),
            "2026-11-08T10:01:00.000Z".to_owned(),
        ];
        // Created after the first message, before the second.
        assert_eq!(insert_index(&messages, "2026-11-08T09:59:30.000Z"), 1);
        // Same stamp as the second message: it shows after that message.
        assert_eq!(insert_index(&messages, "2026-11-08T10:00:00.000Z"), 2);
        // Newer than everything: trails the transcript.
        assert_eq!(insert_index(&messages, "2026-11-08T11:00:00.000Z"), 3);
        assert_eq!(insert_index(&[], "2026-11-08T10:00:00.000Z"), 0);
    }
}
