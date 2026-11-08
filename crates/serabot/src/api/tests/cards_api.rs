//! Chat card routes: an answer or decline posts an owner message and resolves
//! the card exactly once; another account never sees or answers the card.
//! No network: the turns are queued, not run.

use super::*;
use crate::cards::{CardKind, CardStatus};

async fn call(app: &Router, token: &str, method: &str, uri: &str, body: Option<Value>) -> Response {
    app.clone()
        .oneshot(request_with_token(method, uri, token, body))
        .await
        .unwrap()
}

fn closed_question() -> Value {
    json!({
        "question": "Which colour?",
        "options": ["Blue", "Green"],
        "allow_free_text": false,
    })
}

#[tokio::test]
async fn answering_posts_the_answer_once_and_stays_owner_scoped() {
    let (app, _, store) = app();
    let owner = store
        .create_user("subject-card-a", "ca@example.example", "Synthetic CA")
        .unwrap();
    let other = store
        .create_user("subject-card-b", "cb@example.example", "Synthetic CB")
        .unwrap();
    let owner_token = mint_for(&store, &owner.id, "card-a");
    let other_token = mint_for(&store, &other.id, "card-b");
    store
        .ensure_conversation_accepts_turns(&owner.id, "dir:cards")
        .unwrap();
    let card = store
        .create_card(
            &owner.id,
            "dir:cards",
            "main",
            CardKind::Question,
            &closed_question(),
        )
        .unwrap();
    let uri = format!("/v1/cards/{}/answer", card.id);

    let listed = read_json(
        call(
            &app,
            &owner_token,
            "GET",
            "/v1/conversations/dir:cards/cards",
            None,
        )
        .await,
    )
    .await;
    assert_eq!(listed["cards"][0]["id"], card.id);
    assert_eq!(listed["cards"][0]["status"], "pending");
    let hidden = read_json(
        call(
            &app,
            &other_token,
            "GET",
            "/v1/conversations/dir:cards/cards",
            None,
        )
        .await,
    )
    .await;
    assert!(hidden["cards"].as_array().unwrap().is_empty());

    // Another account cannot answer, and the card stays pending.
    let foreign = call(
        &app,
        &other_token,
        "POST",
        &uri,
        Some(json!({"answer": "Blue"})),
    )
    .await;
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        store.get_card(&owner.id, &card.id).unwrap().status,
        CardStatus::Pending
    );

    // Closed options: a free answer is refused and nothing is queued.
    let free = call(
        &app,
        &owner_token,
        "POST",
        &uri,
        Some(json!({"answer": "Purple"})),
    )
    .await;
    assert_eq!(free.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(store.pending_task_count(&owner.id).unwrap(), 0);

    let answered = call(
        &app,
        &owner_token,
        "POST",
        &uri,
        Some(json!({"answer": "Blue"})),
    )
    .await;
    assert_eq!(answered.status(), StatusCode::OK);
    let body = read_json(answered).await;
    let task_id = body["task"]["id"].as_str().unwrap();
    let task = store.get_task(&owner.id, task_id).unwrap();
    assert_eq!(task.bot, "main");
    assert_eq!(task.payload["text"], "Answer to “Which colour?”: Blue");
    let stored = store.get_card(&owner.id, &card.id).unwrap();
    assert_eq!(stored.status, CardStatus::Answered);
    assert_eq!(stored.response, Some(json!({"answer": "Blue"})));
    assert_eq!(store.pending_task_count(&owner.id).unwrap(), 1);

    // The card resolves once: a second answer posts nothing.
    let again = call(
        &app,
        &owner_token,
        "POST",
        &uri,
        Some(json!({"answer": "Green"})),
    )
    .await;
    assert_eq!(again.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(store.pending_task_count(&owner.id).unwrap(), 1);
}

#[tokio::test]
async fn declining_tells_the_bot_and_resolves_the_card_once() {
    let (app, _, store) = app();
    let owner = store
        .create_user("subject-card-c", "cc@example.example", "Synthetic CC")
        .unwrap();
    let token = mint_for(&store, &owner.id, "card-c");
    store
        .ensure_conversation_accepts_turns(&owner.id, "dir:declined")
        .unwrap();
    let card = store
        .create_card(
            &owner.id,
            "dir:declined",
            "main",
            CardKind::Question,
            &closed_question(),
        )
        .unwrap();
    let uri = format!("/v1/cards/{}/decline", card.id);

    let declined = call(&app, &token, "POST", &uri, None).await;
    assert_eq!(declined.status(), StatusCode::OK);
    let body = read_json(declined).await;
    let task = store
        .get_task(&owner.id, body["task"]["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        task.payload["text"],
        "(The user dismissed the question “Which colour?”; proceed with your best judgment.)"
    );
    assert_eq!(
        store.get_card(&owner.id, &card.id).unwrap().status,
        CardStatus::Declined
    );

    let again = call(&app, &token, "POST", &uri, None).await;
    assert_eq!(again.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(store.pending_task_count(&owner.id).unwrap(), 1);
}

fn secret_payload() -> Value {
    json!({
        "purpose": "save",
        "name": "vendor-portal",
        "kind": "password",
        "username": "ada",
        "domains": ["vendor.example"],
        "reason": "Log in to the vendor portal.",
    })
}

#[tokio::test]
async fn a_secret_card_seals_the_value_and_nothing_echoes_it() {
    let (app, _, store) = app();
    let owner = store
        .create_user("subject-secret-a", "sa@example.example", "Synthetic SA")
        .unwrap();
    let other = store
        .create_user("subject-secret-b", "sb@example.example", "Synthetic SB")
        .unwrap();
    let owner_token = mint_for(&store, &owner.id, "secret-a");
    let other_token = mint_for(&store, &other.id, "secret-b");
    store
        .ensure_conversation_accepts_turns(&owner.id, "dir:secret")
        .unwrap();
    let card = store
        .create_card(
            &owner.id,
            "dir:secret",
            "main",
            CardKind::SecretRequest,
            &secret_payload(),
        )
        .unwrap();
    let uri = format!("/v1/cards/{}/secret", card.id);
    let value = "synthetic-card-secret-7731";

    // Another account cannot submit a value to the card, and the card stays pending.
    let foreign = call(
        &app,
        &other_token,
        "POST",
        &uri,
        Some(json!({"value": value})),
    )
    .await;
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        store.get_card(&owner.id, &card.id).unwrap().status,
        CardStatus::Pending
    );

    // A body over the secret route's 16 KiB ceiling is refused.
    let oversized = call(
        &app,
        &owner_token,
        "POST",
        &uri,
        Some(json!({"value": "x".repeat(20 * 1024)})),
    )
    .await;
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        store.get_card(&owner.id, &card.id).unwrap().status,
        CardStatus::Pending
    );

    let saved = call(
        &app,
        &owner_token,
        "POST",
        &uri,
        Some(json!({"value": value})),
    )
    .await;
    assert_eq!(saved.status(), StatusCode::OK);
    let body = read_json(saved).await;
    assert!(!body.to_string().contains(value));
    let task = store
        .get_task(&owner.id, body["task"]["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        task.payload["text"],
        "Secret “vendor-portal” was saved to the vault for vendor.example. Use it with browser_type secret=vendor-portal."
    );
    assert!(!task.payload.to_string().contains(value));
    let stored = store.get_card(&owner.id, &card.id).unwrap();
    assert_eq!(stored.status, CardStatus::Answered);
    assert_eq!(
        stored.response,
        Some(json!({"saved": true, "name": "vendor-portal"}))
    );

    // The entry is in the owner's vault, metadata only.
    let listed = read_json(call(&app, &owner_token, "GET", "/v1/vault", None).await).await;
    assert_eq!(listed["entries"][0]["name"], "vendor-portal");
    assert_eq!(listed["entries"][0]["username"], "ada");
    assert!(!listed.to_string().contains(value));

    // No stored row, event or audit record holds the value.
    let events = store.events_after(&owner.id, 0, 100).unwrap();
    assert!(!serde_json::to_string(&events).unwrap().contains(value));
    let connection = store.connection().unwrap();
    for table in [
        "vault_entries",
        "audit_log",
        "events",
        "chat_cards",
        "tasks",
        "user_credentials",
    ] {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM {table}"))
            .unwrap();
        let columns = statement.column_count();
        let mut rows = statement.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for column in 0..columns {
                if let Ok(text) = row.get::<_, String>(column) {
                    assert!(!text.contains(value), "{table} holds the value");
                }
            }
        }
    }
    let details: String = connection
        .query_row(
            "SELECT details_json FROM audit_log WHERE event = 'vault.saved_via_card'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(details.contains("vendor-portal") && !details.contains(value));
    drop(connection);

    // The card resolves once: a second value is refused and nothing more is posted.
    let again = call(
        &app,
        &owner_token,
        "POST",
        &uri,
        Some(json!({"value": "synthetic-second-value"})),
    )
    .await;
    assert_eq!(again.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(store.pending_task_count(&owner.id).unwrap(), 1);
}

#[tokio::test]
async fn a_provide_card_never_overwrites_and_only_secret_cards_take_values() {
    let (app, _, store) = app();
    let owner = store
        .create_user("subject-secret-c", "sc@example.example", "Synthetic SC")
        .unwrap();
    let token = mint_for(&store, &owner.id, "secret-c");
    store
        .ensure_conversation_accepts_turns(&owner.id, "dir:provide")
        .unwrap();
    let existing = call(
        &app,
        &token,
        "PUT",
        "/v1/vault/vendor-portal",
        Some(json!({"kind": "password", "value": "synthetic-existing", "domains": ["vendor.example"]})),
    )
    .await;
    assert_eq!(existing.status(), StatusCode::OK);
    let mut provide = secret_payload();
    provide["purpose"] = json!("provide");
    let card = store
        .create_card(
            &owner.id,
            "dir:provide",
            "main",
            CardKind::SecretRequest,
            &provide,
        )
        .unwrap();
    let conflict = call(
        &app,
        &token,
        "POST",
        &format!("/v1/cards/{}/secret", card.id),
        Some(json!({"value": "synthetic-replacement"})),
    )
    .await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        store.get_card(&owner.id, &card.id).unwrap().status,
        CardStatus::Pending
    );

    let question = store
        .create_card(
            &owner.id,
            "dir:provide",
            "main",
            CardKind::Question,
            &closed_question(),
        )
        .unwrap();
    let refused = call(
        &app,
        &token,
        "POST",
        &format!("/v1/cards/{}/secret", question.id),
        Some(json!({"value": "synthetic-not-a-secret"})),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_save_card_replaces_an_entry_only_when_it_says_so_and_never_a_password_manager_entry() {
    let (app, _, store) = app();
    let owner = store
        .create_user("subject-secret-d", "sd@example.example", "Synthetic SD")
        .unwrap();
    let token = mint_for(&store, &owner.id, "secret-d");
    store
        .ensure_conversation_accepts_turns(&owner.id, "dir:replace")
        .unwrap();
    let existing = call(
        &app,
        &token,
        "PUT",
        "/v1/vault/vendor-portal",
        Some(json!({"kind": "password", "value": "synthetic-existing", "domains": ["vendor.example"]})),
    )
    .await;
    assert_eq!(existing.status(), StatusCode::OK);
    let external = call(
        &app,
        &token,
        "PUT",
        "/v1/vault/bank-login",
        Some(json!({
            "kind": "password",
            "value": "",
            "domains": ["bank.example"],
            "source": "onepassword",
            "reference": "op://Vault/Bank/password"
        })),
    )
    .await;
    assert_eq!(external.status(), StatusCode::OK);
    let raise = |name: &str, replaces: Option<bool>| {
        let mut payload = secret_payload();
        payload["name"] = json!(name);
        if let Some(replaces) = replaces {
            payload["replaces"] = json!(replaces);
        }
        store
            .create_card(
                &owner.id,
                "dir:replace",
                "main",
                CardKind::SecretRequest,
                &payload,
            )
            .unwrap()
    };

    // A save over an existing name is refused unless the card says it replaces it.
    let plain = raise("vendor-portal", None);
    let refused = call(
        &app,
        &token,
        "POST",
        &format!("/v1/cards/{}/secret", plain.id),
        Some(json!({"value": "synthetic-plain-value"})),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert_eq!(
        store.get_card(&owner.id, &plain.id).unwrap().status,
        CardStatus::Pending
    );

    // A card raised for that name replaces it.
    let replacing = raise("vendor-portal", Some(true));
    let saved = call(
        &app,
        &token,
        "POST",
        &format!("/v1/cards/{}/secret", replacing.id),
        Some(json!({"value": "synthetic-replaced-value"})),
    )
    .await;
    assert_eq!(saved.status(), StatusCode::OK);
    assert_eq!(
        store.get_card(&owner.id, &replacing.id).unwrap().status,
        CardStatus::Answered
    );

    // A password manager entry is never replaced, even by a card that says so.
    let over_external = raise("bank-login", Some(true));
    let refused = call(
        &app,
        &token,
        "POST",
        &format!("/v1/cards/{}/secret", over_external.id),
        Some(json!({"value": "synthetic-external-value"})),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert_eq!(
        store.get_card(&owner.id, &over_external.id).unwrap().status,
        CardStatus::Pending
    );
    let listed = read_json(call(&app, &token, "GET", "/v1/vault", None).await).await;
    let bank = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "bank-login")
        .unwrap();
    assert_eq!(bank["source"], "onepassword");
    assert!(!listed.to_string().contains("synthetic-external-value"));
}
