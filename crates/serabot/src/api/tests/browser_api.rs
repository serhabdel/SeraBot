//! Owner browser API: auth, per-user isolation and the controller flow, with a
//! fake session backend (no Chromium).

use std::time::Duration;

use super::*;
use crate::browser::testing::{FakeLauncher, manager_with};
use crate::browser::{BrowserManager, Controller};

struct Rig {
    app: Router,
    manager: Arc<BrowserManager>,
    launcher: Arc<FakeLauncher>,
    admin_id: String,
    member_id: String,
    member_token: String,
}

fn rig(with_browser: bool) -> Rig {
    let (manager, launcher) = manager_with(4, Duration::from_secs(600));
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .unwrap();
    let (app, _, store) = app_with_stack(
        true,
        Policy::empty(),
        root,
        vec![test_bot(
            "main",
            crate::orchestration::BotRole::Orchestrator,
        )],
        None,
        None,
        with_browser.then(|| manager.clone()),
    );
    let (member, member_token) = member_with_session(&store);
    Rig {
        app,
        manager,
        launcher,
        admin_id: admin(&store),
        member_id: member.id,
        member_token,
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
    drop(rig.manager.bot_lease(&rig.admin_id, true).await.unwrap());
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

    drop(rig.manager.bot_lease(&rig.member_id, true).await.unwrap());
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
        rig.manager.info(&rig.admin_id).await.controller,
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
    assert!(rig.manager.info(&rig.admin_id).await.active);
    assert!(!rig.manager.info(&rig.member_id).await.active);
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
    drop(rig.manager.bot_lease(&rig.admin_id, true).await.unwrap());

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
    assert!(rig.manager.bot_lease(&rig.admin_id, false).await.is_err());
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
    assert!(rig.manager.bot_lease(&rig.admin_id, false).await.is_ok());
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
