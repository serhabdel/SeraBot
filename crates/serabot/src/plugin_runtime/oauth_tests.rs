//! Plugin runtime with a hosted MCP server that uses OAuth (v0.51, M3 and M4).
//! One wiremock server plays both the authorization server and the MCP server;
//! every token below is a synthetic fixture. Nothing leaves loopback.

use std::collections::HashSet;

use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;
use crate::gateway::{Gateway, ToolOutcome, ToolRegistry};
use crate::marketplace::{Auth, Field, HeaderSpec, OauthSpec, Permissions};
use crate::marketplace_auth::McpSignIn;
use crate::policy::Policy;

pub(crate) const FIRST_TOKEN: &str = "synthetic-access-1";
pub(crate) const SECOND_TOKEN: &str = "synthetic-access-2";

/// An MCP server that answers only a request carrying one exact header value.
struct Mcp {
    header: &'static str,
    expected: String,
}

impl Respond for Mcp {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let sent = request
            .headers
            .get(self.header)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if sent != self.expected {
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
                {"name": "lookup", "description": "Read a record",
                 "inputSchema": {"type": "object"},
                 "annotations": {"readOnlyHint": true}},
                {"name": "post_comment", "description": "Write a comment",
                 "inputSchema": {"type": "object"},
                 "annotations": {"readOnlyHint": false, "destructiveHint": false}},
                {"name": "delete_item", "description": "Delete a record",
                 "inputSchema": {"type": "object"},
                 "annotations": {"destructiveHint": true}},
                {"name": "liar", "description": "Claims both",
                 "inputSchema": {"type": "object"},
                 "annotations": {"readOnlyHint": true, "destructiveHint": true}},
            ]})),
            Some("tools/call") => {
                let name = body["params"]["name"].as_str().unwrap_or("?");
                reply(json!({"content": [{"type": "text", "text": format!("ran {name}")}]}))
            }
            _ => ResponseTemplate::new(202),
        }
    }
}

/// An authorization server and an MCP server in one. The MCP endpoint accepts
/// `accepted` as its bearer token. `refresh_ok` decides what a refresh request
/// gets: a fresh token, or the error body of a revoked grant.
pub(crate) async fn oauth_server(accepted: &str, refresh_ok: bool) -> MockServer {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-protected-resource/mcp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resource": format!("{base}/mcp"),
            "authorization_servers": [base],
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "registration_endpoint": format!("{base}/register"),
            "revocation_endpoint": format!("{base}/revoke"),
            "code_challenge_methods_supported": ["S256"],
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/register"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"client_id": "synthetic-client"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": FIRST_TOKEN,
            "refresh_token": "synthetic-refresh-1",
            "token_type": "Bearer",
            "expires_in": 3600,
        })))
        .mount(&server)
        .await;
    let refreshed = if refresh_ok {
        ResponseTemplate::new(200).set_body_json(json!({
            "access_token": SECOND_TOKEN,
            "refresh_token": "synthetic-refresh-2",
            "token_type": "Bearer",
            "expires_in": 3600,
        }))
    } else {
        ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"}))
    };
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(refreshed)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(Mcp {
            header: "authorization",
            expected: format!("Bearer {accepted}"),
        })
        .mount(&server)
        .await;
    server
}

/// How many requests the mock saw at `route`.
pub(crate) async fn hits(server: &MockServer, route: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path() == route)
        .count()
}

fn plugin(slug: &str, apps: Vec<App>) -> PluginManifest {
    PluginManifest {
        schema_version: crate::marketplace::SCHEMA_VERSION_V2,
        slug: slug.into(),
        name: "Fixture App".into(),
        tagline: "A synthetic app".into(),
        description: "A synthetic app for tests.".into(),
        category: "test".into(),
        icon_url: "https://example.invalid/icon.png".into(),
        version: "0.1.0".into(),
        min_server_version: "0.51.0".into(),
        prompts: vec![],
        apps,
        permissions: Permissions {
            egress: vec!["127.0.0.1".into()],
        },
        website_url: "https://example.invalid".into(),
        privacy_policy_url: "https://example.invalid/privacy".into(),
        terms_url: "https://example.invalid/terms".into(),
    }
}

pub(crate) fn oauth_manifest(server: &MockServer) -> PluginManifest {
    plugin(
        "fixture",
        vec![App {
            id: "main".into(),
            name: "Main".into(),
            server: Server::Http {
                url: format!("{}/mcp", server.uri()),
            },
            auth: vec![Auth {
                id: "oauth".into(),
                kind: AuthKind::Oauth,
                fields: vec![],
                oauth: Some(OauthSpec {
                    scopes: vec!["read".into()],
                    ..OauthSpec::default()
                }),
            }],
            tools: None,
            accounts: None,
        }],
    )
}

struct World {
    tools: PluginTools,
    signin: McpSignIn,
    store: Store,
    user: String,
    manifest: PluginManifest,
}

fn world(server: &MockServer) -> World {
    let store = Store::open_in_memory(384).unwrap();
    let users = Arc::new(UserSecrets::with_master_key(store.clone(), [5u8; 32]).unwrap());
    let user = store
        .create_user("microsoft:oauth-run", "run@example.example", "R")
        .unwrap()
        .id;
    let manifest = oauth_manifest(server);
    let net = OauthNet::loopback_for_tests();
    World {
        tools: PluginTools::new(store.clone(), users.clone(), vec![manifest.clone()])
            .with_net(net.clone()),
        signin: McpSignIn::new(store.clone(), users, net),
        store,
        user,
        manifest,
    }
}

impl World {
    fn install(&self) {
        self.store
            .marketplace_install_insert(&self.user, "fixture", "0.1.0")
            .unwrap();
        self.store
            .marketplace_install_set_enabled(&self.user, "fixture", true)
            .unwrap();
    }

    async fn sign_in(&self) {
        let started = self
            .signin
            .start(&self.user, &self.manifest, None, "default", None)
            .await
            .expect("start");
        self.signin
            .complete(&self.user, "fixture", "synthetic-code", &started.state)
            .await
            .expect("complete");
    }

    fn gateway(&self) -> Gateway {
        Gateway::new(
            self.store.clone(),
            Arc::new(Policy::empty()),
            ToolRegistry::default(),
            1,
            false,
        )
        .with_plugin_tools(self.tools.clone())
    }

    async fn context(&self, gateway: &Gateway) -> ToolContext {
        let task = self
            .store
            .enqueue_task(
                &self.user,
                "turn",
                "main",
                &json!({}),
                0,
                chrono::Utc::now(),
            )
            .unwrap();
        let mut context = ToolContext {
            user_id: self.user.clone(),
            actor: self.user.clone(),
            bot: "main".into(),
            task_id: Some(task.id),
            turn_id: None,
            conversation_id: None,
            allowed_tools: HashSet::new(),
            require_approval: HashSet::new(),
        };
        gateway.enable_plugin_tools(&mut context).await;
        context
    }
}

async fn run(gateway: &Gateway, context: &ToolContext, tool: &str) -> ToolOutcome {
    gateway
        .execute(&format!("plugin.fixture.{tool}"), json!({}), context)
        .await
        .unwrap_or_else(|error| panic!("{tool}: {error}"))
}

#[tokio::test]
async fn install_sign_in_list_and_call_with_approval_by_annotation() {
    let server = oauth_server(FIRST_TOKEN, true).await;
    let world = world(&server);
    world.install();

    // Installed but not signed in: nothing is offered, and the main bot is told.
    assert!(world.tools.specs_for(&world.user).await.is_empty());
    assert_eq!(
        world.tools.notes_for(&world.user),
        vec!["Fixture App needs sign-in".to_string()]
    );

    world.sign_in().await;
    let specs = world.tools.specs_for(&world.user).await;
    let names: Vec<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "plugin.fixture.lookup",
            "plugin.fixture.post_comment",
            "plugin.fixture.delete_item",
            "plugin.fixture.liar"
        ]
    );
    assert!(world.tools.notes_for(&world.user).is_empty());
    // The token is read at call time: it is in no spec the model sees.
    let shown = serde_json::to_string(&specs).unwrap();
    assert!(!shown.contains(FIRST_TOKEN) && !shown.contains("synthetic-refresh"));
    let effect = |name: &str| {
        specs
            .iter()
            .find(|spec| spec.name == name)
            .unwrap()
            .external_effect
    };
    assert!(!effect("plugin.fixture.lookup"));
    assert!(effect("plugin.fixture.post_comment"));

    let gateway = world.gateway();
    let context = world.context(&gateway).await;

    // A read tool runs without approval, and is audited.
    match run(&gateway, &context, "lookup").await {
        ToolOutcome::Completed(output) => assert_eq!(output.content, "ran lookup"),
        other => panic!("read tool should run: {other:?}"),
    }
    let audit = world.store.list_audit_for_user(&world.user, 50).unwrap();
    assert!(
        audit.iter().any(|entry| entry.event == "plugin.call"
            && entry.tool.as_deref() == Some("plugin.fixture.lookup")),
        "{audit:?}"
    );

    // A write tool, a destructive tool and a contradictory claim all wait.
    for tool in ["post_comment", "delete_item", "liar"] {
        assert!(
            matches!(
                run(&gateway, &context, tool).await,
                ToolOutcome::PendingApproval { .. }
            ),
            "{tool} should need approval"
        );
    }

    // Owner override: `allow` frees the write tool but never a destructive one;
    // `ask` makes a read tool ask.
    for (tool, mode) in [
        ("post_comment", "allow"),
        ("delete_item", "allow"),
        ("lookup", "ask"),
    ] {
        world
            .store
            .marketplace_tool_mode_set(&world.user, "fixture", tool, Some(mode))
            .unwrap();
    }
    assert!(matches!(
        run(&gateway, &context, "post_comment").await,
        ToolOutcome::Completed(_)
    ));
    assert!(matches!(
        run(&gateway, &context, "delete_item").await,
        ToolOutcome::PendingApproval { .. }
    ));
    assert!(matches!(
        run(&gateway, &context, "lookup").await,
        ToolOutcome::PendingApproval { .. }
    ));

    // Sign-out revokes at the server and deletes the account here.
    let revoked = world
        .signin
        .accounts()
        .forget_account(&world.user, "fixture", "default")
        .await
        .unwrap();
    assert!(revoked);
    assert_eq!(hits(&server, "/revoke").await, 1);
    world.tools.forget_cache(&world.user, "fixture");
    assert!(world.tools.specs_for(&world.user).await.is_empty());
    assert_eq!(
        world.tools.notes_for(&world.user),
        vec!["Fixture App needs sign-in".to_string()]
    );
}

#[tokio::test]
async fn a_401_refreshes_the_token_once_and_retries() {
    // The server only accepts the refreshed token; sign-in hands out the first.
    let server = oauth_server(SECOND_TOKEN, true).await;
    let world = world(&server);
    world.install();
    world.sign_in().await;

    let specs = world.tools.specs_for(&world.user).await;
    assert_eq!(specs.len(), 4);
    assert_eq!(
        hits(&server, "/token").await,
        2,
        "code exchange + one refresh"
    );
    let record = world
        .tools
        .oauth
        .load(&world.user, "fixture", "default")
        .unwrap()
        .unwrap();
    assert_eq!(record.access_token, SECOND_TOKEN);
    let rows = world
        .store
        .marketplace_accounts(&world.user, "fixture")
        .unwrap();
    assert_eq!(rows[0].state, "ready");
}

#[tokio::test]
async fn a_revoked_grant_drops_the_tokens_and_asks_for_sign_in() {
    let server = oauth_server(SECOND_TOKEN, false).await;
    let world = world(&server);
    world.install();
    world.sign_in().await;

    assert!(world.tools.specs_for(&world.user).await.is_empty());
    let rows = world
        .store
        .marketplace_accounts(&world.user, "fixture")
        .unwrap();
    assert_eq!(rows[0].state, "needs_sign_in");
    assert_eq!(
        world.tools.notes_for(&world.user),
        vec!["Fixture App needs sign-in".to_string()]
    );
    // Further turns do not hammer the token endpoint with a dead grant.
    let before = hits(&server, "/token").await;
    assert!(world.tools.specs_for(&world.user).await.is_empty());
    assert_eq!(hits(&server, "/token").await, before);
}

#[tokio::test]
async fn an_api_key_goes_in_the_header_the_manifest_names() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(Mcp {
            header: "x-fixture-key",
            expected: "synthetic-key".into(),
        })
        .mount(&server)
        .await;
    let mut manifest = plugin(
        "keyed",
        vec![App {
            id: "main".into(),
            name: "Main".into(),
            server: Server::Http {
                url: format!("{}/mcp", server.uri()),
            },
            auth: vec![Auth {
                id: "key".into(),
                kind: AuthKind::ApiKey,
                fields: vec![Field {
                    id: "api_key".into(),
                    label: "Key".into(),
                    secret_key: "api_key".into(),
                    hint: String::new(),
                    docs_url: None,
                    header: Some(HeaderSpec {
                        name: "x-fixture-key".into(),
                        scheme: HeaderScheme::Raw,
                    }),
                }],
                oauth: None,
            }],
            tools: None,
            accounts: None,
        }],
    );
    manifest.name = "Keyed".into();
    let store = Store::open_in_memory(384).unwrap();
    let users = Arc::new(UserSecrets::with_master_key(store.clone(), [5u8; 32]).unwrap());
    let user = store
        .create_user("microsoft:keyed", "keyed@example.example", "K")
        .unwrap()
        .id;
    store
        .marketplace_install_insert(&user, "keyed", "0.1.0")
        .unwrap();
    store
        .marketplace_install_set_enabled(&user, "keyed", true)
        .unwrap();
    let tools = PluginTools::new(store.clone(), users.clone(), vec![manifest])
        .with_net(OauthNet::loopback_for_tests());

    assert!(tools.specs_for(&user).await.is_empty());
    assert_eq!(
        tools.notes_for(&user),
        vec!["Keyed needs setup".to_string()]
    );

    users
        .set(&user, "marketplace.keyed.api_key", "synthetic-key")
        .unwrap();
    assert_eq!(tools.specs_for(&user).await.len(), 4);
    assert!(tools.notes_for(&user).is_empty());
}

#[tokio::test]
async fn every_http_app_is_used_and_names_stay_apart() {
    let first = oauth_server(FIRST_TOKEN, true).await;
    let second = oauth_server(FIRST_TOKEN, true).await;
    let mut manifest = oauth_manifest(&first);
    let mut other = oauth_manifest(&second).apps.remove(0);
    other.id = "other".into();
    manifest.apps.push(other);
    let store = Store::open_in_memory(384).unwrap();
    let users = Arc::new(UserSecrets::with_master_key(store.clone(), [5u8; 32]).unwrap());
    let user = store
        .create_user("microsoft:two-apps", "two@example.example", "T")
        .unwrap()
        .id;
    store
        .marketplace_install_insert(&user, "fixture", "0.1.0")
        .unwrap();
    store
        .marketplace_install_set_enabled(&user, "fixture", true)
        .unwrap();
    let net = OauthNet::loopback_for_tests();
    let tools = PluginTools::new(store.clone(), users.clone(), vec![manifest.clone()])
        .with_net(net.clone());
    let signin = McpSignIn::new(store, users, net);
    let started = signin
        .start(&user, &manifest, Some("main"), "default", None)
        .await
        .unwrap();
    signin
        .complete(&user, "fixture", "synthetic-code", &started.state)
        .await
        .unwrap();

    let specs = tools.specs_for(&user).await;
    assert_eq!(specs.len(), 8, "four tools from each app");
    assert!(
        specs
            .iter()
            .any(|spec| spec.name == "plugin.fixture.main.lookup")
    );
    assert!(
        specs
            .iter()
            .any(|spec| spec.name == "plugin.fixture.other.lookup")
    );
    assert!(
        tools
            .tool_for(&user, "plugin.fixture.other.lookup")
            .await
            .is_some()
    );
}

#[tokio::test]
async fn a_pending_sign_in_belongs_to_one_user_and_one_plugin_and_is_single_use() {
    let server = oauth_server(FIRST_TOKEN, true).await;
    let world = world(&server);
    world.install();
    let intruder = world
        .store
        .create_user("microsoft:intruder", "intruder@example.example", "I")
        .unwrap()
        .id;
    let started = world
        .signin
        .start(&world.user, &world.manifest, None, "default", Some("Work"))
        .await
        .unwrap();
    assert!(started.authorize_url.contains("code_challenge_method=S256"));
    assert!(started.authorize_url.contains("resource="));
    assert!(!started.authorize_url.contains("verifier"));
    // Another user, another plugin, or a made-up state: all unknown.
    for (user, slug, state) in [
        (intruder.as_str(), "fixture", started.state.as_str()),
        (world.user.as_str(), "other", started.state.as_str()),
        (
            world.user.as_str(),
            "fixture",
            "not-a-real-state-value-0000",
        ),
    ] {
        assert!(
            world
                .signin
                .complete(user, slug, "code", state)
                .await
                .is_err(),
            "{user} {slug} {state}"
        );
    }
    // None of those spent it: the owner can still finish, once.
    world
        .signin
        .complete(&world.user, "fixture", "synthetic-code", &started.state)
        .await
        .unwrap();
    assert!(
        world
            .signin
            .complete(&world.user, "fixture", "synthetic-code", &started.state)
            .await
            .is_err()
    );
    assert!(
        world
            .store
            .marketplace_accounts(&intruder, "fixture")
            .unwrap()
            .is_empty()
    );
    // A plugin that allows one account refuses another id.
    assert!(
        world
            .signin
            .start(&world.user, &world.manifest, None, "second", None)
            .await
            .is_err()
    );
}

#[test]
fn annotations_decide_approval_and_a_destructive_hint_cannot_be_overridden() {
    let read = |hints: Value| hints_of(&json!({"annotations": hints}));
    let plain = hints_of(&json!({}));
    let only = read(json!({"readOnlyHint": true}));
    let both = read(json!({"readOnlyHint": true, "destructiveHint": true}));
    let hurts = read(json!({"destructiveHint": true}));
    assert!(approval_needed(plain, None));
    assert!(!approval_needed(only, None));
    assert!(
        approval_needed(both, None),
        "a contradictory claim is destructive"
    );
    assert!(approval_needed(hurts, None));
    assert!(approval_needed(only, Some(ToolMode::Ask)));
    assert!(!approval_needed(plain, Some(ToolMode::Allow)));
    assert!(approval_needed(hurts, Some(ToolMode::Allow)));
    assert!(approval_needed(both, Some(ToolMode::Allow)));
}
