//! Optional third-party recall source: Mem0 Platform (<https://mem0.ai>).
//!
//! Off unless `[memory] provider = "mem0"`. Even then a user gets remote
//! recall only after saving a Mem0 key under the per-user credential name
//! `conn.mem0` (envelope-encrypted, never returned, logged, or prompted). A
//! user without a key gets no remote recall and nothing is sent for them.
//!
//! Endpoints (base `https://api.mem0.ai`, header `Authorization: Token <key>`):
//! `POST /v1/memories/` writes `{messages, user_id}` and
//! `POST /v2/memories/search/` searches `{query, filters: {user_id}}`.
//!
//! `user_id` is an opaque SHA-256 of our user id, never the raw id or an
//! email. Every outbound text passes the same blocklist scan as model
//! requests, and remote hits are untrusted data: a hit the blocklist rejects
//! is dropped before it can reach a prompt. The local store stays the source
//! of truth. Deletes and edits are not propagated to Mem0, so the owner should
//! also forget a fact there (see docs/memory.md).

use std::sync::Arc;
use std::time::Duration;

use secrecy::ExposeSecret;
use serde_json::{Value, json};

use crate::config::MemoryConfig;
use crate::connectors::map_transport;
use crate::error::{Result, SeraError};
use crate::gateway::Gateway;
use crate::user_secrets::UserSecrets;
use crate::util::{sha256_hex, truncate_chars};

/// The per-user credential row holding a Mem0 API key.
pub const CREDENTIAL_ID: &str = "conn.mem0";
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_HIT_CHARS: usize = 300;
const MAX_QUERY_CHARS: usize = 500;
const MAX_HITS: usize = 20;

/// One remote memory returned by a search. `score` is higher for better
/// matches, as reported by Mem0 (or 0.5 when it reports none).
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteHit {
    pub content: String,
    pub score: f64,
}

/// Client for the Mem0 Platform. Cheap to clone; the key is read per call.
#[derive(Clone)]
pub struct Mem0Memory {
    http: reqwest::Client,
    base: String,
    users: Arc<UserSecrets>,
    gateway: Gateway,
}

impl std::fmt::Debug for Mem0Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mem0Memory")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// The opaque identifier Mem0 receives for one of our users.
pub fn remote_user_id(user_id: &str) -> String {
    sha256_hex(format!("serabot-mem0:{user_id}"))
}

impl Mem0Memory {
    pub fn new(config: &MemoryConfig, users: Arc<UserSecrets>, gateway: Gateway) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.mem0_timeout_secs.clamp(2, 30)))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| SeraError::Internal(format!("Mem0 HTTP client: {error}")))?;
        Ok(Self {
            http,
            base: config.mem0_api_base.trim().trim_end_matches('/').to_owned(),
            users,
            gateway,
        })
    }

    /// The acting user's Mem0 key, or `None` when they have not saved one.
    fn key(&self, user_id: &str) -> Result<Option<String>> {
        Ok(self
            .users
            .get(user_id, CREDENTIAL_ID)?
            .map(|secret| secret.expose_secret().clone()))
    }

    /// Write one fact to the acting user's Mem0 space. Without a key this does
    /// nothing.
    pub async fn remember(&self, user_id: &str, content: &str) -> Result<()> {
        let Some(key) = self.key(user_id)? else {
            return Ok(());
        };
        self.gateway.scan_egress(user_id, content)?;
        let body = json!({
            "messages": [{"role": "user", "content": content}],
            "user_id": remote_user_id(user_id),
        });
        self.post(&key, "/v1/memories/", &body).await?;
        Ok(())
    }

    /// Search the acting user's Mem0 space. Without a key this returns no hits
    /// and sends no request.
    pub async fn search(&self, user_id: &str, query: &str, k: usize) -> Result<Vec<RemoteHit>> {
        let query = truncate_chars(query.trim(), MAX_QUERY_CHARS);
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let Some(key) = self.key(user_id)? else {
            return Ok(Vec::new());
        };
        self.gateway.scan_egress(user_id, &query)?;
        let body = json!({
            "query": query,
            "filters": {"user_id": remote_user_id(user_id)},
        });
        let value = self.post(&key, "/v2/memories/search/", &body).await?;
        Ok(parse_hits(&value, k))
    }

    async fn post(&self, key: &str, path: &str, body: &Value) -> Result<Value> {
        let mut response = self
            .http
            .post(format!("{}{path}", self.base))
            .header("Authorization", format!("Token {key}"))
            .header("accept", "application/json")
            .json(body)
            .send()
            .await
            .map_err(|error| map_transport("Mem0", error))?;
        let status = response.status();
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| map_transport("Mem0", error))?
        {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(SeraError::External(
                    "Mem0 response exceeded the size limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        // Errors name the status only: never the body, the request, or the key.
        if !status.is_success() {
            return Err(match status.as_u16() {
                401 | 403 => SeraError::Forbidden(
                    "Mem0 rejected the saved key; add it again in Connectors".into(),
                ),
                429 => {
                    SeraError::External("Mem0 is rate limiting requests; try again later".into())
                }
                code => SeraError::External(format!("Mem0 returned HTTP {code}")),
            });
        }
        Ok(serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
}

/// Parse a Mem0 search body. Accepts `{"results": [...]}` or a bare array and
/// reads each item's `memory` (or `text` / `content`) and optional `score`.
/// Items without text are skipped.
pub fn parse_hits(value: &Value, k: usize) -> Vec<RemoteHit> {
    let items = value
        .get("results")
        .and_then(Value::as_array)
        .or_else(|| value.as_array());
    let Some(items) = items else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let text = item
                .get("memory")
                .or_else(|| item.get("text"))
                .or_else(|| item.get("content"))
                .and_then(Value::as_str)?;
            let content = truncate_chars(text.trim(), MAX_HIT_CHARS);
            if content.is_empty() {
                return None;
            }
            let score = item
                .get("score")
                .and_then(Value::as_f64)
                .filter(|score| score.is_finite())
                .unwrap_or(0.5);
            Some(RemoteHit { content, score })
        })
        .take(k.clamp(1, MAX_HITS))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::ToolRegistry;
    use crate::memory::{self, MemoryHub, MemoryProvider};
    use crate::policy::Policy;
    use crate::store::Store;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const KEY_A: &str = "m0_synthetic_key_alice_0001";
    const KEY_LEAK: &str = "m0_synthetic_key_leak_0002";

    struct Fixture {
        hub: MemoryHub,
        store: Store,
        users: Arc<UserSecrets>,
        gateway: Gateway,
        alice: String,
        bob: String,
    }

    fn fixture(server: &MockServer, provider: &str, policy: Policy) -> Fixture {
        let store = Store::open_in_memory(384).unwrap();
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [7u8; 32]).unwrap());
        let alice = store
            .create_user("microsoft:mem0-alice", "a@example.example", "A")
            .unwrap()
            .id;
        let bob = store
            .create_user("microsoft:mem0-bob", "b@example.example", "B")
            .unwrap()
            .id;
        let gateway = Gateway::new(
            store.clone(),
            Arc::new(policy),
            ToolRegistry::default(),
            1,
            false,
        )
        .with_vault(users.clone());
        let config = MemoryConfig {
            provider: provider.into(),
            mem0_api_base: server.uri(),
            mem0_timeout_secs: 5,
            ..MemoryConfig::default()
        };
        let hub = MemoryHub::new(&config, gateway.clone()).unwrap();
        Fixture {
            hub,
            store,
            users,
            gateway,
            alice,
            bob,
        }
    }

    #[test]
    fn remote_user_ids_are_opaque_and_stable() {
        let id = remote_user_id("0190-user-uuid");
        assert_eq!(id.len(), 64);
        assert!(!id.contains("0190"));
        assert_eq!(id, remote_user_id("0190-user-uuid"));
        assert_ne!(id, remote_user_id("another-user"));
    }

    #[test]
    fn parses_both_response_shapes_and_skips_empty_items() {
        let wrapped = json!({"results": [
            {"memory": "Prefers tea", "score": 0.9},
            {"memory": "   "},
            {"text": "Lives in Rome"}
        ]});
        let hits = parse_hits(&wrapped, 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].score, 0.9);
        assert_eq!(hits[1].score, 0.5);
        let bare = json!([{"content": "Owns a bike", "score": 0.4}]);
        assert_eq!(parse_hits(&bare, 5)[0].content, "Owns a bike");
        assert!(parse_hits(&json!({"error": "x"}), 5).is_empty());
    }

    /// No saved key: remote recall is off and nothing is sent. The mock
    /// expects zero requests.
    #[tokio::test]
    async fn without_a_key_the_remote_source_is_disabled() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let fixture = fixture(&server, "mem0", Policy::empty());
        assert!(fixture.hub.has_remote());
        let hits = fixture
            .hub
            .recall(&fixture.alice, "tea", 5, None)
            .await
            .unwrap();
        assert!(hits.is_empty());
        fixture
            .hub
            .write(
                &fixture.alice,
                &memory::Candidate::explicit("fact", "Likes green tea", "operator:test"),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn builtin_provider_has_no_remote_source() {
        let server = MockServer::start().await;
        let fixture = fixture(&server, "builtin", Policy::empty());
        assert!(!fixture.hub.has_remote());
    }

    #[test]
    fn mem0_provider_requires_the_per_user_secret_store() {
        let store = Store::open_in_memory(384).unwrap();
        let gateway = Gateway::new(
            store,
            Arc::new(Policy::empty()),
            ToolRegistry::default(),
            1,
            false,
        );
        let config = MemoryConfig {
            provider: "mem0".into(),
            ..MemoryConfig::default()
        };
        assert!(MemoryHub::new(&config, gateway).is_err());
    }

    #[tokio::test]
    async fn search_sends_the_key_and_a_hashed_user_id_then_merges_by_score() {
        let server = MockServer::start().await;
        let fixture = fixture(&server, "mem0", Policy::empty());
        fixture
            .users
            .set(&fixture.alice, CREDENTIAL_ID, KEY_A)
            .unwrap();
        Mock::given(method("POST"))
            .and(path("/v2/memories/search/"))
            .and(header("authorization", format!("Token {KEY_A}").as_str()))
            .and(body_partial_json(json!({
                "query": "tea",
                "filters": {"user_id": remote_user_id(&fixture.alice)}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{"memory": "Drinks green tea in the morning", "score": 0.95}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        memory::add(
            &fixture.store,
            &fixture.alice,
            "preference",
            "Prefers black coffee",
            "operator:test",
        )
        .unwrap();
        let hits = fixture
            .hub
            .recall(&fixture.alice, "tea", 5, None)
            .await
            .unwrap();
        assert!(hits.iter().any(|item| item.memory.origin == "mem0"));
        assert_eq!(hits[0].memory.origin, "mem0", "higher score first");
        // The remote fact has no local id, so the prompt cannot cite it.
        assert!(hits[0].memory.id.is_empty());
    }

    /// Bob has no key: Alice's Mem0 space is never queried on his behalf.
    #[tokio::test]
    async fn one_users_key_never_serves_another_user() {
        let server = MockServer::start().await;
        let fixture = fixture(&server, "mem0", Policy::empty());
        fixture
            .users
            .set(&fixture.alice, CREDENTIAL_ID, KEY_A)
            .unwrap();
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
            .expect(0)
            .mount(&server)
            .await;
        let hits = fixture
            .hub
            .recall(&fixture.bob, "tea", 5, None)
            .await
            .unwrap();
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn write_through_uses_the_acting_users_key_and_hashed_id() {
        let server = MockServer::start().await;
        let fixture = fixture(&server, "mem0", Policy::empty());
        fixture
            .users
            .set(&fixture.alice, CREDENTIAL_ID, KEY_A)
            .unwrap();
        Mock::given(method("POST"))
            .and(path("/v1/memories/"))
            .and(header("authorization", format!("Token {KEY_A}").as_str()))
            .and(body_partial_json(json!({
                "messages": [{"role": "user", "content": "Owns a blue bicycle"}],
                "user_id": remote_user_id(&fixture.alice)
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let outcome = fixture
            .hub
            .write(
                &fixture.alice,
                &memory::Candidate::explicit("fact", "Owns a blue bicycle", "operator:test"),
            )
            .await
            .unwrap();
        assert_eq!(outcome.label(), "inserted");
        // A duplicate is not sent a second time.
        let again = fixture
            .hub
            .write(
                &fixture.alice,
                &memory::Candidate::explicit("fact", "Owns a blue bicycle", "operator:test"),
            )
            .await
            .unwrap();
        assert_eq!(again.label(), "duplicate");
    }

    /// Outbound text that fails the blocklist never leaves the process, and
    /// remote hits that fail it never reach a prompt.
    #[tokio::test]
    async fn blocklisted_text_is_neither_sent_nor_recalled() {
        let server = MockServer::start().await;
        let fixture = fixture(
            &server,
            "mem0",
            Policy::from_text("forbiddenterm\nblockedword"),
        );
        fixture
            .users
            .set(&fixture.alice, CREDENTIAL_ID, KEY_A)
            .unwrap();
        Mock::given(method("POST"))
            .and(path("/v1/memories/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let blocked = fixture
            .hub
            .write(
                &fixture.alice,
                &memory::Candidate::explicit(
                    "fact",
                    "Mentions forbiddenterm here",
                    "operator:test",
                ),
            )
            .await
            .unwrap();
        // The local write still happens; only the remote copy is refused.
        assert_eq!(blocked.label(), "inserted");

        Mock::given(method("POST"))
            .and(path("/v2/memories/search/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    {"memory": "Has a blockedword in the note", "score": 0.99},
                    {"memory": "Plain remote tea fact", "score": 0.5}
                ]
            })))
            .mount(&server)
            .await;
        let hits = fixture
            .hub
            .recall(&fixture.alice, "tea", 5, None)
            .await
            .unwrap();
        assert!(
            hits.iter()
                .all(|item| !item.memory.content.contains("blockedword"))
        );
        assert!(
            hits.iter()
                .any(|item| item.memory.content.contains("Plain remote"))
        );
    }

    /// Mem0 errors never echo the key or the response body, and a failed
    /// remote read does not fail the recall.
    #[tokio::test]
    async fn errors_never_carry_the_key_and_recall_survives_them() {
        let server = MockServer::start().await;
        let fixture = fixture(&server, "mem0", Policy::empty());
        fixture
            .users
            .set(&fixture.alice, CREDENTIAL_ID, KEY_LEAK)
            .unwrap();
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_string(format!("upstream echoed Token {KEY_LEAK}")),
            )
            .mount(&server)
            .await;
        let remote = Mem0Memory::new(
            &MemoryConfig {
                mem0_api_base: server.uri(),
                ..MemoryConfig::default()
            },
            fixture.users.clone(),
            fixture.gateway.clone(),
        )
        .unwrap();
        let error = remote
            .search(&fixture.alice, "tea", 5)
            .await
            .expect_err("HTTP 500 is an error");
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains(KEY_LEAK), "{rendered}");
        assert!(!rendered.contains("upstream echoed"), "{rendered}");

        // Through the hub the same failure is logged, not returned.
        memory::add(
            &fixture.store,
            &fixture.alice,
            "fact",
            "Local tea fact survives",
            "operator:test",
        )
        .unwrap();
        let hits = fixture
            .hub
            .recall(&fixture.alice, "tea", 5, None)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
    }
}
