//! Typed model-call failures and the classifier that decides whether a turn
//! may move to another account or provider.
//!
//! Model calls fail with `SeraError::Upstream { status, code, .. }`. `code` is
//! one of the stable words below, chosen once at the point of failure from the
//! HTTP status plus the provider's own error code (OpenAI `insufficient_quota`,
//! Google `RESOURCE_EXHAUSTED`, Anthropic `overloaded_error`, Cloud Code 403,
//! 404 and 429). [`classify_llm`] then answers one question for the failover
//! engine: is this failure [`FailKind`] (another candidate may succeed) or
//! [`NoSwitch`] (switching cannot help, or must not happen)?
//!
//! Bodies are read only to pick a code. They are never copied into the error:
//! provider errors can echo credentials.

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde_json::Value;

use crate::error::SeraError;

/// Longest error body inspected.
const MAX_BODY_BYTES: usize = 64 * 1024;
/// Longest back-off hint kept. A quota reset can be hours away.
const MAX_HOLD: Duration = Duration::from_secs(24 * 3600);

pub const CODE_RATE_LIMITED: &str = "rate_limited";
pub const CODE_OVERLOADED: &str = "overloaded";
pub const CODE_MODEL_UNAVAILABLE: &str = "model_unavailable";
pub const CODE_QUOTA_EXHAUSTED: &str = "quota_exhausted";
pub const CODE_CREDENTIALS_REJECTED: &str = "credentials_rejected";
pub const CODE_ACCOUNT_FORBIDDEN: &str = "account_forbidden";
pub const CODE_PROVIDER_DOWN: &str = "provider_down";
pub const CODE_TRANSPORT: &str = "transport";
pub const CODE_CONTEXT_TOO_LONG: &str = "context_too_long";
pub const CODE_INVALID_REQUEST: &str = "invalid_request";
pub const CODE_CONTENT_FILTERED: &str = "content_filtered";
pub const CODE_POLICY_BLOCKED: &str = "policy_blocked";
pub const CODE_EGRESS_DENIED: &str = "egress_denied";
pub const CODE_CANCELLED: &str = "cancelled";

/// A failure another account, model or provider may get past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailKind {
    RateLimited {
        retry_after: Option<Duration>,
    },
    Overloaded,
    ModelUnavailable,
    QuotaExhausted {
        resets_at: Option<DateTime<Utc>>,
    },
    CredentialsRejected,
    AccountForbidden,
    ProviderDown,
    Transport,
    /// Switching alone does not help; the caller compacts first.
    ContextTooLong,
}

/// A failure that must end the turn on this route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoSwitch {
    PolicyBlocked,
    EgressDenied,
    Cancelled,
    InvalidRequest,
    Content,
}

/// Whether the failed call may be tried on another candidate.
pub fn classify_llm(error: &SeraError) -> Result<FailKind, NoSwitch> {
    match error {
        SeraError::Upstream {
            code, retry_after, ..
        } => from_code(code, *retry_after),
        SeraError::PolicyViolation => Err(NoSwitch::PolicyBlocked),
        SeraError::Forbidden(_) => Err(NoSwitch::EgressDenied),
        SeraError::Rejected { code, .. } => match *code {
            "model_unavailable" | "unknown_provider" => Ok(FailKind::ModelUnavailable),
            CODE_CANCELLED | "turn_cancelled" => Err(NoSwitch::Cancelled),
            CODE_CONTENT_FILTERED => Err(NoSwitch::Content),
            CODE_POLICY_BLOCKED => Err(NoSwitch::PolicyBlocked),
            CODE_EGRESS_DENIED => Err(NoSwitch::EgressDenied),
            _ => Err(NoSwitch::InvalidRequest),
        },
        // An error nobody typed: the request left but no answer was read.
        SeraError::External(_) => Ok(FailKind::Transport),
        // Local faults (configuration, storage, serialization): another
        // provider would hit the same one.
        _ => Err(NoSwitch::InvalidRequest),
    }
}

fn from_code(code: &str, retry_after: Option<Duration>) -> Result<FailKind, NoSwitch> {
    Ok(match code {
        CODE_RATE_LIMITED => FailKind::RateLimited { retry_after },
        CODE_OVERLOADED => FailKind::Overloaded,
        CODE_MODEL_UNAVAILABLE => FailKind::ModelUnavailable,
        CODE_QUOTA_EXHAUSTED => FailKind::QuotaExhausted {
            resets_at: retry_after.and_then(|wait| {
                chrono::Duration::from_std(wait)
                    .ok()
                    .map(|wait| Utc::now() + wait)
            }),
        },
        CODE_CREDENTIALS_REJECTED => FailKind::CredentialsRejected,
        CODE_ACCOUNT_FORBIDDEN => FailKind::AccountForbidden,
        CODE_PROVIDER_DOWN => FailKind::ProviderDown,
        CODE_TRANSPORT => FailKind::Transport,
        CODE_CONTEXT_TOO_LONG => FailKind::ContextTooLong,
        CODE_CONTENT_FILTERED => return Err(NoSwitch::Content),
        CODE_POLICY_BLOCKED => return Err(NoSwitch::PolicyBlocked),
        CODE_EGRESS_DENIED => return Err(NoSwitch::EgressDenied),
        CODE_CANCELLED => return Err(NoSwitch::Cancelled),
        _ => return Err(NoSwitch::InvalidRequest),
    })
}

/// Fixed, provider-free wording for a failure code, for API responses. `None`
/// for codes that are not model-call codes.
pub fn public_message(code: &str) -> Option<&'static str> {
    Some(match code {
        CODE_RATE_LIMITED => "The model provider is rate limiting this account. Try again shortly.",
        CODE_OVERLOADED => "The model provider is overloaded. Try again shortly.",
        CODE_MODEL_UNAVAILABLE => "That model isn't available from the provider right now.",
        CODE_QUOTA_EXHAUSTED => "The model provider reports this account's quota is used up.",
        CODE_CREDENTIALS_REJECTED => {
            "The model provider rejected the credentials. Sign in or add the key again."
        }
        CODE_ACCOUNT_FORBIDDEN => "The model provider refused this account access to the model.",
        CODE_PROVIDER_DOWN => "The model provider is having trouble. Try again shortly.",
        CODE_TRANSPORT => "Could not reach the model provider. Check the connection and try again.",
        CODE_CONTEXT_TOO_LONG => "The conversation is too long for this model.",
        CODE_INVALID_REQUEST => "The model provider rejected the request.",
        CODE_CONTENT_FILTERED => "The model provider declined to answer this request.",
        _ => return None,
    })
}

/// The stable code for an HTTP failure. `status` is 0 when no response came.
pub fn code_for(status: u16, body: &str) -> &'static str {
    let signals = Signals::parse(body);
    if status == 0 {
        return CODE_TRANSPORT;
    }
    if status == 402
        || signals.any(&[
            "insufficient_quota",
            "billing_hard_limit_reached",
            "quota_exceeded",
            "quota_exhausted",
            "insufficient_funds",
            "credit_balance_too_low",
            "payment_required",
        ])
    {
        return CODE_QUOTA_EXHAUSTED;
    }
    if status == 529 || signals.any(&["overloaded_error", "overloaded"]) {
        return CODE_OVERLOADED;
    }
    if status == 413
        || signals.any(&[
            "context_length_exceeded",
            "string_above_max_length",
            "request_too_large",
        ])
        || signals.message_has(&[
            "context length",
            "context window",
            "maximum context",
            "prompt is too long",
            "input is too long",
            "too many tokens",
            "exceeds the maximum number of tokens",
        ])
    {
        return CODE_CONTEXT_TOO_LONG;
    }
    if signals.any(&[
        "content_filter",
        "content_policy_violation",
        "blocked_prompt",
        "prohibited_content",
    ]) {
        return CODE_CONTENT_FILTERED;
    }
    if status == 404
        || signals.any(&[
            "model_not_found",
            "model_not_available",
            "model_decommissioned",
            "not_found_error",
        ])
    {
        return CODE_MODEL_UNAVAILABLE;
    }
    if status == 401
        || signals.any(&[
            "authentication_error",
            "invalid_api_key",
            "invalid_authentication",
            "unauthenticated",
        ])
    {
        return CODE_CREDENTIALS_REJECTED;
    }
    if status == 403 || signals.any(&["permission_error", "permission_denied"]) {
        return CODE_ACCOUNT_FORBIDDEN;
    }
    if status == 429
        || signals.any(&[
            "rate_limit_error",
            "rate_limit_exceeded",
            "rate_limited",
            "too_many_requests",
            "resource_exhausted",
        ])
    {
        // Google reports both a per-minute limit and a spent quota as
        // RESOURCE_EXHAUSTED; only the words tell them apart.
        if signals.message_has(&[
            "reset after",
            "exhausted your capacity",
            "exceeded your current quota",
            "daily",
            "billing",
        ]) && signals.message_has(&["quota", "capacity"])
        {
            return CODE_QUOTA_EXHAUSTED;
        }
        return CODE_RATE_LIMITED;
    }
    if status == 408 {
        return CODE_TRANSPORT;
    }
    if status >= 500 || signals.any(&["api_error", "server_error"]) {
        return CODE_PROVIDER_DOWN;
    }
    CODE_INVALID_REQUEST
}

/// A failed HTTP response as a typed error. `message` must already be safe to
/// show (no body, or a sanitized one). `hint` is the `Retry-After` header; a
/// "quota will reset after 2h30m" sentence in the body fills in when absent.
pub fn upstream_error(
    status: u16,
    body: &str,
    hint: Option<Duration>,
    message: String,
) -> SeraError {
    let code = code_for(status, body);
    let retry_after = hint.or_else(|| reset_after(body));
    SeraError::Upstream {
        status,
        code,
        message,
        retry_after,
        ambiguous: false,
    }
}

/// A request that produced no response. A model call has no external effect,
/// so this is never "outcome unknown".
pub fn transport_error(message: String) -> SeraError {
    SeraError::upstream(0, CODE_TRANSPORT, message)
}

/// The `Retry-After` header in seconds, capped at one day. The shared
/// `retry::retry_after` caps at 30 s because it paces retries; a failover hold
/// needs the real wait.
pub fn retry_after_hold(headers: &HeaderMap) -> Option<Duration> {
    let seconds: u64 = headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds).min(MAX_HOLD))
}

/// Read a bounded body for classification only.
pub fn bounded_body(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_BODY_BYTES)]).into_owned()
}

/// The first [`MAX_BODY_BYTES`] of `body`, cut on a character boundary.
fn clip(body: &str) -> &str {
    let end = (0..=MAX_BODY_BYTES.min(body.len()))
        .rev()
        .find(|at| body.is_char_boundary(*at))
        .unwrap_or(0);
    &body[..end]
}

/// "quota will reset after 2h30m15s" (Cloud Code) or "retry in 12.5s" (Gemini).
fn reset_after(body: &str) -> Option<Duration> {
    let lower = clip(body).to_ascii_lowercase();
    let at = lower
        .find("reset after ")
        .map(|at| at + "reset after ".len())
        .or_else(|| lower.find("retry in ").map(|at| at + "retry in ".len()))?;
    let mut total = 0f64;
    let mut number = String::new();
    let mut seen = false;
    for ch in lower[at..].chars() {
        if ch.is_ascii_digit() || ch == '.' {
            number.push(ch);
            continue;
        }
        let unit = match ch {
            'h' => 3600.0,
            'm' => 60.0,
            's' => 1.0,
            _ => break,
        };
        let value: f64 = number.parse().ok()?;
        total += value * unit;
        number.clear();
        seen = true;
    }
    (seen && total.is_finite() && total > 0.0).then(|| Duration::from_secs_f64(total).min(MAX_HOLD))
}

/// The lowercase error identifiers and message text of a provider body.
struct Signals {
    tokens: Vec<String>,
    message: String,
}

impl Signals {
    fn parse(body: &str) -> Self {
        let mut signals = Self {
            tokens: Vec::new(),
            message: String::new(),
        };
        let body = clip(body);
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            signals.message = body.to_ascii_lowercase();
            return signals;
        };
        // Providers wrap the error object, sometimes in an array (Google).
        let value = match &value {
            Value::Array(items) => items.first().cloned().unwrap_or(Value::Null),
            other => other.clone(),
        };
        signals.collect(&value);
        if let Some(error) = value.get("error") {
            signals.collect(error);
            if let Some(details) = error.get("details").and_then(Value::as_array) {
                for detail in details {
                    signals.collect(detail);
                }
            }
        }
        signals
    }

    fn collect(&mut self, node: &Value) {
        for key in ["code", "type", "status", "reason", "error"] {
            match node.get(key) {
                Some(Value::String(text)) => self.tokens.push(text.to_ascii_lowercase()),
                Some(Value::Number(number)) => self.tokens.push(number.to_string()),
                _ => {}
            }
        }
        for key in ["message", "error_description"] {
            if let Some(text) = node.get(key).and_then(Value::as_str) {
                self.message.push_str(&text.to_ascii_lowercase());
                self.message.push(' ');
            }
        }
    }

    fn any(&self, wanted: &[&str]) -> bool {
        self.tokens
            .iter()
            .any(|token| wanted.contains(&token.as_str()))
    }

    fn message_has(&self, needles: &[&str]) -> bool {
        needles.iter().any(|needle| self.message.contains(needle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream(status: u16, body: &str) -> SeraError {
        upstream_error(status, body, None, "failed".into())
    }

    fn kind(status: u16, body: &str) -> Result<FailKind, NoSwitch> {
        classify_llm(&upstream(status, body))
    }

    #[test]
    fn status_and_body_code_table() {
        let openai_quota = r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#;
        let google_rate = r#"{"error":{"code":429,"message":"Resource has been exhausted (e.g. check quota).","status":"RESOURCE_EXHAUSTED"}}"#;
        let google_quota = r#"{"error":{"code":429,"message":"You have exhausted your capacity on this model. Your quota will reset after 2h30m15s.","status":"RESOURCE_EXHAUSTED"}}"#;
        let anthropic_overloaded =
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        let anthropic_rate =
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#;
        let anthropic_auth = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        let anthropic_perm =
            r#"{"type":"error","error":{"type":"permission_error","message":"nope"}}"#;
        let anthropic_missing =
            r#"{"type":"error","error":{"type":"not_found_error","message":"model: x"}}"#;
        let context = r#"{"error":{"code":"context_length_exceeded","message":"too long"}}"#;
        let filtered = r#"{"error":{"code":"content_filter","message":"blocked"}}"#;
        let invalid = r#"{"error":{"type":"invalid_request_error","message":"bad field"}}"#;

        #[allow(clippy::type_complexity)]
        let cases: Vec<(&str, u16, &str, Result<&str, NoSwitch>)> = vec![
            ("openai quota", 429, openai_quota, Ok(CODE_QUOTA_EXHAUSTED)),
            ("openai quota 402", 402, "", Ok(CODE_QUOTA_EXHAUSTED)),
            ("openai plain 429", 429, "{}", Ok(CODE_RATE_LIMITED)),
            ("google rate", 429, google_rate, Ok(CODE_RATE_LIMITED)),
            ("google quota", 429, google_quota, Ok(CODE_QUOTA_EXHAUSTED)),
            (
                "anthropic overloaded",
                529,
                anthropic_overloaded,
                Ok(CODE_OVERLOADED),
            ),
            (
                "anthropic overloaded 200 body",
                503,
                anthropic_overloaded,
                Ok(CODE_OVERLOADED),
            ),
            ("anthropic rate", 429, anthropic_rate, Ok(CODE_RATE_LIMITED)),
            (
                "anthropic auth",
                401,
                anthropic_auth,
                Ok(CODE_CREDENTIALS_REJECTED),
            ),
            (
                "anthropic permission",
                403,
                anthropic_perm,
                Ok(CODE_ACCOUNT_FORBIDDEN),
            ),
            (
                "anthropic missing model",
                404,
                anthropic_missing,
                Ok(CODE_MODEL_UNAVAILABLE),
            ),
            ("anthropic too large", 413, "", Ok(CODE_CONTEXT_TOO_LONG)),
            ("context code", 400, context, Ok(CODE_CONTEXT_TOO_LONG)),
            ("cloud code forbidden", 403, "", Ok(CODE_ACCOUNT_FORBIDDEN)),
            ("cloud code missing", 404, "", Ok(CODE_MODEL_UNAVAILABLE)),
            ("cloud code 429 no body", 429, "", Ok(CODE_RATE_LIMITED)),
            (
                "cloud code unauthorized",
                401,
                "",
                Ok(CODE_CREDENTIALS_REJECTED),
            ),
            ("server error", 500, "", Ok(CODE_PROVIDER_DOWN)),
            ("bad gateway", 502, "<html>", Ok(CODE_PROVIDER_DOWN)),
            ("gateway timeout", 504, "", Ok(CODE_PROVIDER_DOWN)),
            ("no response", 0, "", Ok(CODE_TRANSPORT)),
            ("request timeout", 408, "", Ok(CODE_TRANSPORT)),
            ("content filter", 400, filtered, Err(NoSwitch::Content)),
            (
                "invalid request",
                400,
                invalid,
                Err(NoSwitch::InvalidRequest),
            ),
            ("unprocessable", 422, "", Err(NoSwitch::InvalidRequest)),
        ];
        for (name, status, body, expected) in cases {
            let error = upstream(status, body);
            let SeraError::Upstream { code, .. } = &error else {
                unreachable!()
            };
            match (&expected, classify_llm(&error)) {
                (Ok(want), Ok(_)) => assert_eq!(code, want, "{name}"),
                (Err(want), Err(got)) => assert_eq!(*want, got, "{name}"),
                (want, got) => panic!("{name}: expected {want:?}, got {got:?}"),
            }
        }
    }

    #[test]
    fn quota_carries_its_reset_and_rate_limit_its_hint() {
        let body = r#"{"error":{"code":429,"message":"Your quota will reset after 1h30m.","status":"RESOURCE_EXHAUSTED"}}"#;
        let Ok(FailKind::QuotaExhausted {
            resets_at: Some(at),
        }) = kind(429, body)
        else {
            panic!("expected a quota reset");
        };
        let wait = (at - Utc::now()).num_seconds();
        assert!((5390..=5400).contains(&wait), "{wait}");

        let error = upstream_error(429, "{}", Some(Duration::from_secs(7)), "x".into());
        assert_eq!(
            classify_llm(&error),
            Ok(FailKind::RateLimited {
                retry_after: Some(Duration::from_secs(7))
            })
        );
    }

    #[test]
    fn reset_phrases_parse_and_are_bounded() {
        assert_eq!(
            reset_after("Quota will reset after 2h30m15s."),
            Some(Duration::from_secs(9015))
        );
        assert_eq!(
            reset_after("retry in 12.5s"),
            Some(Duration::from_millis(12_500))
        );
        assert_eq!(reset_after("reset after 999999h"), Some(MAX_HOLD));
        assert_eq!(reset_after("nothing here"), None);
        assert_eq!(reset_after("reset after soon"), None);
    }

    #[test]
    fn retry_after_header_is_not_capped_at_the_pacing_limit() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, "3600".parse().unwrap());
        assert_eq!(retry_after_hold(&headers), Some(Duration::from_secs(3600)));
        headers.insert(RETRY_AFTER, "999999999".parse().unwrap());
        assert_eq!(retry_after_hold(&headers), Some(MAX_HOLD));
        headers.insert(
            RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after_hold(&headers), None);
    }

    #[test]
    fn local_and_policy_errors_never_switch() {
        assert_eq!(
            classify_llm(&SeraError::PolicyViolation),
            Err(NoSwitch::PolicyBlocked)
        );
        assert_eq!(
            classify_llm(&SeraError::Forbidden(
                "provider data-egress consent is not configured".into()
            )),
            Err(NoSwitch::EgressDenied)
        );
        assert_eq!(
            classify_llm(&SeraError::Rejected {
                code: "cancelled",
                message: "stopped".into()
            }),
            Err(NoSwitch::Cancelled)
        );
        assert_eq!(
            classify_llm(&SeraError::Validation("bad".into())),
            Err(NoSwitch::InvalidRequest)
        );
        assert_eq!(
            classify_llm(&SeraError::Config("missing".into())),
            Err(NoSwitch::InvalidRequest)
        );
        // A router refusal for a model that is gone is switchable.
        assert_eq!(
            classify_llm(&SeraError::Rejected {
                code: "model_unavailable",
                message: "gone".into()
            }),
            Ok(FailKind::ModelUnavailable)
        );
        // An untyped transport failure is switchable.
        assert_eq!(
            classify_llm(&SeraError::External("connection reset".into())),
            Ok(FailKind::Transport)
        );
    }

    #[test]
    fn the_body_never_reaches_the_error_message() {
        let error = upstream_error(
            401,
            r#"{"error":{"message":"bad key sk-secret-marker"}}"#,
            None,
            "LLM request failed with status 401".into(),
        );
        assert!(!error.to_string().contains("sk-secret-marker"));
        assert!(!format!("{error:?}").contains("sk-secret-marker"));
    }
}
