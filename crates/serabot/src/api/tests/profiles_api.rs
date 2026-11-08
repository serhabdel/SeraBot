use super::*;
use crate::avatar::testing::synthetic;
use image::ImageFormat;

/// Stands in for the onboarding model call so no request leaves the process.
pub(super) struct ScriptedDrafter;

#[async_trait::async_trait]
impl crate::onboarding::Drafter for ScriptedDrafter {
    async fn complete(&self, _: &str, _: &str, system: &str, input: &str) -> Result<String> {
        if system.contains("clarifying questions") {
            // Ask only when the owner's words are thin.
            if input.contains("hire") {
                return Ok(r#"{"questions":["Which roles?","Which city?"]}"#.into());
            }
            return Ok(r#"{"questions":[]}"#.into());
        }
        Ok(r#"{"role":"Recruiter","objective":"Shortlist candidates.","prompt":"You screen CVs."}"#
            .into())
    }
}

async fn call(app: &Router, token: &str, method: &str, uri: &str, body: Option<Value>) -> Response {
    app.clone()
        .oneshot(request_with_token(method, uri, token, body))
        .await
        .unwrap()
}

async fn put_bytes(app: &Router, token: &str, uri: &str, bytes: Vec<u8>) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/octet-stream")
                .body(Body::from(bytes))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_bytes(response: Response) -> Vec<u8> {
    http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

fn member(store: &Store, subject: &str) -> String {
    let user = store
        .create_user(subject, &format!("{subject}@example.example"), subject)
        .unwrap();
    mint_for(store, &user.id, subject)
}

async fn make_bot(app: &Router, token: &str, name: &str) -> String {
    let created = call(app, token, "POST", "/v1/bots", Some(json!({"name": name}))).await;
    assert_eq!(created.status(), StatusCode::OK, "create {name}");
    read_json(created).await["bot"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn bot_avatar_round_trip_reencodes_and_lists_a_version() {
    let (app, _, store) = app();
    let token = member(&store, "avatar-owner");
    let id = make_bot(&app, &token, "Recruiter").await;
    assert_eq!(id, "recruiter");

    let missing = call(&app, &token, "GET", "/v1/bots/recruiter/avatar", None).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let mut upload = synthetic(ImageFormat::Jpeg, 640, 300);
    upload.extend_from_slice(b"SYNTHETIC-TRAILER");
    let put = put_bytes(&app, &token, "/v1/bots/recruiter/avatar", upload).await;
    assert_eq!(put.status(), StatusCode::OK);

    let served = call(&app, &token, "GET", "/v1/bots/recruiter/avatar", None).await;
    assert_eq!(served.status(), StatusCode::OK);
    assert_eq!(served.headers()["content-type"], "image/png");
    assert_eq!(served.headers()["x-content-type-options"], "nosniff");
    let bytes = body_bytes(served).await;
    let decoded = image::load_from_memory(&bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (256, 256));
    assert!(!bytes.windows(9).any(|w| w == b"SYNTHETIC"));

    let listed = read_json(call(&app, &token, "GET", "/v1/bots", None).await).await;
    let bot = listed["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|bot| bot["id"] == "recruiter")
        .unwrap();
    assert!(bot["avatar_version"].is_string());

    let removed =
        read_json(call(&app, &token, "DELETE", "/v1/bots/recruiter/avatar", None).await).await;
    assert_eq!(removed["deleted"], true);
    let gone = call(&app, &token, "GET", "/v1/bots/recruiter/avatar", None).await;
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
}

/// A shipped bot without an upload serves its bundled picture; an upload
/// overrides it; deleting the upload restores it. A bot with no default and
/// no upload keeps answering 404.
#[tokio::test]
async fn bundled_default_avatar_falls_back_and_yields_to_an_upload() {
    let (app, _, store) = app();
    let token = member(&store, "avatar-defaults");
    make_bot(&app, &token, "Analyst").await;

    let default = call(&app, &token, "GET", "/v1/bots/analyst/avatar", None).await;
    assert_eq!(default.status(), StatusCode::OK);
    assert_eq!(default.headers()["content-type"], "image/png");
    let bundled = body_bytes(default).await;
    assert_eq!(
        bundled,
        crate::default_avatars::default_avatar("analyst")
            .unwrap()
            .to_vec()
    );

    let listed = read_json(call(&app, &token, "GET", "/v1/bots", None).await).await;
    let bot = listed["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|bot| bot["id"] == "analyst")
        .unwrap();
    assert!(
        bot["avatar_version"]
            .as_str()
            .unwrap()
            .starts_with(crate::default_avatars::DEFAULT_AVATAR_VERSION_PREFIX)
    );

    let upload = synthetic(ImageFormat::Png, 64, 48);
    let put = put_bytes(&app, &token, "/v1/bots/analyst/avatar", upload).await;
    assert_eq!(put.status(), StatusCode::OK);
    let overridden =
        body_bytes(call(&app, &token, "GET", "/v1/bots/analyst/avatar", None).await).await;
    assert_ne!(overridden, bundled);
    assert_eq!(
        image::load_from_memory(&overridden).unwrap().width(),
        crate::avatar::AVATAR_EDGE
    );

    let removed =
        read_json(call(&app, &token, "DELETE", "/v1/bots/analyst/avatar", None).await).await;
    assert_eq!(removed["deleted"], true);
    let restored = call(&app, &token, "GET", "/v1/bots/analyst/avatar", None).await;
    assert_eq!(restored.status(), StatusCode::OK);
    assert_eq!(body_bytes(restored).await, bundled);

    // The owner's own fleet has main with the brand icon, and a bot the
    // templates do not know still has no picture.
    let main = call(&app, &token, "GET", "/v1/bots/main/avatar", None).await;
    assert_eq!(main.status(), StatusCode::OK);
    assert_eq!(main.headers()["content-type"], "image/png");
    make_bot(&app, &token, "Scribe").await;
    let none = call(&app, &token, "GET", "/v1/bots/scribe/avatar", None).await;
    assert_eq!(none.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn avatar_upload_rejects_bad_input() {
    let (app, _, store) = app();
    let token = member(&store, "avatar-bad");
    make_bot(&app, &token, "Scribe").await;
    for bytes in [
        Vec::new(),
        b"GIF89a-not-allowed".to_vec(),
        b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec(),
        vec![0u8; 1024 * 1024 + 1],
    ] {
        let response = put_bytes(&app, &token, "/v1/bots/scribe/avatar", bytes).await;
        assert!(response.status().is_client_error());
    }
    // Far over the transport cap is refused before the handler runs.
    let huge = put_bytes(
        &app,
        &token,
        "/v1/bots/scribe/avatar",
        vec![1u8; 3 * 1024 * 1024],
    )
    .await;
    assert_eq!(huge.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let none = call(&app, &token, "GET", "/v1/bots/scribe/avatar", None).await;
    assert_eq!(none.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn avatars_are_isolated_between_users() {
    let (app, _, store) = app();
    let alice = member(&store, "iso-alice");
    let bob = member(&store, "iso-bob");
    make_bot(&app, &alice, "Planner").await;
    make_bot(&app, &bob, "Planner").await;
    let png = synthetic(ImageFormat::Png, 100, 100);
    assert_eq!(
        put_bytes(&app, &alice, "/v1/bots/planner/avatar", png.clone())
            .await
            .status(),
        StatusCode::OK
    );
    // Bob has a bot with the same id and still sees no image.
    let bobs = call(&app, &bob, "GET", "/v1/bots/planner/avatar", None).await;
    assert_eq!(bobs.status(), StatusCode::NOT_FOUND);
    // Bob deleting does not touch Alice's image.
    let del = read_json(call(&app, &bob, "DELETE", "/v1/bots/planner/avatar", None).await).await;
    assert_eq!(del["deleted"], false);
    assert_eq!(
        call(&app, &alice, "GET", "/v1/bots/planner/avatar", None)
            .await
            .status(),
        StatusCode::OK
    );
    // A bot the user does not own is a 404 for every verb, even with bytes.
    let carol = member(&store, "iso-carol");
    for response in [
        put_bytes(&app, &carol, "/v1/bots/ghost/avatar", png.clone()).await,
        call(&app, &carol, "GET", "/v1/bots/ghost/avatar", None).await,
        call(&app, &carol, "DELETE", "/v1/bots/ghost/avatar", None).await,
    ] {
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    // The admin does not read a member's avatar through the bot route either.
    let admin_token = crate::token_cli::mint(&store, "admin-avatar", None).unwrap();
    let admin_view = call(&app, &admin_token, "GET", "/v1/bots/planner/avatar", None).await;
    assert_eq!(admin_view.status(), StatusCode::NOT_FOUND);
    // Deleting the bot removes its image.
    assert_eq!(
        call(&app, &alice, "DELETE", "/v1/bots/planner", None)
            .await
            .status(),
        StatusCode::OK
    );
    make_bot(&app, &alice, "Planner").await;
    assert_eq!(
        call(&app, &alice, "GET", "/v1/bots/planner/avatar", None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn bot_ids_come_from_names_and_stay_unique() {
    let (app, _, store) = app();
    let token = member(&store, "slugger");
    assert_eq!(
        make_bot(&app, &token, "Marketing Lead").await,
        "marketing-lead"
    );
    assert_eq!(
        make_bot(&app, &token, "Marketing Lead").await,
        "marketing-lead-2"
    );
    assert_eq!(make_bot(&app, &token, "42 Things").await, "bot-42-things");
    // The orchestrator id is reserved, so the name gets a suffix.
    assert_eq!(make_bot(&app, &token, "Main").await, "main-2");
    let empty = call(
        &app,
        &token,
        "POST",
        "/v1/bots",
        Some(json!({"name": "   "})),
    )
    .await;
    assert!(empty.status().is_client_error());
    let neither = call(&app, &token, "POST", "/v1/bots", Some(json!({}))).await;
    assert!(neither.status().is_client_error());
    let bad_model = call(
        &app,
        &token,
        "POST",
        "/v1/bots",
        Some(json!({"name": "Fancy", "model": "no-such-model"})),
    )
    .await;
    assert!(bad_model.status().is_client_error());
    let good_model = call(
        &app,
        &token,
        "POST",
        "/v1/bots",
        Some(json!({"name": "Fancy", "model": "test"})),
    )
    .await;
    assert_eq!(good_model.status(), StatusCode::OK);
    // The legacy explicit id still works.
    let legacy = call(
        &app,
        &token,
        "POST",
        "/v1/bots",
        Some(json!({"id": "legacy-bot"})),
    )
    .await;
    assert_eq!(legacy.status(), StatusCode::OK);
}

#[tokio::test]
async fn onboarding_runs_the_interview_and_saves_on_confirm() {
    let (app, _, store) = app();
    let token = member(&store, "onboard-owner");
    let id = make_bot(&app, &token, "Recruiter").await;

    let started =
        read_json(call(&app, &token, "GET", "/v1/bots/recruiter/onboarding", None).await).await;
    assert_eq!(started["state"], "awaiting_goal");
    assert_eq!(
        started["messages"][0]["text"],
        "Hi, I'm Recruiter. What would you like me to help you with, and what is my main goal?"
    );
    // Confirm is refused before there is a proposal.
    let early = call(
        &app,
        &token,
        "POST",
        "/v1/bots/recruiter/onboarding/confirm",
        None,
    )
    .await;
    assert!(early.status().is_client_error());

    let asked = read_json(
        call(
            &app,
            &token,
            "POST",
            "/v1/bots/recruiter/onboarding/reply",
            Some(json!({"text": "Help me hire engineers"})),
        )
        .await,
    )
    .await;
    assert_eq!(asked["state"], "clarifying");
    assert!(
        asked["messages"][2]["text"]
            .as_str()
            .unwrap()
            .contains("Which roles?")
    );

    let proposed = read_json(
        call(
            &app,
            &token,
            "POST",
            "/v1/bots/recruiter/onboarding/reply",
            Some(json!({"text": "Backend engineers in Casablanca"})),
        )
        .await,
    )
    .await;
    assert_eq!(proposed["state"], "proposed");
    assert_eq!(proposed["can_confirm"], true);
    assert_eq!(proposed["proposal"]["role"], "Recruiter");

    // Nothing is saved until confirm: the bot is still disabled.
    let before = read_json(call(&app, &token, "GET", "/v1/bots/recruiter", None).await).await;
    assert_eq!(before["enabled"], false);

    let confirmed = read_json(
        call(
            &app,
            &token,
            "POST",
            "/v1/bots/recruiter/onboarding/confirm",
            None,
        )
        .await,
    )
    .await;
    assert_eq!(confirmed["done"], true);
    let after = read_json(call(&app, &token, "GET", &format!("/v1/bots/{id}"), None).await).await;
    assert_eq!(after["enabled"], true);
    let instructions = after["instructions"].as_str().unwrap();
    assert!(instructions.contains("You screen CVs."));
    assert!(instructions.contains("Main goal: Shortlist candidates."));

    // A finished interview accepts no more answers.
    let late = call(
        &app,
        &token,
        "POST",
        "/v1/bots/recruiter/onboarding/reply",
        Some(json!({"text": "one more thing"})),
    )
    .await;
    assert!(late.status().is_client_error());
    let blank = call(
        &app,
        &token,
        "POST",
        "/v1/bots/recruiter/onboarding/reply",
        Some(json!({"text": "  "})),
    )
    .await;
    assert!(blank.status().is_client_error());
}

#[tokio::test]
async fn onboarding_is_private_to_the_bot_owner() {
    let (app, _, store) = app();
    let alice = member(&store, "onb-alice");
    let bob = member(&store, "onb-bob");
    make_bot(&app, &alice, "Recruiter").await;
    for (method, uri, body) in [
        ("GET", "/v1/bots/recruiter/onboarding", None),
        ("POST", "/v1/bots/recruiter/onboarding", None),
        (
            "POST",
            "/v1/bots/recruiter/onboarding/reply",
            Some(json!({"text": "hello there"})),
        ),
        ("POST", "/v1/bots/recruiter/onboarding/confirm", None),
    ] {
        let response = call(&app, &bob, method, uri, body).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn groups_are_created_in_one_step_with_a_generated_id() {
    let (app, _, store) = app();
    let token = member(&store, "group-owner");
    // A bot must be enabled to join; the onboarding confirm enables it.
    make_bot(&app, &token, "Recruiter").await;
    call(
        &app,
        &token,
        "POST",
        "/v1/bots/recruiter/onboarding/reply",
        Some(json!({"text": "Screen CVs for engineering roles each week"})),
    )
    .await;
    call(
        &app,
        &token,
        "POST",
        "/v1/bots/recruiter/onboarding/confirm",
        None,
    )
    .await;

    let created = call(
        &app,
        &token,
        "POST",
        "/v1/groups",
        Some(json!({
            "name": "Hiring Team",
            "description": "Everything about open roles",
            "members": ["recruiter"],
            "rules": "Keep replies short.",
            "model": {"provider": "default", "model": "test"}
        })),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let created = read_json(created).await;
    let id = created["group"]["id"].as_str().unwrap().to_owned();
    assert_eq!(id, "room:hiring-team");
    assert_eq!(created["group"]["title"], "Hiring Team");

    let team = read_json(
        call(
            &app,
            &token,
            "GET",
            &format!("/v1/conversations/{id}/team"),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(team["members"][0]["bot"], "recruiter");
    assert_eq!(team["rules"], "Keep replies short.");
    let profile = read_json(
        call(
            &app,
            &token,
            "GET",
            &format!("/v1/conversations/{id}/profile"),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(profile["description"], "Everything about open roles");
    assert_eq!(profile["has_avatar"], false);

    // The same name gets a fresh id.
    let again = read_json(
        call(
            &app,
            &token,
            "POST",
            "/v1/groups",
            Some(json!({"name": "Hiring Team"})),
        )
        .await,
    )
    .await;
    assert_eq!(again["group"]["id"], "room:hiring-team-2");

    // Bad input is refused and creates nothing.
    for body in [
        json!({"name": ""}),
        json!({"name": "X", "members": ["nobody"]}),
        json!({"name": "X", "members": ["main"]}),
        json!({"name": "X", "model": {"provider": "default", "model": "unknown"}}),
        json!({"name": "X", "mode": "sideways"}),
        json!({"name": "X", "extra": true}),
    ] {
        let response = call(&app, &token, "POST", "/v1/groups", Some(body.clone())).await;
        assert!(response.status().is_client_error(), "{body}");
    }
    let listed = read_json(call(&app, &token, "GET", "/v1/conversations", None).await).await;
    let rooms = listed["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["kind"] == "room")
        .count();
    assert_eq!(rooms, 2);
}

#[tokio::test]
async fn group_profiles_and_avatars_are_isolated_between_users() {
    let (app, _, store) = app();
    let alice = member(&store, "grp-alice");
    let bob = member(&store, "grp-bob");
    let made = read_json(
        call(
            &app,
            &alice,
            "POST",
            "/v1/groups",
            Some(json!({"name": "Secret Club", "description": "alice only"})),
        )
        .await,
    )
    .await;
    let id = made["group"]["id"].as_str().unwrap().to_owned();
    let png = synthetic(ImageFormat::Png, 80, 120);
    assert_eq!(
        put_bytes(
            &app,
            &alice,
            &format!("/v1/conversations/{id}/avatar"),
            png.clone()
        )
        .await
        .status(),
        StatusCode::OK
    );
    let served = call(
        &app,
        &alice,
        "GET",
        &format!("/v1/conversations/{id}/avatar"),
        None,
    )
    .await;
    assert_eq!(served.status(), StatusCode::OK);
    let decoded = image::load_from_memory(&body_bytes(served).await).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (256, 256));
    let profile = read_json(
        call(
            &app,
            &alice,
            "GET",
            &format!("/v1/conversations/{id}/profile"),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(profile["has_avatar"], true);

    // Bob can read, write or delete none of it.
    for response in [
        call(
            &app,
            &bob,
            "GET",
            &format!("/v1/conversations/{id}/avatar"),
            None,
        )
        .await,
        put_bytes(
            &app,
            &bob,
            &format!("/v1/conversations/{id}/avatar"),
            png.clone(),
        )
        .await,
        call(
            &app,
            &bob,
            "DELETE",
            &format!("/v1/conversations/{id}/avatar"),
            None,
        )
        .await,
        call(
            &app,
            &bob,
            "GET",
            &format!("/v1/conversations/{id}/profile"),
            None,
        )
        .await,
        call(
            &app,
            &bob,
            "PUT",
            &format!("/v1/conversations/{id}/profile"),
            Some(json!({"description": "hijacked"})),
        )
        .await,
        call(
            &app,
            &bob,
            "GET",
            &format!("/v1/conversations/{id}/team"),
            None,
        )
        .await,
    ] {
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    let still = read_json(
        call(
            &app,
            &alice,
            "GET",
            &format!("/v1/conversations/{id}/profile"),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(still["description"], "alice only");

    let updated = read_json(
        call(
            &app,
            &alice,
            "PUT",
            &format!("/v1/conversations/{id}/profile"),
            Some(json!({"description": "new blurb"})),
        )
        .await,
    )
    .await;
    assert_eq!(updated["description"], "new blurb");
    assert_eq!(updated["has_avatar"], true);
    let removed = read_json(
        call(
            &app,
            &alice,
            "DELETE",
            &format!("/v1/conversations/{id}/avatar"),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(removed["deleted"], true);
}

#[tokio::test]
async fn a_bot_joins_the_picked_group_when_onboarding_is_confirmed() {
    let (app, _, store) = app();
    let token = member(&store, "join-owner");
    make_bot(&app, &token, "Scribe").await;
    call(
        &app,
        &token,
        "POST",
        "/v1/bots/scribe/onboarding/reply",
        Some(json!({"text": "Write meeting notes for me every day"})),
    )
    .await;
    call(
        &app,
        &token,
        "POST",
        "/v1/bots/scribe/onboarding/confirm",
        None,
    )
    .await;
    let group = read_json(
        call(
            &app,
            &token,
            "POST",
            "/v1/groups",
            Some(json!({"name": "Ops", "members": ["scribe"]})),
        )
        .await,
    )
    .await;
    let group_id = group["group"]["id"].as_str().unwrap().to_owned();

    // A new bot created with that group joins once it is confirmed.
    let created = call(
        &app,
        &token,
        "POST",
        "/v1/bots",
        Some(json!({"name": "Planner", "group": group_id})),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let before = read_json(
        call(
            &app,
            &token,
            "GET",
            &format!("/v1/conversations/{group_id}/team"),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(before["members"].as_array().unwrap().len(), 1);
    call(
        &app,
        &token,
        "POST",
        "/v1/bots/planner/onboarding/reply",
        Some(json!({"text": "Plan the week for me each Monday"})),
    )
    .await;
    let done = read_json(
        call(
            &app,
            &token,
            "POST",
            "/v1/bots/planner/onboarding/confirm",
            None,
        )
        .await,
    )
    .await;
    assert_eq!(done["joined_group"], true);
    let after = read_json(
        call(
            &app,
            &token,
            "GET",
            &format!("/v1/conversations/{group_id}/team"),
            None,
        )
        .await,
    )
    .await;
    let members: Vec<_> = after["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["bot"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(members, vec!["scribe", "planner"]);

    // A group that is not the user's own is refused at create time.
    let other = member(&store, "join-other");
    let foreign = call(
        &app,
        &other,
        "POST",
        "/v1/bots",
        Some(json!({"name": "Spy", "group": group_id})),
    )
    .await;
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
}

/// `GET /v1/bots/catalog` lists the shipped templates, and `installed` comes
/// from the session user's own rows. Installing a removed template restores
/// only that user's row; the other user's removal stands, and a second install
/// is a conflict.
#[tokio::test]
async fn bot_catalog_lists_templates_and_install_is_per_user() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf();
    let mut watcher = test_bot("watcher", crate::orchestration::BotRole::Specialist);
    watcher.config.title = Some("Watcher".into());
    watcher.config.description = Some("Monitors sources.".into());
    let main = test_bot("main", crate::orchestration::BotRole::Orchestrator);
    let (app, _, store) = app_with_fleet(
        true,
        crate::policy::Policy::empty(),
        root,
        vec![main, watcher],
    );
    let owner = member(&store, "catalog-owner");
    let other = member(&store, "catalog-other");
    for token in [&owner, &other] {
        // The first read seeds the user's fleet from the templates.
        assert_eq!(
            call(&app, token, "GET", "/v1/bots", None).await.status(),
            StatusCode::OK
        );
        let removed = call(&app, token, "DELETE", "/v1/bots/watcher", None).await;
        assert_eq!(removed.status(), StatusCode::OK);
    }

    let entry = |body: &Value, id: &str| {
        body["bots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|bot| bot["id"] == id)
            .unwrap()
            .clone()
    };
    let catalog = read_json(call(&app, &owner, "GET", "/v1/bots/catalog", None).await).await;
    assert_eq!(entry(&catalog, "watcher")["installed"], false);
    assert_eq!(entry(&catalog, "watcher")["title"], "Watcher");
    // The bundled picture of a removed shipped bot is still served, so the
    // catalog can show it; an unknown bot without a default stays a 404.
    let picture = call(&app, &owner, "GET", "/v1/bots/watcher/avatar", None).await;
    assert_eq!(picture.status(), StatusCode::OK);
    let unknown = call(&app, &owner, "GET", "/v1/bots/nobody/avatar", None).await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(entry(&catalog, "main")["installed"], true);

    let installed = call(
        &app,
        &owner,
        "POST",
        "/v1/bots/catalog/watcher/install",
        None,
    )
    .await;
    assert_eq!(installed.status(), StatusCode::OK);
    let body = read_json(installed).await;
    assert_eq!(body["bot"]["id"], "watcher");
    assert_eq!(body["bot"]["installed"], true);
    assert_eq!(body["bot"]["title"], "Watcher");

    let again = call(
        &app,
        &owner,
        "POST",
        "/v1/bots/catalog/watcher/install",
        None,
    )
    .await;
    assert_eq!(again.status(), StatusCode::CONFLICT);
    let unknown = call(&app, &owner, "POST", "/v1/bots/catalog/ghost/install", None).await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    // The restored row is listed with its shipped title as the display name.
    let listed = read_json(call(&app, &owner, "GET", "/v1/bots", None).await).await;
    let row = entry(&listed, "watcher");
    assert_eq!(row["display_name"], "Watcher");
    assert_eq!(row["title"], "Watcher");
    assert_eq!(row["description"], "Monitors sources.");

    // The other user's catalog still says the bot is removed.
    let other_catalog = read_json(call(&app, &other, "GET", "/v1/bots/catalog", None).await).await;
    assert_eq!(entry(&other_catalog, "watcher")["installed"], false);
    let other_list = read_json(call(&app, &other, "GET", "/v1/bots", None).await).await;
    assert!(
        other_list["bots"]
            .as_array()
            .unwrap()
            .iter()
            .all(|bot| bot["id"] != "watcher")
    );
}
