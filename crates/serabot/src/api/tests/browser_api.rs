//! Owner browser API: auth, per-user isolation and the controller flow, with a
//! fake session backend (no Chromium).

use std::time::Duration;

use super::*;
use crate::browser::testing::{FakeLauncher, manager_with_base};
use crate::browser::{BrowserManager, Controller};

struct Rig {
    app: Router,
    manager: Arc<BrowserManager>,
    launcher: Arc<FakeLauncher>,
    admin_id: String,
    member_id: String,
    member_token: String,
    base: std::path::PathBuf,
}

fn rig(with_browser: bool) -> Rig {
    let base = std::env::temp_dir().join(format!("serabot-bapi-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&base).unwrap();
    let (manager, launcher) = manager_with_base(4, Duration::from_secs(600), base.clone());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .unwrap();
    let (app, _, store) = app_with_stack(
        true,
        Policy::empty(),
        root,
        vec![
            test_bot("main", crate::orchestration::BotRole::Orchestrator),
            test_bot("notes", crate::orchestration::BotRole::Specialist),
        ],
        None,
        None,
        with_browser.then(|| manager.clone()),
        None,
    );
    let (member, member_token) = member_with_session(&store);
    Rig {
        app,
        manager,
        launcher,
        admin_id: admin(&store),
        member_id: member.id,
        member_token,
        base,
    }
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Vec<u8>, HeaderMap) {
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, bytes, headers)
}

fn admin_req(app: &Router, method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    request(app, method, uri, true, body)
}

fn member_req(rig: &Rig, method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    request_with_token(method, uri, &rig.member_token, body)
}

#[tokio::test]
async fn every_route_requires_auth() {
    let rig = rig(true);
    for (method, uri) in [
        ("GET", "/v1/browser"),
        ("GET", "/v1/browser/screenshot"),
        ("POST", "/v1/browser/takeover"),
        ("POST", "/v1/browser/handback"),
        ("POST", "/v1/browser/input"),
        ("POST", "/v1/browser/close"),
    ] {
        let (status, _, _) = send(&rig.app, request(&rig.app, method, uri, false, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn status_is_inactive_without_a_session_or_when_disabled() {
    for enabled in [true, false] {
        let rig = rig(enabled);
        let (status, body, _) =
            send(&rig.app, admin_req(&rig.app, "GET", "/v1/browser", None)).await;
        assert_eq!(status, StatusCode::OK);
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["active"], false);
        assert_eq!(value["controller"], "bot");
        assert!(value.get("url").is_none());
        let (status, _, _) = send(
            &rig.app,
            admin_req(&rig.app, "GET", "/v1/browser/screenshot", None),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn sessions_and_screenshots_are_isolated_per_user() {
    let rig = rig(true);
    drop(
        rig.manager
            .bot_lease(&rig.admin_id, "default", true, None)
            .await
            .unwrap(),
    );
    // The member has no session even though the admin does.
    let (status, body, _) = send(&rig.app, member_req(&rig, "GET", "/v1/browser", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["active"],
        false
    );
    let (status, _, _) = send(
        &rig.app,
        member_req(&rig, "GET", "/v1/browser/screenshot", None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    drop(
        rig.manager
            .bot_lease(&rig.member_id, "default", true, None)
            .await
            .unwrap(),
    );
    let (status, admin_shot, headers) = send(
        &rig.app,
        admin_req(&rig.app, "GET", "/v1/browser/screenshot", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
    assert_eq!(&admin_shot[..2], &[0xff, 0xd8]);
    assert!(admin_shot.len() <= 512 * 1024);
    let (_, member_shot, _) = send(
        &rig.app,
        member_req(&rig, "GET", "/v1/browser/screenshot", None),
    )
    .await;
    assert!(admin_shot.ends_with(rig.admin_id.as_bytes()));
    assert!(member_shot.ends_with(rig.member_id.as_bytes()));

    // Member takeover does not touch the admin's session.
    let (status, body, _) = send(
        &rig.app,
        member_req(&rig, "POST", "/v1/browser/takeover", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["controller"],
        "user"
    );
    assert_eq!(
        rig.manager.info(&rig.admin_id, "default").await.controller,
        Controller::Bot
    );
    // Member closing leaves the admin's session open.
    let (_, body, _) = send(
        &rig.app,
        member_req(&rig, "POST", "/v1/browser/close", None),
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["closed"],
        true
    );
    assert!(rig.manager.info(&rig.admin_id, "default").await.active);
    assert!(!rig.manager.info(&rig.member_id, "default").await.active);
}

#[tokio::test]
async fn takeover_input_handback_flow() {
    let rig = rig(true);
    // Nothing to take over yet.
    let (status, _, _) = send(
        &rig.app,
        admin_req(&rig.app, "POST", "/v1/browser/takeover", None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    drop(
        rig.manager
            .bot_lease(&rig.admin_id, "default", true, None)
            .await
            .unwrap(),
    );

    // Input is refused while the bot is in control.
    let click = json!({"type": "click", "x": 10, "y": 20});
    let (status, _, _) = send(
        &rig.app,
        admin_req(&rig.app, "POST", "/v1/browser/input", Some(click.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _, _) = send(
        &rig.app,
        admin_req(&rig.app, "POST", "/v1/browser/takeover", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for body in [
        click,
        json!({"type": "text", "text": "hello"}),
        json!({"type": "key", "key": "Enter"}),
        json!({"type": "scroll", "dy": 120}),
        json!({"type": "navigate", "url": "https://93.184.216.34/"}),
    ] {
        let (status, _, _) = send(
            &rig.app,
            admin_req(&rig.app, "POST", "/v1/browser/input", Some(body.clone())),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let calls = rig.launcher.calls_for(&rig.admin_id);
    for expected in [
        "click_at 10 20",
        "insert hello",
        "press Enter",
        "scroll 120",
        "navigate https://93.184.216.34/",
    ] {
        assert!(
            calls.contains(&expected.to_string()),
            "{expected}: {calls:?}"
        );
    }

    // Invalid inputs are rejected before reaching the browser.
    let before = rig.launcher.calls_for(&rig.admin_id).len();
    for body in [
        json!({"type": "click", "x": -1, "y": 0}),
        json!({"type": "click", "x": 1, "y": 99999}),
        json!({"type": "text", "text": ""}),
        json!({"type": "key", "key": "Control+Alt+Delete"}),
        json!({"type": "scroll", "dy": 1e9}),
        json!({"type": "navigate", "url": "http://169.254.169.254/latest"}),
        json!({"type": "navigate", "url": "http://localhost:9222/"}),
        json!({"type": "navigate", "url": "file:///etc/passwd"}),
        json!({"type": "warp"}),
        json!({"type": "click", "x": 1, "y": 1, "extra": true}),
    ] {
        let (status, _, _) = send(
            &rig.app,
            admin_req(&rig.app, "POST", "/v1/browser/input", Some(body.clone())),
        )
        .await;
        assert!(
            status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST,
            "{body}: {status}"
        );
    }
    assert_eq!(rig.launcher.calls_for(&rig.admin_id).len(), before);

    // The bot is locked out until hand back.
    assert!(
        rig.manager
            .bot_lease(&rig.admin_id, "default", false, None)
            .await
            .is_err()
    );
    let (status, body, _) = send(
        &rig.app,
        admin_req(&rig.app, "POST", "/v1/browser/handback", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["controller"],
        "bot"
    );
    assert!(
        rig.manager
            .bot_lease(&rig.admin_id, "default", false, None)
            .await
            .is_ok()
    );
    let (status, _, _) = send(
        &rig.app,
        admin_req(
            &rig.app,
            "POST",
            "/v1/browser/input",
            Some(json!({"type": "key", "key": "Tab"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn mutating_routes_report_not_found_when_disabled() {
    let rig = rig(false);
    for uri in [
        "/v1/browser/takeover",
        "/v1/browser/handback",
        "/v1/browser/close",
    ] {
        let (status, _, _) = send(&rig.app, admin_req(&rig.app, "POST", uri, None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}

// ---- 0.16: profiles, per-profile sessions, aliases, bot settings ----

fn json_of(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let (status, body, _) = send(app, req).await;
    (status, json_of(&body))
}

fn profile_ids(list: &Value) -> Vec<String> {
    list["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn new_routes_require_auth() {
    let rig = rig(true);
    for (method, uri) in [
        ("GET", "/v1/browser/sessions"),
        ("GET", "/v1/browser/sessions/default/screenshot"),
        ("POST", "/v1/browser/sessions/default/takeover"),
        ("POST", "/v1/browser/sessions/default/handback"),
        ("POST", "/v1/browser/sessions/default/input"),
        ("POST", "/v1/browser/sessions/default/close"),
        ("GET", "/v1/browser/profiles"),
        ("POST", "/v1/browser/profiles"),
        ("PUT", "/v1/browser/profiles/default"),
        ("DELETE", "/v1/browser/profiles/work"),
    ] {
        let (status, _, _) = send(&rig.app, request(&rig.app, method, uri, false, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn profiles_crud_validation_and_per_user_isolation() {
    let rig = rig(true);
    let (status, list) = call(
        &rig.app,
        admin_req(&rig.app, "GET", "/v1/browser/profiles", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let default = &list["profiles"][0];
    assert_eq!(default["id"], "default");
    assert_eq!(default["is_default"], true);
    // Bots without an assignment count toward the default profile.
    assert_eq!(default["bots"], json!(["main", "notes"]));
    assert!(default["created_at"].is_string());

    let (status, created) = call(
        &rig.app,
        admin_req(
            &rig.app,
            "POST",
            "/v1/browser/profiles",
            Some(json!({"name": "Work Login"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(created["id"], "work-login");
    assert_eq!(created["name"], "Work Login");
    assert_eq!(created["is_default"], false);
    assert_eq!(created["bots"], json!([]));
    assert!(created.get("last_used_at").is_none());

    // The member sees only their own default; the admin's profile is invisible.
    let (_, member_list) = call(
        &rig.app,
        member_req(&rig, "GET", "/v1/browser/profiles", None),
    )
    .await;
    assert_eq!(profile_ids(&member_list), vec!["default"]);
    for (method, uri, body) in [
        (
            "PUT",
            "/v1/browser/profiles/work-login",
            Some(json!({"name": "stolen"})),
        ),
        ("DELETE", "/v1/browser/profiles/work-login", None),
    ] {
        let (status, _) = call(&rig.app, member_req(&rig, method, uri, body)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
    }
    let (_, admin_list) = call(
        &rig.app,
        admin_req(&rig.app, "GET", "/v1/browser/profiles", None),
    )
    .await;
    assert_eq!(profile_ids(&admin_list), vec!["default", "work-login"]);
    assert_eq!(admin_list["profiles"][1]["name"], "Work Login");

    // Rename keeps the id.
    let (status, renamed) = call(
        &rig.app,
        admin_req(
            &rig.app,
            "PUT",
            "/v1/browser/profiles/work-login",
            Some(json!({"name": "Office"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renamed["id"], "work-login");
    assert_eq!(renamed["name"], "Office");

    // Validation: empty, too long, unknown fields; malformed ids are 404.
    for body in [
        json!({"name": "  "}),
        json!({"name": "x".repeat(49)}),
        json!({"name": "a", "id": "b"}),
    ] {
        let (status, _) = call(
            &rig.app,
            admin_req(&rig.app, "POST", "/v1/browser/profiles", Some(body)),
        )
        .await;
        assert!(
            status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST,
            "{status}"
        );
    }
    let (status, _) = call(
        &rig.app,
        admin_req(&rig.app, "DELETE", "/v1/browser/profiles/NOPE", None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // At most eight, `default` included.
    for n in 0..6 {
        let (status, _) = call(
            &rig.app,
            admin_req(
                &rig.app,
                "POST",
                "/v1/browser/profiles",
                Some(json!({"name": format!("extra {n}")})),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, _) = call(
        &rig.app,
        admin_req(
            &rig.app,
            "POST",
            "/v1/browser/profiles",
            Some(json!({"name": "one too many"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    // The member's quota is their own.
    let (status, _) = call(
        &rig.app,
        member_req(
            &rig,
            "POST",
            "/v1/browser/profiles",
            Some(json!({"name": "mine"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn sessions_list_and_per_profile_routes_are_isolated() {
    let rig = rig(true);
    call(
        &rig.app,
        admin_req(
            &rig.app,
            "POST",
            "/v1/browser/profiles",
            Some(json!({"name": "Work"})),
        ),
    )
    .await;
    let driver = crate::browser::Driver {
        bot: "notes",
        conversation_id: Some("conv-1"),
    };
    drop(
        rig.manager
            .bot_lease(&rig.admin_id, "work", true, Some(driver))
            .await
            .unwrap(),
    );

    let (status, list) = call(
        &rig.app,
        admin_req(&rig.app, "GET", "/v1/browser/sessions", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sessions = list["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0]["id"], "default");
    assert_eq!(sessions[0]["profile_name"], "Default");
    assert_eq!(sessions[0]["active"], false);
    assert_eq!(sessions[0]["controller"], "bot");
    assert!(sessions[0]["updated_at"].is_string());
    assert_eq!(sessions[1]["id"], "work");
    assert_eq!(sessions[1]["profile_name"], "Work");
    assert_eq!(sessions[1]["active"], true);
    assert_eq!(sessions[1]["controller"], "bot");
    assert_eq!(sessions[1]["bot"], "notes");
    assert_eq!(sessions[1]["conversation_id"], "conv-1");
    assert_eq!(sessions[1]["url"], "about:blank");
    assert!(sessions[1]["title"].is_string());

    // The member's list has no trace of the admin's profile or session.
    let (_, member_list) = call(
        &rig.app,
        member_req(&rig, "GET", "/v1/browser/sessions", None),
    )
    .await;
    let member_sessions = member_list["sessions"].as_array().unwrap();
    assert_eq!(member_sessions.len(), 1);
    assert_eq!(member_sessions[0]["id"], "default");
    assert_eq!(member_sessions[0]["active"], false);
    for (method, uri) in [
        ("GET", "/v1/browser/sessions/work/screenshot"),
        ("POST", "/v1/browser/sessions/work/takeover"),
        ("POST", "/v1/browser/sessions/work/handback"),
        ("POST", "/v1/browser/sessions/work/close"),
    ] {
        let (status, _) = call(&rig.app, member_req(&rig, method, uri, None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
    }
    // Even with a same-named profile of their own the member has no session,
    // and closing theirs does not touch the admin's.
    call(
        &rig.app,
        member_req(
            &rig,
            "POST",
            "/v1/browser/profiles",
            Some(json!({"name": "Work"})),
        ),
    )
    .await;
    let (status, _) = call(
        &rig.app,
        member_req(&rig, "GET", "/v1/browser/sessions/work/screenshot", None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, closed) = call(
        &rig.app,
        member_req(&rig, "POST", "/v1/browser/sessions/work/close", None),
    )
    .await;
    assert_eq!(
        (status, closed["closed"].as_bool()),
        (StatusCode::OK, Some(false))
    );
    assert!(rig.manager.info(&rig.admin_id, "work").await.active);

    // The work screenshot is the work Chrome, not the default one.
    drop(
        rig.manager
            .bot_lease(&rig.admin_id, "default", true, None)
            .await
            .unwrap(),
    );
    let (status, shot, headers) = send(
        &rig.app,
        admin_req(
            &rig.app,
            "GET",
            "/v1/browser/sessions/work/screenshot",
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
    assert!(shot.ends_with(format!("{}/work", rig.admin_id).as_bytes()));
    let (_, default_shot, _) = send(
        &rig.app,
        admin_req(
            &rig.app,
            "GET",
            "/v1/browser/sessions/default/screenshot",
            None,
        ),
    )
    .await;
    assert!(default_shot.ends_with(rig.admin_id.as_bytes()));

    // Takeover, input and handback on one profile leave the other alone.
    let (status, _) = call(
        &rig.app,
        admin_req(
            &rig.app,
            "POST",
            "/v1/browser/sessions/work/input",
            Some(json!({"type": "key", "key": "Tab"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, info) = call(
        &rig.app,
        admin_req(&rig.app, "POST", "/v1/browser/sessions/work/takeover", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["controller"], "user");
    assert_eq!(
        rig.manager.info(&rig.admin_id, "default").await.controller,
        Controller::Bot
    );
    let (status, _) = call(
        &rig.app,
        admin_req(
            &rig.app,
            "POST",
            "/v1/browser/sessions/work/input",
            Some(json!({"type": "click", "x": 5, "y": 6})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        rig.launcher
            .calls_for_profile(&rig.admin_id, "work")
            .contains(&"click_at 5 6".to_owned())
    );
    assert!(
        !rig.launcher
            .calls_for_profile(&rig.admin_id, "default")
            .contains(&"click_at 5 6".to_owned())
    );
    let (_, list) = call(
        &rig.app,
        admin_req(&rig.app, "GET", "/v1/browser/sessions", None),
    )
    .await;
    assert_eq!(list["sessions"][1]["controller"], "user");
    let (status, info) = call(
        &rig.app,
        admin_req(&rig.app, "POST", "/v1/browser/sessions/work/handback", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["controller"], "bot");
    let (status, closed) = call(
        &rig.app,
        admin_req(&rig.app, "POST", "/v1/browser/sessions/work/close", None),
    )
    .await;
    assert_eq!(
        (status, closed["closed"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    assert!(!rig.manager.info(&rig.admin_id, "work").await.active);
    assert!(rig.manager.info(&rig.admin_id, "default").await.active);
}

#[tokio::test]
async fn legacy_routes_are_default_profile_aliases() {
    let rig = rig(true);
    call(
        &rig.app,
        admin_req(
            &rig.app,
            "POST",
            "/v1/browser/profiles",
            Some(json!({"name": "Work"})),
        ),
    )
    .await;
    drop(
        rig.manager
            .bot_lease(&rig.admin_id, "default", true, None)
            .await
            .unwrap(),
    );
    drop(
        rig.manager
            .bot_lease(&rig.admin_id, "work", true, None)
            .await
            .unwrap(),
    );

    let (_, status_body) = call(&rig.app, admin_req(&rig.app, "GET", "/v1/browser", None)).await;
    assert_eq!(status_body["active"], true);
    let (_, shot, _) = send(
        &rig.app,
        admin_req(&rig.app, "GET", "/v1/browser/screenshot", None),
    )
    .await;
    assert!(
        shot.ends_with(rig.admin_id.as_bytes()),
        "default Chrome, not work"
    );
    let (status, info) = call(
        &rig.app,
        admin_req(&rig.app, "POST", "/v1/browser/takeover", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["controller"], "user");
    assert_eq!(
        rig.manager.info(&rig.admin_id, "default").await.controller,
        Controller::User
    );
    assert_eq!(
        rig.manager.info(&rig.admin_id, "work").await.controller,
        Controller::Bot
    );
    let (status, _) = call(
        &rig.app,
        admin_req(
            &rig.app,
            "POST",
            "/v1/browser/input",
            Some(json!({"type": "scroll", "dy": 10})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        rig.launcher
            .calls_for_profile(&rig.admin_id, "default")
            .contains(&"scroll 10".to_owned())
    );
    assert!(
        rig.launcher
            .calls_for_profile(&rig.admin_id, "work")
            .is_empty()
    );
    let (status, info) = call(
        &rig.app,
        admin_req(&rig.app, "POST", "/v1/browser/handback", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["controller"], "bot");
    let (_, closed) = call(
        &rig.app,
        admin_req(&rig.app, "POST", "/v1/browser/close", None),
    )
    .await;
    assert_eq!(closed["closed"], true);
    assert!(!rig.manager.info(&rig.admin_id, "default").await.active);
    assert!(rig.manager.info(&rig.admin_id, "work").await.active);
    // The status alias of an inactive default is inactive even while work runs.
    let (_, status_body) = call(&rig.app, admin_req(&rig.app, "GET", "/v1/browser", None)).await;
    assert_eq!(status_body["active"], false);
}

#[tokio::test]
async fn deleting_a_profile_closes_it_removes_data_and_reassigns_bots() {
    let rig = rig(true);
    call(
        &rig.app,
        admin_req(
            &rig.app,
            "POST",
            "/v1/browser/profiles",
            Some(json!({"name": "Work"})),
        ),
    )
    .await;
    call(
        &rig.app,
        member_req(
            &rig,
            "POST",
            "/v1/browser/profiles",
            Some(json!({"name": "Work"})),
        ),
    )
    .await;
    let (status, put) = call(
        &rig.app,
        admin_req(
            &rig.app,
            "PUT",
            "/v1/bots/notes",
            Some(json!({"browser_profile": "work"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{put}");
    assert_eq!(put["bot"]["browser_profile"], "work");
    let (_, detail) = call(&rig.app, admin_req(&rig.app, "GET", "/v1/bots/notes", None)).await;
    assert_eq!(detail["browser_profile"], "work");
    let (_, list) = call(
        &rig.app,
        admin_req(&rig.app, "GET", "/v1/browser/profiles", None),
    )
    .await;
    assert_eq!(list["profiles"][0]["bots"], json!(["main"]));
    assert_eq!(list["profiles"][1]["bots"], json!(["notes"]));

    let mine = rig.base.join(&rig.admin_id).join("work");
    let default_dir = rig.base.join(&rig.admin_id).join("default");
    let members = rig.base.join(&rig.member_id).join("work");
    for dir in [&mine, &default_dir, &members] {
        std::fs::create_dir_all(dir.join("Default")).unwrap();
        std::fs::write(dir.join("Default").join("Cookies"), b"x").unwrap();
    }
    drop(
        rig.manager
            .bot_lease(&rig.admin_id, "work", true, None)
            .await
            .unwrap(),
    );

    let (status, deleted) = call(
        &rig.app,
        admin_req(&rig.app, "DELETE", "/v1/browser/profiles/work", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        deleted,
        json!({"deleted": true, "id": "work", "reassigned_bots": ["notes"]})
    );
    assert!(!mine.exists(), "profile directory removed");
    assert!(default_dir.exists(), "other profiles untouched");
    assert!(
        members.exists(),
        "another user's same-named profile untouched"
    );
    assert!(
        !rig.manager.info(&rig.admin_id, "work").await.active,
        "session closed"
    );
    let (_, detail) = call(&rig.app, admin_req(&rig.app, "GET", "/v1/bots/notes", None)).await;
    assert_eq!(detail["browser_profile"], "default");
    let (_, list) = call(
        &rig.app,
        admin_req(&rig.app, "GET", "/v1/browser/profiles", None),
    )
    .await;
    assert_eq!(profile_ids(&list), vec!["default"]);
    assert_eq!(list["profiles"][0]["bots"], json!(["main", "notes"]));
    // The member's profile is still there.
    let (_, member_list) = call(
        &rig.app,
        member_req(&rig, "GET", "/v1/browser/profiles", None),
    )
    .await;
    assert_eq!(profile_ids(&member_list), vec!["default", "work"]);

    // `default` can never be deleted and its data stays.
    let (status, _) = call(
        &rig.app,
        admin_req(&rig.app, "DELETE", "/v1/browser/profiles/default", None),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(default_dir.exists());
    let (status, _) = call(
        &rig.app,
        admin_req(&rig.app, "DELETE", "/v1/browser/profiles/work", None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn bot_settings_are_validated_stored_and_returned() {
    let rig = rig(true);
    let (status, put) = call(
        &rig.app,
        admin_req(
            &rig.app,
            "PUT",
            "/v1/bots/notes",
            Some(json!({
                "display_name": "  Scout  ",
                "description": "Finds things.",
                "model": {"provider": "default", "model": "test-alternative"},
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{put}");
    assert_eq!(put["bot"]["display_name"], "Scout");
    assert_eq!(put["bot"]["description"], "Finds things.");
    assert_eq!(put["bot"]["model"], "test-alternative");
    assert_eq!(put["bot"]["browser_profile"], "default");
    let (_, detail) = call(&rig.app, admin_req(&rig.app, "GET", "/v1/bots/notes", None)).await;
    assert_eq!(detail["display_name"], "Scout");
    assert_eq!(detail["description"], "Finds things.");
    assert_eq!(detail["model"], "test-alternative");
    assert_eq!(detail["provider"], "default");
    assert_eq!(detail["browser_profile"], "default");
    let (_, bots) = call(&rig.app, admin_req(&rig.app, "GET", "/v1/bots", None)).await;
    let notes = bots["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == "notes")
        .unwrap()
        .clone();
    assert_eq!(notes["display_name"], "Scout");
    assert_eq!(notes["browser_profile"], "default");

    // Validation failures change nothing.
    for body in [
        json!({"display_name": ""}),
        json!({"display_name": "x".repeat(49)}),
        json!({"description": "d".repeat(281)}),
        json!({"model": {"provider": "nope", "model": "test"}}),
        json!({"model": {"provider": "default", "model": "no-such-model"}}),
        json!({"model": {"provider": "default"}}),
        json!({"browser_profile": "missing"}),
        json!({"browser_profile": "../x"}),
    ] {
        let (status, _) = call(
            &rig.app,
            admin_req(&rig.app, "PUT", "/v1/bots/notes", Some(body.clone())),
        )
        .await;
        assert!(
            status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST,
            "{body}: {status}"
        );
    }
    let (status, _) = call(
        &rig.app,
        admin_req(&rig.app, "PUT", "/v1/bots/notes", Some(json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, detail) = call(&rig.app, admin_req(&rig.app, "GET", "/v1/bots/notes", None)).await;
    assert_eq!(detail["display_name"], "Scout");

    // null resets model/profile; an empty description clears it.
    let (status, put) = call(
        &rig.app,
        admin_req(
            &rig.app,
            "PUT",
            "/v1/bots/notes",
            Some(json!({"model": null, "browser_profile": null, "description": ""})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{put}");
    assert!(put["bot"]["model"].is_null());
    assert!(put["bot"]["description"].is_null());
    let (_, detail) = call(&rig.app, admin_req(&rig.app, "GET", "/v1/bots/notes", None)).await;
    assert_eq!(detail["description"], "");
    assert_eq!(detail["model"], "test");

    // A profile of another user cannot be assigned.
    call(
        &rig.app,
        member_req(
            &rig,
            "POST",
            "/v1/browser/profiles",
            Some(json!({"name": "Mine"})),
        ),
    )
    .await;
    let (status, _) = call(
        &rig.app,
        admin_req(
            &rig.app,
            "PUT",
            "/v1/bots/notes",
            Some(json!({"browser_profile": "mine"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn bot_settings_do_not_leak_between_users() {
    let rig = rig(true);
    call(
        &rig.app,
        admin_req(
            &rig.app,
            "PUT",
            "/v1/bots/notes",
            Some(json!({"display_name": "Admin Notes"})),
        ),
    )
    .await;
    // The member's own `notes` bot (seeded from the same templates) is a
    // separate row with separate settings.
    let (_, before) = call(&rig.app, member_req(&rig, "GET", "/v1/bots/notes", None)).await;
    assert_eq!(before["display_name"], "notes");
    let (status, _) = call(
        &rig.app,
        member_req(
            &rig,
            "PUT",
            "/v1/bots/notes",
            Some(json!({"display_name": "Member Notes", "browser_profile": "missing"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(
        &rig.app,
        member_req(
            &rig,
            "PUT",
            "/v1/bots/notes",
            Some(json!({"display_name": "Member Notes"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, detail) = call(&rig.app, member_req(&rig, "GET", "/v1/bots/notes", None)).await;
    assert_eq!(detail["display_name"], "Member Notes");
    // A bot the member does not have cannot be edited through their token.
    let (status, _) = call(
        &rig.app,
        member_req(
            &rig,
            "PUT",
            "/v1/bots/ghost",
            Some(json!({"display_name": "x"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, admin_detail) =
        call(&rig.app, admin_req(&rig.app, "GET", "/v1/bots/notes", None)).await;
    assert_eq!(admin_detail["display_name"], "Admin Notes");
}

/// Regression: the desktop parses the takeover/handback/input reply as a
/// session (`id` + `profile_name` required). A reply without them made
/// "Take control" fail with an API error although the server had switched.
#[tokio::test]
async fn session_control_replies_carry_id_and_profile_name() {
    let rig = rig(true);
    drop(
        rig.manager
            .bot_lease(&rig.admin_id, "default", true, None)
            .await
            .unwrap(),
    );
    for (uri, body) in [
        ("/v1/browser/sessions/default/takeover", None),
        (
            "/v1/browser/sessions/default/input",
            Some(json!({"type": "key", "key": "Tab"})),
        ),
        ("/v1/browser/sessions/default/handback", None),
        ("/v1/browser/takeover", None),
    ] {
        let (status, info) = call(&rig.app, admin_req(&rig.app, "POST", uri, body)).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(info["id"], "default", "{uri}");
        assert_eq!(info["profile_name"], "Default", "{uri}");
        assert!(info["updated_at"].is_string() && info["controller"].is_string());
    }
}

#[tokio::test]
async fn tabs_and_pointer_flow_through_the_session_api() {
    let rig = rig(true);
    drop(
        rig.manager
            .bot_lease(&rig.admin_id, "default", true, None)
            .await
            .unwrap(),
    );
    let base = "/v1/browser/sessions/default";
    // Tab operations need the user in control, like input.
    let open = json!({"url": "https://93.184.216.34/two"});
    let (status, _) = call(
        &rig.app,
        admin_req(
            &rig.app,
            "POST",
            &format!("{base}/tabs"),
            Some(open.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    call(
        &rig.app,
        admin_req(&rig.app, "POST", &format!("{base}/takeover"), None),
    )
    .await;

    let (status, info) = call(
        &rig.app,
        admin_req(&rig.app, "POST", &format!("{base}/tabs"), Some(open)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{info}");
    let tabs = info["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[1]["id"], "t2");
    assert_eq!(tabs[1]["active"], true);
    assert_eq!(tabs[1]["url"], "https://93.184.216.34/two");
    assert_eq!(tabs[0]["active"], false);

    // Non-public URLs never open a tab.
    for url in [
        "http://10.0.0.1/",
        "http://localhost:9222/",
        "file:///etc/passwd",
    ] {
        let (status, _) = call(
            &rig.app,
            admin_req(
                &rig.app,
                "POST",
                &format!("{base}/tabs"),
                Some(json!({"url": url})),
            ),
        )
        .await;
        assert!(
            status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST,
            "{url}: {status}"
        );
    }
    assert_eq!(
        rig.manager.info(&rig.admin_id, "default").await.tabs.len(),
        2
    );

    let (status, info) = call(
        &rig.app,
        admin_req(&rig.app, "POST", &format!("{base}/tabs/t1/switch"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["tabs"][0]["active"], true);
    for bad in ["x1", "t", "t1234567"] {
        let (status, _) = call(
            &rig.app,
            admin_req(&rig.app, "POST", &format!("{base}/tabs/{bad}/close"), None),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{bad}");
    }
    let (status, _) = call(
        &rig.app,
        admin_req(&rig.app, "POST", &format!("{base}/tabs/t9/close"), None),
    )
    .await;
    assert!(
        status.is_client_error() && status != StatusCode::NOT_FOUND,
        "{status}"
    );
    let (status, info) = call(
        &rig.app,
        admin_req(&rig.app, "POST", &format!("{base}/tabs/t2/close"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["tabs"].as_array().unwrap().len(), 1);
    // The last tab cannot be closed.
    let (status, _) = call(
        &rig.app,
        admin_req(&rig.app, "POST", &format!("{base}/tabs/t1/close"), None),
    )
    .await;
    assert!(
        status.is_client_error() && status != StatusCode::NOT_FOUND,
        "{status}"
    );

    // The last click is reported for the live-view cursor, with a growing seq.
    let click = |x: u32, y: u32| {
        admin_req(
            &rig.app,
            "POST",
            &format!("{base}/input"),
            Some(json!({"type": "click", "x": x, "y": y})),
        )
    };
    let (_, info) = call(&rig.app, click(30, 40)).await;
    assert_eq!(info["pointer"]["x"], 30.0);
    assert_eq!(info["pointer"]["action"], "click");
    assert_eq!(info["pointer"]["seq"], 1);
    assert_eq!(info["pointer"]["vw"], 1280);
    let (_, info) = call(&rig.app, click(31, 41)).await;
    assert_eq!(info["pointer"]["seq"], 2);

    // Another user has no session here and sees none of it.
    let (status, _) = call(
        &rig.app,
        member_req(&rig, "POST", &format!("{base}/tabs/t1/switch"), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, member) = call(&rig.app, member_req(&rig, "GET", "/v1/browser", None)).await;
    assert!(member.get("tabs").is_none() && member.get("pointer").is_none());
}

#[tokio::test]
async fn bot_thinking_level_is_validated_stored_and_returned() {
    let rig = rig(true);
    let (status, put) = call(
        &rig.app,
        admin_req(
            &rig.app,
            "PUT",
            "/v1/bots/notes",
            Some(json!({"thinking": "high"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{put}");
    assert_eq!(put["bot"]["thinking"], "high");
    let (_, detail) = call(&rig.app, admin_req(&rig.app, "GET", "/v1/bots/notes", None)).await;
    assert_eq!(detail["thinking"], "high");
    // The test model has no thinking control, so no levels are offered.
    assert_eq!(detail["thinking_levels"], json!([]));
    let (status, body) = call(
        &rig.app,
        admin_req(
            &rig.app,
            "PUT",
            "/v1/bots/notes",
            Some(json!({"thinking": "extreme"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "thinking_unavailable");
    let (status, put) = call(
        &rig.app,
        admin_req(
            &rig.app,
            "PUT",
            "/v1/bots/notes",
            Some(json!({"thinking": null})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{put}");
    assert!(put["bot"]["thinking"].is_null());
}
