//! The native GitHub connector against one wiremock server that plays github.com,
//! api.github.com and the hosted MCP server. Every token is a synthetic fixture
//! and nothing leaves loopback.

use std::collections::HashSet;

use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;
use crate::gateway::{Gateway, ToolOutcome, ToolRegistry};
use crate::policy::Policy;

const CLIENT_ID: &str = "synthetic-client-id";
const FIRST: &str = "synthetic-access-1";
const SECOND: &str = "synthetic-access-2";
const REFRESH_ONE: &str = "synthetic-refresh-1";
const REFRESH_TWO: &str = "synthetic-refresh-2";

struct World {
    store: Store,
    users: Arc<UserSecrets>,
    user: String,
    other: String,
    connector: GithubConnector,
}

fn world(server: &MockServer) -> World {
    let store = Store::open_in_memory(384).unwrap();
    let users = Arc::new(UserSecrets::with_master_key(store.clone(), [4u8; 32]).unwrap());
    let user = store
        .create_user("microsoft:gh-a", "a@example.example", "A")
        .unwrap()
        .id;
    let other = store
        .create_user("microsoft:gh-b", "b@example.example", "B")
        .unwrap()
        .id;
    let connector =
        GithubConnector::for_test(store.clone(), users.clone(), CLIENT_ID, &server.uri()).unwrap();
    World {
        store,
        users,
        user,
        other,
        connector,
    }
}

fn in_secs(seconds: i64) -> String {
    rfc3339(Utc::now().timestamp() + seconds).unwrap()
}

/// A signed-in member whose access token expires in `access_in` seconds.
fn seed(world: &World, access_in: i64, refresh_in: i64) {
    world
        .connector
        .seal(
            &world.user,
            &Sealed {
                access_token: FIRST.into(),
                refresh_token: Some(REFRESH_ONE.into()),
            },
        )
        .unwrap();
    world
        .store
        .github_connector_put(
            &world.user,
            "synthetic-octocat",
            4242,
            Some(&in_secs(access_in)),
            Some(&in_secs(refresh_in)),
        )
        .unwrap();
}

async fn mount_device(server: &MockServer, page: &str) {
    Mock::given(method("POST"))
        .and(path("/login/device/code"))
        .and(body_string_contains("client_id=synthetic-client-id"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dc-synthetic",
            "user_code": "ABCD-1234",
            "verification_uri": page,
            "expires_in": 900,
            "interval": 5,
        })))
        .mount(server)
        .await;
}

/// GitHub answers every poll with HTTP 200, errors included.
async fn mount_polls(server: &MockServer, answers: Vec<Value>) {
    for answer in answers {
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .and(body_string_contains("device_code=dc-synthetic"))
            .respond_with(ResponseTemplate::new(200).set_body_json(answer))
            .up_to_n_times(1)
            .mount(server)
            .await;
    }
}

fn approved() -> Value {
    json!({
        "access_token": FIRST,
        "refresh_token": REFRESH_ONE,
        "expires_in": 28_800,
        "refresh_token_expires_in": 15_897_600,
        "token_type": "bearer",
    })
}

async fn mount_user(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/user"))
        .and(wiremock::matchers::header(
            "authorization",
            format!("Bearer {FIRST}").as_str(),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"login": "synthetic-octocat", "id": 4242})),
        )
        .mount(server)
        .await;
}

fn refreshed() -> Value {
    json!({
        "access_token": SECOND,
        "refresh_token": REFRESH_TWO,
        "expires_in": 28_800,
        "refresh_token_expires_in": 15_897_600,
    })
}

async fn mount_refresh(server: &MockServer, template: ResponseTemplate, times: u64) {
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=synthetic-refresh-1"))
        .respond_with(template)
        .expect(times)
        .mount(server)
        .await;
}

async fn run_to_end(world: &World) -> DeviceStatus {
    world.connector.lock_runs().insert(
        world.user.clone(),
        Run {
            id: 1,
            status: DeviceStatus::Pending,
        },
    );
    world
        .connector
        .run_device(
            &world.user,
            1,
            Zeroizing::new("dc-synthetic".to_owned()),
            5,
            900,
            |_| async {},
        )
        .await;
    world.connector.device_status(&world.user).unwrap()
}

// ---- Device sign-in ----

#[tokio::test]
async fn the_device_flow_waits_then_stores_the_login_and_seals_the_tokens() {
    let server = MockServer::start().await;
    mount_user(&server).await;
    mount_polls(
        &server,
        vec![
            json!({"error": "authorization_pending"}),
            json!({"error": "slow_down"}),
            approved(),
        ],
    )
    .await;
    let world = world(&server);
    let status = run_to_end(&world).await;
    assert_eq!(
        status,
        DeviceStatus::Connected {
            login: "synthetic-octocat".into()
        }
    );
    let row = world.store.github_connector(&world.user).unwrap().unwrap();
    assert_eq!(
        (row.login.as_str(), row.gh_user_id),
        ("synthetic-octocat", 4242)
    );
    assert_eq!(row.state, "connected");
    assert!(row.access_expires_at.is_some() && row.refresh_expires_at.is_some());
    // The tokens are sealed, owned by this member and in no view.
    let sealed = world.connector.unseal(&world.user).unwrap().unwrap();
    assert_eq!(sealed.access_token, FIRST);
    assert!(world.connector.unseal(&world.other).unwrap().is_none());
    assert!(
        world
            .store
            .github_connector(&world.other)
            .unwrap()
            .is_none()
    );
    let view = world.connector.view(&world.user).unwrap();
    assert_eq!(view["state"], "connected");
    assert!(!view.to_string().contains(FIRST) && !view.to_string().contains(REFRESH_ONE));
    assert_eq!(
        world.connector.view(&world.other).unwrap()["state"],
        "ready"
    );
}

#[tokio::test]
async fn an_expired_code_ends_the_flow_and_stores_nothing() {
    let server = MockServer::start().await;
    mount_polls(
        &server,
        vec![
            json!({"error": "authorization_pending"}),
            json!({"error": "expired_token"}),
        ],
    )
    .await;
    let world = world(&server);
    assert_eq!(run_to_end(&world).await, DeviceStatus::Expired);
    assert!(world.store.github_connector(&world.user).unwrap().is_none());
    assert!(world.connector.unseal(&world.user).unwrap().is_none());
}

#[tokio::test]
async fn a_code_nobody_approves_runs_out_on_its_own_clock() {
    let server = MockServer::start().await;
    // Every poll is "pending"; the 900 second bound ends it with no wall clock.
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"error": "authorization_pending"})),
        )
        .mount(&server)
        .await;
    let world = world(&server);
    assert_eq!(run_to_end(&world).await, DeviceStatus::Expired);
    assert!(world.store.github_connector(&world.user).unwrap().is_none());
}

#[tokio::test]
async fn a_denied_sign_in_stores_nothing() {
    let server = MockServer::start().await;
    mount_polls(&server, vec![json!({"error": "access_denied"})]).await;
    let world = world(&server);
    assert_eq!(
        run_to_end(&world).await,
        DeviceStatus::Denied {
            reason: "access_denied".into()
        }
    );
    assert!(world.store.github_connector(&world.user).unwrap().is_none());
}

#[tokio::test]
async fn an_unknown_login_is_not_saved() {
    let server = MockServer::start().await;
    mount_polls(&server, vec![approved()]).await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let world = world(&server);
    assert!(matches!(
        run_to_end(&world).await,
        DeviceStatus::Denied { .. }
    ));
    assert!(world.store.github_connector(&world.user).unwrap().is_none());
    assert!(world.connector.unseal(&world.user).unwrap().is_none());
}

#[tokio::test]
async fn starting_returns_the_code_and_refuses_a_page_off_github() {
    let server = MockServer::start().await;
    mount_device(&server, "https://verify.example/login/device").await;
    let world = world(&server);
    let started = world.connector.start_device(&world.user).await.unwrap();
    assert_eq!(started.user_code, "ABCD-1234");
    assert_eq!(
        started.verification_uri,
        "https://verify.example/login/device"
    );
    assert_eq!(
        world.connector.device_status(&world.user),
        Some(DeviceStatus::Pending)
    );

    let hostile = MockServer::start().await;
    mount_device(&hostile, "https://evil.example/login/device").await;
    let other = self::world(&hostile);
    assert!(other.connector.start_device(&other.user).await.is_err());
    assert!(other.connector.device_status(&other.user).is_none());
}

#[tokio::test]
async fn without_a_client_id_the_connector_is_not_configured() {
    let server = MockServer::start().await;
    let base = world(&server);
    let connector =
        GithubConnector::for_test(base.store.clone(), base.users.clone(), " ", &server.uri())
            .unwrap();
    assert!(!connector.configured());
    assert_eq!(
        connector.view(&base.user).unwrap()["state"],
        "not_configured"
    );
    assert!(connector.manage_url().is_none());
    assert!(matches!(
        connector.start_device(&base.user).await,
        Err(SeraError::Rejected {
            code: "not_configured",
            ..
        })
    ));
}

// ---- Refresh ----

#[test]
fn the_refresh_window_opens_ten_minutes_early() {
    let now = 1_000_000;
    let at = |offset: i64| rfc3339(now + offset).unwrap();
    assert!(!within_refresh_window(Some(&at(601)), now));
    assert!(within_refresh_window(Some(&at(600)), now));
    assert!(within_refresh_window(Some(&at(-5)), now));
    // No expiry on record: leave the token alone.
    assert!(!within_refresh_window(None, now));
}

#[tokio::test]
async fn a_token_far_from_expiry_is_used_as_it_is() {
    let server = MockServer::start().await;
    mount_refresh(
        &server,
        ResponseTemplate::new(200).set_body_json(refreshed()),
        0,
    )
    .await;
    let world = world(&server);
    seed(&world, 3_000, 100_000);
    let token = world
        .connector
        .access_token(&world.user, None)
        .await
        .unwrap();
    assert_eq!(token.expose_secret(), FIRST);
}

#[tokio::test]
async fn a_token_inside_the_window_is_refreshed_and_the_rotation_is_kept() {
    let server = MockServer::start().await;
    mount_refresh(
        &server,
        ResponseTemplate::new(200).set_body_json(refreshed()),
        1,
    )
    .await;
    let world = world(&server);
    seed(&world, 300, 100_000);
    let token = world
        .connector
        .access_token(&world.user, None)
        .await
        .unwrap();
    assert_eq!(token.expose_secret(), SECOND);
    let sealed = world.connector.unseal(&world.user).unwrap().unwrap();
    assert_eq!(sealed.access_token, SECOND);
    assert_eq!(sealed.refresh_token.as_deref(), Some(REFRESH_TWO));
    let row = world.store.github_connector(&world.user).unwrap().unwrap();
    assert_eq!(row.state, "connected");
    assert!(
        timestamp(row.access_expires_at.as_deref().unwrap()).unwrap()
            > Utc::now().timestamp() + 20_000
    );
}

#[tokio::test]
async fn concurrent_calls_share_one_refresh() {
    let server = MockServer::start().await;
    // The delay makes the calls overlap; `expect(1)` fails the test on a second refresh.
    mount_refresh(
        &server,
        ResponseTemplate::new(200)
            .set_body_json(refreshed())
            .set_delay(Duration::from_millis(150)),
        1,
    )
    .await;
    let world = world(&server);
    seed(&world, 60, 100_000);
    let (a, b, c) = tokio::join!(
        world.connector.access_token(&world.user, None),
        world.connector.access_token(&world.user, None),
        world.connector.access_token(&world.user, None),
    );
    for token in [a, b, c] {
        assert_eq!(token.unwrap().expose_secret(), SECOND);
    }
}

#[tokio::test]
async fn a_rejected_refresh_expires_the_sign_in_and_keeps_the_login() {
    let server = MockServer::start().await;
    // GitHub reports a dead refresh token with HTTP 200 and an error body.
    mount_refresh(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({"error": "bad_refresh_token"})),
        1,
    )
    .await;
    let world = world(&server);
    seed(&world, 60, 100_000);
    let failure = world
        .connector
        .access_token(&world.user, None)
        .await
        .unwrap_err();
    assert_eq!(failure.problem, Problem::NeedsSignIn);
    let row = world.store.github_connector(&world.user).unwrap().unwrap();
    assert_eq!(row.state, "expired");
    assert_eq!(row.login, "synthetic-octocat");
    // The dead tokens are gone; the member is told, and nothing more is sent.
    assert!(world.connector.unseal(&world.user).unwrap().is_none());
    assert_eq!(
        world.connector.notes_for(&world.user),
        vec!["GitHub needs sign-in".to_string()]
    );
    let view = world.connector.view(&world.user).unwrap();
    assert_eq!(
        (view["state"].as_str(), view["login"].as_str()),
        (Some("expired"), Some("synthetic-octocat"))
    );
    let again = world
        .connector
        .access_token(&world.user, None)
        .await
        .unwrap_err();
    assert_eq!(again.problem, Problem::NeedsSignIn);
    assert!(world.connector.specs_for(&world.user).await.is_empty());
}

#[tokio::test]
async fn a_refresh_that_cannot_reach_github_keeps_everything() {
    let server = MockServer::start().await;
    mount_refresh(&server, ResponseTemplate::new(503), 1).await;
    let world = world(&server);
    seed(&world, 60, 100_000);
    let failure = world
        .connector
        .access_token(&world.user, None)
        .await
        .unwrap_err();
    assert_eq!(failure.problem, Problem::Unreachable);
    let row = world.store.github_connector(&world.user).unwrap().unwrap();
    assert_eq!(row.state, "connected");
    assert_eq!(
        world
            .connector
            .unseal(&world.user)
            .unwrap()
            .unwrap()
            .refresh_token
            .as_deref(),
        Some(REFRESH_ONE)
    );
    assert!(world.connector.notes_for(&world.user).is_empty());
}

#[tokio::test]
async fn a_lapsed_refresh_token_expires_the_sign_in_without_a_request() {
    let server = MockServer::start().await;
    mount_refresh(
        &server,
        ResponseTemplate::new(200).set_body_json(refreshed()),
        0,
    )
    .await;
    let world = world(&server);
    seed(&world, -3_600, -10);
    let failure = world
        .connector
        .access_token(&world.user, None)
        .await
        .unwrap_err();
    assert_eq!(failure.problem, Problem::NeedsSignIn);
    assert_eq!(
        world
            .store
            .github_connector(&world.user)
            .unwrap()
            .unwrap()
            .state,
        "expired"
    );
}

#[tokio::test]
async fn nothing_is_offered_to_a_member_who_has_not_signed_in() {
    let server = MockServer::start().await;
    let world = world(&server);
    assert!(world.connector.specs_for(&world.user).await.is_empty());
    assert!(
        world
            .connector
            .tool_for(&world.user, "github.list_issues")
            .await
            .is_none()
    );
    let failure = world
        .connector
        .access_token(&world.user, None)
        .await
        .unwrap_err();
    assert_eq!(failure.problem, Problem::NotConnected);
}

// ---- Hosted MCP tools ----

/// An MCP server that answers only a request carrying an accepted bearer token.
struct Mcp {
    accepted: Vec<String>,
}

impl Respond for Mcp {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let sent = request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if !self
            .accepted
            .iter()
            .any(|ok| sent == format!("Bearer {ok}"))
        {
            return ResponseTemplate::new(401);
        }
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let reply = |result: Value| {
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .insert_header("mcp-session-id", "synthetic-session")
                .set_body_json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
        };
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => reply(json!({"protocolVersion": "2025-06-18"})),
            Some("tools/list") => reply(json!({"tools": [
                {"name": "list_issues", "description": "List issues",
                 "inputSchema": {"type": "object"},
                 "annotations": {"readOnlyHint": true}},
                {"name": "create_issue", "description": "Open an issue",
                 "inputSchema": {"type": "object"},
                 "annotations": {"readOnlyHint": false}},
                {"name": "delete_file", "description": "Delete a file",
                 "inputSchema": {"type": "object"},
                 "annotations": {"destructiveHint": true}},
                {"name": "unmarked", "description": "No hints",
                 "inputSchema": {"type": "object"}},
                {"name": "bad name!", "description": "Dropped",
                 "inputSchema": {"type": "object"}},
            ]})),
            Some("tools/call") => {
                let name = body["params"]["name"].as_str().unwrap_or("?");
                reply(json!({"content": [{"type": "text", "text": format!("ran {name}")}]}))
            }
            _ => ResponseTemplate::new(202),
        }
    }
}

async fn mount_mcp(server: &MockServer, accepted: &[&str]) {
    Mock::given(method("POST"))
        .and(path("/mcp/"))
        .respond_with(Mcp {
            accepted: accepted.iter().map(|token| token.to_string()).collect(),
        })
        .mount(server)
        .await;
}

#[tokio::test]
async fn tools_are_named_github_dot_and_follow_the_servers_hints() {
    let server = MockServer::start().await;
    mount_mcp(&server, &[FIRST]).await;
    let world = world(&server);
    seed(&world, 3_000, 100_000);
    let specs = world.connector.specs_for(&world.user).await;
    let names: Vec<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "github.list_issues",
            "github.create_issue",
            "github.delete_file",
            "github.unmarked"
        ]
    );
    let effect = |name: &str| {
        specs
            .iter()
            .find(|s| s.name == name)
            .unwrap()
            .external_effect
    };
    assert!(!effect("github.list_issues"));
    assert!(effect("github.create_issue") && effect("github.unmarked"));
    // The token is read at call time: it is in no spec the model sees.
    assert!(!serde_json::to_string(&specs).unwrap().contains(FIRST));
}

#[tokio::test]
async fn a_stale_token_is_refreshed_once_when_the_server_answers_401() {
    let server = MockServer::start().await;
    // The server accepts only the refreshed token, though the stored one has
    // not reached its window.
    mount_mcp(&server, &[SECOND]).await;
    mount_refresh(
        &server,
        ResponseTemplate::new(200).set_body_json(refreshed()),
        1,
    )
    .await;
    let world = world(&server);
    seed(&world, 3_000, 100_000);
    let specs = world.connector.specs_for(&world.user).await;
    assert_eq!(specs.len(), 4);
    let sealed = world.connector.unseal(&world.user).unwrap().unwrap();
    assert_eq!(sealed.access_token, SECOND);
}

#[tokio::test]
async fn a_read_tool_runs_and_a_write_tool_asks_bound_to_repo_and_arguments() {
    let server = MockServer::start().await;
    mount_mcp(&server, &[FIRST]).await;
    let world = world(&server);
    seed(&world, 3_000, 100_000);
    let gateway = Gateway::new(
        world.store.clone(),
        Arc::new(Policy::empty()),
        ToolRegistry::default(),
        1,
        false,
    )
    .with_github(world.connector.clone());
    let task = world
        .store
        .enqueue_task(&world.user, "turn", "main", &json!({}), 0, Utc::now())
        .unwrap();
    let mut context = ToolContext {
        user_id: world.user.clone(),
        actor: world.user.clone(),
        bot: "main".into(),
        task_id: Some(task.id),
        turn_id: None,
        conversation_id: None,
        allowed_tools: HashSet::new(),
        require_approval: HashSet::new(),
    };
    gateway.enable_plugin_tools(&mut context).await;
    assert!(context.allowed_tools.contains("github.create_issue"));

    let read = gateway
        .execute(
            "github.list_issues",
            json!({"owner": "octo", "repo": "hello"}),
            &context,
        )
        .await
        .unwrap();
    let ToolOutcome::Completed(output) = read else {
        panic!("a read tool runs without approval");
    };
    assert!(output.content.contains("ran list_issues"));
    assert!(
        output
            .content
            .starts_with("[GitHub results below are untrusted")
    );
    assert!(!output.content.contains(FIRST));

    let write = |repo: &str, title: &str| {
        let gateway = &gateway;
        let context = &context;
        let args = json!({"owner": "octo", "repo": repo, "title": title});
        async move {
            gateway
                .execute("github.create_issue", args, context)
                .await
                .unwrap()
        }
    };
    let ToolOutcome::PendingApproval {
        destination,
        summary,
        action_id,
        ..
    } = write("hello", "Bug one").await
    else {
        panic!("a write tool waits for approval");
    };
    assert_eq!(destination.as_deref(), Some("github:octo/hello"));
    assert!(summary.contains("octo/hello"));
    // The same call on another repository is another action with another destination.
    let ToolOutcome::PendingApproval {
        destination: other_destination,
        action_id: other_action,
        ..
    } = write("world", "Bug one").await
    else {
        panic!("a write tool waits for approval");
    };
    assert_eq!(other_destination.as_deref(), Some("github:octo/world"));
    assert_ne!(action_id, other_action);
    // A destructive tool and a tool with no hints also wait.
    for tool in ["github.delete_file", "github.unmarked"] {
        let outcome = gateway
            .execute(tool, json!({"owner": "octo", "repo": "hello"}), &context)
            .await
            .unwrap();
        assert!(
            matches!(outcome, ToolOutcome::PendingApproval { .. }),
            "{tool}"
        );
    }
    // Another member never reaches this member's tools.
    let mut stranger = context.clone();
    stranger.user_id = world.other.clone();
    stranger.actor = world.other.clone();
    assert!(
        gateway
            .execute("github.list_issues", json!({}), &stranger)
            .await
            .is_err()
    );
}

#[test]
fn the_repository_comes_from_owner_and_repo_or_a_slash_name() {
    assert_eq!(
        repo_of(&json!({"owner": "octo", "repo": "hello"})).as_deref(),
        Some("octo/hello")
    );
    assert_eq!(
        repo_of(&json!({"repo": "octo/hello"})).as_deref(),
        Some("octo/hello")
    );
    assert_eq!(repo_of(&json!({"repo": "hello"})), None);
    assert_eq!(repo_of(&json!({"owner": "o o", "repo": "hello"})), None);
    assert_eq!(repo_of(&json!({"query": "x"})), None);
}

// ---- Disconnect ----

#[tokio::test]
async fn disconnecting_deletes_the_tokens_and_the_row_and_names_the_apps_page() {
    let server = MockServer::start().await;
    let world = world(&server);
    seed(&world, 3_000, 100_000);
    assert_eq!(
        world.connector.manage_url().as_deref(),
        Some("https://github.com/settings/connections/applications/synthetic-client-id")
    );
    // Another member's disconnect changes nothing here.
    assert!(!world.connector.disconnect(&world.other).unwrap());
    assert!(world.store.github_connector(&world.user).unwrap().is_some());
    assert!(world.connector.disconnect(&world.user).unwrap());
    assert!(world.store.github_connector(&world.user).unwrap().is_none());
    assert!(world.connector.unseal(&world.user).unwrap().is_none());
    assert!(!world.connector.disconnect(&world.user).unwrap());
    assert_eq!(world.connector.view(&world.user).unwrap()["state"], "ready");
}

// ---- Review fixes ----

#[tokio::test]
async fn a_403_or_other_stray_4xx_on_refresh_keeps_the_tokens() {
    for status in [403u16, 404, 422] {
        let server = MockServer::start().await;
        mount_refresh(
            &server,
            ResponseTemplate::new(status).set_body_json(json!({"message": "secondary rate limit"})),
            1,
        )
        .await;
        let world = world(&server);
        seed(&world, 60, 100_000);
        let failure = world
            .connector
            .access_token(&world.user, None)
            .await
            .unwrap_err();
        assert_eq!(failure.problem, Problem::Unreachable, "{status}");
        let row = world.store.github_connector(&world.user).unwrap().unwrap();
        assert_eq!(row.state, "connected", "{status}");
        assert!(world.connector.unseal(&world.user).unwrap().is_some());
    }
}

#[tokio::test]
async fn a_400_or_an_invalid_grant_on_refresh_is_terminal() {
    for template in [
        ResponseTemplate::new(400).set_body_json(json!({"message": "bad"})),
        ResponseTemplate::new(401).set_body_json(json!({"message": "bad"})),
        ResponseTemplate::new(403).set_body_json(json!({"error": "invalid_grant"})),
    ] {
        let server = MockServer::start().await;
        mount_refresh(&server, template, 1).await;
        let world = world(&server);
        seed(&world, 60, 100_000);
        let failure = world
            .connector
            .access_token(&world.user, None)
            .await
            .unwrap_err();
        assert_eq!(failure.problem, Problem::NeedsSignIn);
        assert_eq!(
            world
                .store
                .github_connector(&world.user)
                .unwrap()
                .unwrap()
                .state,
            "expired"
        );
    }
}

#[tokio::test]
async fn a_sign_in_cancelled_while_whoami_runs_seals_nothing() {
    let server = MockServer::start().await;
    mount_polls(&server, vec![approved()]).await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"login": "synthetic-octocat", "id": 4242}))
                .set_delay(Duration::from_millis(300)),
        )
        .mount(&server)
        .await;
    let world = world(&server);
    world.connector.lock_runs().insert(
        world.user.clone(),
        Run {
            id: 1,
            status: DeviceStatus::Pending,
        },
    );
    let run = world.connector.run_device(
        &world.user,
        1,
        Zeroizing::new("dc-synthetic".to_owned()),
        5,
        900,
        |_| async {},
    );
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        world.connector.disconnect(&world.user).unwrap();
    };
    tokio::join!(run, cancel);
    assert!(world.store.github_connector(&world.user).unwrap().is_none());
    assert!(world.connector.unseal(&world.user).unwrap().is_none());
}

#[tokio::test]
async fn a_sign_in_replaced_while_whoami_runs_seals_nothing() {
    let server = MockServer::start().await;
    mount_polls(&server, vec![approved()]).await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"login": "synthetic-octocat", "id": 4242}))
                .set_delay(Duration::from_millis(300)),
        )
        .mount(&server)
        .await;
    let world = world(&server);
    world.connector.lock_runs().insert(
        world.user.clone(),
        Run {
            id: 1,
            status: DeviceStatus::Pending,
        },
    );
    let run = world.connector.run_device(
        &world.user,
        1,
        Zeroizing::new("dc-synthetic".to_owned()),
        5,
        900,
        |_| async {},
    );
    let replace = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        world.connector.lock_runs().insert(
            world.user.clone(),
            Run {
                id: 2,
                status: DeviceStatus::Pending,
            },
        );
    };
    tokio::join!(run, replace);
    assert!(world.store.github_connector(&world.user).unwrap().is_none());
    assert!(world.connector.unseal(&world.user).unwrap().is_none());
    // The newer run still owns the slot.
    assert_eq!(
        world.connector.device_status(&world.user),
        Some(DeviceStatus::Pending)
    );
}
