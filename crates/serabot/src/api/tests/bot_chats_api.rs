//! Several chats per bot (v0.35.0): list, create with a carried summary,
//! rename and archive through the existing conversation routes, and the
//! per-bot refresh interval. Synthetic data only.

use super::*;

async fn call(app: &Router, token: &str, method: &str, uri: &str, body: Option<Value>) -> Response {
    app.clone()
        .oneshot(request_with_token(method, uri, token, body))
        .await
        .unwrap()
}

async fn member_with_analyst(app: &Router, store: &Store, subject: &str) -> (String, String) {
    let user = store
        .create_user(subject, &format!("{subject}@example.example"), subject)
        .unwrap();
    let token = mint_for(store, &user.id, subject);
    let created = call(
        app,
        &token,
        "POST",
        "/v1/bots",
        Some(json!({"name": "Analyst"})),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    (user.id, token)
}

#[tokio::test]
async fn a_bot_lists_its_primary_chat_with_the_default_interval() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let (_, token) = member_with_analyst(&app, &store, "v035-list").await;

    let listed = read_json(call(&app, &token, "GET", "/v1/bots/analyst/chats", None).await).await;
    assert_eq!(listed["refresh_every_turns"], 40, "{listed}");
    assert_eq!(listed["chats"][0]["id"], "dir:analyst", "{listed}");
    assert_eq!(listed["chats"][0]["primary"], true, "{listed}");
    assert_eq!(listed["chats"][0]["turns"], 0, "{listed}");
}

#[tokio::test]
async fn a_new_chat_is_bound_to_the_bot_and_can_be_renamed_and_archived() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let (user_id, token) = member_with_analyst(&app, &store, "v035-chat").await;

    let created = call(
        &app,
        &token,
        "POST",
        "/v1/bots/analyst/chats",
        Some(json!({"title": "Quarterly review"})),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let body = read_json(created).await;
    let id = body["chat"]["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("dir:chat-"), "{body}");
    assert_eq!(body["chat"]["primary"], false, "{body}");
    assert_eq!(body["chat"]["title"], "Quarterly review", "{body}");
    assert!(body["carried_from"].is_null(), "{body}");
    assert_eq!(
        store.bot_for_chat(&user_id, &id).unwrap().as_deref(),
        Some("analyst")
    );

    let listed = read_json(call(&app, &token, "GET", "/v1/bots/analyst/chats", None).await).await;
    assert_eq!(listed["chats"].as_array().unwrap().len(), 2, "{listed}");

    // Rename and archive use the existing conversation routes.
    let renamed = call(
        &app,
        &token,
        "PATCH",
        &format!("/v1/conversations/{id}"),
        Some(json!({"title": "Budget review"})),
    )
    .await;
    assert_eq!(renamed.status(), StatusCode::OK);
    let archived = call(
        &app,
        &token,
        "DELETE",
        &format!("/v1/conversations/{id}"),
        None,
    )
    .await;
    assert_eq!(archived.status(), StatusCode::OK);

    let hidden = read_json(call(&app, &token, "GET", "/v1/bots/analyst/chats", None).await).await;
    assert_eq!(hidden["chats"].as_array().unwrap().len(), 1, "{hidden}");
    let shown = read_json(
        call(
            &app,
            &token,
            "GET",
            "/v1/bots/analyst/chats?include_archived=true",
            None,
        )
        .await,
    )
    .await;
    let row = shown["chats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id.as_str())
        .unwrap();
    assert_eq!(row["archived"], true, "{shown}");
    assert_eq!(row["title"], "Budget review", "{shown}");
}

#[tokio::test]
async fn a_new_chat_starts_with_the_bots_latest_summary() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let (user_id, token) = member_with_analyst(&app, &store, "v035-carry").await;
    store
        .create_conversation(&user_id, "dir:analyst", "direct", None)
        .unwrap();
    crate::memory::store_summary(
        &store,
        &user_id,
        "dir:analyst",
        Some("analyst"),
        "The owner is preparing the Q3 budget and prefers short tables.",
        None,
    )
    .unwrap();

    let created = read_json(
        call(
            &app,
            &token,
            "POST",
            "/v1/bots/analyst/chats",
            Some(json!({})),
        )
        .await,
    )
    .await;
    assert_eq!(created["carried_from"], "dir:analyst", "{created}");
    assert_eq!(created["chat"]["has_summary"], true, "{created}");
    let id = created["chat"]["id"].as_str().unwrap();
    let carried = crate::memory::latest_summary(&store, &user_id, id)
        .unwrap()
        .unwrap();
    assert_eq!(
        carried.summary,
        "The owner is preparing the Q3 budget and prefers short tables."
    );
}

#[tokio::test]
async fn the_refresh_interval_is_per_bot_and_bounded() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let (_, token) = member_with_analyst(&app, &store, "v035-every").await;

    let set = call(
        &app,
        &token,
        "PUT",
        "/v1/bots/analyst/chat-settings",
        Some(json!({"refresh_every_turns": 12})),
    )
    .await;
    assert_eq!(set.status(), StatusCode::OK);
    let listed = read_json(call(&app, &token, "GET", "/v1/bots/analyst/chats", None).await).await;
    assert_eq!(listed["refresh_every_turns"], 12, "{listed}");

    let too_small = call(
        &app,
        &token,
        "PUT",
        "/v1/bots/analyst/chat-settings",
        Some(json!({"refresh_every_turns": 4})),
    )
    .await;
    assert_eq!(too_small.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_json(too_small).await;
    assert_eq!(
        body["error"], "refresh_every_turns must be 5 to 500",
        "{body}"
    );
}

#[tokio::test]
async fn unknown_bots_and_unknown_fields_are_refused() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let (_, token) = member_with_analyst(&app, &store, "v035-refuse").await;

    let missing = call(&app, &token, "GET", "/v1/bots/nobody/chats", None).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let extra = call(
        &app,
        &token,
        "POST",
        "/v1/bots/analyst/chats",
        Some(json!({"title": "x", "owner": "someone-else"})),
    )
    .await;
    assert_eq!(extra.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
