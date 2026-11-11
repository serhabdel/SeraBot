//! Shared retry policy for outbound HTTP calls (v0.33.0 reliability).
//!
//! Three attempts in total, with exponential backoff from a per-client base.
//! A server's `Retry-After` (seconds form) is honoured when it is longer than
//! the backoff, capped at [`MAX_RETRY_AFTER`].
//!
//! Which failures are retried depends on whether repeating the request can do
//! harm:
//!
//! - A 429 means the request was refused for rate reasons, so it did not run.
//!   Every method may retry it.
//! - 500, 502, 503 and 504 from an idempotent call (GET, DELETE) may retry.
//!   A 5xx from a non-idempotent call may come from a proxy after the upstream
//!   acted, so it is not retried there. The LLM completion is the exception:
//!   it has no external effect, so it retries 5xx too (see `llm.rs`).
//! - A transport error where the connection was never made (`is_connect`) is
//!   safe to retry for any method.
//! - A timeout or a connection lost mid-request may have reached the server.
//!   Those are retried only for idempotent methods.

use std::time::Duration;

use reqwest::header::{HeaderMap, RETRY_AFTER};

/// Total attempts for one call, the first included.
pub const MAX_ATTEMPTS: u32 = 3;

/// Upper bound on any single wait, including a server's `Retry-After`.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

/// Statuses that mean "try again later" rather than "this request is wrong".
pub fn transient_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

/// Whether a status response is worth another attempt for a call with this
/// idempotency. Non-idempotent calls retry only a 429, see the module docs.
pub fn retryable_status(status: u16, idempotent: bool) -> bool {
    if idempotent {
        transient_status(status)
    } else {
        status == 429
    }
}

/// Parse a `Retry-After` header in seconds. The HTTP-date form is ignored,
/// since a missing hint just means the normal backoff applies.
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    retry_after_value(headers.get(RETRY_AFTER)?.to_str().ok())
}

/// The seconds value of a `Retry-After` header, capped at [`MAX_RETRY_AFTER`].
pub fn retry_after_value(value: Option<&str>) -> Option<Duration> {
    let seconds: u64 = value?.trim().parse().ok()?;
    Some(Duration::from_secs(seconds).min(MAX_RETRY_AFTER))
}

/// Wait before retry number `attempt` (0 for the first retry). The backoff
/// doubles each time. A longer server hint replaces it. The result is capped.
pub fn delay(base: Duration, attempt: u32, hint: Option<Duration>) -> Duration {
    let backoff = base.saturating_mul(1u32 << attempt.min(8));
    hint.map_or(backoff, |hint| hint.max(backoff))
        .min(MAX_RETRY_AFTER)
}

/// Base wait for the shared [`send`] helper.
pub const SEND_BASE: Duration = Duration::from_millis(250);

/// Send a request with the policy above, for a call of the given idempotency.
/// A request whose body cannot be cloned is sent once, without retries. The
/// final response or error is returned unchanged for the caller to handle.
pub async fn send(
    request: reqwest::RequestBuilder,
    idempotent: bool,
) -> std::result::Result<reqwest::Response, reqwest::Error> {
    let mut attempt: u32 = 0;
    loop {
        let Some(copy) = request.try_clone() else {
            return request.send().await;
        };
        let more = attempt + 1 < MAX_ATTEMPTS;
        match copy.send().await {
            Ok(response) => {
                if more && retryable_status(response.status().as_u16(), idempotent) {
                    let hint = retry_after(response.headers());
                    tokio::time::sleep(delay(SEND_BASE, attempt, hint)).await;
                    attempt += 1;
                    continue;
                }
                return Ok(response);
            }
            Err(error) => {
                if more && transport_retryable(&error, idempotent) {
                    tokio::time::sleep(delay(SEND_BASE, attempt, None)).await;
                    attempt += 1;
                    continue;
                }
                return Err(error);
            }
        }
    }
}

/// Whether a transport error is safe to retry for a call with this method.
pub fn transport_retryable(error: &reqwest::Error, idempotent: bool) -> bool {
    error.is_connect() || (idempotent && (error.is_timeout() || error.is_request()))
}

/// Whether `method` is idempotent in the HTTP sense.
pub fn idempotent_method(method: &str) -> bool {
    matches!(method, "GET" | "HEAD" | "DELETE" | "OPTIONS")
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn only_transient_statuses_are_retried() {
        for status in [429, 500, 502, 503, 504] {
            assert!(transient_status(status), "{status}");
        }
        for status in [200, 201, 301, 400, 401, 403, 404, 409, 422] {
            assert!(!transient_status(status), "{status}");
        }
    }

    #[test]
    fn retry_after_is_read_in_seconds_and_capped() {
        assert_eq!(retry_after_value(Some("2")), Some(Duration::from_secs(2)));
        assert_eq!(retry_after_value(Some(" 5 ")), Some(Duration::from_secs(5)));
        assert_eq!(
            retry_after_value(Some("3600")),
            Some(MAX_RETRY_AFTER),
            "a huge hint is capped"
        );
        // The HTTP-date form and junk are ignored.
        assert_eq!(
            retry_after_value(Some("Wed, 21 Oct 2026 07:28:00 GMT")),
            None
        );
        assert_eq!(retry_after_value(Some("soon")), None);
        assert_eq!(retry_after_value(None), None);

        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("4"));
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(4)));
    }

    #[test]
    fn backoff_doubles_and_a_longer_hint_wins() {
        let base = Duration::from_millis(500);
        assert_eq!(delay(base, 0, None), Duration::from_millis(500));
        assert_eq!(delay(base, 1, None), Duration::from_secs(1));
        assert_eq!(delay(base, 2, None), Duration::from_secs(2));
        // A hint longer than the backoff is used; a shorter one is not.
        assert_eq!(
            delay(base, 0, Some(Duration::from_secs(7))),
            Duration::from_secs(7)
        );
        assert_eq!(
            delay(base, 2, Some(Duration::from_millis(100))),
            Duration::from_secs(2)
        );
        // Never longer than the cap, whatever the attempt number.
        assert_eq!(delay(base, 40, None), MAX_RETRY_AFTER);
    }

    #[test]
    fn non_idempotent_calls_retry_only_rate_limits() {
        assert!(retryable_status(429, false));
        assert!(!retryable_status(500, false));
        assert!(!retryable_status(503, false));
        assert!(retryable_status(503, true));
        assert!(!retryable_status(404, true));
    }

    #[test]
    fn idempotent_methods_are_the_safe_read_and_delete_set() {
        assert!(idempotent_method("GET"));
        assert!(idempotent_method("DELETE"));
        assert!(!idempotent_method("POST"));
        assert!(!idempotent_method("PATCH"));
    }

    mod send_behaviour {
        use super::super::*;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        #[tokio::test]
        async fn a_transient_get_is_retried_until_it_succeeds() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/items"))
                .respond_with(ResponseTemplate::new(503))
                .up_to_n_times(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/items"))
                .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
                .mount(&server)
                .await;
            let client = reqwest::Client::new();
            let response = send(client.get(format!("{}/items", server.uri())), true)
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
        }

        #[tokio::test]
        async fn a_non_idempotent_post_is_not_repeated_after_a_server_error() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/send"))
                .respond_with(ResponseTemplate::new(503))
                .expect(1)
                .mount(&server)
                .await;
            let client = reqwest::Client::new();
            let response = send(
                client
                    .post(format!("{}/send", server.uri()))
                    .json(&serde_json::json!({"text": "once"})),
                false,
            )
            .await
            .unwrap();
            assert_eq!(response.status(), 503);
        }

        #[tokio::test]
        async fn a_rate_limited_post_is_retried_and_honours_retry_after() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/send"))
                .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
                .up_to_n_times(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/send"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;
            let client = reqwest::Client::new();
            let response = send(
                client
                    .post(format!("{}/send", server.uri()))
                    .json(&serde_json::json!({"text": "later"})),
                false,
            )
            .await
            .unwrap();
            assert_eq!(response.status(), 200);
        }
    }
}
