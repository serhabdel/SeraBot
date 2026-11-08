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
