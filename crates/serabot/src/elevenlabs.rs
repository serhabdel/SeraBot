//! Native ElevenLabs client and gateway tools (TTS, STT, sound effects, image/video flows).
//!
//! Outbound calls use the configured HTTPS API base and the age-stored API key.
//! Media generation tools require owner approval via the gateway.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::multipart::{Form, Part};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audio::resolve_inbox_audio;
use crate::config::{Config, ElevenLabsConfig};
use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::gateway::{
    ApprovalDetails, Tool, ToolContext, ToolOutput, ToolRegistry, ToolSpec,
};
use crate::llm::validate_base_url;
use crate::maker::{ArtifactMeta, ArtifactStore};
use crate::policy::Policy;
use crate::util::truncate_chars;

const MAX_FLOW_DOWNLOAD_BYTES: usize = 100 * 1024 * 1024;
const MAX_SOUND_PROMPT_CHARS: usize = 500;
const MAX_IMAGE_PROMPT_CHARS: usize = 4_000;
const MAX_VIDEO_PROMPT_CHARS: usize = 4_000;

#[derive(Clone)]
pub struct ElevenLabsClient {
    http: reqwest::Client,
    base_url: String,
    api_key: SecretString,
}

impl std::fmt::Debug for ElevenLabsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElevenLabsClient")
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl ElevenLabsClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, timeout_secs: u64) -> Result<Self> {
        let base_url = validate_base_url(&base_url.into())?;
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(SeraError::Config("ElevenLabs credential is missing".into()));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs.max(1)))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| SeraError::External(error.to_string()))?;
        Ok(Self {
            http,
            base_url,
            api_key: SecretString::new(api_key),
        })
    }

    fn auth_headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "xi-api-key",
            HeaderValue::from_str(self.api_key.expose_secret())
                .map_err(|_| SeraError::Config("invalid ElevenLabs API key".into()))?,
        );
        Ok(headers)
    }

    pub async fn list_voices(&self, page_size: u32, search: Option<&str>) -> Result<Value> {
        let page_size = page_size.clamp(1, 100);
        let mut request = self
            .http
            .get(format!("{}/v2/voices", self.base_url))
            .query(&[("page_size", page_size.to_string())]);
        if let Some(search) = search.filter(|value| !value.is_empty()) {
            request = request.query(&[("search", search)]);
        }
        let response = request
            .headers(self.auth_headers()?)
            .send()
            .await
            .map_err(|error| SeraError::External(format!("ElevenLabs voices request failed: {error}")))?;
        read_json_response(response, "voices").await
    }

    pub async fn text_to_speech(
        &self,
        voice_id: &str,
        text: &str,
        model_id: &str,
        output_format: &str,
    ) -> Result<Vec<u8>> {
        validate_voice_id(voice_id)?;
        validate_model_id(model_id)?;
        validate_output_format(output_format)?;
        let body = json!({
            "text": text,
            "model_id": model_id,
        });
        let response = self
            .http
            .post(format!("{}/v1/text-to-speech/{voice_id}", self.base_url))
            .query(&[("output_format", output_format)])
            .headers(self.auth_headers()?)
            .json(&body)
            .send()
            .await
            .map_err(|error| SeraError::External(format!("ElevenLabs TTS request failed: {error}")))?;
        read_bytes_response(response, "text-to-speech").await
    }

    pub async fn sound_effect(
        &self,
        text: &str,
        output_format: &str,
        duration_seconds: Option<f64>,
    ) -> Result<Vec<u8>> {
        validate_output_format(output_format)?;
        let mut body = json!({ "text": text });
        if let Some(duration) = duration_seconds {
            body["duration_seconds"] = json!(duration);
        }
        let response = self
            .http
            .post(format!("{}/v1/sound-generation", self.base_url))
            .query(&[("output_format", output_format)])
            .headers(self.auth_headers()?)
            .json(&body)
            .send()
            .await
            .map_err(|error| SeraError::External(format!("ElevenLabs sound effect request failed: {error}")))?;
        read_bytes_response(response, "sound-generation").await
    }

    pub async fn transcribe(
        &self,
        bytes: &[u8],
        filename: &str,
        model_id: &str,
        language_code: Option<&str>,
    ) -> Result<Value> {
        validate_model_id(model_id)?;
        if bytes.is_empty() || bytes.len() > 100 * 1024 * 1024 {
            return Err(SeraError::Validation("invalid transcription input size".into()));
        }
        let part = Part::bytes(bytes.to_vec())
            .file_name(filename.to_owned())
            .mime_str(mime_for_filename(filename))
            .map_err(|error| SeraError::External(error.to_string()))?;
        let mut form = Form::new().part("file", part).text("model_id", model_id.to_owned());
        if let Some(code) = language_code.filter(|value| !value.is_empty()) {
            form = form.text("language_code", code.to_owned());
        }
        let response = self
            .http
            .post(format!("{}/v1/speech-to-text", self.base_url))
            .headers(self.auth_headers()?)
            .multipart(form)
            .send()
            .await
            .map_err(|error| SeraError::External(format!("ElevenLabs STT request failed: {error}")))?;
        read_json_response(response, "speech-to-text").await
    }

    pub async fn create_image(&self, model_id: &str, prompt: &str, aspect_ratio: &str) -> Result<FlowCreateResponse> {
        validate_model_id(model_id)?;
        let body = json!({
            "model_id": model_id,
            "prompt": prompt,
            "aspect_ratio": aspect_ratio,
        });
        self.post_flow("/v1/flows/image", &body).await
    }

    pub async fn create_video(
        &self,
        model_id: &str,
        prompt: &str,
        aspect_ratio: &str,
        duration_secs: u32,
    ) -> Result<FlowCreateResponse> {
        validate_model_id(model_id)?;
        let body = json!({
            "model_id": model_id,
            "prompt": prompt,
            "aspect_ratio": aspect_ratio,
            "duration_secs": duration_secs.clamp(1, 30),
        });
        self.post_flow("/v1/flows/video", &body).await
    }

    pub async fn get_image_generation(&self, generation_id: &str) -> Result<Value> {
        self.get_flow(&format!("/v1/flows/image/{generation_id}")).await
    }

    pub async fn get_video_generation(&self, generation_id: &str) -> Result<Value> {
        self.get_flow(&format!("/v1/flows/video/{generation_id}")).await
    }

    async fn post_flow(&self, path: &str, body: &Value) -> Result<FlowCreateResponse> {
        let response = self
            .http
            .post(format!("{}{path}", self.base_url))
            .headers(self.auth_headers()?)
            .json(body)
            .send()
            .await
            .map_err(|error| SeraError::External(format!("ElevenLabs flow create failed: {error}")))?;
        let value = read_json_response(response, "flow create").await?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::External("flow create response missing id".into()))?
            .to_owned();
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending")
            .to_owned();
        Ok(FlowCreateResponse { id, status })
    }

    async fn get_flow(&self, path: &str) -> Result<Value> {
        let response = self
            .http
            .get(format!("{}{path}", self.base_url))
            .headers(self.auth_headers()?)
            .send()
            .await
            .map_err(|error| SeraError::External(format!("ElevenLabs flow status failed: {error}")))?;
        read_json_response(response, "flow status").await
    }

    pub async fn poll_flow<F>(
        &self,
        mut fetch: F,
        poll_interval: Duration,
        max_attempts: u32,
    ) -> Result<CompletedFlow>
    where
        F: FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send>>,
    {
        for attempt in 0..max_attempts.max(1) {
            let value = fetch().await?;
            match value.get("status").and_then(Value::as_str) {
                Some("completed") => {
                    let content_url = value
                        .get("content_url")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            SeraError::External("completed flow missing content_url".into())
                        })?;
                    let mime = value
                        .get("content_mime_type")
                        .and_then(Value::as_str)
                        .unwrap_or("application/octet-stream")
                        .to_owned();
                    let bytes = self.download_signed_url(content_url).await?;
                    return Ok(CompletedFlow {
                        id: value
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        content_mime_type: mime,
                        bytes,
                    });
                }
                Some("failed") => {
                    let message = value
                        .get("error_message")
                        .and_then(Value::as_str)
                        .unwrap_or("generation failed");
                    return Err(SeraError::External(format!(
                        "ElevenLabs generation failed: {message}"
                    )));
                }
                _ if attempt + 1 >= max_attempts.max(1) => {
                    return Err(SeraError::External(
                        "ElevenLabs generation timed out while polling".into(),
                    ));
                }
                _ => tokio::time::sleep(poll_interval).await,
            }
        }
        Err(SeraError::External(
            "ElevenLabs generation polling exhausted".into(),
        ))
    }

    async fn download_signed_url(&self, url: &str) -> Result<Vec<u8>> {
        let parsed = url::Url::parse(url)
            .map_err(|_| SeraError::External("invalid media download URL".into()))?;
        let loopback = matches!(parsed.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
            || matches!(parsed.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback());
        if parsed.scheme() != "https" && !(parsed.scheme() == "http" && loopback) {
            return Err(SeraError::Forbidden(
                "ElevenLabs media URL must use HTTPS".into(),
            ));
        }
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|error| SeraError::External(format!("media download failed: {error}")))?;
        if !response.status().is_success() {
            return Err(SeraError::External(format!(
                "media download failed with status {}",
                response.status()
            )));
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| SeraError::External(format!("media download read failed: {error}")))?;
            if body.len() + chunk.len() > MAX_FLOW_DOWNLOAD_BYTES {
                return Err(SeraError::Validation("generated media exceeds size limit".into()));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowCreateResponse {
    pub id: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct CompletedFlow {
    pub id: String,
    pub content_mime_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone)]
struct ElevenLabsToolDeps {
    client: Arc<ElevenLabsClient>,
    artifacts: Arc<ArtifactStore>,
    policy: Arc<Policy>,
    app_config: Arc<Config>,
    config: ElevenLabsConfig,
}

fn check_outbound_text(policy: &Policy, text: &str) -> Result<()> {
    if !policy.is_allowed(text) {
        return Err(SeraError::PolicyViolation);
    }
    Ok(())
}

fn validate_voice_id(voice_id: &str) -> Result<()> {
    if voice_id.is_empty()
        || voice_id.len() > 64
        || voice_id.chars().any(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    {
        return Err(SeraError::Validation("invalid voice_id".into()));
    }
    Ok(())
}

fn validate_model_id(model: &str) -> Result<()> {
    if model.is_empty()
        || model.len() > 128
        || model.trim() != model
        || model.chars().any(char::is_control)
    {
        return Err(SeraError::Validation("invalid model id".into()));
    }
    Ok(())
}

fn validate_output_format(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 64 || value.chars().any(char::is_whitespace) {
        return Err(SeraError::Validation("invalid output_format".into()));
    }
    Ok(())
}

fn mime_for_filename(filename: &str) -> &'static str {
    let extension = Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "wav" => "audio/wav",
        "mp3" | "mpeg" => "audio/mpeg",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "webm" => "video/webm",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

fn extension_for_mime(mime: &str) -> &'static str {
    match mime {
        "audio/mpeg" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/ogg" => "ogg",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        _ => "bin",
    }
}

async fn read_json_response(response: reqwest::Response, context: &str) -> Result<Value> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| SeraError::External(format!("{context} read failed: {error}")))?;
    if !status.is_success() {
        return Err(SeraError::External(format!(
            "{context} failed with status {status}: {}",
            sanitize_provider_error(&bytes)
        )));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| SeraError::External(format!("{context} returned invalid JSON: {error}")))
}

async fn read_bytes_response(response: reqwest::Response, context: &str) -> Result<Vec<u8>> {
    let status = response.status();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| SeraError::External(format!("{context} read failed: {error}")))?;
    if !status.is_success() {
        return Err(SeraError::External(format!(
            "{context} failed with status {status}: {}",
            sanitize_provider_error(&bytes)
        )));
    }
    if content_type.contains("json") {
        return Err(SeraError::External(format!(
            "{context} returned JSON instead of audio: {}",
            sanitize_provider_error(&bytes)
        )));
    }
    Ok(bytes.to_vec())
}

fn sanitize_provider_error(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let parsed: Option<Value> = serde_json::from_str(&text).ok();
    parsed
        .as_ref()
        .and_then(|value| value.pointer("/detail/0/msg"))
        .and_then(Value::as_str)
        .or_else(|| parsed.as_ref().and_then(|value| value.get("message")).and_then(Value::as_str))
        .unwrap_or(&text)
        .chars()
        .take(500)
        .collect()
}

fn store_media_artifact(
    artifacts: &ArtifactStore,
    bytes: &[u8],
    filename: &str,
    title: &str,
) -> Result<(String, String)> {
    let meta = ArtifactMeta {
        title: title.to_owned(),
        filename: filename.to_owned(),
        language: Language::En,
        source_artifact: None,
        created_at: Utc::now(),
    };
    let stored = artifacts.save(bytes, filename, meta)?;
    Ok((stored.hash, stored.filename))
}

pub fn register_elevenlabs_tools(
    registry: &mut ToolRegistry,
    client: Arc<ElevenLabsClient>,
    artifacts: Arc<ArtifactStore>,
    policy: Arc<Policy>,
    app_config: Arc<Config>,
    config: ElevenLabsConfig,
) -> Result<()> {
    let deps = ElevenLabsToolDeps {
        client,
        artifacts,
        policy,
        app_config,
        config,
    };
    registry.register(ElevenLabsListVoicesTool { deps: deps.clone() })?;
    registry.register(ElevenLabsTextToSpeechTool { deps: deps.clone() })?;
    registry.register(ElevenLabsSoundEffectTool { deps: deps.clone() })?;
    registry.register(ElevenLabsTranscribeTool { deps: deps.clone() })?;
    registry.register(ElevenLabsGenerateImageTool { deps: deps.clone() })?;
    registry.register(ElevenLabsGenerateVideoTool { deps })?;
    Ok(())
}

#[derive(Clone)]
struct ElevenLabsListVoicesTool {
    deps: ElevenLabsToolDeps,
}

#[async_trait]
impl Tool for ElevenLabsListVoicesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "elevenlabs_list_voices".into(),
            description:
                "List ElevenLabs voices available to the workspace (search and pagination). \
                 Returns untrusted metadata only; does not spend generation credits."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "page_size": {"type": "integer", "minimum": 1, "maximum": 100},
                    "search": {"type": "string"}
                },
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let page_size = args.get("page_size").and_then(Value::as_u64).unwrap_or(20) as u32;
        let search = args.get("search").and_then(Value::as_str);
        let data = self.deps.client.list_voices(page_size, search).await?;
        let content = truncate_chars(&serde_json::to_string_pretty(&data)?, 20_000);
        Ok(ToolOutput {
            content,
            data: Some(data),
            ..ToolOutput::default()
        })
    }
}

#[derive(Clone)]
struct ElevenLabsTextToSpeechTool {
    deps: ElevenLabsToolDeps,
}

#[async_trait]
impl Tool for ElevenLabsTextToSpeechTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "elevenlabs_text_to_speech".into(),
            description:
                "Generate speech audio from text with ElevenLabs and store it as a local artifact."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "voice_id": {"type": "string"},
                    "model_id": {"type": "string"},
                    "output_format": {"type": "string", "default": "mp3_44100_128"},
                    "filename": {"type": "string"}
                },
                "required": ["text"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let preview = args
            .get("text")
            .and_then(Value::as_str)
            .map(|text| truncate_chars(text, 120))
            .unwrap_or_default();
        Ok(ApprovalDetails {
            summary: format!("ElevenLabs text-to-speech: {preview}"),
            destination: Some("elevenlabs:text-to-speech".into()),
            artifact_hash: None,
        })
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("text is required".into()))?;
        if text.len() > self.deps.config.max_tts_chars {
            return Err(SeraError::Validation(format!(
                "text exceeds max_tts_chars ({})",
                self.deps.config.max_tts_chars
            )));
        }
        check_outbound_text(&self.deps.policy, text)?;
        let voice_id = args
            .get("voice_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(self.deps.config.default_voice_id.as_str());
        if voice_id.is_empty() {
            return Err(SeraError::Config(
                "voice_id is required when elevenlabs.default_voice_id is empty".into(),
            ));
        }
        let model_id = args
            .get("model_id")
            .and_then(Value::as_str)
            .unwrap_or(self.deps.config.tts_model.as_str());
        let output_format = args
            .get("output_format")
            .and_then(Value::as_str)
            .unwrap_or("mp3_44100_128");
        let bytes = self
            .deps
            .client
            .text_to_speech(voice_id, text, model_id, output_format)
            .await?;
        let filename = args
            .get("filename")
            .and_then(Value::as_str)
            .unwrap_or("elevenlabs-speech.mp3");
        let (hash, filename) =
            store_media_artifact(&self.deps.artifacts, &bytes, filename, "ElevenLabs TTS")?;
        Ok(ToolOutput {
            content: format!(
                "Generated speech ({output_format}) for voice {voice_id}. artifact_hash={hash} filename={filename} bytes={}",
                bytes.len()
            ),
            data: Some(json!({
                "artifact_hash": hash,
                "filename": filename,
                "voice_id": voice_id,
                "model_id": model_id,
                "output_format": output_format,
                "bytes": bytes.len(),
            })),
            artifact_path: Some(format!("artifacts/{filename}")),
            artifact_hash: Some(hash),
            filename: Some(filename),
            destination: Some("elevenlabs:text-to-speech".into()),
        })
    }
}

#[derive(Clone)]
struct ElevenLabsSoundEffectTool {
    deps: ElevenLabsToolDeps,
}

#[async_trait]
impl Tool for ElevenLabsSoundEffectTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "elevenlabs_sound_effect".into(),
            description: "Generate a sound effect from a text prompt with ElevenLabs.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "duration_seconds": {"type": "number", "minimum": 0.5, "maximum": 30},
                    "output_format": {"type": "string", "default": "mp3_44100_128"},
                    "filename": {"type": "string"}
                },
                "required": ["text"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let preview = args
            .get("text")
            .and_then(Value::as_str)
            .map(|text| truncate_chars(text, 120))
            .unwrap_or_default();
        Ok(ApprovalDetails {
            summary: format!("ElevenLabs sound effect: {preview}"),
            destination: Some("elevenlabs:sound-generation".into()),
            artifact_hash: None,
        })
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("text is required".into()))?;
        if text.len() > MAX_SOUND_PROMPT_CHARS {
            return Err(SeraError::Validation("sound effect prompt is too long".into()));
        }
        check_outbound_text(&self.deps.policy, text)?;
        let duration = args.get("duration_seconds").and_then(Value::as_f64);
        let output_format = args
            .get("output_format")
            .and_then(Value::as_str)
            .unwrap_or("mp3_44100_128");
        let bytes = self
            .deps
            .client
            .sound_effect(text, output_format, duration)
            .await?;
        let filename = args
            .get("filename")
            .and_then(Value::as_str)
            .unwrap_or("elevenlabs-sfx.mp3");
        let (hash, filename) =
            store_media_artifact(&self.deps.artifacts, &bytes, filename, "ElevenLabs SFX")?;
        Ok(ToolOutput {
            content: format!(
                "Generated sound effect. artifact_hash={hash} filename={filename} bytes={}",
                bytes.len()
            ),
            data: Some(json!({"artifact_hash": hash, "filename": filename, "bytes": bytes.len()})),
            artifact_path: Some(format!("artifacts/{filename}")),
            artifact_hash: Some(hash),
            filename: Some(filename),
            destination: Some("elevenlabs:sound-generation".into()),
        })
    }
}

#[derive(Clone)]
struct ElevenLabsTranscribeTool {
    deps: ElevenLabsToolDeps,
}

#[async_trait]
impl Tool for ElevenLabsTranscribeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "elevenlabs_transcribe".into(),
            description:
                "Transcribe audio from a local artifact hash or a path relative to the document inbox."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "artifact_hash": {"type": "string"},
                    "path": {"type": "string", "description": "Relative to documents.watch_dir"},
                    "model_id": {"type": "string"},
                    "language_code": {"type": "string"}
                },
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let target = args
            .get("artifact_hash")
            .and_then(Value::as_str)
            .map(|hash| format!("artifact:{hash}"))
            .or_else(|| {
                args.get("path")
                    .and_then(Value::as_str)
                    .map(|path| format!("inbox:{path}"))
            })
            .unwrap_or_else(|| "unknown source".into());
        Ok(ApprovalDetails {
            summary: format!("ElevenLabs speech-to-text for {target}"),
            destination: Some("elevenlabs:speech-to-text".into()),
            artifact_hash: args
                .get("artifact_hash")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let (bytes, filename) = if let Some(hash) = args.get("artifact_hash").and_then(Value::as_str) {
            let artifact = self.deps.artifacts.load(hash)?;
            (artifact.bytes, artifact.filename)
        } else if let Some(path) = args.get("path").and_then(Value::as_str) {
            let resolved = resolve_inbox_audio(self.deps.app_config.as_ref(), path)?;
            let bytes = std::fs::read(&resolved)?;
            let filename = resolved
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("audio.wav")
                .to_owned();
            (bytes, filename)
        } else {
            return Err(SeraError::Validation(
                "artifact_hash or path is required".into(),
            ));
        };
        let model_id = args
            .get("model_id")
            .and_then(Value::as_str)
            .unwrap_or(self.deps.config.stt_model.as_str());
        let language_code = args.get("language_code").and_then(Value::as_str);
        let data = self
            .deps
            .client
            .transcribe(&bytes, &filename, model_id, language_code)
            .await?;
        let text = data
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Ok(ToolOutput {
            content: text.clone(),
            data: Some(data),
            destination: Some("elevenlabs:speech-to-text".into()),
            ..ToolOutput::default()
        })
    }
}

#[derive(Clone)]
struct ElevenLabsGenerateImageTool {
    deps: ElevenLabsToolDeps,
}

#[async_trait]
impl Tool for ElevenLabsGenerateImageTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "elevenlabs_generate_image".into(),
            description:
                "Generate an image with ElevenLabs Image & Video flows, poll until complete, and store the result."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "prompt": {"type": "string"},
                    "model_id": {"type": "string"},
                    "aspect_ratio": {"type": "string", "default": "16:9"},
                    "filename": {"type": "string"}
                },
                "required": ["prompt"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let preview = args
            .get("prompt")
            .and_then(Value::as_str)
            .map(|text| truncate_chars(text, 120))
            .unwrap_or_default();
        Ok(ApprovalDetails {
            summary: format!("ElevenLabs image generation: {preview}"),
            destination: Some("elevenlabs:flows:image".into()),
            artifact_hash: None,
        })
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let prompt = args
            .get("prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("prompt is required".into()))?;
        if prompt.len() > MAX_IMAGE_PROMPT_CHARS {
            return Err(SeraError::Validation("image prompt is too long".into()));
        }
        check_outbound_text(&self.deps.policy, prompt)?;
        let model_id = args
            .get("model_id")
            .and_then(Value::as_str)
            .unwrap_or(self.deps.config.image_model.as_str());
        let aspect_ratio = args
            .get("aspect_ratio")
            .and_then(Value::as_str)
            .unwrap_or("16:9");
        let created = self
            .deps
            .client
            .create_image(model_id, prompt, aspect_ratio)
            .await?;
        let generation_id = created.id.clone();
        let generation_id_log = generation_id.clone();
        let client = self.deps.client.clone();
        let poll_interval = Duration::from_millis(self.deps.config.poll_interval_ms.max(500));
        let max_attempts = self.deps.config.poll_max_attempts;
        let completed = self
            .deps
            .client
            .poll_flow(
                move || {
                    let client = client.clone();
                    let id = generation_id.clone();
                    Box::pin(async move { client.get_image_generation(&id).await })
                },
                poll_interval,
                max_attempts,
            )
            .await?;
        let ext = extension_for_mime(&completed.content_mime_type);
        let filename = args
            .get("filename")
            .and_then(Value::as_str)
            .map(|value| value.to_owned())
            .unwrap_or_else(|| format!("elevenlabs-image.{ext}"));
        let (hash, filename) = store_media_artifact(
            &self.deps.artifacts,
            &completed.bytes,
            &filename,
            "ElevenLabs image",
        )?;
        Ok(ToolOutput {
            content: format!(
                "Image generation {generation_id_log} completed. artifact_hash={hash} filename={filename}"
            ),
            data: Some(json!({
                "generation_id": generation_id_log,
                "artifact_hash": hash,
                "filename": filename,
                "content_mime_type": completed.content_mime_type,
            })),
            artifact_path: Some(format!("artifacts/{filename}")),
            artifact_hash: Some(hash),
            filename: Some(filename),
            destination: Some("elevenlabs:flows:image".into()),
        })
    }
}

#[derive(Clone)]
struct ElevenLabsGenerateVideoTool {
    deps: ElevenLabsToolDeps,
}

#[async_trait]
impl Tool for ElevenLabsGenerateVideoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "elevenlabs_generate_video".into(),
            description:
                "Generate a short video with ElevenLabs Image & Video flows, poll until complete, and store the result."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "prompt": {"type": "string"},
                    "model_id": {"type": "string"},
                    "aspect_ratio": {"type": "string", "default": "16:9"},
                    "duration_secs": {"type": "integer", "minimum": 1, "maximum": 30},
                    "filename": {"type": "string"}
                },
                "required": ["prompt"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let preview = args
            .get("prompt")
            .and_then(Value::as_str)
            .map(|text| truncate_chars(text, 120))
            .unwrap_or_default();
        Ok(ApprovalDetails {
            summary: format!("ElevenLabs video generation: {preview}"),
            destination: Some("elevenlabs:flows:video".into()),
            artifact_hash: None,
        })
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let prompt = args
            .get("prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("prompt is required".into()))?;
        if prompt.len() > MAX_VIDEO_PROMPT_CHARS {
            return Err(SeraError::Validation("video prompt is too long".into()));
        }
        check_outbound_text(&self.deps.policy, prompt)?;
        let model_id = args
            .get("model_id")
            .and_then(Value::as_str)
            .unwrap_or(self.deps.config.video_model.as_str());
        let aspect_ratio = args
            .get("aspect_ratio")
            .and_then(Value::as_str)
            .unwrap_or("16:9");
        let duration_secs = args.get("duration_secs").and_then(Value::as_u64).unwrap_or(5) as u32;
        let created = self
            .deps
            .client
            .create_video(model_id, prompt, aspect_ratio, duration_secs)
            .await?;
        let generation_id = created.id.clone();
        let generation_id_log = generation_id.clone();
        let client = self.deps.client.clone();
        let poll_interval = Duration::from_millis(self.deps.config.poll_interval_ms.max(500));
        let max_attempts = self.deps.config.poll_max_attempts;
        let completed = self
            .deps
            .client
            .poll_flow(
                move || {
                    let client = client.clone();
                    let id = generation_id.clone();
                    Box::pin(async move { client.get_video_generation(&id).await })
                },
                poll_interval,
                max_attempts,
            )
            .await?;
        let ext = extension_for_mime(&completed.content_mime_type);
        let filename = args
            .get("filename")
            .and_then(Value::as_str)
            .map(|value| value.to_owned())
            .unwrap_or_else(|| format!("elevenlabs-video.{ext}"));
        let (hash, filename) = store_media_artifact(
            &self.deps.artifacts,
            &completed.bytes,
            &filename,
            "ElevenLabs video",
        )?;
        Ok(ToolOutput {
            content: format!(
                "Video generation {generation_id_log} completed. artifact_hash={hash} filename={filename}"
            ),
            data: Some(json!({
                "generation_id": generation_id_log,
                "artifact_hash": hash,
                "filename": filename,
                "content_mime_type": completed.content_mime_type,
            })),
            artifact_path: Some(format!("artifacts/{filename}")),
            artifact_hash: Some(hash),
            filename: Some(filename),
            destination: Some("elevenlabs:flows:video".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mock_client(server: &MockServer) -> ElevenLabsClient {
        ElevenLabsClient::new(server.uri(), "synthetic-key", 10).unwrap()
    }

    #[tokio::test]
    async fn text_to_speech_returns_audio_bytes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/text-to-speech/voice123"))
            .and(header("xi-api-key", "synthetic-key"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"FAKEAUDIO"))
            .mount(&server)
            .await;
        let client = mock_client(&server);
        let bytes = client
            .text_to_speech("voice123", "Hello", "eleven_multilingual_v2", "mp3_44100_128")
            .await
            .unwrap();
        assert_eq!(bytes, b"FAKEAUDIO");
    }

    #[tokio::test]
    async fn list_voices_returns_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"/v2/voices.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"voices": []})))
            .mount(&server)
            .await;
        let client = mock_client(&server);
        let value = client.list_voices(10, None).await.unwrap();
        assert!(value.get("voices").is_some());
    }

    #[tokio::test]
    async fn sound_effect_returns_audio() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/sound-generation"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"SFX"))
            .mount(&server)
            .await;
        let client = mock_client(&server);
        let bytes = client
            .sound_effect("Thunder rumble", "mp3_44100_128", None)
            .await
            .unwrap();
        assert_eq!(bytes, b"SFX");
    }

    #[tokio::test]
    async fn image_flow_polls_until_completed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/flows/image"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "gen1",
                "status": "pending"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/flows/image/gen1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "gen1",
                "status": "completed",
                "content_mime_type": "image/png",
                "content_url": format!("{}/media.png", server.uri())
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/media.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PNG"))
            .mount(&server)
            .await;
        let client = mock_client(&server);
        let created = client
            .create_image("gemini-2.5-flash-image", "A red cube", "1:1")
            .await
            .unwrap();
        let id = created.id.clone();
        let client2 = mock_client(&server);
        let completed = client
            .poll_flow(
                move || {
                    let client = client2.clone();
                    let id = id.clone();
                    Box::pin(async move { client.get_image_generation(&id).await })
                },
                Duration::from_millis(10),
                5,
            )
            .await
            .unwrap();
        assert_eq!(completed.bytes, b"PNG");
    }
}
