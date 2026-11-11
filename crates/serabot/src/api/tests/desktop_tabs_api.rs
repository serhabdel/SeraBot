//! v0.34.0 desktop workflows: run, delete and re-schedule a routine; edit or
//! disable the caller's own feed; the browser open route. Synthetic data only.

use super::*;

const ADMIN_FEED: &str = "https://example.invalid/admin-feed?key=v034-admin-key";

fn base_with_admin_source() -> std::path::PathBuf {
    let base = temp_base();
    std::fs::create_dir_all(base.join("config")).unwrap();
    std::fs::write(
        base.join("config").join("sources.toml"),
        format!(
            "[[sources]]\nname = \"v034-admin-source\"\nurl = \"{ADMIN_FEED}\"\nenabled = true\n"
        ),
    )
    .unwrap();
    base
}

async fn call(app: &Router, token: &str, method: &str, uri: &str, body: Option<Value>) -> Response {
    app.clone()
        .oneshot(request_with_token(method, uri, token, body))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_routine_can_run_now_be_deleted_and_be_scheduled_again() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let member = store
        .create_user("sub-v034-r", "v034-r@example.example", "R")
        .unwrap();
    let token = mint_for(&store, &member.id, "v034-routine");
    crate::scheduler::ensure_daily_routine(
        &store,
        &member.id,
        "morning_brief",
        "08:00",
        chrono_tz::Europe::Rome,
    )
    .unwrap();

    let run = call(&app, &token, "POST", "/v1/routines/morning_brief/run", None).await;
    assert_eq!(run.status(), StatusCode::OK);
    let body = read_json(run).await;
    assert_eq!(body["kind"], "brief", "{body}");
    assert!(body["task_id"].is_string(), "{body}");

    let unknown = call(&app, &token, "POST", "/v1/routines/not_a_routine/run", None).await;
    assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let unscheduled = call(&app, &token, "POST", "/v1/routines/watch_0/run", None).await;
    assert_eq!(unscheduled.status(), StatusCode::NOT_FOUND);

    let deleted = call(&app, &token, "DELETE", "/v1/routines/morning_brief", None).await;
    assert_eq!(deleted.status(), StatusCode::OK);
    let listed = read_json(call(&app, &token, "GET", "/v1/routines", None).await).await;
    assert!(
        listed["routines"].as_array().unwrap().is_empty(),
        "{listed}"
    );
    let again = call(&app, &token, "DELETE", "/v1/routines/morning_brief", None).await;
    assert_eq!(again.status(), StatusCode::NOT_FOUND);

    let put = call(
        &app,
        &token,
        "PUT",
        "/v1/routines/morning_brief",
        Some(json!({"enabled": true, "time": "07:30"})),
    )
    .await;
    assert_eq!(put.status(), StatusCode::OK);
    let relisted = read_json(call(&app, &token, "GET", "/v1/routines", None).await).await;
    assert_eq!(
        relisted["routines"][0]["name"], "morning_brief",
        "{relisted}"
    );
}

#[tokio::test]
async fn a_member_edits_and_disables_only_their_own_feed() {
    let (app, _, store) = app_with_root(true, Policy::empty(), base_with_admin_source());
    let member = store
        .create_user("sub-v034-f", "v034-f@example.example", "F")
        .unwrap();
    let token = mint_for(&store, &member.id, "v034-feed");
    store
        .add_member_rss_source(&member.id, "v034-feed", "https://example.invalid/feed")
        .unwrap();

    let disabled = call(
        &app,
        &token,
        "PATCH",
        "/v1/sources/v034-feed",
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(disabled.status(), StatusCode::OK);
    let sources = read_json(call(&app, &token, "GET", "/v1/sources", None).await).await;
    let row = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "v034-feed")
        .unwrap();
    assert_eq!(row["enabled"], false, "{sources}");

    // A new URL must pass the same host allowlist as adding a feed.
    let off_list = call(
        &app,
        &token,
        "PATCH",
        "/v1/sources/v034-feed",
        Some(json!({"url": "https://evil.example/feed"})),
    )
    .await;
    assert_eq!(off_list.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Admin feeds: enable and disable only, never a URL change.
    let admin_url = call(
        &app,
        &token,
        "PATCH",
        "/v1/sources/v034-admin-source",
        Some(json!({"url": "https://example.invalid/other"})),
    )
    .await;
    assert_eq!(admin_url.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let admin_toggle = call(
        &app,
        &token,
        "PATCH",
        "/v1/sources/v034-admin-source",
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(admin_toggle.status(), StatusCode::OK);

    let other = store
        .create_user("sub-v034-g", "v034-g@example.example", "G")
        .unwrap();
    let other_token = mint_for(&store, &other.id, "v034-other");
    let not_mine = call(
        &app,
        &other_token,
        "PATCH",
        "/v1/sources/v034-feed",
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(not_mine.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_browser_open_route_refuses_an_unknown_profile() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let member = store
        .create_user("sub-v034-b", "v034-b@example.example", "B")
        .unwrap();
    let token = mint_for(&store, &member.id, "v034-browser");
    let response = call(
        &app,
        &token,
        "POST",
        "/v1/browser/sessions/not-a-profile/open",
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
