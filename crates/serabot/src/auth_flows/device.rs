//! The OAuth 2.0 device authorization grant (RFC 8628), generic over the
//! provider: start the flow, then poll the token endpoint until the member
//! approves, denies, or the code expires.
//!
//! Poll rules: `authorization_pending`, and a 400 that carries no error code,
//! keep waiting; `slow_down` adds 5 seconds to the interval, up to 30;
//! `access_denied` and `expired_token` end the flow. The whole wait is bounded
//! at 900 seconds whatever the provider's `expires_in` says.

use std::future::Future;
use std::time::Duration;

use serde_json::Value;

use crate::error::{Result, SeraError};

use super::{PinnedEndpoint, TokenSet, credential_ok, read_capped};

/// Longest a device flow may wait, whatever the provider allows.
pub const MAX_WAIT_SECS: u64 = 900;
/// The interval ceiling `slow_down` can raise the wait to.
pub const MAX_INTERVAL_SECS: u64 = 30;
/// What `slow_down` adds to the interval.
pub const SLOW_DOWN_STEP_SECS: u64 = 5;
/// Consecutive transport or 5xx failures the poller tolerates.
const MAX_TRANSIENT_FAILURES: u32 = 5;

/// A provider's device-flow endpoints and client.
#[derive(Debug, Clone)]
pub struct DeviceSpec {
    pub device: PinnedEndpoint,
    pub token: PinnedEndpoint,
    pub client_id: String,
    pub client_secret: Option<String>,
    /// Space-separated scopes; empty sends none.
    pub scope: String,
    /// Hosts the verification page may be on. Anything else is refused, so a
    /// hostile or broken response cannot send the member to another site.
    pub verification_hosts: Vec<String>,
    /// Headers a provider needs on every device-flow call.
    pub headers: Vec<(&'static str, &'static str)>,
}

/// The provider's answer to "start a device flow".
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// Seconds between polls.
    pub interval: u64,
    /// Seconds the codes stay valid, capped at [`MAX_WAIT_SECS`].
    pub expires_in: u64,
}

impl std::fmt::Debug for DeviceStart {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeviceStart")
            .field("verification_uri", &self.verification_uri)
            .field("interval", &self.interval)
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

/// One poll's result.
#[derive(Debug)]
pub enum DevicePoll {
    /// Not approved yet.
    Pending,
    /// Poll less often.
    SlowDown,
    Denied,
    Expired,
    Done(TokenSet),
    /// The provider answered with an error that ends the flow.
    Failed(&'static str),
}

/// How a poll loop ended.
#[derive(Debug)]
pub enum PollEnd {
    Done(TokenSet),
    Denied,
    Expired,
    Failed(&'static str),
}

impl DeviceSpec {
    fn request(&self, http: &reqwest::Client, url: url::Url) -> reqwest::RequestBuilder {
        let mut request = http.post(url).header("accept", "application/json");
        for (name, value) in &self.headers {
            request = request.header(*name, *value);
        }
        request
    }

    /// Ask the provider for a device code.
    pub async fn start(&self, http: &reqwest::Client) -> Result<DeviceStart> {
        let mut form = vec![("client_id", self.client_id.as_str())];
        if !self.scope.is_empty() {
            form.push(("scope", self.scope.as_str()));
        }
        let response = self
            .request(http, self.device.checked_url()?)
            .form(&form)
            .send()
            .await
            .map_err(|_| SeraError::External("the sign-in provider could not be reached".into()))?;
        let status = response.status();
        let body = read_capped(response).await?;
        if !status.is_success() {
            return Err(SeraError::Rejected {
                code: "signin_unavailable",
                message: "The provider could not start a sign-in. Try again later.".into(),
            });
        }
        let value: Value = serde_json::from_slice(&body).map_err(|_| {
            SeraError::External("the sign-in provider sent an invalid reply".into())
        })?;
        let incomplete =
            || SeraError::External("the sign-in provider sent an incomplete reply".into());
        let code = |field: &str| {
            value
                .get(field)
                .and_then(Value::as_str)
                .filter(|text| credential_ok(text) && text.len() <= 128)
                .map(str::to_owned)
        };
        let device_code = code("device_code").ok_or_else(incomplete)?;
        let user_code = code("user_code").ok_or_else(incomplete)?;
        let page = value
            .get("verification_uri_complete")
            .and_then(Value::as_str)
            .or_else(|| value.get("verification_uri").and_then(Value::as_str))
            .ok_or_else(incomplete)?;
        let verification_uri = self.pin_verification(page)?;
        Ok(DeviceStart {
            device_code,
            user_code,
            verification_uri,
            interval: bounded(value.get("interval"), 5, MAX_INTERVAL_SECS),
            expires_in: bounded(value.get("expires_in"), 300, MAX_WAIT_SECS),
        })
    }

    fn pin_verification(&self, value: &str) -> Result<String> {
        let refuse = || SeraError::External("the sign-in page was refused".into());
        let url = url::Url::parse(value).map_err(|_| refuse())?;
        let host = url.host_str().unwrap_or("");
        if url.scheme() != "https"
            || !self
                .verification_hosts
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(host))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(refuse());
        }
        Ok(url.to_string())
    }

    /// Poll the token endpoint once. An `Err` is a transient failure (the
    /// provider could not be reached, or answered 5xx); the caller decides how
    /// many to tolerate.
    pub async fn poll_once(&self, http: &reqwest::Client, device_code: &str) -> Result<DevicePoll> {
        let mut form = vec![
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", self.client_id.as_str()),
        ];
        if let Some(secret) = self.client_secret.as_deref() {
            form.push(("client_secret", secret));
        }
        let response = self
            .request(http, self.token.checked_url()?)
            .form(&form)
            .send()
            .await
            .map_err(|_| SeraError::External("the sign-in provider could not be reached".into()))?;
        let status = response.status();
        if status.is_server_error() {
            return Err(SeraError::External(
                "the sign-in provider is unavailable".into(),
            ));
        }
        let body = read_capped(response).await?;
        let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        Ok(classify_poll(status.as_u16(), &value, &body))
    }
}

/// Map one token-endpoint answer to a poll result.
fn classify_poll(status: u16, value: &Value, body: &[u8]) -> DevicePoll {
    if (200..300).contains(&status) {
        return match TokenSet::from_body(body) {
            Ok(tokens) => DevicePoll::Done(tokens),
            Err(_) => DevicePoll::Failed("bad_token_reply"),
        };
    }
    match value.get("error").and_then(Value::as_str) {
        Some("authorization_pending") => DevicePoll::Pending,
        Some("slow_down") => DevicePoll::SlowDown,
        Some("access_denied") => DevicePoll::Denied,
        Some("expired_token") => DevicePoll::Expired,
        // A 400 that names nothing is the provider saying "not yet".
        None if status == 400 => DevicePoll::Pending,
        _ if status == 429 => DevicePoll::SlowDown,
        _ if status >= 500 => DevicePoll::Failed("provider_error"),
        _ => DevicePoll::Failed("provider_refused"),
    }
}

fn bounded(value: Option<&Value>, fallback: u64, cap: u64) -> u64 {
    value
        .and_then(Value::as_u64)
        .unwrap_or(fallback)
        .clamp(1, cap)
}

/// Poll until the flow ends. `poll` runs one poll; `sleep` waits. Time is
/// counted from the waits themselves, so the 900-second bound holds without a
/// wall clock (and tests run instantly with a recording `sleep`).
pub async fn run_poller<P, PFut, S, SFut>(
    interval_secs: u64,
    expires_in_secs: u64,
    mut poll: P,
    sleep: S,
) -> PollEnd
where
    P: FnMut() -> PFut,
    PFut: Future<Output = Result<DevicePoll>>,
    S: Fn(Duration) -> SFut,
    SFut: Future<Output = ()>,
{
    let budget = expires_in_secs.min(MAX_WAIT_SECS);
    let mut interval = interval_secs.clamp(1, MAX_INTERVAL_SECS);
    let mut waited = 0u64;
    let mut transient = 0u32;
    loop {
        if waited >= budget {
            return PollEnd::Expired;
        }
        let pause = interval.min(budget - waited);
        sleep(Duration::from_secs(pause)).await;
        waited += pause;
        match poll().await {
            Ok(DevicePoll::Pending) => transient = 0,
            Ok(DevicePoll::SlowDown) => {
                transient = 0;
                interval = (interval + SLOW_DOWN_STEP_SECS).min(MAX_INTERVAL_SECS);
            }
            Ok(DevicePoll::Denied) => return PollEnd::Denied,
            Ok(DevicePoll::Expired) => return PollEnd::Expired,
            Ok(DevicePoll::Done(tokens)) => return PollEnd::Done(tokens),
            Ok(DevicePoll::Failed(reason)) => return PollEnd::Failed(reason),
            Err(_) => {
                transient += 1;
                if transient >= MAX_TRANSIENT_FAILURES {
                    return PollEnd::Failed("provider_unreachable");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    fn tokens() -> TokenSet {
        TokenSet::from_body(br#"{"access_token":"a","refresh_token":"r","expires_in":60}"#).unwrap()
    }

    /// Drive the poller with scripted answers and record every wait.
    async fn drive(
        interval: u64,
        expires: u64,
        script: Vec<Result<DevicePoll>>,
    ) -> (PollEnd, Vec<u64>) {
        let answers = RefCell::new(VecDeque::from(script));
        let waits = RefCell::new(Vec::new());
        let end = run_poller(
            interval,
            expires,
            || {
                let next = answers
                    .borrow_mut()
                    .pop_front()
                    .unwrap_or(Ok(DevicePoll::Pending));
                async move { next }
            },
            |wait| {
                waits.borrow_mut().push(wait.as_secs());
                async {}
            },
        )
        .await;
        (end, waits.into_inner())
    }

    #[tokio::test]
    async fn pending_continues_then_the_token_arrives() {
        let (end, waits) = drive(
            5,
            300,
            vec![
                Ok(DevicePoll::Pending),
                Ok(DevicePoll::Pending),
                Ok(DevicePoll::Done(tokens())),
            ],
        )
        .await;
        assert!(matches!(end, PollEnd::Done(_)));
        assert_eq!(waits, [5, 5, 5]);
    }

    #[tokio::test]
    async fn slow_down_adds_five_seconds_up_to_thirty() {
        let (end, waits) = drive(
            20,
            900,
            vec![
                Ok(DevicePoll::SlowDown),
                Ok(DevicePoll::SlowDown),
                Ok(DevicePoll::SlowDown),
                Ok(DevicePoll::Done(tokens())),
            ],
        )
        .await;
        assert!(matches!(end, PollEnd::Done(_)));
        // 20, then 25, then 30, and it stays at 30.
        assert_eq!(waits, [20, 25, 30, 30]);
    }

    #[tokio::test]
    async fn denial_and_expiry_are_terminal() {
        let (end, _) = drive(5, 300, vec![Ok(DevicePoll::Denied)]).await;
        assert!(matches!(end, PollEnd::Denied));
        let (end, _) = drive(5, 300, vec![Ok(DevicePoll::Expired)]).await;
        assert!(matches!(end, PollEnd::Expired));
        let (end, _) = drive(5, 300, vec![Ok(DevicePoll::Failed("provider_refused"))]).await;
        assert!(matches!(end, PollEnd::Failed("provider_refused")));
    }

    #[tokio::test]
    async fn the_wait_is_bounded_at_900_seconds() {
        // The provider claims two hours; the poller stops at 900 seconds.
        let (end, waits) = drive(30, 7_200, vec![]).await;
        assert!(matches!(end, PollEnd::Expired));
        assert_eq!(waits.iter().sum::<u64>(), 900);
        // A short code expires on its own clock, with a clipped last wait.
        let (end, waits) = drive(5, 12, vec![]).await;
        assert!(matches!(end, PollEnd::Expired));
        assert_eq!(waits, [5, 5, 2]);
    }

    #[tokio::test]
    async fn transient_failures_are_tolerated_a_few_times() {
        let flaky = || Err(SeraError::External("down".into()));
        let (end, _) = drive(
            1,
            300,
            vec![flaky(), flaky(), Ok(DevicePoll::Done(tokens()))],
        )
        .await;
        assert!(matches!(end, PollEnd::Done(_)));
        let (end, _) = drive(1, 300, (0..5).map(|_| flaky()).collect()).await;
        assert!(matches!(end, PollEnd::Failed("provider_unreachable")));
    }

    #[test]
    fn token_endpoint_answers_map_to_poll_states() {
        let answer = |status: u16, body: &str| {
            let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            classify_poll(status, &value, body.as_bytes())
        };
        assert!(matches!(
            answer(400, r#"{"error":"authorization_pending"}"#),
            DevicePoll::Pending
        ));
        assert!(matches!(
            answer(400, r#"{"error":"slow_down"}"#),
            DevicePoll::SlowDown
        ));
        assert!(matches!(
            answer(400, r#"{"error":"access_denied"}"#),
            DevicePoll::Denied
        ));
        assert!(matches!(
            answer(400, r#"{"error":"expired_token"}"#),
            DevicePoll::Expired
        ));
        // A 400 with no code, or with a non-JSON body, keeps waiting.
        assert!(matches!(answer(400, "{}"), DevicePoll::Pending));
        assert!(matches!(answer(400, "oops"), DevicePoll::Pending));
        assert!(matches!(answer(429, "{}"), DevicePoll::SlowDown));
        assert!(matches!(
            answer(400, r#"{"error":"invalid_client"}"#),
            DevicePoll::Failed("provider_refused")
        ));
        assert!(matches!(
            answer(503, "{}"),
            DevicePoll::Failed("provider_error")
        ));
        assert!(matches!(
            answer(200, r#"{"access_token":"a"}"#),
            DevicePoll::Done(_)
        ));
        assert!(matches!(
            answer(200, r#"{"nope":1}"#),
            DevicePoll::Failed("bad_token_reply")
        ));
    }
}
