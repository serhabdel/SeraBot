//! OpenRouter "Sign in with OpenRouter" (OAuth PKCE).
//!
//! Documented flow (<https://openrouter.ai/docs/use-cases/oauth-pkce>): the
//! user is sent to `https://openrouter.ai/auth` with a `callback_url` and an
//! S256 `code_challenge`; OpenRouter redirects back with a one-time `code`;
//! the app exchanges it with `POST /api/v1/auth/keys` for a user-controlled
//! API key. There is no client registration, no client secret and no refresh
//! token: the result is a normal OpenRouter API key, billed to the user.
//!
//! The server builds the authorization URL and performs the exchange, so the
//! key goes straight into the acting user's envelope-encrypted store and is
//! never returned to the desktop. Keys, codes and verifiers never appear in
//! errors or logs.
//!
//! The same per-user key also drives image generation through OpenRouter's
//! Image API (`POST {base}/images`, 0.28.0): the `openrouter_generate_image`
//! gateway tool renders prompts with any image-capable model from the catalog
//! and saves each image as an artifact.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use chrono::Utc;
use serde_json::{Value, json};

use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolSpec};
use crate::llm::validate_base_url;
use crate::maker::{ArtifactMeta, ArtifactStore};
use crate::user_secrets::{ProviderCredential, UserSecrets};

/// Where the user approves the app.
pub const AUTH_URL: &str = "https://openrouter.ai/auth";
/// Where the one-time code becomes an API key.
pub const KEYS_URL: &str = "https://openrouter.ai/api/v1/auth/keys";

// --- Image generation (Image API) ---

/// OpenRouter API root; the builtin provider's base URL.
pub const API_BASE: &str = "https://openrouter.ai/api/v1";
/// Model used when `openrouter_generate_image` gets no `model` argument.
pub const DEFAULT_IMAGE_MODEL: &str = "google/gemini-2.5-flash-image";
/// The Image API endpoint, relative to the API base.
pub const IMAGES_PATH: &str = "/images";
/// The models catalog, relative to the API base.
pub const MODELS_PATH: &str = "/models";
/// Prompt bound enforced before anything leaves the host.
pub const MAX_IMAGE_PROMPT_CHARS: usize = 8_000;
/// Largest catalog or image response read into memory.
pub const MAX_IMAGE_RESPONSE_BYTES: usize = 100 * 1024 * 1024;
/// Most variants one `openrouter_generate_image` call may render.
pub const MAX_IMAGE_VARIANTS: u64 = 4;

/// Parse the models catalog (`GET {base}/models`) for image-capable models:
/// entries whose `architecture.output_modalities` contains `"image"`. Fails
/// closed on an unexpected shape; ids and bodies never reach the errors.
pub fn image_capable_models(bytes: &[u8]) -> Result<Vec<String>> {
    if bytes.len() > MAX_IMAGE_RESPONSE_BYTES {
        return Err(SeraError::External("OpenRouter catalog too large".into()));
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| SeraError::External("OpenRouter catalog was not JSON".into()))?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| SeraError::External("OpenRouter catalog has no model list".into()))?;
    let mut models = Vec::new();
    for entry in data {
        let id = entry.get("id").and_then(Value::as_str).unwrap_or("");
        let image_output = entry
            .pointer("/architecture/output_modalities")
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("image")));
        if image_output && crate::providers::valid_model_id(id) {
            models.push(id.to_owned());
        }
    }
    Ok(models)
}

/// One rendered image: decoded bytes plus the provider-reported media type.
#[derive(Debug)]
pub struct GeneratedImage {
    pub bytes: Vec<u8>,
    pub media_type: Option<String>,
}

/// `POST {base}/images` with `{model, prompt, n}`. The API key travels only
/// in the bearer header; failures are reported by status, never by body.
pub async fn generate_images(
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    n: u64,
) -> Result<Vec<GeneratedImage>> {
    if !crate::providers::valid_model_id(model) {
        return Err(SeraError::Validation("invalid model id".into()));
    }
    if prompt.is_empty() || prompt.chars().count() > MAX_IMAGE_PROMPT_CHARS {
        return Err(SeraError::Validation("prompt is empty or too long".into()));
    }
    if !(1..=MAX_IMAGE_VARIANTS).contains(&n) {
        return Err(SeraError::Validation("n must be between 1 and 4".into()));
    }
    let base = validate_base_url(base_url)?;
    let endpoint = format!("{}{}", base.trim_end_matches('/'), IMAGES_PATH);
    let response = http
        .post(&endpoint)
        .bearer_auth(api_key)
        .header("user-agent", crate::providers::USER_AGENT)
        .header("accept", "application/json")
        .json(&json!({
            "model": model,
            "prompt": prompt,
            "n": n,
        }))
        .send()
        .await
        .map_err(|_| SeraError::External("OpenRouter image generation failed".into()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| SeraError::External("OpenRouter image generation failed".into()))?;
    if !status.is_success() {
        return Err(SeraError::External(format!(
            "OpenRouter image generation failed with status {}",
            status.as_u16()
        )));
    }
    if bytes.len() > MAX_IMAGE_RESPONSE_BYTES {
        return Err(SeraError::External(
            "OpenRouter image response exceeded the size limit".into(),
        ));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| SeraError::External("OpenRouter image response was not JSON".into()))?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| SeraError::External("OpenRouter image response has no images".into()))?;
    let mut images = Vec::with_capacity(data.len());
    for item in data {
        let encoded = item
            .get("b64_json")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty() && text.len() <= MAX_IMAGE_RESPONSE_BYTES)
            .ok_or_else(|| SeraError::External("OpenRouter returned no image data".into()))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| SeraError::External("OpenRouter returned invalid image data".into()))?;
        if bytes.is_empty() || bytes.len() > crate::maker::MAX_ARTIFACT_BYTES {
            return Err(SeraError::External(
                "OpenRouter returned an invalid image".into(),
            ));
        }
        let media_type = item
            .get("media_type")
            .and_then(Value::as_str)
            .map(str::to_owned);
        images.push(GeneratedImage { bytes, media_type });
    }
    if images.is_empty() {
        return Err(SeraError::External("OpenRouter returned no images".into()));
    }
    Ok(images)
}

fn extension_for_media_type(media_type: Option<&str>) -> &'static str {
    match media_type {
        Some("image/jpeg") => "jpg",
        Some("image/webp") => "webp",
        Some("image/gif") => "gif",
        Some("image/svg+xml") => "svg",
        _ => "png",
    }
}

/// The `openrouter_generate_image` gateway tool (0.28.0). Renders a prompt
/// with an image-capable OpenRouter model and saves every variant as an
/// artifact; non-destructive, so it runs on the bot's judgement like the
/// other generation tools.
pub struct OpenRouterGenerateImageTool {
    http: reqwest::Client,
    users: Arc<UserSecrets>,
    artifacts: Arc<ArtifactStore>,
    /// The per-user credential row the OpenRouter key lives in.
    provider_id: String,
    base_url: String,
    default_model: String,
}

impl OpenRouterGenerateImageTool {
    pub fn new(
        http: reqwest::Client,
        users: Arc<UserSecrets>,
        artifacts: Arc<ArtifactStore>,
        provider_id: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            http,
            users,
            artifacts,
            provider_id: provider_id.into(),
            base_url: base_url.into(),
            default_model: DEFAULT_IMAGE_MODEL.to_owned(),
        }
    }

    fn credential(&self, user_id: &str) -> Result<String> {
        let stored = self
            .users
            .resolve(user_id, &self.provider_id)?
            .ok_or_else(|| {
                SeraError::Validation(
                    "no OpenRouter credential for this account; sign in with OpenRouter first"
                        .into(),
                )
            })?;
        Ok(ProviderCredential::parse(&stored)?.api_key)
    }
}

#[async_trait]
impl Tool for OpenRouterGenerateImageTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "openrouter_generate_image".into(),
            description:
                "Generate one or more images from a text prompt with an image-capable OpenRouter \
                 model (Image API, POST /api/v1/images). Uses the acting account's OpenRouter \
                 sign-in; spends that account's credits. Each image is saved as an artifact and \
                 its hash is returned."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "prompt": {"type": "string", "description": "Text prompt for the image"},
                    "model": {"type": "string", "description": "Optional image-capable model id from the OpenRouter catalog; defaults to the configured image model"},
                    "n": {"type": "integer", "minimum": 1, "maximum": 4, "description": "Number of variants to render (1-4); default 1"}
                },
                "required": ["prompt"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let prompt = args
            .get("prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("prompt is required".into()))?;
        if prompt.trim().is_empty() || prompt.chars().count() > MAX_IMAGE_PROMPT_CHARS {
            return Err(SeraError::Validation(
                "prompt must be 1-8000 characters".into(),
            ));
        }
        let model = args
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.trim().is_empty())
            .unwrap_or(&self.default_model);
        let n = args
            .get("n")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .clamp(1, MAX_IMAGE_VARIANTS);
        let api_key = self.credential(&context.user_id)?;
        let images =
            generate_images(&self.http, &self.base_url, &api_key, model, prompt, n).await?;
        let mut saved = Vec::with_capacity(images.len());
        for (index, image) in images.iter().enumerate() {
            let extension = extension_for_media_type(image.media_type.as_deref());
            let filename = if saved.is_empty() && images.len() == 1 {
                format!("openrouter-image.{extension}")
            } else {
                format!("openrouter-image-{}.{extension}", index + 1)
            };
            let meta = ArtifactMeta {
                title: format!(
                    "OpenRouter image ({model}): {}",
                    crate::util::truncate_chars(prompt, 120)
                ),
                filename: filename.clone(),
                language: Language::En,
                source_artifact: None,
                created_at: Utc::now(),
            };
            let stored = self.artifacts.save(&image.bytes, &filename, meta)?;
            // Variants beyond the first are listed beside the primary artifact.
            if !saved.is_empty() {
                crate::maker::list_saved_artifact(&stored);
            }
            saved.push(stored);
        }
        let first = saved
            .first()
            .ok_or_else(|| SeraError::External("OpenRouter returned no images".into()))?;
        Ok(ToolOutput {
            content: format!(
                "Generated {} image(s) with OpenRouter model {model}. Artifact hashes: {}",
                saved.len(),
                saved
                    .iter()
                    .map(|stored| stored.hash.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            data: Some(json!({
                "model": model,
                "n": saved.len(),
                "artifacts": saved.iter().map(|stored| json!({
                    "artifact_hash": stored.hash,
                    "filename": stored.filename,
                    "bytes": stored.bytes.len(),
                })).collect::<Vec<_>>(),
            })),
            artifact_path: Some(format!("artifacts/{}", first.filename)),
            artifact_hash: Some(first.hash.clone()),
            filename: Some(first.filename.clone()),
            destination: None,
        })
    }
}

/// Resolve the configured OpenRouter provider: its credential row id and API
/// base. The default `[llm]` provider counts when its kind is OpenRouter.
pub fn openrouter_provider(config: &crate::config::Config) -> Option<(String, String)> {
    let is_openrouter =
        |kind: &str| crate::providers::canonical_kind(kind).ok().as_deref() == Some("openrouter");
    if is_openrouter(&config.llm.kind) {
        let base = if config.llm.base_url.trim().is_empty() {
            API_BASE.to_owned()
        } else {
            config.llm.base_url.clone()
        };
        return Some(("default".to_owned(), base));
    }
    config
        .llm
        .providers
        .iter()
        .find(|(_, profile)| is_openrouter(&profile.kind))
        .map(|(id, profile)| {
            let base = if profile.base_url.trim().is_empty() {
                API_BASE.to_owned()
            } else {
                profile.base_url.clone()
            };
            (id.clone(), base)
        })
}

/// The authorization page for a loopback sign-in.
///
/// `callback_url` must be a loopback address (`localhost` or `127.0.0.1`,
/// any port): OpenRouter supports localhost callbacks, and the desktop only
/// ever listens there. `challenge` is the unpadded base64url S256 digest of
/// the verifier the caller keeps; `state` is echoed back on the callback.
pub fn authorize_url(callback_url: &str, challenge: &str, state: &str) -> Result<String> {
    let callback = url::Url::parse(callback_url)
        .map_err(|_| SeraError::Validation("invalid sign-in request".into()))?;
    let loopback = callback.scheme() == "http"
        && matches!(callback.host_str(), Some("localhost" | "127.0.0.1"))
        && callback.port().is_some()
        && callback.username().is_empty()
        && callback.password().is_none()
        && callback.fragment().is_none();
    let challenge_ok = (43..=128).contains(&challenge.len())
        && challenge
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    let state_ok = !state.is_empty()
        && state.len() <= 128
        && state
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if !loopback || !challenge_ok || !state_ok {
        return Err(SeraError::Validation("invalid sign-in request".into()));
    }
    let mut url = url::Url::parse(AUTH_URL).expect("constant URL parses");
    url.query_pairs_mut()
        .append_pair("callback_url", callback.as_str())
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    // `state` is returned unchanged on the callback; the desktop checks it.
    url.query_pairs_mut().append_pair("state", state);
    Ok(url.to_string())
}

/// Exchange the one-time code for the user's API key.
///
/// `keys_url` is [`KEYS_URL`] in production; tests point it at a synthetic
/// server. Any failure is reported by status only: the response body and the
/// request secrets are never copied into the error.
pub async fn exchange_code(
    http: &reqwest::Client,
    keys_url: &str,
    code: &str,
    verifier: &str,
) -> Result<String> {
    let secretish = |value: &str| {
        !value.is_empty()
            && value.len() <= 512
            && value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
    };
    if !secretish(code) || !secretish(verifier) {
        return Err(SeraError::Validation("invalid sign-in request".into()));
    }
    let endpoint = validate_base_url(keys_url)?;
    let response = http
        .post(&endpoint)
        .header("user-agent", crate::providers::USER_AGENT)
        .header("accept", "application/json")
        .json(&json!({
            "code": code,
            "code_verifier": verifier,
            "code_challenge_method": "S256",
        }))
        .send()
        .await
        .map_err(|_| SeraError::External("OpenRouter sign-in failed".into()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| SeraError::External("OpenRouter sign-in failed".into()))?;
    if !status.is_success() {
        // 403 is an invalid or expired code per the docs.
        return Err(SeraError::External(format!(
            "OpenRouter sign-in failed with status {}",
            status.as_u16()
        )));
    }
    if bytes.len() > 65_536 {
        return Err(SeraError::External("OpenRouter sign-in failed".into()));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| SeraError::External("OpenRouter sign-in failed".into()))?;
    let key = value
        .get("key")
        .and_then(Value::as_str)
        .filter(|key| {
            !key.is_empty()
                && key.len() <= 8_192
                && key.trim() == *key
                && !key.chars().any(char::is_control)
        })
        .ok_or_else(|| SeraError::External("OpenRouter sign-in failed".into()))?;
    Ok(key.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    fn http() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    #[test]
    fn authorize_url_carries_the_documented_parameters() {
        let url = authorize_url("http://localhost:51423/callback", CHALLENGE, "state_1").unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        assert_eq!(parsed.host_str(), Some("openrouter.ai"));
        assert_eq!(parsed.path(), "/auth");
        let pairs: std::collections::BTreeMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(pairs["callback_url"], "http://localhost:51423/callback");
        assert_eq!(pairs["code_challenge"], CHALLENGE);
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert_eq!(pairs["state"], "state_1");
        assert!(authorize_url("http://127.0.0.1:9/cb", CHALLENGE, "s").is_ok());
    }

    #[test]
    fn authorize_url_refuses_non_loopback_callbacks_and_bad_inputs() {
        for callback in [
            "https://evil.example/cb",
            "http://evil.example:80/cb",
            "http://localhost/cb",
            "http://user:pw@localhost:1/cb",
            "not a url",
        ] {
            assert!(
                authorize_url(callback, CHALLENGE, "s").is_err(),
                "{callback}"
            );
        }
        assert!(authorize_url("http://localhost:1/cb", "short", "s").is_err());
        assert!(authorize_url("http://localhost:1/cb", CHALLENGE, "").is_err());
        assert!(authorize_url("http://localhost:1/cb", CHALLENGE, "a b").is_err());
    }

    #[tokio::test]
    async fn exchange_posts_the_documented_body_and_returns_the_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/keys"))
            .and(header("content-type", "application/json"))
            .and(body_json(json!({
                "code": "synthetic-code",
                "code_verifier": "synthetic-verifier",
                "code_challenge_method": "S256"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "key": "sk-or-synthetic-key",
                "user_id": "user_synthetic"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let key = exchange_code(
            &http(),
            &format!("{}/api/v1/auth/keys", server.uri()),
            "synthetic-code",
            "synthetic-verifier",
        )
        .await
        .unwrap();
        assert_eq!(key, "sk-or-synthetic-key");
    }

    #[tokio::test]
    async fn exchange_failures_report_status_only_and_never_echo_secrets() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string("bad code synthetic-code verifier synthetic-verifier"),
            )
            .mount(&server)
            .await;
        let url = format!("{}/api/v1/auth/keys", server.uri());
        let error = exchange_code(&http(), &url, "synthetic-code", "synthetic-verifier")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("403"), "{error}");
        assert!(!error.contains("synthetic-code"), "{error}");
        assert!(!error.contains("synthetic-verifier"), "{error}");

        let odd = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"key": ""})))
            .mount(&odd)
            .await;
        assert!(
            exchange_code(
                &http(),
                &format!("{}/api/v1/auth/keys", odd.uri()),
                "synthetic-code",
                "synthetic-verifier"
            )
            .await
            .is_err()
        );
        // Control characters in the code never reach the network.
        assert!(
            exchange_code(&http(), &url, "bad\ncode", "synthetic-verifier")
                .await
                .is_err()
        );
    }

    #[test]
    fn image_capable_models_reads_output_modalities_and_fails_closed() {
        let catalog = json!({
            "data": [
                {"id": "vendor/painter", "architecture": {"output_modalities": ["image"]}},
                {"id": "vendor/chat", "architecture": {"output_modalities": ["text", "image"]}},
                {"id": "vendor/text-only", "architecture": {"output_modalities": ["text"]}},
                {"id": "vendor/no-arch", "supported_parameters": ["tools"]},
                {"id": "bad\nid", "architecture": {"output_modalities": ["image"]}}
            ]
        });
        let bytes = serde_json::to_vec(&catalog).unwrap();
        assert_eq!(
            image_capable_models(&bytes).unwrap(),
            vec!["vendor/painter".to_owned(), "vendor/chat".to_owned()]
        );
        assert!(image_capable_models(b"not json").is_err());
        assert!(image_capable_models(br#"{"nope": []}"#).is_err());
    }

    #[tokio::test]
    async fn generate_images_posts_to_the_image_endpoint_and_decodes_variants() {
        let png = base64::engine::general_purpose::STANDARD.encode(b"synthetic-png-bytes");
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/images"))
            .and(header("authorization", "Bearer sk-or-synthetic"))
            .and(body_json(json!({
                "model": "vendor/painter",
                "prompt": "a synthetic test image",
                "n": 2
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "created": 1,
                "data": [
                    {"b64_json": png, "media_type": "image/png"},
                    {"b64_json": png}
                ],
                "usage": {"total_tokens": 10, "cost": 0.01}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let base = format!("{}/api/v1", server.uri());
        let images = generate_images(
            &http(),
            &base,
            "sk-or-synthetic",
            "vendor/painter",
            "a synthetic test image",
            2,
        )
        .await
        .unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].bytes, b"synthetic-png-bytes");
        assert_eq!(images[0].media_type.as_deref(), Some("image/png"));
        assert_eq!(images[1].media_type, None);
    }

    #[tokio::test]
    async fn generate_images_validates_inputs_and_reports_status_only() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(402)
                    .set_body_string("prompt synthetic-prompt sk-or-synthetic"),
            )
            .mount(&server)
            .await;
        let base = format!("{}/api/v1", server.uri());
        let error = generate_images(
            &http(),
            &base,
            "sk-or-synthetic",
            "vendor/painter",
            "synthetic-prompt",
            1,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("402"), "{error}");
        assert!(!error.contains("synthetic-prompt"), "{error}");
        assert!(!error.contains("sk-or-synthetic"), "{error}");
        // Argument bounds are enforced before any request leaves the host.
        assert!(
            generate_images(&http(), &base, "k", "bad model", "p", 1)
                .await
                .is_err()
        );
        assert!(
            generate_images(&http(), &base, "k", "vendor/painter", "", 1)
                .await
                .is_err()
        );
        assert!(
            generate_images(&http(), &base, "k", "vendor/painter", "p", 0)
                .await
                .is_err()
        );
        assert!(
            generate_images(&http(), &base, "k", "vendor/painter", "p", 5)
                .await
                .is_err()
        );

        let empty = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .mount(&empty)
            .await;
        assert!(
            generate_images(
                &http(),
                &format!("{}/api/v1", empty.uri()),
                "k",
                "vendor/painter",
                "p",
                1
            )
            .await
            .is_err()
        );
    }

    fn image_tool_fixture() -> (OpenRouterGenerateImageTool, String) {
        let store = crate::store::Store::open_in_memory(384).unwrap();
        let user = store.admin_user_id().unwrap();
        let users = Arc::new(
            crate::user_secrets::UserSecrets::with_master_key(
                store,
                [7u8; crate::user_secrets::KEY_BYTES],
            )
            .unwrap(),
        );
        let credential = ProviderCredential {
            api_key: "sk-or-synthetic".to_owned(),
            ..Default::default()
        };
        users
            .set(&user, "openrouter-test", &credential.encode().unwrap())
            .unwrap();
        let root =
            std::env::temp_dir().join(format!("serabot-openrouter-{}", uuid::Uuid::new_v4()));
        let artifacts = Arc::new(ArtifactStore::new(&root).unwrap());
        let tool = OpenRouterGenerateImageTool::new(
            http(),
            users,
            artifacts,
            "openrouter-test",
            "https://openrouter.invalid/api/v1",
        );
        (tool, user)
    }

    fn tool_context(user: &str) -> ToolContext {
        ToolContext {
            user_id: user.to_owned(),
            actor: user.to_owned(),
            bot: "main".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: Default::default(),
            require_approval: Default::default(),
        }
    }

    #[tokio::test]
    async fn generate_image_tool_saves_each_variant_as_an_artifact() {
        let (tool, user) = image_tool_fixture();
        let png = base64::engine::general_purpose::STANDARD.encode(b"synthetic-png-bytes");
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/images"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"b64_json": png, "media_type": "image/png"},
                    {"b64_json": png, "media_type": "image/png"}
                ]
            })))
            .mount(&server)
            .await;
        // Point the tool at the synthetic server.
        let tool = OpenRouterGenerateImageTool::new(
            http(),
            tool.users.clone(),
            tool.artifacts.clone(),
            "openrouter-test",
            format!("{}/api/v1", server.uri()),
        );
        let output = tool
            .execute(
                json!({"prompt": "a synthetic test image", "model": "vendor/painter", "n": 2}),
                &tool_context(&user),
            )
            .await
            .unwrap();
        let data = output.data.unwrap();
        let artifacts = data["artifacts"].as_array().unwrap();
        assert_eq!(artifacts.len(), 2);
        assert_eq!(
            output.artifact_hash.as_deref(),
            Some(artifacts[0]["artifact_hash"].as_str().unwrap())
        );
        for artifact in artifacts {
            let hash = artifact["artifact_hash"].as_str().unwrap();
            assert_eq!(hash.len(), 64);
            assert!(tool.artifacts.load(hash).is_ok());
        }
        assert!(
            !tool.spec().destructive(),
            "image generation is non-destructive"
        );
    }

    #[tokio::test]
    async fn generate_image_tool_fails_closed_without_a_credential() {
        let (tool, _) = image_tool_fixture();
        let other = crate::store::Store::open_in_memory(384).unwrap();
        let member = other
            .create_user("microsoft:no-key", "n@example.example", "N")
            .unwrap();
        let error = tool
            .execute(json!({"prompt": "x"}), &tool_context(&member.id))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("sign in with OpenRouter"), "{error}");
    }
}
