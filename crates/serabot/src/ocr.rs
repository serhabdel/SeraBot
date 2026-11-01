//! Optical character recognition for documents the built-in extractor cannot
//! read.
//!
//! A scanned PDF, a photographed page, or an image-only export carries no text
//! layer, so `documents.rs` extracts almost nothing from it. This module is the
//! fallback: it sends the bytes to the configured OCR provider (Mistral by
//! default, `POST /v1/ocr`) and returns per-page markdown, which the caller
//! indexes exactly like extracted text.
//!
//! # Permissions
//!
//! Reading: none. Writing: none. This module only performs an outbound request
//! and returns text. It never stores the document, the provider response, or
//! the credential.
//!
//! # Secrets
//!
//! The API key comes from the age store through the same provider resolution
//! the audio pipeline uses ([`crate::audio::resolve_audio_provider`]). It is
//! held in a [`SecretString`], never logged, and redacted in `Debug`. Provider
//! error bodies are never echoed: only the HTTP status and a short error code
//! reach the caller.
//!
//! # Failure semantics
//!
//! Fail closed. OCR disabled, a missing credential, an unapproved data-egress
//! profile, an oversized payload, and a provider error all return explicit
//! errors and never a partial guess. A document whose extraction is short but
//! whose OCR fails stays `failed` with the reason recorded, so the owner can
//! see why rather than silently indexing nothing.
//!
//! # Bounds
//!
//! The payload is capped by `ocr.max_bytes` (default 20 MiB) and the response
//! body by [`MAX_OCR_RESPONSE_BYTES`]. Pages are capped at
//! [`MAX_OCR_PAGES`]; a longer document is truncated with a warning rather than
//! buffered without limit.

use std::fmt;
use std::time::Duration;

use base64::Engine;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use tracing::debug;
use url::Url;

use crate::config::{AudioConfig, Config, LlmConfig, OcrConfig};
use crate::error::{Result, SeraError};
use crate::secrets::SecretStore;

/// Hard cap on the provider response body.
pub const MAX_OCR_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// Hard cap on pages read from one response.
pub const MAX_OCR_PAGES: usize = 500;

/// Per-page text cap, matching the document chunker's tolerance.
pub const MAX_PAGE_CHARS: usize = 200_000;

/// One recognized page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrPage {
    /// Zero-based page index as reported by the provider.
    pub index: usize,
    /// Markdown text for the page; empty when the provider saw no text.
    pub markdown: String,
}

/// The resolved OCR provider: endpoint, credential name, and consent.
#[derive(Debug, Clone)]
pub struct OcrProviderSettings {
    pub base_url: String,
    pub api_key_secret: String,
    pub data_egress_approved: bool,
}

/// Readiness of the configured OCR provider, for the settings surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcrProviderState {
    /// Enabled, resolvable, key stored, egress approved.
    Ready,
    /// Unavailable, with a message naming the missing setting and no secret.
    Unavailable(String),
}

/// Resolve the OCR provider from `[ocr]`, falling back to the `[llm]` profile
/// the same way `[audio]` does, so one Mistral key serves chat, audio, and OCR.
pub fn resolve_ocr_provider(llm: &LlmConfig, ocr: &OcrConfig) -> Result<OcrProviderSettings> {
    if ocr.provider.trim() != "default" {
        let profile = llm.providers.get(&ocr.provider).ok_or_else(|| {
            SeraError::Config(format!(
                "ocr.provider '{}' is not defined under [llm.providers]",
                ocr.provider
            ))
        })?;
        return Ok(OcrProviderSettings {
            base_url: ocr
                .base_url
                .clone()
                .unwrap_or_else(|| profile.base_url.clone()),
            api_key_secret: ocr
                .api_key_secret
                .clone()
                .unwrap_or_else(|| profile.api_key_secret.clone()),
            data_egress_approved: profile.data_egress_approved,
        });
    }
    Ok(OcrProviderSettings {
        base_url: ocr.base_url.clone().unwrap_or_else(|| llm.base_url.clone()),
        api_key_secret: ocr
            .api_key_secret
            .clone()
            .unwrap_or_else(|| llm.api_key_secret.clone()),
        data_egress_approved: llm.data_egress_approved,
    })
}

/// Resolve the provider and read its credential. Errors name the missing
/// setting and never carry a secret.
fn ocr_provider_credential(config: &Config) -> Result<(OcrProviderSettings, String)> {
    if !config.ocr.enabled {
        return Err(SeraError::Config(
            "OCR is not configured: ocr.enabled is off".into(),
        ));
    }
    let provider = resolve_ocr_provider(&config.llm, &config.ocr)
        .map_err(|error| SeraError::Config(format!("OCR is not configured: {error}")))?;
    if !provider.data_egress_approved {
        return Err(SeraError::Config(
            "OCR is not configured: data-egress consent is not approved for the OCR provider"
                .into(),
        ));
    }
    let store = SecretStore::open(config).map_err(|error| {
        SeraError::Config(format!(
            "OCR is not configured: secret store is unavailable ({error})"
        ))
    })?;
    let api_key = store
        .require(&provider.api_key_secret)
        .map(str::to_owned)
        .map_err(|_| {
            SeraError::Config(format!(
                "OCR is not configured: secret '{}' is not stored",
                provider.api_key_secret
            ))
        })?;
    Ok((provider, api_key))
}

/// Owner-facing readiness of the OCR provider.
pub fn ocr_provider_state(config: &Config) -> OcrProviderState {
    match ocr_provider_credential(config) {
        Ok(_) => OcrProviderState::Ready,
        Err(error) => OcrProviderState::Unavailable(error.to_string()),
    }
}

/// Whether OCR is configured well enough to attempt a call.
pub fn ocr_available(config: &Config) -> bool {
    matches!(ocr_provider_state(config), OcrProviderState::Ready)
}

/// Recognize text in a PDF or image with the configured provider.
///
/// `mime` selects the document shape: `application/pdf` becomes a base64
/// `document_url`, an image becomes a base64 `image_url`. Everything else is
/// rejected before any network call.
pub async fn recognize_bytes(config: &Config, bytes: &[u8], mime: &str) -> Result<Vec<OcrPage>> {
    if bytes.is_empty() {
        return Err(SeraError::Validation("OCR payload is empty".into()));
    }
    if bytes.len() as u64 > config.ocr.max_bytes {
        return Err(SeraError::Validation(format!(
            "OCR payload exceeds the {} byte limit",
            config.ocr.max_bytes
        )));
    }
    let (provider, api_key) = ocr_provider_credential(config)?;
    let client = OcrClient::new(
        provider.base_url,
        api_key,
        config.llm.timeout_secs,
        &config.ocr,
        provider.data_egress_approved,
    )?;
    client.recognize(bytes, mime).await
}

/// Mistral-compatible OCR client.
#[derive(Clone)]
pub struct OcrClient {
    http: reqwest::Client,
    base_url: String,
    api_key: SecretString,
    model: String,
    max_bytes: u64,
    data_egress_approved: bool,
}

impl fmt::Debug for OcrClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OcrClient")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("max_bytes", &self.max_bytes)
            .field("data_egress_approved", &self.data_egress_approved)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl OcrClient {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        timeout_secs: u64,
        ocr: &OcrConfig,
        data_egress_approved: bool,
    ) -> Result<Self> {
        let base_url = validate_base_url(&base_url.into())?;
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(SeraError::Config("OCR api key is empty".into()));
        }
        let model = ocr.model.trim().to_owned();
        validate_model_id(&model)?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs.clamp(5, 600)))
            .build()
            .map_err(|error| SeraError::Internal(format!("OCR HTTP client: {error}")))?;
        Ok(Self {
            http,
            base_url,
            api_key: SecretString::from(api_key),
            model,
            max_bytes: ocr.max_bytes,
            data_egress_approved,
        })
    }

    /// The endpoint this client calls, for tests and diagnostics.
    pub fn endpoint(&self) -> String {
        format!("{}/ocr", self.base_url)
    }

    /// Recognize one document. Returns pages in provider order.
    pub async fn recognize(&self, bytes: &[u8], mime: &str) -> Result<Vec<OcrPage>> {
        if !self.data_egress_approved {
            return Err(SeraError::Forbidden(
                "OCR data-egress consent is not approved".into(),
            ));
        }
        if bytes.is_empty() {
            return Err(SeraError::Validation("OCR payload is empty".into()));
        }
        if bytes.len() as u64 > self.max_bytes {
            return Err(SeraError::Validation(format!(
                "OCR payload exceeds the {} byte limit",
                self.max_bytes
            )));
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let document = match mime {
            "application/pdf" => serde_json::json!({
                "type": "document_url",
                "document_url": format!("data:application/pdf;base64,{encoded}"),
            }),
            other if other.starts_with("image/") => serde_json::json!({
                "type": "image_url",
                "image_url": format!("data:{other};base64,{encoded}"),
            }),
            other => {
                return Err(SeraError::Validation(format!(
                    "OCR supports PDF and images, not '{other}'"
                )));
            }
        };
        let body = serde_json::json!({
            "model": self.model,
            "document": document,
            "table_format": "html",
            "include_image_base64": false,
        });

        debug!(model = %self.model, bytes = bytes.len(), "requesting OCR");
        let response = self
            .http
            .post(self.endpoint())
            .bearer_auth(self.api_key.expose_secret())
            .json(&body)
            .send()
            .await
            .map_err(transport_error)?;

        let status = response.status();
        if !status.is_success() {
            let code = error_code(response).await;
            return Err(SeraError::External(format!(
                "OCR provider failed (HTTP {}): {code}",
                status.as_u16()
            )));
        }

        let bytes = read_limited(response).await?;
        let parsed: OcrResponse = serde_json::from_slice(&bytes)
            .map_err(|_| SeraError::External("invalid OCR response".into()))?;
        if parsed.pages.is_empty() {
            return Err(SeraError::External(
                "the OCR provider returned no pages".into(),
            ));
        }
        let mut pages = Vec::with_capacity(parsed.pages.len().min(MAX_OCR_PAGES));
        for page in parsed.pages.into_iter().take(MAX_OCR_PAGES) {
            pages.push(OcrPage {
                index: page.index,
                markdown: truncate_chars(&page.markdown, MAX_PAGE_CHARS),
            });
        }
        Ok(pages)
    }
}

#[derive(Deserialize)]
struct OcrResponse {
    #[serde(default)]
    pages: Vec<OcrPageResponse>,
}

#[derive(Deserialize)]
struct OcrPageResponse {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    markdown: String,
}

/// Join recognized pages into one document body, preserving page boundaries so
/// the chunker can keep page provenance.
pub fn pages_to_text(pages: &[OcrPage]) -> String {
    let mut out = String::new();
    for page in pages {
        let text = page.markdown.trim();
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(text);
    }
    out
}

fn validate_base_url(value: &str) -> Result<String> {
    let value = value.trim().trim_end_matches('/');
    let url = Url::parse(value)
        .map_err(|_| SeraError::Config("OCR base_url is not a valid URL".into()))?;
    let loopback = url
        .host_str()
        .map(|host| {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
        })
        .unwrap_or(false);
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(SeraError::Config(
            "OCR base_url must use https (http is allowed only for loopback test servers)".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SeraError::Config(
            "OCR base_url must not contain credentials".into(),
        ));
    }
    Ok(value.to_owned())
}

fn validate_model_id(model: &str) -> Result<()> {
    let valid = !model.is_empty()
        && model.len() <= 128
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
    if !valid {
        return Err(SeraError::Config("OCR model id is invalid".into()));
    }
    Ok(())
}

fn transport_error(error: reqwest::Error) -> SeraError {
    let detail = if error.is_timeout() {
        "request timed out"
    } else if error.is_connect() {
        "connection failed"
    } else {
        "transport error"
    };
    SeraError::External(format!("OCR request failed: {detail}"))
}

/// Read at most [`MAX_OCR_RESPONSE_BYTES`].
async fn read_limited(mut response: reqwest::Response) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| SeraError::External("OCR response could not be read".into()))?
    {
        if body.len() + chunk.len() > MAX_OCR_RESPONSE_BYTES {
            return Err(SeraError::External(format!(
                "OCR response exceeded the {MAX_OCR_RESPONSE_BYTES} byte limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Extract a short, printable error code without echoing the provider body.
async fn error_code(response: reqwest::Response) -> String {
    let body = read_limited(response).await.unwrap_or_default();
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        error: Option<Body>,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        code: Option<String>,
    }
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        code: Option<String>,
    }
    let Ok(envelope) = serde_json::from_slice::<Envelope>(&body) else {
        return "unknown_error".into();
    };
    let raw = envelope
        .error
        .and_then(|error| error.code.or(error.message))
        .or(envelope.code)
        .or(envelope.message)
        .unwrap_or_else(|| "unknown_error".into());
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(80)
        .collect();
    if cleaned.trim().is_empty() {
        "unknown_error".into()
    } else {
        cleaned.trim().to_owned()
    }
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    value.chars().take(max).collect()
}

/// The audio config is accepted so callers can share one provider resolution
/// path; OCR only reads `[ocr]` and `[llm]`.
#[allow(dead_code)]
fn _shared_provider_shape(_: &AudioConfig) {}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ocr_config(base_url: &str) -> OcrConfig {
        OcrConfig {
            enabled: true,
            provider: "default".into(),
            base_url: Some(base_url.to_owned()),
            api_key_secret: None,
            model: "mistral-ocr-latest".into(),
            max_bytes: 1_048_576,
            min_text_chars: 200,
        }
    }

    fn client(server: &MockServer) -> OcrClient {
        OcrClient::new(
            format!("{}/v1", server.uri()),
            "synthetic-ocr-key",
            30,
            &ocr_config(&server.uri()),
            true,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn recognizes_a_pdf_and_joins_pages() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/ocr"))
            .and(header("authorization", "Bearer synthetic-ocr-key"))
            .and(body_partial_json(serde_json::json!({
                "model": "mistral-ocr-latest",
                "table_format": "html"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "pages": [
                    {"index": 0, "markdown": "# Page one\n\nInvoice total 42."},
                    {"index": 1, "markdown": "  "},
                    {"index": 2, "markdown": "Page three"}
                ]
            })))
            .mount(&server)
            .await;

        let pages = client(&server)
            .recognize(b"%PDF-1.7 synthetic", "application/pdf")
            .await
            .unwrap();
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0].index, 0);
        assert_eq!(
            pages_to_text(&pages),
            "# Page one\n\nInvoice total 42.\n\nPage three"
        );
    }

    #[tokio::test]
    async fn sends_images_as_image_urls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/ocr"))
            .and(body_partial_json(serde_json::json!({
                "document": {"type": "image_url"}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "pages": [{"index": 0, "markdown": "scanned text"}]
            })))
            .mount(&server)
            .await;

        let pages = client(&server)
            .recognize(b"\x89PNG synthetic", "image/png")
            .await
            .unwrap();
        assert_eq!(pages[0].markdown, "scanned text");
    }

    #[tokio::test]
    async fn rejects_unsupported_mime_and_oversized_payloads() {
        let server = MockServer::start().await;
        let client = client(&server);
        let error = client.recognize(b"plain", "text/plain").await.unwrap_err();
        assert!(error.to_string().contains("PDF and images"), "{error}");

        let mut big = vec![0u8; 1_048_577];
        big[0] = 1;
        let error = client.recognize(&big, "application/pdf").await.unwrap_err();
        assert!(error.to_string().contains("byte limit"), "{error}");
    }

    #[tokio::test]
    async fn provider_errors_do_not_echo_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/ocr"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "error": {"code": "invalid_document", "message": "secret-looking detail"}
            })))
            .mount(&server)
            .await;

        let error = client(&server)
            .recognize(b"%PDF", "application/pdf")
            .await
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("invalid_document"), "{text}");
        assert!(!text.contains("secret-looking detail"), "{text}");
    }

    #[tokio::test]
    async fn egress_consent_is_required() {
        let server = MockServer::start().await;
        let client = OcrClient::new(
            format!("{}/v1", server.uri()),
            "synthetic-ocr-key",
            30,
            &ocr_config(&server.uri()),
            false,
        )
        .unwrap();
        let error = client
            .recognize(b"%PDF", "application/pdf")
            .await
            .unwrap_err();
        assert!(matches!(error, SeraError::Forbidden(_)), "{error}");
    }

    #[test]
    fn debug_redacts_the_key() {
        let client = OcrClient::new(
            "https://api.example.example/v1",
            "super-secret-key-value",
            30,
            &ocr_config("https://api.example.example/v1"),
            true,
        )
        .unwrap();
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("super-secret-key-value"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn base_url_must_be_https_or_loopback() {
        let error = OcrClient::new(
            "http://api.example.example/v1",
            "key",
            30,
            &ocr_config("http://api.example.example/v1"),
            true,
        )
        .unwrap_err();
        assert!(error.to_string().contains("https"), "{error}");
    }
}
