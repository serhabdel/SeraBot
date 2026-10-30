//! Google Antigravity / Cloud Code Private Assist.
//!
//! One-time owner login is OAuth 2.0 PKCE on a loopback redirect. Chat and the
//! model catalog then stay on the documented Cloud Code hosts. Tokens are
//! returned to the caller for the age store and are never written into errors.

use std::collections::BTreeSet;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use base64::Engine;
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::domain::{LlmMessage, Role};
use crate::error::{Result, SeraError};
use crate::gateway::ToolSpec;
use crate::llm::validate_base_url;

pub const ANTIGRAVITY_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
pub const ANTIGRAVITY_SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs";
pub const CLOUD_CODE_USER_AGENT: &str =
    "antigravity/hub/2.25.0 (aidev_client; os_type=windows; arch=amd64; cl=1003475615)";
pub const CLOUD_CODE_API_CLIENT: &str = "google-cloud-sdk vscode/1.96.0";
pub const THOUGHT_SIGNATURE_SENTINEL: &str = "skip_thought_signature_validator";

const IDE_VERSION: &str = "2.25.0";
const CALLBACK_PATH: &str = "/oauth-callback";
const PREFERRED_PORT: u16 = 51121;
const SESSION_SPACE: u64 = 9_000_000_000_000_000_000;

pub const CLOUD_CODE_HOSTS: &[&str] = &[
    "daily-cloudcode-pa.googleapis.com",
    "cloudcode-pa.googleapis.com",
    "daily-cloudcode-pa.sandbox.googleapis.com",
    "autopush-cloudcode-pa.sandbox.googleapis.com",
];

#[derive(Clone, Debug)]
pub struct ChatTarget {
    pub stream: String,
    pub generate: String,
}

pub struct CloudCodeToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

pub struct CloudCodeCompletion {
    pub text: String,
    pub tool_calls: Vec<CloudCodeToolCall>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub finish_reason: Option<String>,
}

pub struct AntigravityLogin {
    pub access_token: SecretString,
    pub refresh_token: SecretString,
    pub project_id: Option<String>,
}

impl std::fmt::Debug for AntigravityLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AntigravityLogin")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("project_id", &self.project_id)
            .finish()
    }
}

pub fn validate_project_id(kind: &str, project_id: &str) -> Result<()> {
    if project_id.is_empty() {
        return Ok(());
    }
    let kind = kind.trim();
    let kind = if kind.is_empty() || kind == "openai-compatible" {
        "openai-compatible"
    } else {
        kind
    };
    if kind != "google-antigravity" {
        return Err(SeraError::Config(
            "project_id is only valid for google-antigravity".into(),
        ));
    }
    if !project_id_ok(project_id) {
        return Err(SeraError::Config("invalid Cloud Code project id".into()));
    }
    Ok(())
}

pub fn requests_interleaved_thinking(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.contains("claude") && model.contains("thinking")
}

pub fn cloud_code_origins(base: &str) -> Result<Vec<String>> {
    let endpoint = validate_base_url(base)?;
    let parsed = url::Url::parse(&endpoint)
        .map_err(|_| SeraError::Config("invalid provider base url".into()))?;
    if parsed.path() != "/" && !parsed.path().is_empty() {
        return Err(SeraError::Config(
            "google-antigravity base_url must be the Cloud Code origin".into(),
        ));
    }
    let origin = origin_string(&parsed);
    if is_exact_loopback(&parsed) {
        return Ok(vec![origin]);
    }
    let host = parsed.host_str().unwrap_or("");
    if !pinned_cloud_code_host(host) {
        return Err(SeraError::Config(
            "google-antigravity only contacts documented Cloud Code hosts".into(),
        ));
    }
    let mut origins = vec![origin];
    for host in CLOUD_CODE_HOSTS {
        let candidate = format!("https://{host}");
        if !origins
            .iter()
            .any(|item| item.eq_ignore_ascii_case(&candidate))
        {
            origins.push(candidate);
        }
    }
    Ok(origins)
}

pub fn primary_catalog_url(base: &str) -> Result<String> {
    let origin = cloud_code_origins(base)?
        .into_iter()
        .next()
        .ok_or_else(|| SeraError::Config("invalid provider base url".into()))?;
    validate_base_url(&format!("{origin}/v1internal:fetchAvailableModels"))
}

pub fn catalog_urls(catalog_or_base: &str) -> Result<Vec<String>> {
    let parsed = url::Url::parse(catalog_or_base)
        .map_err(|_| SeraError::Config("invalid catalog base url".into()))?;
    let origin = origin_string(&parsed);
    Ok(cloud_code_origins(&origin)?
        .into_iter()
        .map(|item| format!("{item}/v1internal:fetchAvailableModels"))
        .collect())
}

pub fn chat_targets(base: &str) -> Result<Vec<ChatTarget>> {
    Ok(cloud_code_origins(base)?
        .into_iter()
        .map(|origin| ChatTarget {
            stream: format!("{origin}/v1internal:streamGenerateContent?alt=sse"),
            generate: format!("{origin}/v1internal:generateContent"),
        })
        .collect())
}

pub fn pkce_s256() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

pub fn authorize_url(redirect_uri: &str, challenge: &str, state: &str) -> Result<String> {
    validate_loopback_redirect(redirect_uri)?;
    if !pkce_token_ok(challenge) || !state_ok(state) {
        return Err(SeraError::External("oauth login could not start".into()));
    }
    let mut url = url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
        .map_err(|_| SeraError::External("oauth login could not start".into()))?;
    url.query_pairs_mut()
        .append_pair("client_id", ANTIGRAVITY_CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", ANTIGRAVITY_SCOPES)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");
    if url.host_str() != Some("accounts.google.com") || url.path() != "/o/oauth2/v2/auth" {
        return Err(SeraError::External("oauth login could not start".into()));
    }
    Ok(url.to_string())
}

pub async fn login_antigravity<F>(client_secret: &str, on_url: F) -> Result<AntigravityLogin>
where
    F: FnOnce(&str),
{
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| SeraError::External("oauth login could not start".into()))?;
    let bases: Vec<String> = CLOUD_CODE_HOSTS
        .iter()
        .map(|host| format!("https://{host}"))
        .collect();
    login_antigravity_at(
        &http,
        client_secret,
        "https://oauth2.googleapis.com/token",
        &bases,
        Duration::from_secs(300),
        Duration::from_secs(2),
        on_url,
    )
    .await
}

pub fn parse_cloud_code_catalog(bytes: &[u8]) -> Result<BTreeSet<String>> {
    if bytes.len() > 1_048_576 {
        return Err(SeraError::External("model catalog is too large".into()));
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| SeraError::External("malformed model catalog".into()))?;
    let models = value
        .get("models")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            SeraError::External("model catalog was not a Cloud Code model map".into())
        })?;
    if models.len() > 4_000 {
        return Err(SeraError::External("model catalog is too large".into()));
    }
    let excluded = id_set(value.get("tabModelIds"))
        .into_iter()
        .chain(id_set(value.get("imageGenerationModelIds")))
        .chain(id_set(value.get("audioTranscriptionModelIds")))
        .chain(id_set(value.get("deprecatedModelIds")))
        .collect::<BTreeSet<_>>();
    let mut ids = BTreeSet::new();
    for id in models.keys() {
        if excluded.contains(id) || !cloud_code_model_id(id) {
            continue;
        }
        ids.insert(id.clone());
    }
    if ids.is_empty() {
        return Err(SeraError::External(
            "model catalog contained no usable model ids".into(),
        ));
    }
    Ok(ids)
}

pub fn parse_cloud_code_completion(bytes: &[u8]) -> Result<CloudCodeCompletion> {
    if bytes.len() > 4_194_304 {
        return Err(SeraError::External(
            "Cloud Code response is too large".into(),
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| SeraError::External("malformed Cloud Code response".into()))?;
    let trimmed = text.trim();
    let mut completion = CloudCodeCompletion {
        text: String::new(),
        tool_calls: Vec::new(),
        prompt_tokens: 0,
        completion_tokens: 0,
        finish_reason: None,
    };
    if trimmed.starts_with('{') {
        let value: Value = serde_json::from_str(trimmed)
            .map_err(|_| SeraError::External("malformed Cloud Code response".into()))?;
        absorb_completion(&mut completion, &value)?;
    } else {
        let mut saw = false;
        for line in text.lines() {
            let Some(data) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let value: Value = serde_json::from_str(data)
                .map_err(|_| SeraError::External("malformed Cloud Code response".into()))?;
            absorb_completion(&mut completion, &value)?;
            saw = true;
        }
        if !saw {
            return Err(SeraError::External("malformed Cloud Code response".into()));
        }
    }
    if completion.text.trim().is_empty() && completion.tool_calls.is_empty() {
        return Err(SeraError::External(
            "Cloud Code response contained no content".into(),
        ));
    }
    if completion.text.trim().is_empty() {
        completion.text.clear();
    }
    Ok(completion)
}

pub fn sanitize_schema(value: &Value) -> Value {
    let mut value = value.clone();
    let defs = value
        .get("$defs")
        .cloned()
        .or_else(|| value.get("definitions").cloned())
        .unwrap_or(Value::Null);
    let mut stack = Vec::new();
    hoist_refs(&mut value, &defs, 0, &mut stack);
    sanitize_schema_in(&mut value, 0);
    if !value.is_object() {
        placeholder_object()
    } else {
        value
    }
}

pub fn build_cloud_code_envelope(
    model: &str,
    project_id: &str,
    messages: &[LlmMessage],
    tools: &[ToolSpec],
    max_output_tokens: u32,
    temperature: Option<f32>,
) -> Result<Value> {
    if !cloud_code_model_id(model) {
        return Err(SeraError::Config("invalid configured model id".into()));
    }
    if !project_id.is_empty() && !project_id_ok(project_id) {
        return Err(SeraError::Config("invalid Cloud Code project id".into()));
    }
    if tools.len() > 128 {
        return Err(SeraError::Validation("too many tools".into()));
    }
    let mut system = Vec::new();
    let mut names = std::collections::BTreeMap::new();
    for message in messages {
        if message.role == Role::System {
            if let Some(text) = message
                .content
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                system.push(text.to_owned());
            }
        }
        if message.role == Role::Assistant {
            for call in &message.tool_calls {
                names.insert(call.id.clone(), sanitize_tool_name(&call.name));
            }
        }
    }
    let mut turns: Vec<DraftTurn> = Vec::new();
    for message in messages {
        match message.role {
            Role::System => {}
            Role::User => {
                if let Some(text) = non_empty(message.content.as_deref()) {
                    push_turn(&mut turns, "user", vec![json!({"text": text})], false);
                }
            }
            Role::Assistant => {
                let mut parts = Vec::new();
                if let Some(text) = non_empty(message.content.as_deref()) {
                    parts.push(json!({"text": text}));
                }
                for call in &message.tool_calls {
                    let id = if call_id_ok(&call.id) {
                        call.id.clone()
                    } else {
                        format!("call_{}", parts.len())
                    };
                    let arguments = call
                        .arguments
                        .as_object()
                        .cloned()
                        .map(Value::Object)
                        .unwrap_or_else(|| json!({}));
                    parts.push(json!({
                        "thoughtSignature": THOUGHT_SIGNATURE_SENTINEL,
                        "functionCall": {
                            "id": id,
                            "name": sanitize_tool_name(&call.name),
                            "args": arguments,
                        }
                    }));
                }
                push_turn(&mut turns, "model", parts, false);
            }
            Role::Tool => {
                let id = message
                    .tool_call_id
                    .as_deref()
                    .filter(|id| call_id_ok(id))
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("call_{}", turns.len()));
                let name = message
                    .name
                    .as_deref()
                    .map(sanitize_tool_name)
                    .filter(|name| tool_name_ok(name))
                    .or_else(|| names.get(&id).cloned())
                    .unwrap_or_else(|| sanitize_tool_name(&id));
                let result = message.content.clone().unwrap_or_default();
                push_turn(
                    &mut turns,
                    "user",
                    vec![json!({
                        "functionResponse": {
                            "id": id,
                            "name": name,
                            "response": {"result": result, "is_error": false}
                        }
                    })],
                    true,
                );
            }
        }
    }
    let model_lower = model.to_ascii_lowercase();
    if model_lower.contains("claude") && model_lower.contains("thinking") {
        while turns.last().is_some_and(|turn| turn.role == "model") {
            turns.pop();
        }
    }
    if turns.is_empty() {
        return Err(SeraError::Validation(
            "Cloud Code request has no messages".into(),
        ));
    }
    let contents = turns
        .into_iter()
        .map(|turn| json!({"role": turn.role, "parts": turn.parts}))
        .collect::<Vec<_>>();
    let mut request = json!({
        "sessionId": fresh_session_id(),
        "contents": contents,
        "generationConfig": generation_config(model, max_output_tokens, temperature),
    });
    if !system.is_empty() {
        request["systemInstruction"] = json!({"parts": [{"text": system.join("\n\n")}]});
    }
    if !tools.is_empty() {
        let declarations = tools
            .iter()
            .map(|tool| {
                let description = if tool.description.trim().is_empty() {
                    sanitize_tool_name(&tool.name)
                } else {
                    clean_text(&tool.description, 1_024)
                };
                json!({
                    "name": sanitize_tool_name(&tool.name),
                    "description": description,
                    "parameters": sanitize_schema(&tool.parameters),
                })
            })
            .collect::<Vec<_>>();
        request["tools"] = json!([{"functionDeclarations": declarations}]);
        request["toolConfig"] = json!({"functionCallingConfig": {"mode": "VALIDATED"}});
    }
    let mut envelope = json!({
        "model": model,
        "userAgent": "antigravity",
        "requestType": "agent",
        "requestId": fresh_request_id(),
        "request": request,
    });
    if !project_id.is_empty() {
        envelope["project"] = json!(project_id);
    }
    Ok(envelope)
}

/// Finish a PKCE sign-in whose browser leg ran on the owner's machine: the
/// console holds the verifier and loopback listener, this side holds the client
/// secret. Tokens go back to the caller only as `SecretString`.
pub async fn complete_antigravity_login(
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<AntigravityLogin> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| SeraError::External("oauth login could not start".into()))?;
    let bases: Vec<String> = CLOUD_CODE_HOSTS
        .iter()
        .map(|host| format!("https://{host}"))
        .collect();
    complete_antigravity_login_at(
        &http,
        client_secret,
        "https://oauth2.googleapis.com/token",
        &bases,
        Duration::from_secs(2),
        code,
        redirect_uri,
        verifier,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn complete_antigravity_login_at(
    http: &reqwest::Client,
    client_secret: &str,
    token_url: &str,
    bases: &[String],
    pause: Duration,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<AntigravityLogin> {
    if !credential_ok(client_secret) {
        return Err(SeraError::Config("oauth client secret is missing".into()));
    }
    if !credential_ok(code) || code.len() > 2_048 || !pkce_token_ok(verifier) {
        return Err(SeraError::Validation(
            "sign-in code or verifier is not valid".into(),
        ));
    }
    let token_url = pin_token_url(token_url)?;
    let (access_token, refresh_token) = exchange_code(
        http,
        &token_url,
        client_secret,
        code,
        redirect_uri,
        verifier,
    )
    .await?;
    let project_id = discover_project(http, access_token.expose_secret(), bases, pause).await;
    Ok(AntigravityLogin {
        access_token,
        refresh_token,
        project_id,
    })
}

async fn login_antigravity_at<F>(
    http: &reqwest::Client,
    client_secret: &str,
    token_url: &str,
    bases: &[String],
    timeout: Duration,
    pause: Duration,
    on_url: F,
) -> Result<AntigravityLogin>
where
    F: FnOnce(&str),
{
    if !credential_ok(client_secret) {
        return Err(SeraError::Config("oauth client secret is missing".into()));
    }
    let token_url = pin_token_url(token_url)?;
    let (listener, port) = bind_loopback().await?;
    let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
    let (mut verifier, challenge) = pkce_s256();
    let state = random_token();
    let url = authorize_url(&redirect_uri, &challenge, &state)?;
    on_url(&url);
    let code = match tokio::time::timeout(timeout, accept_callback(listener, &state)).await {
        Ok(result) => result,
        Err(_) => {
            verifier.zeroize();
            return Err(SeraError::External("Google sign-in timed out".into()));
        }
    };
    let mut code = match code {
        Ok(code) => code,
        Err(error) => {
            verifier.zeroize();
            return Err(error);
        }
    };
    let mut secret = client_secret.to_owned();
    let grant = exchange_code(http, &token_url, &secret, &code, &redirect_uri, &verifier).await;
    verifier.zeroize();
    code.zeroize();
    secret.zeroize();
    let (access_token, refresh_token) = grant?;
    let project_id = discover_project(http, access_token.expose_secret(), bases, pause).await;
    Ok(AntigravityLogin {
        access_token,
        refresh_token,
        project_id,
    })
}

fn generation_config(model: &str, max_output_tokens: u32, temperature: Option<f32>) -> Value {
    let lower = model.to_ascii_lowercase();
    let claude = lower.contains("claude");
    let cap: u32 = if claude { 64_000 } else { 65_536 };
    let mut config = json!({
        "maxOutputTokens": max_output_tokens.clamp(1, cap),
    });
    let thinking = claude && lower.contains("thinking");
    if let Some(temperature) = temperature.filter(|_| !thinking) {
        config["temperature"] = json!(temperature.clamp(0.0, 2.0));
    }
    if thinking {
        config["thinkingConfig"] = json!({
            "includeThoughts": true,
            "thinkingBudget": 8192
        });
    } else if level_thinking_model(&lower) {
        config["thinkingConfig"] = json!({
            "includeThoughts": true,
            "thinkingLevel": "medium"
        });
    }
    config
}

fn level_thinking_model(model: &str) -> bool {
    if model.contains("claude") || model.contains("gpt-oss") {
        return false;
    }
    if model.ends_with("-tiered") {
        return true;
    }
    if model.ends_with("-low") || model.ends_with("-medium") || model.ends_with("-high") {
        return false;
    }
    model.starts_with("gemini-3") || model.starts_with("gemini-4")
}

struct DraftTurn {
    role: &'static str,
    parts: Vec<Value>,
    function_only: bool,
}

fn push_turn(
    turns: &mut Vec<DraftTurn>,
    role: &'static str,
    parts: Vec<Value>,
    function_only: bool,
) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = turns.last_mut() {
        if last.role == role && last.function_only == function_only {
            last.parts.extend(parts);
            return;
        }
    }
    turns.push(DraftTurn {
        role,
        parts,
        function_only,
    });
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

fn absorb_completion(completion: &mut CloudCodeCompletion, value: &Value) -> Result<()> {
    let root = value.get("response").unwrap_or(value);
    if let Some(usage) = root.get("usageMetadata") {
        if let Some(prompt) = usage.get("promptTokenCount").and_then(json_u64) {
            completion.prompt_tokens = prompt;
        }
        if let Some(output) = usage.get("candidatesTokenCount").and_then(json_u64) {
            completion.completion_tokens = output;
        }
    }
    let Some(candidate) = root
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
    else {
        return Ok(());
    };
    if let Some(reason) = candidate
        .get("finishReason")
        .and_then(Value::as_str)
        .and_then(clean_finish)
    {
        completion.finish_reason = Some(reason);
    }
    let Some(parts) = candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    for part in parts {
        if part.get("thought").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            completion.text.push_str(text);
        }
        if let Some(call) = part.get("functionCall") {
            let Some(name) = call.get("name").and_then(Value::as_str) else {
                continue;
            };
            if !tool_name_ok(name) {
                continue;
            }
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| call_id_ok(id))
                .map(str::to_owned)
                .unwrap_or_else(|| format!("call_{}", completion.tool_calls.len()));
            let arguments = match call.get("args") {
                Some(Value::Object(map)) => Value::Object(map.clone()),
                Some(Value::String(text)) => {
                    serde_json::from_str(text).unwrap_or_else(|_| json!({}))
                }
                _ => json!({}),
            };
            if let Some(existing) = completion.tool_calls.iter_mut().find(|item| item.id == id) {
                existing.name = name.to_owned();
                existing.arguments = arguments;
            } else {
                completion.tool_calls.push(CloudCodeToolCall {
                    id,
                    name: name.to_owned(),
                    arguments,
                });
            }
        }
    }
    Ok(())
}

fn json_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_i64()
            .filter(|number| *number >= 0)
            .map(|number| number as u64)
    })
}

fn clean_finish(value: &str) -> Option<String> {
    if (1..=64).contains(&value.len())
        && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        Some(value.to_owned())
    } else {
        None
    }
}

fn id_set(value: Option<&Value>) -> BTreeSet<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|id| cloud_code_model_id(id))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn hoist_refs(value: &mut Value, defs: &Value, depth: u8, stack: &mut Vec<String>) {
    if depth > 8 {
        *value = json!({"type": "STRING"});
        return;
    }
    let reference = value.get("$ref").and_then(Value::as_str).map(str::to_owned);
    if let Some(reference) = reference {
        if stack.iter().any(|item| item == &reference) {
            *value = json!({"type": "STRING"});
            return;
        }
        let name = reference
            .strip_prefix("#/$defs/")
            .or_else(|| reference.strip_prefix("#/definitions/"));
        if let Some(name) = name {
            if let Some(target) = defs.get(name).cloned() {
                stack.push(reference);
                *value = target;
                hoist_refs(value, defs, depth + 1, stack);
                stack.pop();
                return;
            }
        }
    }
    match value {
        Value::Array(items) => {
            for item in items {
                hoist_refs(item, defs, depth + 1, stack);
            }
        }
        Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if let Some(child) = map.get_mut(&key) {
                    hoist_refs(child, defs, depth + 1, stack);
                }
            }
        }
        _ => {}
    }
}

fn sanitize_schema_in(value: &mut Value, depth: u8) {
    if depth > 12 {
        *value = json!({"type": "STRING"});
        return;
    }
    let Value::Object(obj) = value else {
        return;
    };
    if let Some(Value::Array(branches)) = obj.remove("allOf") {
        for branch in branches {
            if let Value::Object(map) = branch {
                for (key, child) in map {
                    obj.entry(key).or_insert(child);
                }
            }
        }
    }
    if let Some(Value::Array(branches)) = obj.remove("anyOf").or_else(|| obj.remove("oneOf")) {
        for branch in branches {
            let Value::Object(map) = branch else {
                continue;
            };
            let is_null = map
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("null"));
            if is_null {
                continue;
            }
            for (key, child) in map {
                obj.entry(key).or_insert(child);
            }
            break;
        }
    }
    if let Some(Value::Array(types)) = obj.get("type").cloned() {
        let mut nullable = false;
        let mut chosen = None;
        for item in types {
            let Some(name) = item.as_str() else {
                continue;
            };
            if name.eq_ignore_ascii_case("null") {
                nullable = true;
            } else if chosen.is_none() {
                chosen = Some(name.to_owned());
            }
        }
        if nullable {
            obj.insert("nullable".into(), Value::Bool(true));
        }
        match chosen {
            Some(chosen) => {
                obj.insert("type".into(), Value::String(chosen));
            }
            None => {
                obj.remove("type");
            }
        }
    }
    if let Some(constant) = obj.remove("const") {
        if !obj.contains_key("enum") {
            obj.insert("enum".into(), Value::Array(vec![json_string(&constant)]));
        }
    }
    if let Some(Value::Array(items)) = obj.get_mut("enum") {
        for item in items.iter_mut() {
            if !item.is_string() {
                *item = json_string(item);
            }
        }
    }
    append_hints(obj);
    const STRIPPED: &[&str] = &[
        "$schema",
        "$defs",
        "definitions",
        "$id",
        "$ref",
        "additionalProperties",
        "patternProperties",
        "unevaluatedProperties",
        "minLength",
        "maxLength",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "pattern",
        "format",
        "default",
        "examples",
        "title",
        "const",
        "anyOf",
        "oneOf",
        "allOf",
        "not",
        "if",
        "then",
        "else",
        "dependentRequired",
        "dependentSchemas",
        "propertyNames",
        "contentMediaType",
        "contentEncoding",
    ];
    for key in STRIPPED {
        obj.remove(*key);
    }
    if let Some(name) = obj.get("type").and_then(Value::as_str).map(str::to_owned) {
        let upper = match name.to_ascii_lowercase().as_str() {
            "object" => "OBJECT",
            "string" => "STRING",
            "integer" => "INTEGER",
            "number" => "NUMBER",
            "boolean" => "BOOLEAN",
            "array" => "ARRAY",
            _ => "STRING",
        };
        obj.insert("type".into(), Value::String(upper.to_owned()));
    }
    let property_names = obj
        .get("properties")
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    if let Some(Value::Object(properties)) = obj.get_mut("properties") {
        for name in property_names {
            if let Some(child) = properties.get_mut(&name) {
                sanitize_schema_in(child, depth + 1);
            }
        }
    }
    match obj.get_mut("items") {
        Some(Value::Array(items)) => {
            for item in items {
                sanitize_schema_in(item, depth + 1);
            }
        }
        Some(other) => sanitize_schema_in(other, depth + 1),
        None => {}
    }
    let is_object = obj.get("type").and_then(Value::as_str) == Some("OBJECT");
    if is_object {
        let empty = match obj.get("properties") {
            None => true,
            Some(Value::Object(map)) => map.is_empty(),
            Some(_) => true,
        };
        if empty {
            obj.insert(
                "properties".into(),
                json!({"_placeholder": {"type": "BOOLEAN", "description": "unused"}}),
            );
        }
    }
}

fn append_hints(obj: &mut Map<String, Value>) {
    const HINTS: &[&str] = &[
        "minLength",
        "maxLength",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "pattern",
        "format",
    ];
    let mut hints = Vec::new();
    for key in HINTS {
        if let Some(text) = obj.get(*key).and_then(hint_text) {
            hints.push(format!("{key}={text}"));
        }
    }
    if hints.is_empty() {
        return;
    }
    let extra = hints.join(", ");
    let description = obj.get("description").and_then(Value::as_str).unwrap_or("");
    let mut combined = if description.is_empty() {
        extra
    } else {
        format!("{description} ({extra})")
    };
    combined.truncate(512);
    obj.insert("description".into(), Value::String(combined));
}

fn hint_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if text.len() <= 80 && !text.chars().any(char::is_control) => {
            Some(text.clone())
        }
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn json_string(value: &Value) -> Value {
    Value::String(match value {
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Null => "null".into(),
        other => other.to_string(),
    })
}

fn placeholder_object() -> Value {
    json!({
        "type": "OBJECT",
        "properties": {"_placeholder": {"type": "BOOLEAN", "description": "unused"}}
    })
}

fn sanitize_tool_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else if !out.is_empty() {
            out.push('_');
        }
        if out.len() == 64 {
            break;
        }
    }
    if out.is_empty() || !out.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        out.insert(0, 't');
    }
    out.truncate(64);
    out
}

fn clean_text(value: &str, max: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}

pub(crate) fn pinned_cloud_code_host(host: &str) -> bool {
    CLOUD_CODE_HOSTS
        .iter()
        .any(|item| host.eq_ignore_ascii_case(item))
}

fn origin_string(url: &url::Url) -> String {
    match url.port() {
        Some(port) => format!("{}://{}:{port}", url.scheme(), url.host_str().unwrap_or("")),
        None => format!("{}://{}", url.scheme(), url.host_str().unwrap_or("")),
    }
}

fn is_exact_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip == std::net::Ipv4Addr::LOCALHOST,
        Some(url::Host::Ipv6(ip)) => ip == std::net::Ipv6Addr::LOCALHOST,
        _ => false,
    }
}

fn project_id_ok(value: &str) -> bool {
    (1..=256).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'))
}

fn cloud_code_model_id(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 256
        && model.trim() == model
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '+'))
}

fn tool_name_ok(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

fn call_id_ok(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\')
}

fn credential_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 8_192
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn pkce_token_ok(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn state_ok(value: &str) -> bool {
    (16..=128).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn random_token() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn fresh_session_id() -> String {
    let mut bytes = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let value = u64::from_be_bytes(bytes) % SESSION_SPACE;
    format!("-{}", value.max(1))
}

fn fresh_request_id() -> String {
    format!("agent-{}", uuid::Uuid::new_v4().simple())
}

fn validate_loopback_redirect(redirect: &str) -> Result<()> {
    let url = url::Url::parse(redirect)
        .map_err(|_| SeraError::External("oauth redirect was rejected".into()))?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || url.path() != CALLBACK_PATH
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SeraError::External("oauth redirect was rejected".into()));
    }
    Ok(())
}

fn pin_token_url(url: &str) -> Result<String> {
    let endpoint = validate_base_url(url)?;
    let parsed = url::Url::parse(&endpoint)
        .map_err(|_| SeraError::Config("invalid oauth token url".into()))?;
    let loopback = is_exact_loopback(&parsed) && parsed.path() == "/token";
    let google = parsed
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("oauth2.googleapis.com"))
        && parsed.path() == "/token";
    if loopback || google {
        Ok(endpoint)
    } else {
        Err(SeraError::Config(
            "oauth token url is not the documented endpoint for this provider".into(),
        ))
    }
}

async fn bind_loopback() -> Result<(tokio::net::TcpListener, u16)> {
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", PREFERRED_PORT)).await {
        Ok(listener) => listener,
        Err(_) => tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|_| {
                SeraError::External("oauth callback could not listen on loopback".into())
            })?,
    };
    let port = listener
        .local_addr()
        .map_err(|_| SeraError::External("oauth callback could not listen on loopback".into()))?
        .port();
    Ok((listener, port))
}

async fn accept_callback(listener: tokio::net::TcpListener, state: &str) -> Result<String> {
    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|_| SeraError::External("oauth callback was rejected".into()))?;
    let mut buf = vec![0u8; 8_192];
    let mut filled = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if filled >= buf.len() {
            return Err(SeraError::External("oauth callback was rejected".into()));
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(SeraError::External("oauth callback was rejected".into()));
        }
        let read = tokio::time::timeout(remaining, stream.read(&mut buf[filled..]))
            .await
            .map_err(|_| SeraError::External("oauth callback was rejected".into()))?
            .map_err(|_| SeraError::External("oauth callback was rejected".into()))?;
        if read == 0 {
            break;
        }
        filled += read;
        if buf[..filled].windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buf[..filled]);
    let code = match callback_code(&request, state) {
        Ok(code) => code,
        Err(error) => {
            let _ = write_callback_response(&mut stream, false).await;
            return Err(error);
        }
    };
    let _ = write_callback_response(&mut stream, true).await;
    Ok(code)
}

fn callback_code(request: &str, expected_state: &str) -> Result<String> {
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| SeraError::External("oauth callback was rejected".into()))?;
    if target.len() > 4_096 || !target.starts_with(CALLBACK_PATH) {
        return Err(SeraError::External("oauth callback was rejected".into()));
    }
    let parsed = url::Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|_| SeraError::External("oauth callback was rejected".into()))?;
    if parsed.path() != CALLBACK_PATH {
        return Err(SeraError::External("oauth callback was rejected".into()));
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    if state.as_deref() != Some(expected_state) {
        return Err(SeraError::External("Google sign-in was rejected".into()));
    }
    if error.as_deref() == Some("access_denied") {
        return Err(SeraError::External("Google sign-in was denied".into()));
    }
    if error.is_some() {
        return Err(SeraError::External("Google sign-in was rejected".into()));
    }
    let code = code.ok_or_else(|| SeraError::External("oauth callback was rejected".into()))?;
    if !credential_ok(&code) {
        return Err(SeraError::External("oauth callback was rejected".into()));
    }
    Ok(code)
}

async fn write_callback_response(stream: &mut tokio::net::TcpStream, ok: bool) -> Result<()> {
    let body = if ok {
        "SeraBot received the Google redirect. You can close this window.\n"
    } else {
        "SeraBot rejected the Google redirect. You can close this window.\n"
    };
    let status = if ok { "200 OK" } else { "400 Bad Request" };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(header.as_bytes())
        .await
        .map_err(|_| SeraError::External("oauth callback was rejected".into()))?;
    stream
        .write_all(body.as_bytes())
        .await
        .map_err(|_| SeraError::External("oauth callback was rejected".into()))?;
    let _ = stream.shutdown().await;
    Ok(())
}

async fn exchange_code(
    http: &reqwest::Client,
    token_url: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<(SecretString, SecretString)> {
    validate_loopback_redirect(redirect_uri)?;
    let response = http
        .post(token_url)
        .header("user-agent", "SeraBot/0.1")
        .header("accept", "application/json")
        .form(&[
            ("client_id", ANTIGRAVITY_CLIENT_ID),
            ("client_secret", client_secret),
            ("code", code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|_| SeraError::External("oauth token exchange failed".into()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| SeraError::External("oauth token exchange failed".into()))?;
    if bytes.len() > 65_536 || !status.is_success() {
        return Err(SeraError::External(format!(
            "oauth token exchange failed with status {}",
            status.as_u16()
        )));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| SeraError::External("oauth token exchange failed".into()))?;
    let access = required_token(&value, "access_token")?;
    let refresh = required_token(&value, "refresh_token")?;
    Ok((SecretString::new(access), SecretString::new(refresh)))
}

fn required_token(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|token| credential_ok(token))
        .map(str::to_owned)
        .ok_or_else(|| SeraError::External("oauth token exchange failed".into()))
}

async fn discover_project(
    http: &reqwest::Client,
    access_token: &str,
    bases: &[String],
    pause: Duration,
) -> Option<String> {
    let pause = pause.min(Duration::from_secs(5));
    let metadata = json!({
        "ideType": "ANTIGRAVITY",
        "ideVersion": IDE_VERSION,
        "platform": "WINDOWS_AMD64"
    });
    for base in bases {
        let Ok(origin) = one_origin(base) else {
            continue;
        };
        let Ok((status, body)) = post_cloud_json(
            http,
            &format!("{origin}/v1internal:loadCodeAssist"),
            access_token,
            &json!({"metadata": metadata}),
        )
        .await
        else {
            continue;
        };
        if !status.is_success() {
            continue;
        }
        if let Some(project) = extract_project(&body) {
            return Some(project);
        }
        let tier = default_tier(&body).unwrap_or_else(|| "free-tier".into());
        for _ in 0..3 {
            let Ok((status, onboard)) = post_cloud_json(
                http,
                &format!("{origin}/v1internal:onboardUser"),
                access_token,
                &json!({"tierId": tier, "metadata": metadata}),
            )
            .await
            else {
                break;
            };
            if status.is_success() {
                if let Some(project) = extract_project(&onboard)
                    .or_else(|| onboard.get("response").and_then(extract_project))
                {
                    return Some(project);
                }
            }
            sleep_pause(pause).await;
            let Ok((status, again)) = post_cloud_json(
                http,
                &format!("{origin}/v1internal:loadCodeAssist"),
                access_token,
                &json!({"metadata": metadata}),
            )
            .await
            else {
                break;
            };
            if status.is_success() {
                if let Some(project) = extract_project(&again) {
                    return Some(project);
                }
            }
        }
    }
    None
}

async fn sleep_pause(pause: Duration) {
    tokio::time::sleep(pause).await;
}

fn one_origin(base: &str) -> Result<String> {
    let endpoint = validate_base_url(base)?;
    let parsed = url::Url::parse(&endpoint)
        .map_err(|_| SeraError::Config("invalid provider base url".into()))?;
    if parsed.path() != "/" && !parsed.path().is_empty() {
        return Err(SeraError::Config(
            "google-antigravity base_url must be the Cloud Code origin".into(),
        ));
    }
    if is_exact_loopback(&parsed) || pinned_cloud_code_host(parsed.host_str().unwrap_or("")) {
        Ok(origin_string(&parsed))
    } else {
        Err(SeraError::Config(
            "google-antigravity only contacts documented Cloud Code hosts".into(),
        ))
    }
}

async fn post_cloud_json(
    http: &reqwest::Client,
    url: &str,
    access_token: &str,
    body: &Value,
) -> Result<(reqwest::StatusCode, Value)> {
    let response = http
        .post(url)
        .header("user-agent", CLOUD_CODE_USER_AGENT)
        .header("x-goog-api-client", CLOUD_CODE_API_CLIENT)
        .header("accept", "application/json")
        .bearer_auth(access_token)
        .json(body)
        .send()
        .await
        .map_err(|_| SeraError::External("Cloud Code request failed".into()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| SeraError::External("Cloud Code request failed".into()))?;
    if bytes.len() > 1_048_576 {
        return Err(SeraError::External(
            "Cloud Code response is too large".into(),
        ));
    }
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Ok((status, value))
}

fn extract_project(value: &Value) -> Option<String> {
    let node = value.get("cloudaicompanionProject")?;
    let id = node
        .as_str()
        .or_else(|| node.get("id").and_then(Value::as_str))?;
    project_id_ok(id).then(|| id.to_owned())
}

fn default_tier(value: &Value) -> Option<String> {
    let tiers = value.get("allowedTiers")?.as_array()?;
    let mut first = None;
    for tier in tiers {
        let id = tier.get("id")?.as_str()?;
        if !(1..=64).contains(&id.len())
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            continue;
        }
        if first.is_none() {
            first = Some(id.to_owned());
        }
        if tier.get("isDefault").and_then(Value::as_bool) == Some(true) {
            return Some(id.to_owned());
        }
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ToolCall;
    use secrecy::ExposeSecret;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn pkce_challenge_is_s256_and_authorize_url_stays_on_google() {
        let (verifier, challenge) = pkce_s256();
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
        let url = authorize_url(
            "http://127.0.0.1:51121/oauth-callback",
            &challenge,
            &random_token(),
        )
        .unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        assert_eq!(parsed.host_str(), Some("accounts.google.com"));
        assert_eq!(parsed.path(), "/o/oauth2/v2/auth");
        let query = parsed.query().unwrap_or("");
        assert!(query.contains("code_challenge_method=S256"));
        assert!(query.contains("access_type=offline"));
        assert!(query.contains("prompt=consent"));
        assert!(!url.contains(&verifier));
        assert!(!url.contains("code_verifier"));
        assert!(
            authorize_url(
                "https://evil.example/oauth-callback",
                &challenge,
                &random_token()
            )
            .is_err()
        );
        assert!(validate_project_id("mistral", "proj").is_err());
        assert!(validate_project_id("google-antigravity", "proj-1").is_ok());
        assert!(validate_project_id("google-antigravity", "bad id").is_err());
    }

    #[test]
    fn origins_pin_documented_hosts_and_keep_loopback_alone() {
        let origins = cloud_code_origins("https://cloudcode-pa.googleapis.com").unwrap();
        assert_eq!(origins[0], "https://cloudcode-pa.googleapis.com");
        assert_eq!(origins.len(), 4);
        assert!(origins.iter().all(|origin| {
            CLOUD_CODE_HOSTS
                .iter()
                .any(|host| origin == &format!("https://{host}"))
        }));
        let loopback = cloud_code_origins("http://127.0.0.1:9").unwrap();
        assert_eq!(loopback, vec!["http://127.0.0.1:9".to_string()]);
        assert!(cloud_code_origins("https://evil.example").is_err());
        assert!(cloud_code_origins("https://daily-cloudcode-pa.googleapis.com/v1").is_err());
        assert_eq!(
            primary_catalog_url("https://daily-cloudcode-pa.googleapis.com").unwrap(),
            "https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels"
        );
    }

    #[test]
    fn catalog_parser_is_a_model_map_and_drops_non_chat_ids() {
        let secret = "synthetic-catalog-secret";
        let ids = parse_cloud_code_catalog(
            serde_json::to_vec(&json!({
                "models": {
                    "gemini-3.8-flash": {"displayName": "Flash"},
                    "imagen-dropped": {"note": secret},
                    "bad/id": {"note": secret},
                    "claude-sonnet-5-5-thinking": {}
                },
                "imageGenerationModelIds": ["imagen-dropped"],
                "deprecatedModelIds": ["claude-sonnet-5-5-thinking"]
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap();
        assert_eq!(
            ids.into_iter().collect::<Vec<_>>(),
            vec!["gemini-3.8-flash".to_string()]
        );
        let rejected = parse_cloud_code_catalog(
            serde_json::to_vec(
                &json!({"object": "list", "data": [{"id": "kept"}], "note": secret}),
            )
            .unwrap()
            .as_slice(),
        )
        .unwrap_err();
        let message = rejected.to_string();
        assert!(message.contains("Cloud Code model map"));
        assert!(!message.contains(secret));
    }

    #[test]
    fn schema_sanitizer_uppercases_types_and_strips_rejected_keywords() {
        let sanitized = sanitize_schema(&json!({
            "type": "object",
            "additionalProperties": false,
            "$defs": {"q": {"type": "string", "minLength": 2}},
            "properties": {
                "q": {"$ref": "#/$defs/q"},
                "flag": {"type": ["string", "null"], "const": "yes"}
            }
        }));
        assert!(sanitized.get("$defs").is_none());
        assert_eq!(sanitized["type"], "OBJECT");
        assert_eq!(sanitized["properties"]["q"]["type"], "STRING");
        assert!(sanitized["properties"]["q"].get("$ref").is_none());
        assert!(
            sanitized["properties"]["q"]["description"]
                .as_str()
                .unwrap_or("")
                .contains("minLength=2")
        );
        assert_eq!(sanitized["properties"]["flag"]["type"], "STRING");
        assert_eq!(sanitized["properties"]["flag"]["nullable"], true);
        assert_eq!(sanitized["properties"]["flag"]["enum"], json!(["yes"]));
        let empty = sanitize_schema(&json!({"type": "object", "properties": {}}));
        assert_eq!(empty["properties"]["_placeholder"]["type"], "BOOLEAN");
    }

    #[test]
    fn envelope_omits_empty_project_and_uses_the_signature_sentinel() {
        let envelope = build_cloud_code_envelope(
            "gemini-3.8-flash",
            "",
            &[
                LlmMessage::system("be brief"),
                LlmMessage::user("hello"),
                LlmMessage::assistant(
                    None,
                    vec![ToolCall {
                        id: "call_1".into(),
                        name: "rag_search".into(),
                        arguments: json!({"query": "q"}),
                    }],
                ),
            ],
            &[ToolSpec {
                name: "rag_search".into(),
                description: "search".into(),
                parameters: json!({"type": "object", "properties": {}}),
                external_effect: false,
            }],
            100,
            None,
        )
        .unwrap();
        assert!(envelope.get("project").is_none());
        assert_eq!(
            envelope["request"]["contents"][1]["parts"][0]["thoughtSignature"],
            THOUGHT_SIGNATURE_SENTINEL
        );
        assert_eq!(
            envelope["request"]["tools"][0]["functionDeclarations"][0]["parameters"]["type"],
            "OBJECT"
        );
        assert_eq!(
            envelope["request"]["systemInstruction"]["parts"][0]["text"],
            "be brief"
        );
        let with_project = build_cloud_code_envelope(
            "gemini-3.8-flash",
            "proj-synthetic",
            &[LlmMessage::user("hello")],
            &[],
            100,
            None,
        )
        .unwrap();
        assert_eq!(with_project["project"], "proj-synthetic");
    }

    #[test]
    fn completion_parser_skips_thoughts_and_concatenates_sse() {
        let secret = "synthetic-thought-secret";
        let body = format!(
            "data: {{\"response\":{{\"candidates\":[{{\"content\":{{\"parts\":[{{\"thought\":true,\"text\":\"{secret}\"}},{{\"text\":\"hello \"}}]}}}}]}}}}\n\ndata: {{\"response\":{{\"candidates\":[{{\"content\":{{\"parts\":[{{\"text\":\"world\"}},{{\"functionCall\":{{\"id\":\"call_1\",\"name\":\"rag_search\",\"args\":{{\"query\":\"q\"}}}}}}]}},\"finishReason\":\"STOP\"}}],\"usageMetadata\":{{\"promptTokenCount\":3,\"candidatesTokenCount\":2}}}}}}\n\n"
        );
        let parsed = parse_cloud_code_completion(body.as_bytes()).unwrap();
        assert_eq!(parsed.text, "hello world");
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "rag_search");
        assert_eq!(parsed.prompt_tokens, 3);
        assert!(!parsed.text.contains(secret));
    }

    #[tokio::test]
    async fn pkce_login_exchanges_on_loopback_and_does_not_echo_secrets() {
        let token_server = MockServer::start().await;
        let api_server = MockServer::start().await;
        let client_secret = "synthetic-client-secret";
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier="))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-synthetic",
                "refresh_token": "refresh-synthetic",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&token_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1internal:loadCodeAssist"))
            .and(header("authorization", "Bearer access-synthetic"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "cloudaicompanionProject": "proj-synthetic"
            })))
            .expect(1)
            .mount(&api_server)
            .await;
        let seen = Arc::new(Mutex::new(String::new()));
        let seen_url = seen.clone();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let login = login_antigravity_at(
            &http,
            client_secret,
            &format!("{}/token", token_server.uri()),
            &[api_server.uri()],
            Duration::from_secs(10),
            Duration::from_millis(1),
            move |url| {
                *seen_url.lock().unwrap() = url.to_owned();
                let url = url.to_owned();
                tokio::spawn(async move {
                    let parsed = url::Url::parse(&url).unwrap();
                    let redirect = parsed
                        .query_pairs()
                        .find(|(key, _)| key == "redirect_uri")
                        .unwrap()
                        .1
                        .into_owned();
                    let state = parsed
                        .query_pairs()
                        .find(|(key, _)| key == "state")
                        .unwrap()
                        .1
                        .into_owned();
                    let callback = format!("{redirect}?code=synthetic-auth-code&state={state}");
                    let _ = reqwest::Client::builder()
                        .redirect(reqwest::redirect::Policy::none())
                        .build()
                        .unwrap()
                        .get(callback)
                        .send()
                        .await;
                });
            },
        )
        .await
        .unwrap();
        let url = seen.lock().unwrap().clone();
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(!url.contains(client_secret));
        assert!(!url.contains("code_verifier"));
        assert_eq!(login.access_token.expose_secret(), "access-synthetic");
        assert_eq!(login.refresh_token.expose_secret(), "refresh-synthetic");
        assert_eq!(login.project_id.as_deref(), Some("proj-synthetic"));
        let rendered = format!("{login:?}");
        assert!(!rendered.contains("access-synthetic"));
        assert!(!rendered.contains("refresh-synthetic"));
        assert!(!rendered.contains(client_secret));
    }

    #[tokio::test]
    async fn complete_login_exchanges_a_client_supplied_code_without_echoing_it() {
        let token_server = MockServer::start().await;
        let api_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("code=synthetic-auth-code"))
            .and(body_string_contains(
                "code_verifier=synthetic-verifier-synthetic-verifier-0123456789",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-synthetic",
                "refresh_token": "refresh-synthetic"
            })))
            .expect(1)
            .mount(&token_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1internal:loadCodeAssist"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "cloudaicompanionProject": "proj-synthetic"
            })))
            .mount(&api_server)
            .await;
        let http = reqwest::Client::new();
        let login = complete_antigravity_login_at(
            &http,
            "synthetic-client-secret",
            &format!("{}/token", token_server.uri()),
            &[api_server.uri()],
            Duration::from_millis(1),
            "synthetic-auth-code",
            "http://127.0.0.1:51121/oauth-callback",
            "synthetic-verifier-synthetic-verifier-0123456789",
        )
        .await
        .unwrap();
        assert_eq!(login.refresh_token.expose_secret(), "refresh-synthetic");
        assert_eq!(login.project_id.as_deref(), Some("proj-synthetic"));
        assert!(!format!("{login:?}").contains("access-synthetic"));
    }

    #[tokio::test]
    async fn complete_login_rejects_bad_redirect_and_verifier_before_any_request() {
        let http = reqwest::Client::new();
        for (redirect, verifier) in [
            (
                "https://evil.example/oauth-callback",
                "synthetic-verifier-synthetic-verifier-0123456789",
            ),
            ("http://127.0.0.1:51121/oauth-callback", "short"),
        ] {
            let error = complete_antigravity_login_at(
                &http,
                "synthetic-client-secret",
                "http://127.0.0.1:9/token",
                &[],
                Duration::from_millis(1),
                "synthetic-auth-code",
                redirect,
                verifier,
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(!error.contains("synthetic-client-secret"));
        }
    }

    #[tokio::test]
    async fn token_exchange_error_omits_the_response_body() {
        let server = MockServer::start().await;
        let secret = "synthetic-client-secret";
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_string(secret))
            .mount(&server)
            .await;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let error = exchange_code(
            &http,
            &format!("{}/token", server.uri()),
            secret,
            "synthetic-auth-code",
            "http://127.0.0.1:9/oauth-callback",
            &pkce_s256().0,
        )
        .await
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("400"));
        assert!(!message.contains(secret));
        assert!(pin_token_url("https://evil.example/token").is_err());
    }
}
