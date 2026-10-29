//! Mistral Voxtral audio transcription and chat-based understanding.
//!
//! Outbound calls use the configured HTTPS provider endpoint and age-stored API
//! key. Tools fail closed when provider data-egress consent is not configured.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use reqwest::multipart::{Form, Part};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{AudioConfig, Config, LlmConfig};
use crate::error::{Result, SeraError};
use crate::gateway::{ApprovalDetails, Tool, ToolContext, ToolOutput, ToolSpec};
use crate::llm::validate_base_url;
use crate::secrets::SecretStore;
use crate::util::sha256_hex;

/// Conservative default aligned with Mistral offline transcription limits (docs ~2026-10).
pub const DEFAULT_MAX_AUDIO_BYTES: u64 = 52_428_800; // 50 MiB

/// Telegram voice notes are small; audio documents may be larger (see docs/audio.md).
pub const DEFAULT_TELEGRAM_AUDIO_MAX_BYTES: u64 = 20_971_520; // 20 MiB

const ALLOWED_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "mpeg", "ogg", "oga", "opus", "m4a", "aac", "flac", "webm",
];

#[derive(Debug, Clone)]
pub struct AudioProviderSettings {
    pub base_url: String,
    pub api_key_secret: String,
    pub data_egress_approved: bool,
}

pub fn resolve_audio_provider(
    llm: &LlmConfig,
    audio: &AudioConfig,
) -> Result<AudioProviderSettings> {
    if audio.provider.trim() != "default" {
        let profile = llm.providers.get(&audio.provider).ok_or_else(|| {
            SeraError::Config(format!(
                "audio.provider '{}' is not defined under [llm.providers]",
                audio.provider
            ))
        })?;
        return Ok(AudioProviderSettings {
            base_url: audio
                .base_url
                .clone()
                .unwrap_or_else(|| profile.base_url.clone()),
            api_key_secret: audio
                .api_key_secret
                .clone()
                .unwrap_or_else(|| profile.api_key_secret.clone()),
            data_egress_approved: profile.data_egress_approved,
        });
    }
    Ok(AudioProviderSettings {
        base_url: audio
            .base_url
            .clone()
            .unwrap_or_else(|| llm.base_url.clone()),
        api_key_secret: audio
            .api_key_secret
            .clone()
            .unwrap_or_else(|| llm.api_key_secret.clone()),
        data_egress_approved: llm.data_egress_approved,
    })
}

/// Readiness of the configured audio provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioProviderState {
    /// Audio is enabled, the provider resolves, its key is stored, and data
    /// egress is approved.
    Ready,
    /// Transcription is unavailable. The message names the missing setting and
    /// never carries a secret.
    Unavailable(String),
}

/// Resolve the configured audio provider and read its credential from the age
/// store. Failures name the missing configuration; no secret is ever returned
/// in an error message.
fn audio_provider_credential(config: &Config) -> Result<(AudioProviderSettings, String)> {
    if !config.audio.enabled {
        return Err(SeraError::Config(
            "audio provider is not configured: audio.enabled is off".into(),
        ));
    }
    let provider = resolve_audio_provider(&config.llm, &config.audio)
        .map_err(|error| SeraError::Config(format!("audio provider is not configured: {error}")))?;
    if !provider.data_egress_approved {
        return Err(SeraError::Config(
            "audio provider is not configured: data-egress consent is not approved for the audio provider"
                .into(),
        ));
    }
    let store = SecretStore::open(config).map_err(|error| {
        SeraError::Config(format!(
            "audio provider is not configured: secret store is unavailable ({error})"
        ))
    })?;
    let api_key = store
        .require(&provider.api_key_secret)
        .map(str::to_owned)
        .map_err(|_| {
            SeraError::Config(format!(
                "audio provider is not configured: secret '{}' is not stored",
                provider.api_key_secret
            ))
        })?;
    Ok((provider, api_key))
}

/// Owner-facing readiness of the audio provider.
pub fn audio_provider_state(config: &Config) -> AudioProviderState {
    match audio_provider_credential(config) {
        Ok(_) => AudioProviderState::Ready,
        Err(error) => AudioProviderState::Unavailable(error.to_string()),
    }
}

/// Transcribe raw audio bytes with the configured audio provider.
///
/// The provider, the transcription model and the credential are the ones the
/// server already uses — no new provider, secret or setting. Unsupported
/// formats and oversized payloads are rejected before any network call. Every
/// error is safe to show the owner: it names the missing setting or the
/// provider failure and never carries a secret or a raw provider body.
pub async fn transcribe_audio_bytes(
    config: &Config,
    bytes: &[u8],
    filename: &str,
) -> Result<String> {
    let extension = Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !ALLOWED_EXTENSIONS.contains(&extension.as_str()) {
        return Err(SeraError::Validation(format!(
            "unsupported audio type '.{extension}'; allowed: {}",
            ALLOWED_EXTENSIONS.join(", ")
        )));
    }
    if bytes.is_empty() {
        return Err(SeraError::Validation("audio payload is empty".into()));
    }
    if bytes.len() as u64 > DEFAULT_TELEGRAM_AUDIO_MAX_BYTES {
        return Err(SeraError::Validation(format!(
            "audio payload exceeds the {DEFAULT_TELEGRAM_AUDIO_MAX_BYTES} byte limit"
        )));
    }
    let (provider, api_key) = audio_provider_credential(config)?;
    let client = MistralAudioClient::new(
        provider.base_url,
        api_key,
        config.llm.timeout_secs,
        &config.audio,
        provider.data_egress_approved,
    )?;
    client.transcribe_bytes(bytes, filename, None).await
}

#[derive(Clone)]
pub struct MistralAudioClient {
    http: reqwest::Client,
    base_url: String,
    api_key: SecretString,
    transcription_model: String,
    understanding_model: String,
    max_bytes: u64,
    data_egress_approved: bool,
}

impl std::fmt::Debug for MistralAudioClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MistralAudioClient")
            .field("base_url", &self.base_url)
            .field("transcription_model", &self.transcription_model)
            .field("understanding_model", &self.understanding_model)
            .field("data_egress_approved", &self.data_egress_approved)
            .finish()
    }
}

impl MistralAudioClient {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        timeout_secs: u64,
        audio: &AudioConfig,
        data_egress_approved: bool,
    ) -> Result<Self> {
        let base_url = validate_base_url(&base_url.into())?;
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(SeraError::Config(
                "audio provider credential is missing".into(),
            ));
        }
        validate_model_id(&audio.transcription_model)?;
        validate_model_id(&audio.understanding_model)?;
        let max_bytes = audio.max_bytes.max(1);
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs.max(1)))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| SeraError::External(error.to_string()))?;
        Ok(Self {
            http,
            base_url,
            api_key: SecretString::new(api_key),
            transcription_model: audio.transcription_model.clone(),
            understanding_model: audio.understanding_model.clone(),
            max_bytes,
            data_egress_approved,
        })
    }

    fn ensure_egress(&self) -> Result<()> {
        if !self.data_egress_approved {
            return Err(SeraError::Forbidden(
                "provider data-egress consent is not configured; set llm.data_egress_approved (or the selected provider profile) after owner review"
                    .into(),
            ));
        }
        Ok(())
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    pub fn destination_transcribe(&self) -> String {
        format!("{}/audio/transcriptions", self.base_url)
    }

    pub fn destination_understand(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    pub async fn transcribe_file(&self, path: &Path, language: Option<&str>) -> Result<String> {
        let bytes = std::fs::read(path)?;
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("audio.wav");
        self.transcribe_bytes(&bytes, filename, language).await
    }

    /// Transcribe in-memory audio. `filename` only carries the format (its
    /// extension) and the provider-side file name; the bytes never touch disk.
    pub async fn transcribe_bytes(
        &self,
        bytes: &[u8],
        filename: &str,
        language: Option<&str>,
    ) -> Result<String> {
        self.ensure_egress()?;
        if bytes.is_empty() {
            return Err(SeraError::Validation("audio payload is empty".into()));
        }
        if bytes.len() as u64 > self.max_bytes {
            return Err(SeraError::Validation(format!(
                "audio file exceeds configured limit of {} bytes",
                self.max_bytes
            )));
        }
        let filename = safe_audio_filename(filename);
        let mime = mime_for_filename(&filename);
        let mut form = Form::new()
            .text("model", self.transcription_model.clone())
            .part(
                "file",
                Part::bytes(bytes.to_vec())
                    .file_name(filename.to_owned())
                    .mime_str(mime)
                    .map_err(|error| SeraError::External(error.to_string()))?,
            );
        if let Some(language) = language {
            if !language.is_empty() && language.len() <= 16 {
                form = form.text("language", language.to_owned());
            }
        }
        let url = self.destination_transcribe();
        let response = self
            .http
            .post(&url)
            .bearer_auth(self.api_key.expose_secret())
            .multipart(form)
            .send()
            .await
            .map_err(|error| {
                SeraError::External(format!("audio transcription request failed: {error}"))
            })?;
        let status = response.status();
        let body = response.bytes().await.map_err(|error| {
            SeraError::External(format!("audio transcription read failed: {error}"))
        })?;
        if !status.is_success() {
            return Err(SeraError::External(format!(
                "audio transcription failed with status {}: {}",
                status.as_u16(),
                self.redact_secret(sanitize_provider_error(&body))
            )));
        }
        parse_transcription_response(&body)
    }

    /// Provider errors are shown to the owner, so the credential must never
    /// survive into an error message.
    fn redact_secret(&self, text: String) -> String {
        let secret = self.api_key.expose_secret();
        if secret.trim().is_empty() {
            return text;
        }
        text.replace(secret, "<redacted>")
    }

    pub async fn understand_file(&self, path: &Path, instruction: &str) -> Result<String> {
        self.ensure_egress()?;
        if instruction.trim().is_empty() || instruction.len() > 8_192 {
            return Err(SeraError::Validation(
                "instruction is required and must be at most 8192 characters".into(),
            ));
        }
        let bytes = std::fs::read(path)?;
        if bytes.len() as u64 > self.max_bytes {
            return Err(SeraError::Validation(format!(
                "audio file exceeds configured limit of {} bytes",
                self.max_bytes
            )));
        }
        let encoded = BASE64.encode(&bytes);
        let body = json!({
            "model": self.understanding_model,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "input_audio", "input_audio": encoded},
                    {"type": "text", "text": instruction},
                ],
            }],
        });
        let url = self.destination_understand();
        let response = self
            .http
            .post(&url)
            .bearer_auth(self.api_key.expose_secret())
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                SeraError::External(format!("audio understanding request failed: {error}"))
            })?;
        let status = response.status();
        let body = response.bytes().await.map_err(|error| {
            SeraError::External(format!("audio understanding read failed: {error}"))
        })?;
        if !status.is_success() {
            return Err(SeraError::External(format!(
                "audio understanding failed with status {}: {}",
                status.as_u16(),
                sanitize_provider_error(&body)
            )));
        }
        parse_chat_completion_text(&body)
    }
}

pub fn resolve_inbox_audio(config: &Config, relative: &str) -> Result<PathBuf> {
    if relative.is_empty() || relative.len() > 512 {
        return Err(SeraError::Validation("invalid audio path".into()));
    }
    let raw = Path::new(relative);
    if raw.is_absolute() {
        return Err(SeraError::Forbidden(
            "audio paths must be relative to the configured inbox".into(),
        ));
    }
    if raw.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return Err(SeraError::Forbidden(
            "audio path must stay within the inbox".into(),
        ));
    }
    let root = config
        .path(&config.documents.watch_dir)
        .canonicalize()
        .map_err(|_| SeraError::NotFound("document inbox is unavailable".into()))?;
    let candidate = root
        .join(raw)
        .canonicalize()
        .map_err(|_| SeraError::NotFound("audio file not found".into()))?;
    if !candidate.starts_with(&root) {
        return Err(SeraError::Forbidden(
            "audio path escapes the configured inbox".into(),
        ));
    }
    let extension = candidate
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !ALLOWED_EXTENSIONS.contains(&extension.as_str()) {
        return Err(SeraError::Validation(format!(
            "unsupported audio type '.{extension}'; allowed: {}",
            ALLOWED_EXTENSIONS.join(", ")
        )));
    }
    let metadata = std::fs::metadata(&candidate)?;
    if !metadata.is_file() {
        return Err(SeraError::Validation("path is not a file".into()));
    }
    Ok(candidate)
}

/// Keep only the base name of an audio file name and strip path separators and
/// control characters. The name is sent to the provider as a file label.
fn safe_audio_filename(filename: &str) -> String {
    let base = Path::new(filename)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("audio.wav");
    let cleaned: String = base
        .chars()
        .filter(|character| !character.is_control() && !matches!(character, '/' | '\\'))
        .take(180)
        .collect();
    if cleaned.is_empty() {
        "audio.wav".into()
    } else {
        cleaned
    }
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
        "webm" => "audio/webm",
        _ => "application/octet-stream",
    }
}

fn validate_model_id(model: &str) -> Result<()> {
    if model.is_empty()
        || model.len() > 128
        || model.trim() != model
        || model.chars().any(char::is_control)
    {
        return Err(SeraError::Config("invalid audio model id".into()));
    }
    Ok(())
}

fn parse_transcription_response(bytes: &[u8]) -> Result<String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        SeraError::External(format!("malformed transcription response: {error}"))
    })?;
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        return Ok(text.to_owned());
    }
    if let Some(segments) = value.get("segments").and_then(Value::as_array) {
        let mut parts = Vec::new();
        for segment in segments {
            if let Some(text) = segment.get("text").and_then(Value::as_str) {
                if !text.trim().is_empty() {
                    parts.push(text.trim());
                }
            }
        }
        if !parts.is_empty() {
            return Ok(parts.join("\n"));
        }
    }
    Err(SeraError::External(
        "transcription response contained no text".into(),
    ))
}

fn parse_chat_completion_text(bytes: &[u8]) -> Result<String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| SeraError::External(format!("malformed chat response: {error}")))?;
    let text = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if text.is_empty() {
        return Err(SeraError::External(
            "audio understanding returned empty content".into(),
        ));
    }
    Ok(text)
}

fn sanitize_provider_error(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let parsed: Option<Value> = serde_json::from_str(&text).ok();
    let message = parsed
        .as_ref()
        .and_then(|value| value.pointer("/error/message"))
        .and_then(Value::as_str)
        .or_else(|| {
            parsed
                .as_ref()
                .and_then(|value| value.get("message"))
                .and_then(Value::as_str)
        })
        .unwrap_or(&text);
    message.chars().take(500).collect()
}

pub struct AudioTranscribeTool {
    client: Arc<MistralAudioClient>,
    config: Arc<Config>,
}

impl AudioTranscribeTool {
    pub fn new(client: Arc<MistralAudioClient>, config: Arc<Config>) -> Self {
        Self { client, config }
    }

    fn resolve(&self, path: &str) -> Result<PathBuf> {
        let path = resolve_inbox_audio(&self.config, path)?;
        let size = std::fs::metadata(&path)?.len();
        if size > self.client.max_bytes() {
            return Err(SeraError::Validation(format!(
                "audio file exceeds configured limit of {} bytes",
                self.client.max_bytes()
            )));
        }
        Ok(path)
    }
}

#[async_trait]
impl Tool for AudioTranscribeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "audio_transcribe".into(),
            description:
                "Transcribe a speech recording from the configured inbox using the Mistral Voxtral \
                 transcription API. Returns verbatim text as untrusted data. Requires owner \
                 data-egress consent and gateway approval before audio leaves this host."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Audio file relative to the inbox"},
                    "language": {"type": "string", "description": "Optional BCP-47 language hint, e.g. en or it"}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("path is required".into()))?;
        let resolved = self.resolve(path)?;
        let bytes = std::fs::read(&resolved)?;
        Ok(ApprovalDetails {
            summary: format!(
                "Send inbox audio '{}' ({} bytes) to Mistral for verbatim transcription",
                path,
                bytes.len()
            ),
            destination: Some(self.client.destination_transcribe()),
            artifact_hash: Some(sha256_hex(&bytes)),
        })
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("path is required".into()))?;
        let language = args.get("language").and_then(Value::as_str);
        let resolved = self.resolve(path)?;
        let text = self.client.transcribe_file(&resolved, language).await?;
        Ok(ToolOutput {
            content: text.clone(),
            data: Some(json!({
                "path": path,
                "transcript": text,
                "provider": "mistral",
                "mode": "transcription",
            })),
            ..ToolOutput::default()
        })
    }
}

pub struct AudioUnderstandTool {
    client: Arc<MistralAudioClient>,
    config: Arc<Config>,
}

impl AudioUnderstandTool {
    pub fn new(client: Arc<MistralAudioClient>, config: Arc<Config>) -> Self {
        Self { client, config }
    }

    fn resolve(&self, path: &str) -> Result<PathBuf> {
        let path = resolve_inbox_audio(&self.config, path)?;
        let size = std::fs::metadata(&path)?.len();
        if size > self.client.max_bytes() {
            return Err(SeraError::Validation(format!(
                "audio file exceeds configured limit of {} bytes",
                self.client.max_bytes()
            )));
        }
        Ok(path)
    }
}

#[async_trait]
impl Tool for AudioUnderstandTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "audio_understand".into(),
            description:
                "Analyze a speech recording from the configured inbox with a natural-language \
                 instruction (summary, Q&A, translation, structured extraction) using Mistral audio \
                 in chat completions. Not a verbatim transcript — use audio_transcribe for that."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Audio file relative to the inbox"},
                    "instruction": {"type": "string", "description": "What to extract or answer about the recording"}
                },
                "required": ["path", "instruction"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("path is required".into()))?;
        let instruction = args
            .get("instruction")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let resolved = self.resolve(path)?;
        let bytes = std::fs::read(&resolved)?;
        Ok(ApprovalDetails {
            summary: format!(
                "Send inbox audio '{}' ({} bytes) to Mistral for: {}",
                path,
                bytes.len(),
                instruction.chars().take(120).collect::<String>()
            ),
            destination: Some(self.client.destination_understand()),
            artifact_hash: Some(sha256_hex(&bytes)),
        })
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("path is required".into()))?;
        let instruction = args
            .get("instruction")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("instruction is required".into()))?;
        let resolved = self.resolve(path)?;
        let text = self.client.understand_file(&resolved, instruction).await?;
        Ok(ToolOutput {
            content: text.clone(),
            data: Some(json!({
                "path": path,
                "instruction": instruction,
                "answer": text,
                "provider": "mistral",
                "mode": "understanding",
            })),
            ..ToolOutput::default()
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramAudioSaveReport {
    pub relative_path: String,
    pub bytes: u64,
    pub sha256: String,
}

/// Persist a Telegram voice/audio attachment under `telegram/` in the inbox.
pub fn save_telegram_audio_bytes(
    config: &Config,
    message_id: i64,
    extension: &str,
    bytes: &[u8],
    max_bytes: u64,
) -> Result<TelegramAudioSaveReport> {
    if bytes.is_empty() {
        return Err(SeraError::Validation("audio payload is empty".into()));
    }
    if bytes.len() as u64 > max_bytes {
        return Err(SeraError::Validation(format!(
            "Telegram audio exceeds configured telegram_max_bytes ({max_bytes})"
        )));
    }
    let extension = extension.trim_start_matches('.').to_ascii_lowercase();
    if !ALLOWED_EXTENSIONS.contains(&extension.as_str()) {
        return Err(SeraError::Validation(format!(
            "unsupported Telegram audio extension '.{extension}'"
        )));
    }
    let root = config.path(&config.documents.watch_dir);
    std::fs::create_dir_all(root.join("telegram"))?;
    let root = root
        .canonicalize()
        .map_err(|_| SeraError::NotFound("document inbox is unavailable".into()))?;
    let filename = format!("telegram/{message_id}.{extension}");
    let target = root.join(&filename);
    if target.exists() {
        return Err(SeraError::Validation(
            "Telegram audio for this message was already saved".into(),
        ));
    }
    std::fs::write(&target, bytes)?;
    let hash = sha256_hex(bytes);
    Ok(TelegramAudioSaveReport {
        relative_path: filename,
        bytes: bytes.len() as u64,
        sha256: hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TEST_SECRET: &str = "sk-test-audio-value";

    fn sample_config(inbox: &Path) -> Arc<Config> {
        let mut config = Config::default();
        config.documents.watch_dir = inbox.to_string_lossy().into_owned();
        config.audio.enabled = true;
        Arc::new(config)
    }

    /// Config with a real age store so the credential lookup can succeed.
    fn provider_config(dir: &TempDir, base_url: &str, store_key: Option<&str>) -> Config {
        use crate::secrets::{SecretStore, ensure_identity};
        let identity = dir.path().join("identity.txt");
        let secrets = dir.path().join("secrets.age");
        ensure_identity(&identity).unwrap();
        if let Some(key) = store_key {
            let mut store = SecretStore::open_paths(&secrets, &identity).unwrap();
            store.set("test_audio_key", key).unwrap();
            store.save().unwrap();
        }
        let mut config = Config {
            base_dir: dir.path().to_path_buf(),
            ..Config::default()
        };
        config.secrets.encrypted_file = secrets.to_string_lossy().into_owned();
        config.secrets.identity_file = identity.to_string_lossy().into_owned();
        config.audio.enabled = true;
        config.audio.base_url = Some(base_url.into());
        config.audio.api_key_secret = Some("test_audio_key".into());
        config.llm.data_egress_approved = true;
        config
    }

    #[test]
    fn rejects_path_traversal() {
        let dir = TempDir::new().unwrap();
        let config = sample_config(dir.path());
        assert!(resolve_inbox_audio(&config, "../secret.wav").is_err());
    }

    #[tokio::test]
    async fn transcribe_requires_egress_consent() {
        let server = MockServer::start().await;
        let client =
            MistralAudioClient::new(server.uri(), "secret", 10, &AudioConfig::default(), false)
                .unwrap();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("note.wav");
        std::fs::write(&file, b"RIFF").unwrap();
        let error = client.transcribe_file(&file, None).await.unwrap_err();
        assert!(matches!(error, SeraError::Forbidden(_)));
    }

    #[tokio::test]
    async fn transcribe_parses_text_field() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/audio/transcriptions"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "hello world"})))
            .mount(&server)
            .await;
        let client =
            MistralAudioClient::new(server.uri(), "secret", 10, &AudioConfig::default(), true)
                .unwrap();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("note.wav");
        std::fs::write(&file, b"RIFFfake").unwrap();
        let text = client.transcribe_file(&file, Some("en")).await.unwrap();
        assert_eq!(text, "hello world");
    }

    #[tokio::test]
    async fn understand_sends_input_audio_payload() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "summary bullet"}}]
            })))
            .mount(&server)
            .await;
        let client =
            MistralAudioClient::new(server.uri(), "secret", 10, &AudioConfig::default(), true)
                .unwrap();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("call.wav");
        std::fs::write(&file, b"RIFFfake").unwrap();
        let answer = client
            .understand_file(&file, "Summarize in one sentence.")
            .await
            .unwrap();
        assert_eq!(answer, "summary bullet");
    }

    #[test]
    fn saves_telegram_audio_under_inbox() {
        let dir = TempDir::new().unwrap();
        let config = sample_config(dir.path());
        let report = save_telegram_audio_bytes(&config, 42, "ogg", b"fake-ogg", 1024).unwrap();
        assert_eq!(report.relative_path, "telegram/42.ogg");
        assert!(dir.path().join("telegram/42.ogg").is_file());
    }

    #[tokio::test]
    async fn transcribe_bytes_rejects_unsupported_extension_before_any_request() {
        let server = MockServer::start().await;
        let dir = TempDir::new().unwrap();
        let config = provider_config(&dir, &server.uri(), Some(TEST_SECRET));
        let error = transcribe_audio_bytes(&config, b"RIFF", "note.txt")
            .await
            .unwrap_err();
        assert!(matches!(error, SeraError::Validation(_)));
        assert!(error.to_string().contains("unsupported audio type"));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn transcribe_bytes_rejects_oversized_payload_before_any_request() {
        let server = MockServer::start().await;
        let dir = TempDir::new().unwrap();
        let config = provider_config(&dir, &server.uri(), Some(TEST_SECRET));
        let bytes = vec![0u8; (DEFAULT_TELEGRAM_AUDIO_MAX_BYTES + 1) as usize];
        let error = transcribe_audio_bytes(&config, &bytes, "note.ogg")
            .await
            .unwrap_err();
        assert!(matches!(error, SeraError::Validation(_)));
        assert!(error.to_string().contains("exceeds"));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn transcribe_bytes_without_a_configured_provider_names_the_setting() {
        let server = MockServer::start().await;
        let disabled_dir = TempDir::new().unwrap();
        let mut disabled = provider_config(&disabled_dir, &server.uri(), Some(TEST_SECRET));
        disabled.audio.enabled = false;
        let error = transcribe_audio_bytes(&disabled, b"RIFF", "note.ogg")
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("audio provider is not configured"),
            "{message}"
        );
        assert!(message.contains("audio.enabled"), "{message}");
        assert!(!message.contains(TEST_SECRET), "{message}");

        let missing_dir = TempDir::new().unwrap();
        let missing_key = provider_config(&missing_dir, &server.uri(), None);
        let error = transcribe_audio_bytes(&missing_key, b"RIFF", "note.ogg")
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("audio provider is not configured"),
            "{message}"
        );
        assert!(message.contains("test_audio_key"), "{message}");
        assert!(!message.contains(TEST_SECRET), "{message}");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn transcribe_bytes_uses_the_configured_provider() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/audio/transcriptions"))
            .and(header("authorization", format!("Bearer {TEST_SECRET}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "buongiorno"})))
            .mount(&server)
            .await;
        let dir = TempDir::new().unwrap();
        let config = provider_config(&dir, &server.uri(), Some(TEST_SECRET));
        let text = transcribe_audio_bytes(&config, b"RIFFfake", "note.ogg")
            .await
            .unwrap();
        assert_eq!(text, "buongiorno");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn transcribe_bytes_never_leaks_the_provider_key_in_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"message": format!("invalid api key {TEST_SECRET}")}
            })))
            .mount(&server)
            .await;
        let dir = TempDir::new().unwrap();
        let config = provider_config(&dir, &server.uri(), Some(TEST_SECRET));
        let error = transcribe_audio_bytes(&config, b"RIFFfake", "note.ogg")
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(!message.contains(TEST_SECRET), "{message}");
        assert!(message.contains("<redacted>"), "{message}");
    }

    #[test]
    fn provider_state_reports_the_missing_setting() {
        let dir = TempDir::new().unwrap();
        let config = provider_config(&dir, "https://example.test/v1", None);
        match audio_provider_state(&config) {
            AudioProviderState::Unavailable(reason) => {
                assert!(
                    reason.contains("audio provider is not configured"),
                    "{reason}"
                );
                assert!(reason.contains("test_audio_key"), "{reason}");
            }
            AudioProviderState::Ready => panic!("provider must not be ready without a key"),
        }
        let ready = provider_config(&dir, "https://example.test/v1", Some(TEST_SECRET));
        assert_eq!(audio_provider_state(&ready), AudioProviderState::Ready);
    }
}
