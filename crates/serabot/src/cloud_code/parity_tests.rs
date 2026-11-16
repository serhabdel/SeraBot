//! Antigravity wire parity with the owner's HielCowork client: session ids,
//! the thinking table, signatures, tool-name aliases, the schema allow-list,
//! project discovery and quota. Synthetic fixtures only.

use super::*;
use crate::domain::ToolCall;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn spec(name: &str, parameters: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: "synthetic tool".into(),
        parameters,
        external_effect: false,
    }
}

fn call(id: &str, name: &str, signature: Option<&str>) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: json!({"q": "x"}),
        reasoning_signature: signature.map(str::to_owned),
    }
}

fn envelope(model: &str, messages: &[LlmMessage], tools: &[ToolSpec]) -> Value {
    build_cloud_code_envelope(model, "proj-synthetic", messages, tools, 100, None).unwrap()
}

#[test]
fn fnv1a_matches_the_published_vectors() {
    assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
}

#[test]
fn session_ids_are_stable_folded_and_rotate_by_generation() {
    let first = session_id("proj-a", "conv-1", 0);
    assert_eq!(first, session_id("proj-a", "conv-1", 0));
    assert!(first.starts_with('-'));
    let folded: u64 = first[1..].parse().unwrap();
    assert!((1..SESSION_SPACE).contains(&folded));
    // The folded value is FNV-1a of the key, modulo 9e18.
    let expected = fnv1a64(b"proj-a:conv-1") % SESSION_SPACE;
    assert_eq!(folded, expected.max(1));
    // Another conversation, project or generation is another session.
    assert_ne!(first, session_id("proj-a", "conv-2", 0));
    assert_ne!(first, session_id("proj-b", "conv-1", 0));
    let bumped = session_id("proj-a", "conv-1", 1);
    assert_ne!(first, bumped);
    let expected = fnv1a64(b"proj-a:conv-1:gen1") % SESSION_SPACE;
    assert_eq!(bumped, format!("-{}", expected.max(1)));
    // No project hashes as `default`.
    let expected = fnv1a64(b"default:conv-1") % SESSION_SPACE;
    assert_eq!(session_id("", "conv-1", 0), format!("-{}", expected.max(1)));
}

#[test]
fn the_envelope_uses_the_deterministic_session_and_a_hielcowork_request_id() {
    let messages = [LlmMessage::user("hi")];
    let options = EnvelopeOptions {
        conversation: "conv-9",
        session_generation: 2,
        thinking_level: None,
    };
    let build = || {
        build_cloud_code_envelope_with(
            "gemini-3.8-flash",
            "proj-synthetic",
            &messages,
            &[],
            100,
            None,
            &options,
        )
        .unwrap()
    };
    let (one, two) = (build(), build());
    assert_eq!(one["request"]["sessionId"], two["request"]["sessionId"]);
    assert_eq!(
        one["request"]["sessionId"],
        json!(session_id("proj-synthetic", "conv-9", 2))
    );
    let request_id = one["requestId"].as_str().unwrap();
    let parts: Vec<&str> = request_id.split('/').collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], "agent");
    assert!(parts[1].parse::<i64>().is_ok());
    assert_eq!(parts[2].len(), 8);
    assert_ne!(one["requestId"], two["requestId"]);
}

#[test]
fn thinking_follows_the_hielcowork_table() {
    let t = antigravity_thinking;
    // Level models.
    assert_eq!(
        t("gemini-3.8-flash", Some("off"), 65_536),
        Some(json!({"thinkingBudget": 0}))
    );
    for (level, wire) in [
        ("low", "low"),
        ("medium", "medium"),
        ("high", "high"),
        ("max", "high"),
    ] {
        assert_eq!(
            t("gemini-3.8-flash", Some(level), 65_536),
            Some(json!({"thinkingLevel": wire, "includeThoughts": true})),
            "{level}"
        );
    }
    assert_eq!(
        t("gemini-3.8-flash-tiered", Some("low"), 65_536),
        Some(json!({"thinkingLevel": "low", "includeThoughts": true}))
    );
    // A level baked into the id sends no config at all.
    for model in ["gemini-4-argon-high", "gemini-3.1-pro-low", "x-medium"] {
        assert_eq!(t(model, Some("high"), 65_536), None, "{model}");
        assert_eq!(t(model, Some("off"), 65_536), None, "{model}");
    }
    assert_eq!(t("gpt-oss-120b", Some("high"), 65_536), None);
    // Claude: budgets, clamped, off sends nothing.
    for (level, budget) in [
        ("low", 4_000),
        ("medium", 10_000),
        ("high", 24_000),
        ("max", 32_000),
    ] {
        assert_eq!(
            t("claude-sonnet-5-5-thinking", Some(level), 64_000),
            Some(json!({"thinkingBudget": budget, "includeThoughts": true})),
            "{level}"
        );
    }
    assert_eq!(t("claude-opus-5-5", Some("off"), 64_000), None);
    // The budget stays at least 1024 under the reply size...
    assert_eq!(
        t("claude-opus-5-5", Some("max"), 20_000),
        Some(json!({"thinkingBudget": 18_976, "includeThoughts": true}))
    );
    // ...and nothing is sent when the reply is not larger than the budget.
    assert_eq!(t("claude-opus-5-5", Some("low"), 1_024), None);
    // No level: a thinking id uses medium, a plain Claude sends nothing.
    assert_eq!(
        t("claude-opus-5-5-thinking", None, 64_000),
        Some(json!({"thinkingBudget": 10_000, "includeThoughts": true}))
    );
    assert_eq!(t("claude-opus-5-5", None, 64_000), None);
    assert_eq!(
        t("gemini-3.8-flash", None, 65_536),
        Some(json!({"thinkingLevel": "medium", "includeThoughts": true}))
    );
}

#[test]
fn output_caps_and_temperature_follow_the_model_family() {
    assert_eq!(effective_max_output("claude-opus-5-5", 4_096), 64_000);
    assert_eq!(effective_max_output("gemini-3.8-flash", 100), 65_536);
    assert_eq!(effective_max_output("gemini-3.8-flash", 8_000), 8_000);
    assert_eq!(effective_max_output("claude-opus-5-5", 90_000), 64_000);
    assert_eq!(effective_max_output("gemini-3.8-flash", 5_000), 5_000);

    // Claude drops the temperature while it thinks; others keep it.
    let thinking = generation_config("claude-opus-5-5", 30_000, Some(0.5), Some("high"));
    assert!(thinking.get("temperature").is_none());
    assert_eq!(thinking["thinkingConfig"]["thinkingBudget"], 24_000);
    let plain = generation_config("claude-opus-5-5", 8_000, Some(0.5), Some("off"));
    assert_eq!(plain["temperature"], 0.5);
    assert!(plain.get("thinkingConfig").is_none());
    let gemini = generation_config("gemini-3.8-flash", 8_000, Some(0.5), Some("high"));
    assert_eq!(gemini["temperature"], 0.5);
    assert_eq!(gemini["thinkingConfig"]["thinkingLevel"], "high");
}

#[test]
fn claude_requests_never_end_on_a_model_turn() {
    let messages = [
        LlmMessage::user("hi"),
        LlmMessage::assistant(Some("prefill".into()), vec![]),
    ];
    for model in ["claude-opus-5-5", "claude-sonnet-5-5-thinking"] {
        let envelope = envelope(model, &messages, &[]);
        let contents = envelope["request"]["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1, "{model}");
        assert_eq!(contents[0]["role"], "user");
    }
    // Other models keep the prefill.
    let gemini = envelope("gemini-3.8-flash", &messages, &[]);
    assert_eq!(gemini["request"]["contents"].as_array().unwrap().len(), 2);
}

#[test]
fn is_error_comes_from_the_tools_real_outcome() {
    let messages = [
        LlmMessage::user("go"),
        LlmMessage::assistant(
            None,
            vec![call("c1", "rag_search", None), call("c2", "other", None)],
        ),
        LlmMessage {
            role: Role::Tool,
            content: Some("it broke".into()),
            tool_call_id: Some("c1".into()),
            name: Some("rag_search".into()),
            is_error: true,
            ..Default::default()
        },
        LlmMessage {
            role: Role::Tool,
            content: Some("fine".into()),
            tool_call_id: Some("c2".into()),
            name: Some("other".into()),
            ..Default::default()
        },
    ];
    let envelope = envelope("gemini-3.8-flash", &messages, &[]);
    let parts = envelope["request"]["contents"][2]["parts"]
        .as_array()
        .unwrap();
    assert_eq!(parts[0]["functionResponse"]["response"]["is_error"], true);
    assert_eq!(parts[1]["functionResponse"]["response"]["is_error"], false);
}

#[test]
fn a_signature_survives_a_round_trip_and_the_sentinel_fills_the_gap() {
    // Google's answer carries a signature on the call...
    let body = br#"data: {"response":{"candidates":[{"content":{"parts":[{"thought":true,"text":"thinking it over"},{"thoughtSignature":"c2lnLXN5bnRoZXRpYw==","functionCall":{"id":"call_1","name":"rag_search","args":{"query":"q"}}}]},"finishReason":"STOP"}]}}

"#;
    let parsed = parse_cloud_code_completion(body).unwrap();
    assert_eq!(parsed.reasoning, "thinking it over");
    assert_eq!(
        parsed.tool_calls[0].signature.as_deref(),
        Some("c2lnLXN5bnRoZXRpYw==")
    );
    // ...which goes back with that call on the next request.
    let sent = ToolCall {
        id: parsed.tool_calls[0].id.clone(),
        name: parsed.tool_calls[0].name.clone(),
        arguments: parsed.tool_calls[0].arguments.clone(),
        reasoning_signature: parsed.tool_calls[0].signature.clone(),
    };
    let envelope = envelope(
        "gemini-3.8-flash",
        &[
            LlmMessage::user("go"),
            LlmMessage::assistant(None, vec![sent, call("call_2", "rag_search", None)]),
        ],
        &[],
    );
    let parts = envelope["request"]["contents"][1]["parts"]
        .as_array()
        .unwrap();
    assert_eq!(parts[0]["thoughtSignature"], "c2lnLXN5bnRoZXRpYw==");
    assert_eq!(parts[1]["thoughtSignature"], THOUGHT_SIGNATURE_SENTINEL);
    // A hostile signature is never replayed.
    let hostile = build_cloud_code_envelope(
        "gemini-3.8-flash",
        "",
        &[
            LlmMessage::user("go"),
            LlmMessage::assistant(None, vec![call("c", "rag_search", Some("bad signature\n"))]),
        ],
        &[],
        100,
        None,
    )
    .unwrap();
    assert_eq!(
        hostile["request"]["contents"][1]["parts"][0]["thoughtSignature"],
        THOUGHT_SIGNATURE_SENTINEL
    );
}

#[test]
fn a_malformed_function_call_is_told_apart_from_an_empty_answer() {
    let malformed = br#"{"response":{"candidates":[{"finishReason":"MALFORMED_FUNCTION_CALL"}]}}"#;
    let parsed = parse_cloud_code_stream(malformed).unwrap();
    assert!(parsed.is_malformed_call());
    // The public parser still refuses an answer with nothing in it.
    assert!(parse_cloud_code_completion(malformed).is_err());
    // A malformed finish with content is just an answer.
    let said = br#"{"response":{"candidates":[{"content":{"parts":[{"text":"hi"}]},"finishReason":"MALFORMED_FUNCTION_CALL"}]}}"#;
    assert!(!parse_cloud_code_stream(said).unwrap().is_malformed_call());
}

#[test]
fn stream_errors_and_thought_tokens_are_read() {
    let error = parse_cloud_code_stream(
        br#"data: {"error":{"code":429,"message":"slow down","status":"RESOURCE_EXHAUSTED"}}

"#,
    )
    .err()
    .unwrap()
    .to_string();
    assert!(
        error.contains("HTTP 429") && error.contains("slow down"),
        "{error}"
    );
    let status_only = parse_cloud_code_stream(
        br#"data: {"error":{"message":"odd","status":"INTERNAL"}}

"#,
    )
    .err()
    .unwrap()
    .to_string();
    assert!(
        status_only.contains("Antigravity error (INTERNAL)"),
        "{status_only}"
    );
    let usage = parse_cloud_code_stream(
        br#"{"response":{"candidates":[{"content":{"parts":[{"text":"x"}]}}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":3,"thoughtsTokenCount":4}}}"#,
    )
    .unwrap();
    assert_eq!(usage.prompt_tokens, 5);
    assert_eq!(usage.completion_tokens, 7);
}

#[test]
fn search_tools_are_aliased_both_ways_and_google_search_is_skipped() {
    let tools = [
        spec(
            "web_search",
            json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        ),
        spec("search_web", json!({"type": "object", "properties": {}})),
        spec("google_search", json!({})),
        spec("googleSearch", json!({})),
        spec(
            "plugin.slug.tool",
            json!({"type": "object", "properties": {}}),
        ),
        spec(
            "PLUGIN_slug_tool",
            json!({"type": "object", "properties": {}}),
        ),
    ];
    let envelope = envelope(
        "gemini-3.8-flash",
        &[
            LlmMessage::user("go"),
            LlmMessage::assistant(None, vec![call("c1", "web_search", None)]),
        ],
        &tools,
    );
    let names: Vec<&str> = envelope["request"]["tools"][0]["functionDeclarations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|declaration| declaration["name"].as_str().unwrap())
        .collect();
    // Aliased, `google_search` skipped, and the case-insensitive duplicate dropped.
    assert_eq!(
        names,
        ["agy_web_search", "agy_search_web", "plugin_slug_tool"]
    );
    assert_eq!(
        envelope["request"]["contents"][1]["parts"][0]["functionCall"]["name"],
        "agy_web_search"
    );
    // The model's calls come back under the names the runtime knows.
    let mut completion = parse_cloud_code_completion(
        br#"{"response":{"candidates":[{"content":{"parts":[
            {"functionCall":{"id":"a","name":"agy_web_search","args":{}}},
            {"functionCall":{"id":"b","name":"agy_search_web","args":{}}},
            {"functionCall":{"id":"c","name":"plugin_slug_tool","args":{}}}
        ]}}]}}"#,
    )
    .unwrap();
    restore_tool_names(&mut completion, &tools);
    let restored: Vec<&str> = completion
        .tool_calls
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(restored, ["web_search", "search_web", "plugin.slug.tool"]);
}

/// The keys a sanitized schema node may carry.
fn assert_allow_listed(node: &Value, trail: &str) {
    let Some(map) = node.as_object() else {
        panic!("{trail}: a schema node must be an object, got {node}");
    };
    for (key, child) in map {
        assert!(
            SCHEMA_KEYS.contains(&key.as_str()),
            "{trail}: unexpected key {key}"
        );
        match key.as_str() {
            "properties" => {
                for (name, property) in child.as_object().expect("properties is a map") {
                    assert_allow_listed(property, &format!("{trail}.{name}"));
                }
            }
            "items" => assert_allow_listed(child, &format!("{trail}[]")),
            _ => {}
        }
    }
    let kind = map["type"].as_str().expect("every node has a type");
    assert!(["OBJECT", "STRING", "INTEGER", "NUMBER", "BOOLEAN", "ARRAY"].contains(&kind));
    if kind == "ARRAY" {
        assert!(map.contains_key("items"), "{trail}: an array needs items");
    }
    if let Some(required) = map.get("required") {
        let properties = map["properties"]
            .as_object()
            .expect("required needs properties");
        let required = required.as_array().unwrap();
        assert!(!required.is_empty());
        for name in required {
            assert!(
                properties.contains_key(name.as_str().unwrap()),
                "{trail}: {name}"
            );
        }
    }
}

#[test]
fn the_sanitizer_emits_only_allow_listed_keys() {
    // A generated spread of the shapes MCP and plugin tools produce.
    let leaves = [
        json!({"type": "string", "format": "uri", "default": "x", "title": "T", "minLength": 1}),
        json!({"type": ["integer", "null"], "minimum": 0, "maximum": 5}),
        json!({"const": "only"}),
        json!({"enum": [1, 2, "three"], "description": "pick"}),
        json!({"anyOf": [{"type": "null"}, {"type": "number", "exclusiveMinimum": 0}]}),
        json!({"oneOf": [{"type": "string"}, {"type": "integer"}]}),
        json!({"allOf": [{"type": "object", "properties": {"a": {"type": "string"}}}]}),
        json!({"type": "array"}),
        json!({"type": "array", "items": [{"type": "string"}, {"type": "number"}]}),
        json!({"type": "object", "additionalProperties": {"type": "string"}}),
        json!({"properties": {"inner": {"type": "boolean", "unknownKeyword": 1}}}),
        json!({"items": {"type": "string", "pattern": "^a"}}),
        json!({"$ref": "#/$defs/node"}),
        json!({"type": "string", "x-vendor": {"deep": true}, "$comment": "no"}),
        json!({}),
        json!(true),
    ];
    let mut count = 0;
    for (index, leaf) in leaves.iter().enumerate() {
        for required in [
            json!(["leaf", "ghost"]),
            json!(["ghost"]),
            json!([]),
            json!("leaf"),
        ] {
            let schema = json!({
                "type": "object",
                "$schema": "http://json-schema.org/draft-07/schema#",
                "$defs": {"node": {"type": "object", "properties": {
                    "label": {"type": "string"},
                    "child": {"$ref": "#/$defs/node"}
                }}},
                "required": required,
                "properties": {
                    "leaf": leaf,
                    "nested": {"type": "object", "properties": {"leaf": leaf}, "required": ["leaf", "nope"]},
                    "list": {"type": "array", "items": leaf},
                },
                "x-extension": 1,
            });
            let sanitized = sanitize_schema(&schema);
            assert_allow_listed(&sanitized, &format!("schema{index}"));
            // `required` names only what exists.
            if let Some(names) = sanitized.get("required").and_then(Value::as_array) {
                assert!(names.iter().all(|name| name == "leaf"));
            }
            count += 1;
        }
    }
    assert_eq!(count, 64);
    // A scalar where a schema belongs becomes a plain string schema.
    assert_eq!(sanitize_schema(&json!(true)), placeholder_object());
}

#[test]
fn the_sanitizer_infers_missing_types_and_array_items() {
    let sanitized = sanitize_schema(&json!({
        "properties": {
            "a": {"properties": {"x": {"type": "string"}}},
            "b": {"items": {"type": "integer"}},
            "c": {"type": "array"},
            "d": {"description": "no type"},
        },
        "required": ["a", "missing"]
    }));
    assert_eq!(sanitized["type"], "OBJECT");
    assert_eq!(sanitized["properties"]["a"]["type"], "OBJECT");
    assert_eq!(sanitized["properties"]["b"]["type"], "ARRAY");
    assert_eq!(sanitized["properties"]["c"]["items"]["type"], "STRING");
    assert_eq!(sanitized["properties"]["d"]["type"], "STRING");
    assert_eq!(sanitized["required"], json!(["a"]));
}

#[test]
fn the_catalog_keeps_limits_and_drops_retired_models() {
    let catalog = parse_cloud_code_models(
        serde_json::to_vec(&json!({
            "models": {
                "gemini-3.8-flash": {"displayName": "Flash", "maxTokens": 500_000, "maxOutputTokens": 8_192},
                "claude-opus-5-5": {},
                "claude-opus-4-6": {},
                "claude-sonnet-4-6-thinking": {},
                "gpt-oss-120b": {},
                "tab-model": {},
                "image-model": {},
                "audio-model": {},
                "old-model": {}
            },
            "tabModelIds": ["tab-model"],
            "imageGenerationModelIds": ["image-model"],
            "audioTranscriptionModelIds": ["audio-model"],
            "deprecatedModelIds": ["old-model"]
        }))
        .unwrap()
        .as_slice(),
    )
    .unwrap();
    let ids: Vec<&str> = catalog.keys().map(String::as_str).collect();
    assert_eq!(ids, ["claude-opus-5-5", "gemini-3.8-flash", "gpt-oss-120b"]);
    let flash = &catalog["gemini-3.8-flash"];
    assert_eq!(flash.name.as_deref(), Some("Flash"));
    assert_eq!(flash.context_length, Some(500_000));
    assert_eq!(flash.max_output_tokens, Some(8_192));
    assert!(flash.tags.contains(&"vision".to_owned()));
    // Family defaults when the entry says nothing.
    assert_eq!(catalog["claude-opus-5-5"].context_length, Some(1_048_576));
    assert_eq!(catalog["claude-opus-5-5"].max_output_tokens, Some(64_000));
    assert_eq!(catalog["gpt-oss-120b"].max_output_tokens, Some(65_536));
    assert!(!catalog["gpt-oss-120b"].tags.contains(&"vision".to_owned()));
}

#[test]
fn the_user_agent_version_can_be_configured() {
    assert_eq!(cloud_code_user_agent(None), CLOUD_CODE_USER_AGENT);
    assert_eq!(cloud_code_user_agent(Some("")), CLOUD_CODE_USER_AGENT);
    assert_eq!(
        cloud_code_user_agent(Some("2.31.4")),
        "antigravity/hub/2.31.4 (aidev_client; os_type=windows; arch=amd64; cl=1003475615)"
    );
    for bad in ["x", "2..1", "2.", "2.1; drop", "a.b", "99999999999999999"] {
        assert_eq!(
            cloud_code_user_agent(Some(bad)),
            CLOUD_CODE_USER_AGENT,
            "{bad}"
        );
        assert!(!client_version_ok(bad), "{bad}");
    }
}

#[tokio::test]
async fn discovery_reads_the_tier_and_onboards_up_to_three_times() {
    let server = MockServer::start().await;
    // The account has no project yet: it is offered tiers, and the third
    // onboarding finishes.
    Mock::given(method("POST"))
        .and(path("/v1internal:loadCodeAssist"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "allowedTiers": [{"id": "legacy-tier"}, {"id": "standard-tier", "isDefault": true}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1internal:onboardUser"))
        .and(body_partial_json(json!({"tierId": "standard-tier"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"done": false})))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1internal:onboardUser"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "done": true,
            "response": {"cloudaicompanionProject": {"id": "proj-onboarded"}}
        })))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let found = discover_project_with(
        &http,
        "synthetic-token",
        &[server.uri()],
        Duration::from_millis(1),
        Some("2.31.4"),
    )
    .await;
    assert_eq!(found.project.as_deref(), Some("proj-onboarded"));
    assert_eq!(found.tier.as_deref(), Some("standard-tier"));
    let onboards = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/v1internal:onboardUser")
        .count();
    assert_eq!(onboards, 3);
}

#[tokio::test]
async fn discovery_reports_the_current_tier_and_no_project_when_none_exists() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1internal:loadCodeAssist"))
        .and(header(
            "user-agent",
            cloud_code_user_agent(Some("2.31.4")).as_str(),
        ))
        .and(body_partial_json(
            json!({"metadata": {"ideVersion": "2.31.4", "ideType": "ANTIGRAVITY"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "currentTier": {"id": "free-tier"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1internal:onboardUser"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let found = discover_project_with(
        &http,
        "synthetic-token",
        &[server.uri()],
        Duration::from_millis(1),
        Some("2.31.4"),
    )
    .await;
    assert_eq!(found.project, None);
    assert_eq!(found.tier.as_deref(), Some("free-tier"));
    // With no project the envelope omits `project`: an empty one is a 404.
    let envelope = build_cloud_code_envelope(
        "gemini-3.8-flash",
        found.project.as_deref().unwrap_or(""),
        &[LlmMessage::user("hi")],
        &[],
        100,
        None,
    )
    .unwrap();
    assert!(envelope.get("project").is_none());
}

#[test]
fn quota_windows_come_from_the_summary_and_fall_back_to_the_model_list() {
    let windows = parse_quota_summary(&json!({"groups": [
        {"displayName": "Claude and GPT", "buckets": [
            {"remainingFraction": 0.25, "window": "5h", "resetTime": "2026-11-16T10:00:00Z"},
            {"remainingFraction": 1.0, "bucketId": "weekly-week", "resetTime": "2026-11-20T00:00:00Z"}
        ]},
        {"groupId": "c-gemini", "buckets": [
            {"remainingFraction": "bad"},
            {"remainingFraction": 0.0, "window": "hourly"}
        ]}
    ]}));
    assert_eq!(windows.len(), 3);
    assert_eq!(windows[0]["scope"], "Claude and GPT");
    assert_eq!(windows[0]["label"], "5h");
    assert_eq!(windows[0]["used_percent"], 75.0);
    assert_eq!(windows[0]["remaining"], 0.25);
    assert_eq!(windows[0]["resets_at"], "2026-11-16T10:00:00Z");
    assert_eq!(windows[1]["label"], "week");
    assert_eq!(windows[1]["used_percent"], 0.0);
    assert_eq!(windows[2]["scope"], "c-gemini");
    assert_eq!(windows[2]["label"], "5h");
    assert_eq!(windows[2]["used_percent"], 100.0);

    let models = parse_quota_models(&json!({"models": {
        "gemini-3.8-flash": {"quotaInfo": {"remainingFraction": 0.5, "resetTime": "2026-11-16T10:00:00Z"}},
        "claude-opus-5-5": {},
        "bad/id": {"quotaInfo": {"remainingFraction": 0.1}}
    }}));
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["scope"], "gemini-3.8-flash");
    assert_eq!(models[0]["used_percent"], 50.0);
    assert!(parse_quota_summary(&json!({})).is_empty());
    assert!(parse_quota_models(&json!({"models": []})).is_empty());
}

#[tokio::test]
async fn reading_quota_falls_back_and_never_errors() {
    let http = reqwest::Client::new();
    let server = MockServer::start().await;
    // Summary refused, the model list carries quotaInfo.
    Mock::given(method("POST"))
        .and(path("/v1internal:retrieveUserQuotaSummary"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1internal:fetchAvailableModels"))
        .and(body_partial_json(json!({"project": "proj-synthetic"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": {
            "gemini-3.8-flash": {"quotaInfo": {"remainingFraction": 0.4, "resetTime": "2026-11-16T10:00:00Z"}}
        }})))
        .mount(&server)
        .await;
    let reading = read_quota(
        &http,
        "synthetic-token",
        "proj-synthetic",
        &[server.uri()],
        None,
    )
    .await
    .unwrap();
    assert_eq!(reading["source"], "models");
    assert_eq!(reading["windows"][0]["used_percent"], 60.0);
    assert!(reading["sampled_at"].as_str().is_some());

    // Both calls failing is "no reading".
    let broken = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&broken)
        .await;
    assert!(
        read_quota(&http, "synthetic-token", "", &[broken.uri()], None)
            .await
            .is_none()
    );
    // An origin that is not a Cloud Code host is skipped.
    assert!(
        read_quota(
            &http,
            "synthetic-token",
            "",
            &["https://evil.example".into()],
            None
        )
        .await
        .is_none()
    );
}

#[test]
fn discovery_asks_daily_then_prod_only() {
    let origins = discovery_origins("https://daily-cloudcode-pa.googleapis.com").unwrap();
    assert_eq!(
        origins,
        [
            "https://daily-cloudcode-pa.googleapis.com",
            "https://cloudcode-pa.googleapis.com"
        ]
    );
    assert_eq!(
        discovery_origins("http://127.0.0.1:9").unwrap(),
        ["http://127.0.0.1:9"]
    );
}
