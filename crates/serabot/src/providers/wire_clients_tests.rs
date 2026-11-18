//! The transports beyond chat completions and the clients built for them:
//! Muse Code on the Responses API, Anthropic on Messages (API-key accounts and
//! the flag-gated Claude plan sign-in), and OpenCode's session header.
//! Synthetic fixtures only; wiremock stands in for every provider.

use std::time::Duration;

use super::*;
use crate::accounts::{AccountStatus, AddAccount, AuthKind, identity_fingerprint};
use crate::domain::{LlmMessage, Role, ToolCall};
use crate::gateway::ToolSpec;
use crate::llm_failure::{FailKind, classify_llm};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CLAUDE: &str = "claude";
const MUSE: &str = "muse";
const GO: &str = "go";

struct Env {
    credentials: UserProviderCredentials,
    accounts: Arc<Accounts>,
    store: Store,
    alice: String,
    bob: String,
}

fn env() -> Env {
    let store = Store::open_in_memory(384).unwrap();
    let users = Arc::new(UserSecrets::with_master_key(store.clone(), [9u8; 32]).unwrap());
    let server_secrets = SharedSecrets::new(crate::secrets::SecretStore::from_map(
        std::collections::BTreeMap::from([(
            "claude_client_id".to_owned(),
            "synthetic-claude-client".to_owned(),
        )]),
    ));
    let credentials = UserProviderCredentials::new(users, server_secrets, 10, 32_768);
    let accounts = credentials.refresher().accounts().clone();
    let alice = store
        .create_user("microsoft:wire-alice", "alice@example.example", "Alice")
        .unwrap()
        .id;
    let bob = store
        .create_user("microsoft:wire-bob", "bob@example.example", "Bob")
        .unwrap()
        .id;
    Env {
        credentials,
        accounts,
        store,
        alice,
        bob,
    }
}

fn claude_profile(server: &MockServer, plan_oauth: bool) -> ProviderConfig {
    ProviderConfig {
        kind: "anthropic".into(),
        base_url: format!("{}/v1", server.uri()),
        models: vec!["claude-sonnet-5-5".into()],
        data_egress_approved: true,
        catalog_fetch: true,
        oauth_client_id_secret: "claude_client_id".into(),
        plan_oauth,
        ..Default::default()
    }
}

fn add_claude_key(env: &Env, user: &str, key: &str) -> String {
    env.accounts
        .add_api_key(user, CLAUDE, "Work key", key)
        .unwrap()
        .account_id
}

fn add_claude_plan(env: &Env, user: &str, access: &str, refresh: &str) -> String {
    env.accounts
        .add(
            user,
            AddAccount {
                provider: CLAUDE.into(),
                label: "plan@example.example".into(),
                auth_kind: AuthKind::Oauth,
                fingerprint: identity_fingerprint("claude", "account:uuid-1"),
                identity: json!({"email": "plan@example.example"}),
                credential: ProviderCredential {
                    api_key: access.into(),
                    refresh_token: refresh.into(),
                    expires_at: None,
                    generation: None,
                },
                priority: 0,
            },
        )
        .unwrap()
        .account_id
}

fn sse(events: &[(&str, Value)]) -> ResponseTemplate {
    let mut out = String::new();
    for (name, data) in events {
        out.push_str(&format!("event: {name}\ndata: {data}\n\n"));
    }
    ResponseTemplate::new(200).set_body_raw(out, "text/event-stream")
}

fn claude_text(text: &str) -> ResponseTemplate {
    sse(&[
        (
            "message_start",
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 11, "output_tokens": 1}}}),
        ),
        (
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
        (
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
        ),
        (
            "message_delta",
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 5}}),
        ),
    ])
}

fn claude_tool_call() -> ResponseTemplate {
    sse(&[
        (
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        ),
        (
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "plan it"}}),
        ),
        (
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig-ONE=="}}),
        ),
        (
            "content_block_start",
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "toolu_01", "name": "lookup", "input": {}}}),
        ),
        (
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"q\":\"cats\"}"}}),
        ),
        (
            "message_delta",
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 9}}),
        ),
    ])
}

fn lookup_tool() -> ToolSpec {
    ToolSpec {
        name: "lookup".into(),
        description: "look something up".into(),
        parameters: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        external_effect: false,
    }
}

fn body_of(request: &Request) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

// ---- Anthropic API-key accounts (P9a) ----

#[tokio::test]
async fn an_api_key_account_streams_a_messages_completion() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-synthetic-1"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(claude_text("Bonjour"))
        .expect(1)
        .mount(&server)
        .await;
    let profile = claude_profile(&server, false);
    let account = add_claude_key(&env, &env.alice, "sk-ant-synthetic-1");
    let client = env
        .credentials
        .client_for_account(&env.alice, CLAUDE, &profile, &account)
        .unwrap();
    let reply = client
        .with_model("claude-sonnet-5-5")
        .complete(
            &[LlmMessage::system("be kind"), LlmMessage::user("hi")],
            &[],
            None,
            Some(0.3),
        )
        .await
        .unwrap();
    assert_eq!(reply.message.content.as_deref(), Some("Bonjour"));
    assert_eq!(reply.usage.prompt_tokens, 11);
    assert_eq!(reply.usage.completion_tokens, 5);
    assert_eq!(reply.finish_reason.as_deref(), Some("stop"));
    let requests = server.received_requests().await.unwrap();
    let body = body_of(&requests[0]);
    assert_eq!(body["system"], "be kind");
    assert_eq!(body["stream"], true);
    assert_eq!(body["max_tokens"], 32_768);
    // An API key is `x-api-key`, never a bearer, and has no OAuth beta.
    assert!(requests[0].headers.get("authorization").is_none());
    assert!(requests[0].headers.get("anthropic-beta").is_none());
}

#[tokio::test]
async fn a_tool_round_and_signed_thinking_survive_two_requests() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(claude_tool_call())
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(claude_text("It is cats."))
        .mount(&server)
        .await;
    let profile = claude_profile(&server, false);
    let account = add_claude_key(&env, &env.alice, "sk-ant-synthetic-2");
    let client = env
        .credentials
        .client_for_account(&env.alice, CLAUDE, &profile, &account)
        .unwrap()
        .with_model("claude-sonnet-5-5");
    let extra = json!({"thinking": {"type": "enabled", "budget_tokens": 4000}});
    let extra = extra.as_object().unwrap();
    let mut messages = vec![LlmMessage::user("what is it?")];
    let first = client
        .complete_with(&messages, &[lookup_tool()], None, None, Some(extra))
        .await
        .unwrap();
    assert_eq!(first.message.tool_calls.len(), 1);
    assert_eq!(first.message.tool_calls[0].name, "lookup");
    assert_eq!(first.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(first.reasoning.as_deref(), Some("plan it"));
    messages.push(first.message.clone());
    messages.push(LlmMessage {
        role: Role::Tool,
        content: Some("cats".into()),
        tool_call_id: Some(first.message.tool_calls[0].id.clone()),
        name: Some("lookup".into()),
        ..Default::default()
    });
    let second = client
        .complete_with(&messages, &[lookup_tool()], None, None, Some(extra))
        .await
        .unwrap();
    assert_eq!(second.message.content.as_deref(), Some("It is cats."));
    let requests = server.received_requests().await.unwrap();
    let replay = body_of(&requests[1]);
    // The thinking block comes back first, unchanged, ahead of the tool call.
    let assistant = &replay["messages"][1]["content"];
    assert_eq!(
        assistant[0],
        json!({"type": "thinking", "thinking": "plan it", "signature": "sig-ONE=="})
    );
    assert_eq!(assistant[1]["type"], "tool_use");
    assert_eq!(assistant[1]["id"], "toolu_01");
    assert_eq!(replay["messages"][2]["content"][0]["type"], "tool_result");
    assert_eq!(
        replay["messages"][2]["content"][0]["tool_use_id"],
        "toolu_01"
    );
    assert_eq!(replay["thinking"]["budget_tokens"], 4000);
    // Thinking on means no temperature.
    assert!(replay.get("temperature").is_none());
}

#[tokio::test]
async fn thinking_signatures_never_leave_anthropic() {
    // A call signed by Anthropic and replayed to Google or anyone else is not
    // sent as a signature.
    let signed = crate::wire::anthropic::encode_thinking(&[
        json!({"type": "thinking", "thinking": "x", "signature": "sig"}),
    ])
    .unwrap();
    let envelope = crate::cloud_code::build_cloud_code_envelope(
        "gemini-3.8-flash",
        "proj-synthetic",
        &[
            LlmMessage::user("go"),
            LlmMessage::assistant(
                None,
                vec![ToolCall {
                    id: "toolu_1".into(),
                    name: "lookup".into(),
                    arguments: json!({}),
                    reasoning_signature: Some(signed.clone()),
                }],
            ),
        ],
        &[lookup_tool()],
        100,
        None,
    )
    .unwrap();
    assert!(!envelope.to_string().contains(&signed));
}

#[tokio::test]
async fn anthropic_failures_are_typed_for_failover() {
    let env = env();
    let server = MockServer::start().await;
    let profile = claude_profile(&server, false);
    let account = add_claude_key(&env, &env.alice, "sk-ant-synthetic-3");
    let client = env
        .credentials
        .client_for_account(&env.alice, CLAUDE, &profile, &account)
        .unwrap()
        .with_model("claude-sonnet-5-5");
    let ask = |client: &LlmClient| {
        let client = client.clone();
        async move {
            client
                .complete(&[LlmMessage::user("hi")], &[], None, None)
                .await
                .unwrap_err()
        }
    };
    // 529 overloaded.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(529).set_body_json(json!({
            "type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let error = ask(&client).await;
    assert!(
        matches!(classify_llm(&error), Ok(FailKind::Overloaded)),
        "{error:?}"
    );
    // 429 with a retry hint keeps the hint for the hold.
    server.reset().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "1")
                .set_body_json(json!({
                    "type": "error", "error": {"type": "rate_limit_error", "message": "slow down"}
                })),
        )
        .mount(&server)
        .await;
    let error = ask(&client).await;
    match classify_llm(&error) {
        Ok(FailKind::RateLimited { retry_after }) => {
            assert_eq!(retry_after, Some(Duration::from_secs(1)));
        }
        other => panic!("{other:?} {error:?}"),
    }
    // 403 permission and 404 model.
    for (status, kind, expected) in [
        (403, "permission_error", FailKind::AccountForbidden),
        (404, "not_found_error", FailKind::ModelUnavailable),
    ] {
        server.reset().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "type": "error", "error": {"type": kind, "message": "no"}
            })))
            .mount(&server)
            .await;
        let error = ask(&client).await;
        assert_eq!(classify_llm(&error), Ok(expected), "{kind}");
    }
    // 413 is a context failure, not a switch.
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(413).set_body_json(json!({
            "type": "error", "error": {"type": "request_too_large", "message": "too big"}
        })))
        .mount(&server)
        .await;
    assert_eq!(
        classify_llm(&ask(&client).await),
        Ok(FailKind::ContextTooLong)
    );
    // An error event inside a 200 stream is typed the same way.
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[(
            "error",
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        )]))
        .mount(&server)
        .await;
    assert_eq!(classify_llm(&ask(&client).await), Ok(FailKind::Overloaded));
}

#[tokio::test]
async fn a_rejected_key_parks_only_that_account() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-bad"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}
        })))
        .mount(&server)
        .await;
    let profile = claude_profile(&server, false);
    let bad = add_claude_key(&env, &env.alice, "sk-ant-bad");
    let good = add_claude_key(&env, &env.alice, "sk-ant-good");
    let error = env
        .credentials
        .client_for_account(&env.alice, CLAUDE, &profile, &bad)
        .unwrap()
        .with_model("claude-sonnet-5-5")
        .complete(&[LlmMessage::user("hi")], &[], None, None)
        .await
        .unwrap_err();
    assert!(matches!(
        classify_llm(&error),
        Ok(FailKind::CredentialsRejected)
    ));
    let status = |account: &str| {
        env.accounts
            .get(&env.alice, CLAUDE, account)
            .unwrap()
            .unwrap()
            .status
    };
    assert_eq!(status(&bad), AccountStatus::NeedsReauth);
    assert_eq!(status(&good), AccountStatus::Ok);
}

#[tokio::test]
async fn one_member_cannot_build_a_client_for_anothers_key() {
    let env = env();
    let server = MockServer::start().await;
    let profile = claude_profile(&server, false);
    let alices = add_claude_key(&env, &env.alice, "sk-ant-alice-only");
    let denied = env
        .credentials
        .client_for_account(&env.bob, CLAUDE, &profile, &alices);
    assert!(matches!(denied, Err(SeraError::Forbidden(_))));
    assert!(server.received_requests().await.unwrap().is_empty());
    // The same key under another account id cannot be forged either.
    assert!(env.accounts.list(&env.bob, CLAUDE).unwrap().is_empty());
}

#[tokio::test]
async fn a_thinking_refusal_is_retried_once_without_thinking() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "type": "error",
            "error": {"type": "invalid_request_error", "message": "thinking.type.enabled is not supported for this model"}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(claude_text("ok"))
        .mount(&server)
        .await;
    let profile = claude_profile(&server, false);
    let account = add_claude_key(&env, &env.alice, "sk-ant-synthetic-4");
    let client = env
        .credentials
        .client_for_account(&env.alice, CLAUDE, &profile, &account)
        .unwrap()
        .with_model("claude-sonnet-5-5");
    let extra = json!({"thinking": {"type": "enabled", "budget_tokens": 4000}});
    let reply = client
        .complete_with(
            &[LlmMessage::user("hi")],
            &[],
            None,
            None,
            extra.as_object(),
        )
        .await
        .unwrap();
    assert_eq!(reply.message.content.as_deref(), Some("ok"));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(body_of(&requests[0]).get("thinking").is_some());
    assert!(body_of(&requests[1]).get("thinking").is_none());
    // Any other 400 is final and not repeated.
    let other = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "type": "error", "error": {"type": "invalid_request_error", "message": "messages: bad"}
        })))
        .mount(&other)
        .await;
    let profile = claude_profile(&other, false);
    let error = env
        .credentials
        .client_for_account(&env.alice, CLAUDE, &profile, &account)
        .unwrap()
        .with_model("claude-sonnet-5-5")
        .complete(&[LlmMessage::user("hi")], &[], None, None)
        .await
        .unwrap_err();
    assert!(matches!(error, SeraError::Upstream { status: 400, .. }));
    assert_eq!(other.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_model_list_is_read_page_by_page_with_the_key() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("x-api-key", "sk-ant-synthetic-5"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(query_param("after_id", "claude-b"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"type": "model", "id": "claude-c", "display_name": "Claude C"}],
            "has_more": false, "last_id": "claude-c"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("x-api-key", "sk-ant-synthetic-5"))
        .and(query_param("limit", "1000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"type": "model", "id": "claude-a", "capabilities": {"thinking": {"supported": true}}},
                {"type": "model", "id": "claude-b"}
            ],
            "has_more": true, "last_id": "claude-b"
        })))
        .mount(&server)
        .await;
    let profile = claude_profile(&server, false);
    let account = add_claude_key(&env, &env.alice, "sk-ant-synthetic-5");
    let client = env
        .credentials
        .client_for_account(&env.alice, CLAUDE, &profile, &account)
        .unwrap();
    let catalog = resolve_catalog(
        "anthropic",
        &resolve_base_url("anthropic", &profile.base_url).unwrap(),
    );
    // Only the documented host is read for the official kind; a mock on
    // loopback is allowed for fixtures.
    let catalog = catalog.unwrap();
    assert_eq!(catalog.url, format!("{}/v1/models", server.uri()));
    let models = client
        .fetch_model_catalog(&catalog.url, catalog.strict)
        .await
        .unwrap();
    assert_eq!(models.len(), 3);
    assert!(models["claude-a"].tags.contains(&"reasoning".to_owned()));
    assert_eq!(models["claude-c"].name.as_deref(), Some("Claude C"));
    // The tags decide which thinking levels a model offers.
    assert!(
        !crate::thinking::levels_for("anthropic", "claude-a", &models["claude-a"].tags).is_empty()
    );
    // A rejected key is a status the router can read; no body is echoed.
    let refused = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_string("sk-ant-secret-echo"))
        .mount(&refused)
        .await;
    let profile = claude_profile(&refused, false);
    let error = env
        .credentials
        .client_for_account(&env.alice, CLAUDE, &profile, &account)
        .unwrap()
        .fetch_model_catalog(&format!("{}/v1/models", refused.uri()), false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("status 401"));
    assert!(!error.to_string().contains("secret-echo"));
}

#[test]
fn anthropic_is_a_builtin_kind_pinned_to_its_host() {
    assert_eq!(canonical_kind("anthropic").unwrap(), "anthropic");
    assert_eq!(
        resolve_base_url("anthropic", "").unwrap(),
        "https://api.anthropic.com/v1"
    );
    assert_eq!(
        resolve_catalog("anthropic", "https://api.anthropic.com/v1")
            .unwrap()
            .url,
        "https://api.anthropic.com/v1/models"
    );
    assert!(resolve_catalog("anthropic", "https://evil.example/v1").is_err());
    // An API-key kind: sign-in is not its default.
    assert!(matches!(
        auth_plan("anthropic", "").unwrap(),
        AuthPlan::ApiKey
    ));
}

// ---- Claude plan sign-in clients (P17) ----

#[tokio::test]
async fn a_plan_account_sends_a_bearer_and_the_oauth_beta() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer plan-access-1"))
        .and(header("anthropic-beta", "oauth-2025-04-20"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(header(
            "user-agent",
            "anthropic-sdk-typescript/0.112.1 userOAuthProvider",
        ))
        .respond_with(claude_text("from the plan"))
        .expect(1)
        .mount(&server)
        .await;
    let profile = claude_profile(&server, true);
    let account = add_claude_plan(&env, &env.alice, "plan-access-1", "plan-refresh-1");
    let client = env
        .credentials
        .client_for_account(&env.alice, CLAUDE, &profile, &account)
        .unwrap()
        .with_model("claude-sonnet-5-5");
    let reply = client
        .complete(&[LlmMessage::user("hi")], &[], None, None)
        .await
        .unwrap();
    assert_eq!(reply.message.content.as_deref(), Some("from the plan"));
    let requests = server.received_requests().await.unwrap();
    assert!(requests[0].headers.get("x-api-key").is_none());
}

#[tokio::test]
async fn a_plan_account_is_refused_while_the_flag_is_off() {
    let env = env();
    let server = MockServer::start().await;
    let account = add_claude_plan(&env, &env.alice, "plan-access-2", "plan-refresh-2");
    let off = claude_profile(&server, false);
    let refused = env
        .credentials
        .client_for_account(&env.alice, CLAUDE, &off, &account);
    assert!(matches!(refused, Err(SeraError::Forbidden(_))));
    // Nothing was sent and the account row is untouched.
    assert!(server.received_requests().await.unwrap().is_empty());
    assert_eq!(
        env.accounts
            .get(&env.alice, CLAUDE, &account)
            .unwrap()
            .unwrap()
            .status,
        AccountStatus::Ok
    );
}

#[tokio::test]
async fn a_plan_credential_is_never_usable_as_the_default_or_a_grant() {
    let env = env();
    let server = MockServer::start().await;
    let profile = claude_profile(&server, true);
    // The legacy default row is what a grant shares. A plan token there is
    // refused even with the flag on.
    let credential = ProviderCredential {
        api_key: "plan-access-3".into(),
        refresh_token: "plan-refresh-3".into(),
        ..Default::default()
    };
    let result = open_provider_client(
        &profile,
        &credential,
        &env.credentials.server_secrets,
        10,
        4096,
        CredentialOwner {
            users: &env.credentials.users,
            user_id: &env.alice,
            provider: CLAUDE,
            account_id: DEFAULT_ACCOUNT,
            refresher: None,
        },
    );
    assert!(matches!(result, Err(SeraError::Forbidden(_))));
}

#[tokio::test]
async fn a_plan_refresh_rotates_the_token_once_for_eight_turns() {
    let env = env();
    let server = MockServer::start().await;
    // The old refresh token is good once; a second spend is the failure.
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(header("anthropic-beta", "oauth-2025-04-20"))
        .and(header(
            "user-agent",
            "anthropic-sdk-typescript/0.112.1 userOAuthProvider",
        ))
        .and(wiremock::matchers::body_json(json!({
            "grant_type": "refresh_token",
            "client_id": "synthetic-claude-client",
            "refresh_token": "plan-refresh-old"
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(150))
                .set_body_json(json!({
                    "access_token": "plan-access-new",
                    "refresh_token": "plan-refresh-new",
                    "expires_in": 28800
                })),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer plan-access-old"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer plan-access-new"))
        .respond_with(claude_text("renewed"))
        .mount(&server)
        .await;
    let account = add_claude_plan(&env, &env.alice, "plan-access-old", "plan-refresh-old");
    let mut turns = Vec::new();
    for _ in 0..8 {
        let client = LlmClient::new(format!("{}/v1", server.uri()), "plan-access-old", 10, 4096)
            .unwrap()
            .with_model("claude-sonnet-5-5")
            .with_oauth_unpinned(OAuthMaterial {
                style: OAuthStyle::ClaudeRefresh,
                refresh_token: "plan-refresh-old".into(),
                token_url: format!("{}/oauth/token", server.uri()),
                client_id: "synthetic-claude-client".into(),
                client_secret: String::new(),
            })
            .unwrap()
            .with_account_refresh(
                AccountRefreshTarget {
                    refresher: env.credentials.refresher().clone(),
                    user_id: env.alice.clone(),
                    provider: CLAUDE.into(),
                    account_id: account.clone(),
                },
                None,
                None,
            )
            .unwrap()
            .with_wire(Wire::AnthropicPlan);
        turns.push(tokio::spawn(async move {
            client
                .complete(&[LlmMessage::user("hi")], &[], None, None)
                .await
        }));
    }
    for turn in turns {
        assert_eq!(
            turn.await.unwrap().unwrap().message.content.as_deref(),
            Some("renewed")
        );
    }
    let token_calls = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/oauth/token")
        .count();
    assert_eq!(token_calls, 1);
    // The rotated pair is what the account holds now.
    let stored = env
        .accounts
        .credential(&env.alice, CLAUDE, &account)
        .unwrap()
        .unwrap();
    assert_eq!(stored.api_key, "plan-access-new");
    assert_eq!(stored.refresh_token, "plan-refresh-new");
    assert!(stored.expires_at.is_some());
}

#[tokio::test]
async fn a_refused_plan_refresh_parks_the_account_and_keeps_the_row() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let account = add_claude_plan(&env, &env.alice, "plan-access-x", "plan-refresh-x");
    let client = LlmClient::new(format!("{}/v1", server.uri()), "plan-access-x", 10, 4096)
        .unwrap()
        .with_model("claude-sonnet-5-5")
        .with_oauth_unpinned(OAuthMaterial {
            style: OAuthStyle::ClaudeRefresh,
            refresh_token: "plan-refresh-x".into(),
            token_url: format!("{}/oauth/token", server.uri()),
            client_id: "synthetic-claude-client".into(),
            client_secret: String::new(),
        })
        .unwrap()
        .with_account_refresh(
            AccountRefreshTarget {
                refresher: env.credentials.refresher().clone(),
                user_id: env.alice.clone(),
                provider: CLAUDE.into(),
                account_id: account.clone(),
            },
            None,
            None,
        )
        .unwrap()
        .with_wire(Wire::AnthropicPlan);
    let error = client
        .complete(&[LlmMessage::user("hi")], &[], None, None)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SeraError::Rejected {
            code: "needs_reauth",
            ..
        }
    ));
    let row = env
        .accounts
        .get(&env.alice, CLAUDE, &account)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, AccountStatus::NeedsReauth);
    // The credential is kept so the member can sign in again over it.
    assert!(
        env.accounts
            .credential(&env.alice, CLAUDE, &account)
            .unwrap()
            .is_some()
    );
    // Another member's accounts are untouched.
    assert!(env.accounts.list(&env.bob, CLAUDE).unwrap().is_empty());
    let _ = &env.store;
}

// ---- Muse Code on the Responses transport (P7b) ----

fn muse_profile(server: &MockServer) -> ProviderConfig {
    ProviderConfig {
        kind: "muse-code".into(),
        base_url: format!("{}/v1", server.uri()),
        models: vec!["muse-spark-1.3".into()],
        data_egress_approved: true,
        ..Default::default()
    }
}

fn add_muse(env: &Env, meta_token: &str) -> String {
    env.accounts
        .add(
            &env.alice,
            AddAccount {
                provider: MUSE.into(),
                label: "m@example.example".into(),
                auth_kind: AuthKind::Device,
                fingerprint: identity_fingerprint("muse-code", "m@example.example"),
                identity: json!({"email": "m@example.example"}),
                credential: ProviderCredential {
                    api_key: "LLM|key-one".into(),
                    refresh_token: meta_token.into(),
                    expires_at: None,
                    generation: None,
                },
                priority: 0,
            },
        )
        .unwrap()
        .account_id
}

fn responses_text(text: &str) -> ResponseTemplate {
    sse(&[
        (
            "response.output_text.delta",
            json!({"type": "response.output_text.delta", "delta": text}),
        ),
        (
            "response.completed",
            json!({"type": "response.completed", "response": {
                "usage": {"input_tokens": 20, "output_tokens": 4}}}),
        ),
    ])
}

#[tokio::test]
async fn muse_chats_on_the_responses_endpoint_with_reasoning() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer LLM|key-one"))
        .and(header("x-api-version", "1.0.0"))
        .respond_with(responses_text("Salut"))
        .expect(1)
        .mount(&server)
        .await;
    let profile = muse_profile(&server);
    let account = add_muse(&env, "meta-token");
    let client = env
        .credentials
        .client_for_account(&env.alice, MUSE, &profile, &account)
        .unwrap()
        .with_model("muse-spark-1.3");
    let fields = crate::thinking::wire_fields("muse-code", "muse-spark-1.3", &[], "high").unwrap();
    let reply = client
        .complete_with(
            &[LlmMessage::system("sys"), LlmMessage::user("hi")],
            &[lookup_tool()],
            None,
            Some(0.7),
            Some(&fields),
        )
        .await
        .unwrap();
    assert_eq!(reply.message.content.as_deref(), Some("Salut"));
    assert_eq!(reply.usage.prompt_tokens, 20);
    let body = body_of(&server.received_requests().await.unwrap()[0]);
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["instructions"], "sys");
    assert_eq!(
        body["reasoning"],
        json!({"effort": "high", "summary": "auto"})
    );
    assert_eq!(body["tools"][0]["name"], "lookup");
    // Muse takes no temperature, cache key or include list.
    for forbidden in ["temperature", "prompt_cache_key", "include"] {
        assert!(body.get(forbidden).is_none(), "{forbidden}");
    }
    assert!(body.get("messages").is_none());
}

#[tokio::test]
async fn a_responses_tool_round_prunes_orphans() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(sse(&[
            (
                "response.output_item.done",
                json!({"type": "response.output_item.done", "output_index": 0,
                       "item": {"type": "function_call", "id": "fc_1", "call_id": "call_7", "name": "lookup", "arguments": "{\"q\":\"x\"}"}}),
            ),
            ("response.completed", json!({"type": "response.completed", "response": {}})),
        ]))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(responses_text("done"))
        .mount(&server)
        .await;
    let profile = muse_profile(&server);
    let account = add_muse(&env, "meta-token");
    let client = env
        .credentials
        .client_for_account(&env.alice, MUSE, &profile, &account)
        .unwrap()
        .with_model("muse-spark-1.3");
    let mut messages = vec![LlmMessage::user("go")];
    let first = client
        .complete(&messages, &[lookup_tool()], None, None)
        .await
        .unwrap();
    assert_eq!(first.message.tool_calls[0].id, "call_7");
    messages.push(first.message.clone());
    messages.push(LlmMessage {
        role: Role::Tool,
        content: Some("result".into()),
        tool_call_id: Some("call_7".into()),
        ..Default::default()
    });
    messages.push(LlmMessage {
        role: Role::Tool,
        content: Some("orphan".into()),
        tool_call_id: Some("never-sent".into()),
        ..Default::default()
    });
    client
        .complete(&messages, &[lookup_tool()], None, None)
        .await
        .unwrap();
    let second = body_of(&server.received_requests().await.unwrap()[1]);
    let text = second.to_string();
    assert!(text.contains("function_call_output"));
    assert!(text.contains("call_7"));
    assert!(!text.contains("orphan"));
}

#[tokio::test]
async fn a_responses_400_is_repaired_once_by_dropping_what_it_blamed() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {"type": "invalid_request_error", "message": "Unsupported parameter: reasoning.effort"}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(responses_text("fine"))
        .mount(&server)
        .await;
    let profile = muse_profile(&server);
    let account = add_muse(&env, "meta-token");
    let client = env
        .credentials
        .client_for_account(&env.alice, MUSE, &profile, &account)
        .unwrap()
        .with_model("muse-spark-1.3");
    let fields =
        crate::thinking::wire_fields("muse-code", "muse-spark-1.3", &[], "medium").unwrap();
    let reply = client
        .complete_with(&[LlmMessage::user("hi")], &[], None, None, Some(&fields))
        .await
        .unwrap();
    assert_eq!(reply.message.content.as_deref(), Some("fine"));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(body_of(&requests[0]).get("reasoning").is_some());
    assert!(body_of(&requests[1]).get("reasoning").is_none());
    // A 400 nobody can repair is returned after one request.
    let other = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {"message": "model not found"}
        })))
        .mount(&other)
        .await;
    let profile = muse_profile(&other);
    let error = env
        .credentials
        .client_for_account(&env.alice, MUSE, &profile, &account)
        .unwrap()
        .with_model("muse-spark-1.3")
        .complete_with(&[LlmMessage::user("hi")], &[], None, None, Some(&fields))
        .await
        .unwrap_err();
    assert!(matches!(error, SeraError::Upstream { status: 400, .. }));
    assert_eq!(other.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_muse_401_re_mints_without_onboarding_and_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer LLM|stale"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer LLM|fresh"))
        .respond_with(responses_text("back again"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/muse-code/key"))
        .and(header("authorization", "Bearer meta-token"))
        .and(wiremock::matchers::body_json(json!({})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"api_key": "LLM|fresh"})))
        .expect(1)
        .mount(&server)
        .await;
    let client = LlmClient::new(format!("{}/v1", server.uri()), "LLM|stale", 10, 4096)
        .unwrap()
        .with_model("muse-spark-1.3")
        .with_oauth_unpinned(OAuthMaterial {
            style: OAuthStyle::MuseMint,
            refresh_token: "meta-token".into(),
            token_url: format!("{}/muse-code/key", server.uri()),
            client_id: String::new(),
            client_secret: String::new(),
        })
        .unwrap()
        .with_header("x-api-version", MUSE_API_VERSION)
        .with_wire(Wire::Responses);
    let reply = client
        .complete(&[LlmMessage::user("hi")], &[], None, None)
        .await
        .unwrap();
    assert_eq!(reply.message.content.as_deref(), Some("back again"));
}

// ---- OpenCode (P10) ----

fn go_profile(server: &MockServer) -> ProviderConfig {
    ProviderConfig {
        kind: "opencode-go".into(),
        base_url: format!("{}/zen/go/v1", server.uri()),
        models: vec!["glm-5.3-flash".into()],
        data_egress_approved: true,
        catalog_fetch: true,
        ..Default::default()
    }
}

fn zen_profile(server: &MockServer) -> ProviderConfig {
    ProviderConfig {
        kind: "opencode-zen".into(),
        base_url: format!("{}/zen/v1", server.uri()),
        models: vec!["claude-fable-5-1".into()],
        data_egress_approved: true,
        catalog_fetch: true,
        ..Default::default()
    }
}

fn chat_ok() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{"message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}]
    }))
}

fn sessions(requests: &[Request]) -> Vec<Option<String>> {
    requests
        .iter()
        .map(|request| {
            request
                .headers
                .get("x-opencode-session")
                .map(|value| value.to_str().unwrap().to_owned())
        })
        .collect()
}

#[test]
fn the_session_id_has_the_documented_shape_and_is_stable() {
    let id = crate::llm::opencode_session_id("conversation-1");
    assert!(id.starts_with("ses_"));
    assert_eq!(id.len(), 4 + 12 + 14);
    let (hex_part, tail) = id[4..].split_at(12);
    assert!(
        hex_part
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert!(tail.chars().all(|c| c.is_ascii_alphanumeric()));
    assert_eq!(id, crate::llm::opencode_session_id("conversation-1"));
    assert_ne!(id, crate::llm::opencode_session_id("conversation-2"));
}

#[tokio::test]
async fn go_requests_all_carry_the_session_header_and_retries_reuse_it() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/zen/go/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/zen/go/v1/chat/completions"))
        .respond_with(chat_ok())
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/zen/go/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "glm-5.3-flash"}]
        })))
        .mount(&server)
        .await;
    let profile = go_profile(&server);
    let account = env
        .accounts
        .add_api_key(&env.alice, GO, "Go key", "sk-go-synthetic")
        .unwrap()
        .account_id;
    let client = env
        .credentials
        .client_for_account(&env.alice, GO, &profile, &account)
        .unwrap()
        .with_model("glm-5.3-flash");
    // Inside a conversation the id comes from it, so a retry reuses it.
    crate::llm::with_conversation("conv-A", async {
        client
            .complete(&[LlmMessage::user("hi")], &[], None, None)
            .await
            .unwrap();
    })
    .await;
    // A catalog read and a call outside any conversation carry one too.
    client.list_models().await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let ids = sessions(&requests);
    assert!(ids.iter().all(Option::is_some), "{ids:?}");
    // 503 then the retry, both on the conversation's id.
    let chat: Vec<_> = requests
        .iter()
        .zip(&ids)
        .filter(|(request, _)| request.url.path().ends_with("/chat/completions"))
        .map(|(_, id)| id.clone().unwrap())
        .collect();
    assert_eq!(chat.len(), 2);
    assert_eq!(chat[0], chat[1]);
    assert_eq!(chat[0], crate::llm::opencode_session_id("conv-A"));
    // Another conversation gets another id.
    crate::llm::with_conversation("conv-B", async {
        client
            .complete(&[LlmMessage::user("hi")], &[], None, None)
            .await
            .unwrap();
    })
    .await;
    let last = sessions(&server.received_requests().await.unwrap());
    assert_eq!(
        last.last().unwrap().as_deref(),
        Some(crate::llm::opencode_session_id("conv-B").as_str())
    );
}

#[tokio::test]
async fn zen_requests_carry_no_session_header() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/zen/v1/chat/completions"))
        .respond_with(chat_ok())
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/zen/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "claude-fable-5-1"}]
        })))
        .mount(&server)
        .await;
    let profile = zen_profile(&server);
    let account = env
        .accounts
        .add_api_key(&env.alice, "zen", "Zen key", "sk-zen-synthetic")
        .unwrap()
        .account_id;
    let client = env
        .credentials
        .client_for_account(&env.alice, "zen", &profile, &account)
        .unwrap()
        .with_model("claude-fable-5-1");
    crate::llm::with_conversation("conv-A", async {
        client
            .complete(&[LlmMessage::user("hi")], &[], None, None)
            .await
            .unwrap();
    })
    .await;
    client.list_models().await.unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(sessions(&requests).iter().all(Option::is_none));
    for request in &requests {
        assert_eq!(
            request.headers.get("authorization").unwrap(),
            "Bearer sk-zen-synthetic"
        );
    }
}

#[test]
fn opencode_kinds_are_builtins_pinned_to_their_host() {
    assert_eq!(
        resolve_base_url("opencode-go", "").unwrap(),
        "https://opencode.ai/zen/go/v1"
    );
    assert_eq!(
        resolve_base_url("opencode-zen", "").unwrap(),
        "https://opencode.ai/zen/v1"
    );
    assert_eq!(
        resolve_catalog("opencode-go", "https://opencode.ai/zen/go/v1")
            .unwrap()
            .url,
        "https://opencode.ai/zen/go/v1/models"
    );
    assert_eq!(
        resolve_catalog("opencode-zen", "https://opencode.ai/zen/v1")
            .unwrap()
            .url,
        "https://opencode.ai/zen/v1/models"
    );
    assert!(resolve_catalog("opencode-go", "https://evil.example/zen/go/v1").is_err());
    // Nous has no kind of its own: it is the plain OpenAI-compatible kind.
    assert_eq!(
        canonical_kind("openai-compatible").unwrap(),
        "openai-compatible"
    );
    assert!(canonical_kind("nous").is_err());
}

// ---- Codex (ChatGPT plan) on the Responses transport ----

const CODEX: &str = "codex";

fn codex_profile(server: &MockServer, plan_oauth: bool) -> ProviderConfig {
    ProviderConfig {
        kind: "openai-codex".into(),
        base_url: format!("{}/backend-api/codex", server.uri()),
        models: crate::auth_flows::codex::MODELS
            .iter()
            .map(|model| (*model).to_owned())
            .collect(),
        data_egress_approved: true,
        plan_oauth,
        ..Default::default()
    }
}

fn add_codex(
    env: &Env,
    user: &str,
    workspace: Option<&str>,
    access: &str,
    refresh: &str,
) -> String {
    let mut identity = json!({"email": "codex@example.example"});
    if let Some(workspace) = workspace {
        identity["chatgpt_account_id"] = json!(workspace);
    }
    env.accounts
        .add(
            user,
            AddAccount {
                provider: CODEX.into(),
                label: "codex@example.example".into(),
                auth_kind: AuthKind::Oauth,
                fingerprint: identity_fingerprint("openai-codex", "account:acct-1"),
                identity,
                credential: ProviderCredential {
                    api_key: access.into(),
                    refresh_token: refresh.into(),
                    expires_at: None,
                    generation: None,
                },
                priority: 0,
            },
        )
        .unwrap()
        .account_id
}

#[test]
fn codex_is_a_builtin_oauth_kind_on_the_chatgpt_backend() {
    assert_eq!(canonical_kind("openai-codex").unwrap(), "openai-codex");
    assert_eq!(
        resolve_base_url("openai-codex", "").unwrap(),
        "https://chatgpt.com/backend-api/codex"
    );
    // Sign-in is its default, and its refresh is pinned to OpenAI's token endpoint.
    match auth_plan("openai-codex", "").unwrap() {
        AuthPlan::OAuth {
            style,
            token_url,
            client_id_fallback,
        } => {
            assert_eq!(style, OAuthStyle::CodexRefresh);
            assert_eq!(token_url, "https://auth.openai.com/oauth/token");
            assert_eq!(client_id_fallback, Some("app_EMoamEEZ73f0CkXaXp7hrann"));
        }
        AuthPlan::ApiKey => panic!("codex signs in"),
    }
    assert!(
        enforce_oauth_target(
            OAuthStyle::CodexRefresh,
            "https://auth.openai.com/oauth/token"
        )
        .is_ok()
    );
    assert!(
        enforce_oauth_target(OAuthStyle::CodexRefresh, "https://evil.example/oauth/token").is_err()
    );
    assert!(
        enforce_oauth_target(
            OAuthStyle::CodexRefresh,
            "https://auth.openai.com/oauth/other"
        )
        .is_err()
    );
}

#[tokio::test]
async fn a_codex_account_chats_with_the_workspace_and_cli_headers() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .and(header("authorization", "Bearer codex-access-1"))
        .and(header("chatgpt-account-id", "acct-1"))
        .and(header("openai-beta", "responses=v1"))
        .and(header("originator", "serabot"))
        .and(header("session_id", "conv-A"))
        .respond_with(responses_text("from the plan"))
        .expect(1)
        .mount(&server)
        .await;
    // Bob's account names no workspace, so the strict mock above never matches.
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .respond_with(responses_text("bare"))
        .mount(&server)
        .await;
    let profile = codex_profile(&server, true);
    let account = add_codex(
        &env,
        &env.alice,
        Some("acct-1"),
        "codex-access-1",
        "codex-refresh-1",
    );
    let client = env
        .credentials
        .client_for_account(&env.alice, CODEX, &profile, &account)
        .unwrap()
        .with_model("gpt-6.1-sol");
    let fields = crate::thinking::wire_fields("openai-codex", "gpt-6.1-sol", &[], "high").unwrap();
    let reply = crate::llm::with_conversation("conv-A", async {
        client
            .complete_with(
                &[LlmMessage::system("sys"), LlmMessage::user("hi")],
                &[lookup_tool()],
                None,
                Some(0.7),
                Some(&fields),
            )
            .await
            .unwrap()
    })
    .await;
    assert_eq!(reply.message.content.as_deref(), Some("from the plan"));
    let body = body_of(&server.received_requests().await.unwrap()[0]);
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["instructions"], "sys");
    assert_eq!(body["prompt_cache_key"], "conv-A");
    assert_eq!(body["model"], "gpt-6.1-sol");
    assert_eq!(
        body["reasoning"],
        json!({"effort": "high", "summary": "auto"})
    );
    assert!(body.get("temperature").is_none());
    // No workspace named: the header is simply left off, and a request outside
    // a conversation still carries one stable session id.
    let bare = add_codex(&env, &env.bob, None, "codex-access-2", "codex-refresh-2");
    let client = env
        .credentials
        .client_for_account(&env.bob, CODEX, &profile, &bare)
        .unwrap()
        .with_model("gpt-6-luna");
    for _ in 0..2 {
        client
            .complete(&[LlmMessage::user("hi")], &[], None, None)
            .await
            .unwrap();
    }
    let requests = server.received_requests().await.unwrap();
    assert!(requests[1].headers.get("chatgpt-account-id").is_none());
    let sessions: Vec<_> = requests[1..]
        .iter()
        .map(|request| request.headers.get("session_id").unwrap().clone())
        .collect();
    assert_eq!(sessions[0], sessions[1]);
    assert_ne!(sessions[0], "conv-A");
    // Even with no system message the backend gets the `instructions` it needs.
    assert!(body_of(&requests[1]).get("instructions").is_some());
}

#[tokio::test]
async fn a_codex_account_is_refused_while_the_flag_is_off_or_as_a_default_or_without_a_refresh_token()
 {
    let env = env();
    let server = MockServer::start().await;
    let account = add_codex(
        &env,
        &env.alice,
        Some("acct-1"),
        "codex-access-1",
        "codex-refresh-1",
    );
    let off = codex_profile(&server, false);
    assert!(matches!(
        env.credentials
            .client_for_account(&env.alice, CODEX, &off, &account),
        Err(SeraError::Forbidden(_))
    ));
    let on = codex_profile(&server, true);
    let build = |account_id: &str, refresh: &str| {
        open_provider_client(
            &on,
            &ProviderCredential {
                api_key: "codex-access-3".into(),
                refresh_token: refresh.into(),
                ..Default::default()
            },
            &env.credentials.server_secrets,
            10,
            4096,
            CredentialOwner {
                users: &env.credentials.users,
                user_id: &env.alice,
                provider: CODEX,
                account_id,
                refresher: None,
            },
        )
    };
    // The legacy default row is what a grant shares: never usable.
    assert!(matches!(
        build(DEFAULT_ACCOUNT, "codex-refresh-3"),
        Err(SeraError::Forbidden(_))
    ));
    // An API key (no refresh token) is not a plan sign-in.
    assert!(matches!(build(&account, ""), Err(SeraError::Forbidden(_))));
    assert!(build(&account, "codex-refresh-3").is_ok());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_codex_refresh_rotates_the_token_once_for_eight_turns() {
    let env = env();
    let server = MockServer::start().await;
    // The old refresh token is good once; a second spend is the failure.
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(wiremock::matchers::body_string_contains(
            "grant_type=refresh_token",
        ))
        .and(wiremock::matchers::body_string_contains(
            "refresh_token=codex-refresh-old",
        ))
        .and(wiremock::matchers::body_string_contains(
            "client_id=synthetic-codex-client",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(150))
                .set_body_json(json!({
                    "access_token": "codex-access-new",
                    "refresh_token": "codex-refresh-new",
                    "expires_in": 3600
                })),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .and(header("authorization", "Bearer codex-access-old"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .and(header("authorization", "Bearer codex-access-new"))
        .respond_with(responses_text("renewed"))
        .mount(&server)
        .await;
    let account = add_codex(
        &env,
        &env.alice,
        Some("acct-1"),
        "codex-access-old",
        "codex-refresh-old",
    );
    let mut turns = Vec::new();
    for _ in 0..8 {
        let client = LlmClient::new(
            format!("{}/backend-api/codex", server.uri()),
            "codex-access-old",
            10,
            4096,
        )
        .unwrap()
        .with_model("gpt-6.1-sol")
        .with_oauth_unpinned(OAuthMaterial {
            style: OAuthStyle::CodexRefresh,
            refresh_token: "codex-refresh-old".into(),
            token_url: format!("{}/oauth/token", server.uri()),
            client_id: "synthetic-codex-client".into(),
            client_secret: String::new(),
        })
        .unwrap()
        .with_account_refresh(
            AccountRefreshTarget {
                refresher: env.credentials.refresher().clone(),
                user_id: env.alice.clone(),
                provider: CODEX.into(),
                account_id: account.clone(),
            },
            None,
            None,
        )
        .unwrap()
        .with_wire(Wire::Responses)
        .with_codex(Some("acct-1".into()));
        turns.push(tokio::spawn(async move {
            client
                .complete(&[LlmMessage::user("hi")], &[], None, None)
                .await
        }));
    }
    for turn in turns {
        assert_eq!(
            turn.await.unwrap().unwrap().message.content.as_deref(),
            Some("renewed")
        );
    }
    let token_calls = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/oauth/token")
        .count();
    assert_eq!(token_calls, 1);
    // The rotated pair is what the account holds now.
    let stored = env
        .accounts
        .credential(&env.alice, CODEX, &account)
        .unwrap()
        .unwrap();
    assert_eq!(stored.api_key, "codex-access-new");
    assert_eq!(stored.refresh_token, "codex-refresh-new");
    assert!(stored.expires_at.is_some());
}

#[tokio::test]
async fn a_refused_codex_refresh_parks_the_account_and_keeps_the_row() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let account = add_codex(
        &env,
        &env.alice,
        Some("acct-1"),
        "codex-access-x",
        "codex-refresh-x",
    );
    let client = LlmClient::new(
        format!("{}/backend-api/codex", server.uri()),
        "codex-access-x",
        10,
        4096,
    )
    .unwrap()
    .with_model("gpt-6.1-sol")
    .with_oauth_unpinned(OAuthMaterial {
        style: OAuthStyle::CodexRefresh,
        refresh_token: "codex-refresh-x".into(),
        token_url: format!("{}/oauth/token", server.uri()),
        client_id: "synthetic-codex-client".into(),
        client_secret: String::new(),
    })
    .unwrap()
    .with_account_refresh(
        AccountRefreshTarget {
            refresher: env.credentials.refresher().clone(),
            user_id: env.alice.clone(),
            provider: CODEX.into(),
            account_id: account.clone(),
        },
        None,
        None,
    )
    .unwrap()
    .with_wire(Wire::Responses);
    let error = client
        .complete(&[LlmMessage::user("hi")], &[], None, None)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SeraError::Rejected {
            code: "needs_reauth",
            ..
        }
    ));
    let row = env
        .accounts
        .get(&env.alice, CODEX, &account)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, AccountStatus::NeedsReauth);
    assert!(
        env.accounts
            .credential(&env.alice, CODEX, &account)
            .unwrap()
            .is_some()
    );
    assert!(env.accounts.list(&env.bob, CODEX).unwrap().is_empty());
}

// ---- GitHub Copilot (v0.51) ----

const COPILOT: &str = "copilot";

fn copilot_profile(server: &MockServer, plan_oauth: bool) -> ProviderConfig {
    ProviderConfig {
        kind: "github-copilot".into(),
        base_url: server.uri(),
        data_egress_approved: true,
        plan_oauth,
        ..Default::default()
    }
}

fn add_copilot(env: &Env, user: &str, access: &str, github: &str) -> String {
    env.accounts
        .add(
            user,
            AddAccount {
                provider: COPILOT.into(),
                label: "synthetic-user".into(),
                auth_kind: AuthKind::Device,
                fingerprint: identity_fingerprint("github-copilot", "id:42"),
                identity: json!({"login": "synthetic-user"}),
                credential: ProviderCredential {
                    api_key: access.into(),
                    refresh_token: github.into(),
                    expires_at: None,
                    generation: None,
                },
                priority: 0,
            },
        )
        .unwrap()
        .account_id
}

fn chat_reply(text: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{"message": {"role": "assistant", "content": text}, "finish_reason": "stop"}]
    }))
}

#[test]
fn copilot_is_a_builtin_oauth_kind_with_a_pinned_token_exchange() {
    assert_eq!(canonical_kind("github-copilot").unwrap(), "github-copilot");
    assert_eq!(
        resolve_base_url("github-copilot", "").unwrap(),
        "https://api.githubcopilot.com"
    );
    match auth_plan("github-copilot", "").unwrap() {
        AuthPlan::OAuth {
            style,
            token_url,
            client_id_fallback,
        } => {
            assert_eq!(style, OAuthStyle::CopilotExchange);
            assert_eq!(
                token_url,
                "https://api.github.com/copilot_internal/v2/token"
            );
            assert_eq!(client_id_fallback, None);
        }
        AuthPlan::ApiKey => panic!("copilot signs in"),
    }
    assert!(
        enforce_oauth_target(
            OAuthStyle::CopilotExchange,
            "https://api.github.com/copilot_internal/v2/token"
        )
        .is_ok()
    );
    for bad in [
        "https://evil.example/copilot_internal/v2/token",
        "https://api.github.com/user",
    ] {
        assert!(enforce_oauth_target(OAuthStyle::CopilotExchange, bad).is_err());
    }
}

#[test]
fn copilot_catalog_keeps_only_chat_models_the_picker_shows() {
    let catalog = parse_model_catalog(
        json!({"data": [
            {"id": "cp-chat", "name": "Chat", "model_picker_enabled": true,
             "supported_endpoints": ["/chat/completions"],
             "capabilities": {"type": "chat",
                "limits": {"max_context_window_tokens": 400000, "max_prompt_tokens": 128000,
                           "max_output_tokens": 16000},
                "supports": {"vision": true, "tool_calls": true, "thinking": true}}},
            {"id": "cp-legacy", "model_picker_enabled": true, "capabilities": {"type": "chat"}},
            {"id": "cp-responses-only", "model_picker_enabled": true,
             "supported_endpoints": ["/responses"], "capabilities": {"type": "chat"}},
            {"id": "cp-hidden", "model_picker_enabled": false,
             "supported_endpoints": ["/chat/completions"], "capabilities": {"type": "chat"}},
            {"id": "cp-embedding", "model_picker_enabled": true,
             "capabilities": {"type": "embeddings"}}
        ]})
        .to_string()
        .as_bytes(),
        false,
    )
    .unwrap();
    assert_eq!(
        catalog.keys().map(String::as_str).collect::<Vec<_>>(),
        ["cp-chat", "cp-legacy"]
    );
    let chat = &catalog["cp-chat"];
    // The prompt limit, not the native window, is what a conversation must fit.
    assert_eq!(chat.context_length, Some(128_000));
    assert_eq!(chat.max_output_tokens, Some(16_000));
    assert_eq!(chat.tags, ["vision", "tools", "reasoning"]);
}

#[tokio::test]
async fn a_copilot_account_chats_and_lists_models_with_the_editor_headers() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer tid=copilot-token"))
        .and(header("user-agent", "GitHubCopilotChat/0.32.4"))
        .and(header("editor-version", "vscode/1.105.1"))
        .and(header("editor-plugin-version", "copilot-chat/0.32.4"))
        .and(header("copilot-integration-id", "vscode-chat"))
        .and(header("x-github-api-version", "2026-08-01"))
        .respond_with(chat_reply("from copilot"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("authorization", "Bearer tid=copilot-token"))
        .and(header("openai-intent", "model-access"))
        .and(header("copilot-integration-id", "vscode-chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
            {"id": "cp-chat", "model_picker_enabled": true,
             "supported_endpoints": ["/chat/completions"], "capabilities": {"type": "chat"}}
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    let profile = copilot_profile(&server, true);
    let account = add_copilot(&env, &env.alice, "tid=copilot-token", "gho_synthetic");
    let client = env
        .credentials
        .client_for_account(&env.alice, COPILOT, &profile, &account)
        .unwrap()
        .with_model("cp-chat");
    let reply = client
        .complete(&[LlmMessage::user("hi")], &[], None, None)
        .await
        .unwrap();
    assert_eq!(reply.message.content.as_deref(), Some("from copilot"));
    // The model list is read from this client's own host, whatever URL is passed.
    let catalog = client
        .fetch_model_catalog("https://api.githubcopilot.com/models", false)
        .await
        .unwrap();
    assert_eq!(
        catalog.keys().map(String::as_str).collect::<Vec<_>>(),
        ["auto", "cp-chat"]
    );
    // Exactly one user agent went out on the catalog call.
    let requests = server.received_requests().await.unwrap();
    let listing = requests.iter().find(|r| r.url.path() == "/models").unwrap();
    assert_eq!(listing.headers.get_all("user-agent").iter().count(), 1);
    // Another member holds no account.
    assert!(env.accounts.list(&env.bob, COPILOT).unwrap().is_empty());
}

#[tokio::test]
async fn a_copilot_account_is_refused_while_the_flag_is_off_or_as_a_default_or_without_a_github_token()
 {
    let env = env();
    let server = MockServer::start().await;
    let account = add_copilot(&env, &env.alice, "tid=copilot-token", "gho_synthetic");
    let off = copilot_profile(&server, false);
    assert!(matches!(
        env.credentials
            .client_for_account(&env.alice, COPILOT, &off, &account),
        Err(SeraError::Forbidden(_))
    ));
    let on = copilot_profile(&server, true);
    let build = |account_id: &str, github: &str| {
        open_provider_client(
            &on,
            &ProviderCredential {
                api_key: "tid=copilot-token".into(),
                refresh_token: github.into(),
                ..Default::default()
            },
            &env.credentials.server_secrets,
            10,
            4096,
            CredentialOwner {
                users: &env.credentials.users,
                user_id: &env.alice,
                provider: COPILOT,
                account_id,
                refresher: None,
            },
        )
    };
    assert!(matches!(
        build(DEFAULT_ACCOUNT, "gho_synthetic"),
        Err(SeraError::Forbidden(_))
    ));
    assert!(matches!(build(&account, ""), Err(SeraError::Forbidden(_))));
    assert!(build(&account, "gho_synthetic").is_ok());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_expired_copilot_token_is_exchanged_again_once_for_eight_turns() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .and(header("authorization", "Bearer gho_synthetic"))
        .and(header("editor-version", "vscode/1.105.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token": "tid=renewed",
            "expires_at": chrono::Utc::now().timestamp() + 1800,
            "endpoints": {"api": "https://api.githubcopilot.com"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer tid=expired"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer tid=renewed"))
        .respond_with(chat_reply("renewed"))
        .mount(&server)
        .await;
    let account = add_copilot(&env, &env.alice, "tid=expired", "gho_synthetic");
    let mut turns = Vec::new();
    for _ in 0..8 {
        let client = LlmClient::new(server.uri(), "tid=expired", 10, 4096)
            .unwrap()
            .with_model("cp-chat")
            .with_oauth_unpinned(OAuthMaterial {
                style: OAuthStyle::CopilotExchange,
                refresh_token: "gho_synthetic".into(),
                token_url: format!("{}/copilot_internal/v2/token", server.uri()),
                client_id: String::new(),
                client_secret: String::new(),
            })
            .unwrap()
            .with_account_refresh(
                AccountRefreshTarget {
                    refresher: env.credentials.refresher().clone(),
                    user_id: env.alice.clone(),
                    provider: COPILOT.into(),
                    account_id: account.clone(),
                },
                None,
                None,
            )
            .unwrap()
            .with_copilot();
        turns.push(tokio::spawn(async move {
            client
                .complete(&[LlmMessage::user("hi")], &[], None, None)
                .await
        }));
    }
    for turn in turns {
        assert_eq!(
            turn.await.unwrap().unwrap().message.content.as_deref(),
            Some("renewed")
        );
    }
    let exchanges = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/copilot_internal/v2/token")
        .count();
    assert_eq!(exchanges, 1);
    // The new Copilot token is stored; the GitHub token is unchanged.
    let stored = env
        .accounts
        .credential(&env.alice, COPILOT, &account)
        .unwrap()
        .unwrap();
    assert_eq!(stored.api_key, "tid=renewed");
    assert_eq!(stored.refresh_token, "gho_synthetic");
    assert!(stored.expires_at.is_some());
}

#[tokio::test]
async fn a_github_token_that_github_refuses_parks_the_copilot_account_and_keeps_the_row() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let account = add_copilot(&env, &env.alice, "tid=expired", "gho_revoked");
    let client = LlmClient::new(server.uri(), "tid=expired", 10, 4096)
        .unwrap()
        .with_model("cp-chat")
        .with_oauth_unpinned(OAuthMaterial {
            style: OAuthStyle::CopilotExchange,
            refresh_token: "gho_revoked".into(),
            token_url: format!("{}/copilot_internal/v2/token", server.uri()),
            client_id: String::new(),
            client_secret: String::new(),
        })
        .unwrap()
        .with_account_refresh(
            AccountRefreshTarget {
                refresher: env.credentials.refresher().clone(),
                user_id: env.alice.clone(),
                provider: COPILOT.into(),
                account_id: account.clone(),
            },
            None,
            None,
        )
        .unwrap()
        .with_copilot();
    let error = client
        .complete(&[LlmMessage::user("hi")], &[], None, None)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SeraError::Rejected {
            code: "needs_reauth",
            ..
        }
    ));
    let row = env
        .accounts
        .get(&env.alice, COPILOT, &account)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, AccountStatus::NeedsReauth);
    assert!(
        env.accounts
            .credential(&env.alice, COPILOT, &account)
            .unwrap()
            .is_some()
    );
}
