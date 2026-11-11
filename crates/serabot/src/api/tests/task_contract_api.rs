//! Tasks window contract (v0.35.0): the exact requests the desktop sends, and
//! the answers it must get. A 422 must name the field that failed.

use super::*;

async fn call(app: &Router, token: &str, method: &str, uri: &str, body: Option<Value>) -> Response {
    app.clone()
        .oneshot(request_with_token(method, uri, token, body))
        .await
        .unwrap()
}

#[tokio::test]
async fn the_tasks_window_loads_with_no_calendar_provider_chosen() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let member = store
        .create_user("sub-v035-cal", "v035-cal@example.example", "Cal")
        .unwrap();
    let token = mint_for(&store, &member.id, "v035-calendar");

    // The Tasks view asks for the calendar on open, with the date strings the
    // desktop formats (YYYY-MM-DD). With the default provider this must be an
    // empty calendar, never a rejected request.
    let calendar = call(
        &app,
        &token,
        "GET",
        "/v1/tasks/calendar?from=2026-11-01&to=2026-11-30",
        None,
    )
    .await;
    assert_eq!(calendar.status(), StatusCode::OK);
    let body = read_json(calendar).await;
    assert_eq!(body["events"], json!([]), "{body}");
    assert_eq!(body["provider"], "none", "{body}");

    // The other loads the view makes on open.
    let list = call(&app, &token, "GET", "/v1/tasks?limit=500", None).await;
    assert_eq!(list.status(), StatusCode::OK);
    let prefs = call(&app, &token, "GET", "/v1/tasks/prefs", None).await;
    assert_eq!(prefs.status(), StatusCode::OK);
    let sync = call(&app, &token, "GET", "/v1/tasks/sync/status", None).await;
    assert_eq!(sync.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_desktop_create_and_status_payloads_are_accepted() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let member = store
        .create_user("sub-v035-create", "v035-create@example.example", "Create")
        .unwrap();
    let token = mint_for(&store, &member.id, "v035-create");

    // The exact body the desktop sends for "Add" with a date.
    let created = call(
        &app,
        &token,
        "POST",
        "/v1/tasks",
        Some(json!({"title": "Pay invoice", "due": "2026-11-12"})),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let task = read_json(created).await;
    let id = task["task"]["id"].as_str().unwrap().to_owned();

    // The exact body the desktop sends when a row is cycled.
    let moved = call(
        &app,
        &token,
        "PATCH",
        &format!("/v1/tasks/{id}"),
        Some(json!({"status": "doing"})),
    )
    .await;
    assert_eq!(moved.status(), StatusCode::OK);

    // A task without a date is also a plain create.
    let undated = call(
        &app,
        &token,
        "POST",
        "/v1/tasks",
        Some(json!({"title": "Water the plants"})),
    )
    .await;
    assert_eq!(undated.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_rejected_field_is_named_in_the_422_message() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let member = store
        .create_user("sub-v035-422", "v035-422@example.example", "Four")
        .unwrap();
    let token = mint_for(&store, &member.id, "v035-422");

    // A date typed in another format: the server names `due`.
    let bad_due = call(
        &app,
        &token,
        "POST",
        "/v1/tasks",
        Some(json!({"title": "Pay invoice", "due": "12/11/2026"})),
    )
    .await;
    assert_eq!(bad_due.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_json(bad_due).await;
    assert_eq!(
        body["error"], "due must be an RFC 3339 timestamp or date",
        "{body}"
    );

    // A status outside the three columns: the server names `status`.
    let created = read_json(
        call(
            &app,
            &token,
            "POST",
            "/v1/tasks",
            Some(json!({"title": "Plan trip"})),
        )
        .await,
    )
    .await;
    let id = created["task"]["id"].as_str().unwrap().to_owned();
    let bad_status = call(
        &app,
        &token,
        "PATCH",
        &format!("/v1/tasks/{id}"),
        Some(json!({"status": "later"})),
    )
    .await;
    assert_eq!(bad_status.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_json(bad_status).await;
    assert_eq!(
        body["error"], "status must be todo, doing or done",
        "{body}"
    );

    // An empty title: the server names `title`, and does not echo the value.
    let empty_title = call(
        &app,
        &token,
        "POST",
        "/v1/tasks",
        Some(json!({"title": "   ", "due": null})),
    )
    .await;
    assert_eq!(empty_title.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_json(empty_title).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .starts_with("title must be "),
        "{body}"
    );
}

#[tokio::test]
async fn the_provider_choice_is_accepted_and_bad_choices_name_the_field() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let member = store
        .create_user("sub-v035-prov", "v035-prov@example.example", "Prov")
        .unwrap();
    let token = mint_for(&store, &member.id, "v035-prov");

    let ok = call(
        &app,
        &token,
        "PUT",
        "/v1/tasks/prefs",
        Some(json!({"sync_provider": "none"})),
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);

    let bad = call(
        &app,
        &token,
        "PUT",
        "/v1/tasks/prefs",
        Some(json!({"sync_provider": "dropbox"})),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_json(bad).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .starts_with("sync_provider must be "),
        "{body}"
    );
}
