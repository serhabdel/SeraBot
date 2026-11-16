//! Quota readers for providers that report an account's remaining allowance:
//! Meta Muse Code (`POST /muse-code/key`), Google Antigravity
//! (`retrieveUserQuotaSummary`, with `fetchAvailableModels` as the fallback)
//! and OpenCode Go (`GET /zen/go/v1/usage`).
//!
//! A reading is a small JSON snapshot kept in the account's `quota_json`:
//!
//! ```json
//! {"sampled_at": "…", "source": "muse|summary|models", "state": "ok",
//!  "windows": [{"scope": "…", "label": "5h", "used_percent": 37.5,
//!               "remaining": 0.625, "resets_at": "…"}]}
//! ```
//!
//! `remaining` is a fraction from 0 to 1. A Muse subscription that is inactive
//! or needs payment is a reading too (`state` is `inactive` or
//! `payment_required`, with no windows): the member should see why. Every other
//! failure is "no reading", never an error.

use super::*;

/// Where Muse reports the subscription's usage.
const MUSE_QUOTA_URL: &str = "https://api.meta.ai/muse-code/key";
/// Where OpenCode Go reports the subscription's usage meters.
const OPENCODE_GO_USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";

/// The readers every server has, by provider kind.
pub(super) fn standard_readers() -> HashMap<String, Arc<dyn QuotaReader>> {
    let mut readers: HashMap<String, Arc<dyn QuotaReader>> = HashMap::new();
    if let Ok(http) = crate::auth_flows::http_client() {
        readers.insert(
            "muse-code".into(),
            Arc::new(MuseQuota {
                http: http.clone(),
                url: MUSE_QUOTA_URL.into(),
            }),
        );
        readers.insert(
            "opencode-go".into(),
            Arc::new(OpenCodeGoQuota {
                http: http.clone(),
                url: OPENCODE_GO_USAGE_URL.into(),
            }),
        );
        if let Ok(origins) =
            crate::cloud_code::discovery_origins("https://daily-cloudcode-pa.googleapis.com")
        {
            readers.insert(
                "google-antigravity".into(),
                Arc::new(AntigravityQuota { http, origins }),
            );
        }
    }
    readers
}

fn sampled_at() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Meta Muse Code: the Meta identity token (kept as the credential's refresh
/// token) reads the subscription's windows.
pub(super) struct MuseQuota {
    http: reqwest::Client,
    url: String,
}

#[async_trait::async_trait]
impl QuotaReader for MuseQuota {
    async fn read(
        &self,
        credential: &ProviderCredential,
        _identity: &Value,
    ) -> Result<Option<Value>> {
        let token = credential.refresh_token.trim();
        // No identity token (or the legacy value that was the API key itself):
        // the member signs in again to see live quota.
        if token.is_empty() || token.starts_with("LLM|") {
            return Ok(None);
        }
        let response = self
            .http
            .post(&self.url)
            .header("accept", "application/json")
            .header("x-api-version", crate::providers::MUSE_API_VERSION)
            .timeout(Duration::from_secs(20))
            .bearer_auth(token)
            // Quota never sends `onboard`.
            .json(&json!({}))
            .send()
            .await
            .map_err(|_| SeraError::External("quota could not be read".into()))?;
        let status = response.status();
        let body = crate::auth_flows::read_capped(response).await?;
        if !status.is_success() {
            return Ok(None);
        }
        Ok(serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|value| parse_muse_quota(&value)))
    }
}

/// A Muse usage reply as a snapshot, or `None` when it has nothing to show.
pub(super) fn parse_muse_quota(value: &Value) -> Option<Value> {
    value.as_object()?;
    let snapshot = |state: &str, windows: Vec<Value>| {
        json!({
            "sampled_at": sampled_at(),
            "source": "muse",
            "state": state,
            "windows": windows,
        })
    };
    if value.get("is_subs_active").and_then(Value::as_bool) == Some(false) {
        return Some(snapshot("inactive", Vec::new()));
    }
    if value.get("require_payment").and_then(Value::as_bool) == Some(true) {
        return Some(snapshot("payment_required", Vec::new()));
    }
    let usage = value.get("subs_usage")?;
    let mut windows = Vec::new();
    if let Some(window) = usage.get("window").and_then(|entry| {
        muse_window(
            entry,
            &duration_label(entry.get("window_duration_mins").and_then(number)),
        )
    }) {
        windows.push(window);
    }
    if let Some(weekly) = usage
        .get("weekly")
        .and_then(|entry| muse_window(entry, "week"))
    {
        windows.push(weekly);
    }
    (!windows.is_empty()).then(|| snapshot("ok", windows))
}

/// A JSON number, or a string that holds one.
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .filter(|number| number.is_finite())
}

/// `5h`, `1.5h`, `45m`; `window` when the length is unknown.
fn duration_label(minutes: Option<f64>) -> String {
    match minutes {
        Some(minutes) if minutes >= 60.0 => {
            let hours = minutes / 60.0;
            if (hours - hours.round()).abs() < 1e-9 {
                format!("{}h", hours.round() as u64)
            } else {
                format!("{hours:.1}h")
            }
        }
        Some(minutes) if minutes > 0.0 => format!("{}m", minutes.round() as u64),
        _ => "window".into(),
    }
}

/// A reset time given as unix seconds, unix milliseconds (from 1e12 up), a
/// numeric string of either, or ISO-8601.
fn reset_time(value: &Value) -> Option<String> {
    let from_epoch = |raw: f64| {
        let seconds = if raw >= 1e12 { raw / 1000.0 } else { raw };
        DateTime::<Utc>::from_timestamp(seconds as i64, 0)
    };
    let time = match value {
        Value::Number(_) => from_epoch(number(value)?),
        Value::String(text) => match text.trim().parse::<f64>() {
            Ok(raw) if raw.is_finite() => from_epoch(raw),
            _ => DateTime::parse_from_rfc3339(text.trim())
                .ok()
                .map(|time| time.with_timezone(&Utc)),
        },
        _ => None,
    }?;
    Some(time.to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn muse_window(entry: &Value, label: &str) -> Option<Value> {
    let used = number(entry.get("used_percent")?)?.clamp(0.0, 100.0);
    Some(json!({
        "scope": "subscription",
        "label": label,
        "used_percent": used,
        "remaining": ((100.0 - used) / 100.0 * 1000.0).round() / 1000.0,
        "resets_at": entry.get("resets_at").and_then(reset_time),
    }))
}

/// OpenCode Go: the account's API key reads the rolling, weekly and monthly
/// usage meters. The request carries a session id like every Go request.
pub(super) struct OpenCodeGoQuota {
    http: reqwest::Client,
    url: String,
}

#[async_trait::async_trait]
impl QuotaReader for OpenCodeGoQuota {
    async fn read(
        &self,
        credential: &ProviderCredential,
        _identity: &Value,
    ) -> Result<Option<Value>> {
        let key = credential.api_key.trim();
        if key.is_empty() {
            return Ok(None);
        }
        let session = {
            use rand::RngCore;
            let mut seed = [0u8; 16];
            rand::rngs::OsRng.fill_bytes(&mut seed);
            crate::llm::opencode_session_id(&hex::encode(seed))
        };
        let response = self
            .http
            .get(&self.url)
            .header("accept", "application/json")
            .header("x-opencode-session", session)
            .timeout(Duration::from_secs(20))
            .bearer_auth(key)
            .send()
            .await
            .map_err(|_| SeraError::External("quota could not be read".into()))?;
        let status = response.status();
        let body = crate::auth_flows::read_capped(response).await?;
        if !status.is_success() {
            return Ok(None);
        }
        Ok(serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|value| parse_opencode_go_usage(&value)))
    }
}

/// An OpenCode Go usage reply as a snapshot: `usage.rolling` (5h),
/// `usage.weekly` and `usage.monthly`, each `{status, percent, resetsAt}`. A
/// meter whose status is neither absent nor `ok` is skipped. `None` when no
/// meter is readable.
pub(super) fn parse_opencode_go_usage(value: &Value) -> Option<Value> {
    let usage = value.get("usage")?;
    let mut windows = Vec::new();
    for (key, label) in [("rolling", "5h"), ("weekly", "week"), ("monthly", "month")] {
        let Some(entry) = usage.get(key) else {
            continue;
        };
        if entry
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status != "ok")
        {
            continue;
        }
        let Some(used) = entry.get("percent").and_then(number) else {
            continue;
        };
        let used = used.clamp(0.0, 100.0);
        windows.push(json!({
            "scope": "subscription",
            "label": label,
            "used_percent": used,
            "remaining": ((100.0 - used) / 100.0 * 1000.0).round() / 1000.0,
            "resets_at": entry.get("resetsAt").and_then(reset_time),
        }));
    }
    (!windows.is_empty()).then(|| {
        json!({
            "sampled_at": sampled_at(),
            "source": "opencode-go",
            "state": "ok",
            "windows": windows,
        })
    })
}

/// Google Antigravity: the account's Google access token asks Cloud Code.
pub(super) struct AntigravityQuota {
    http: reqwest::Client,
    origins: Vec<String>,
}

#[async_trait::async_trait]
impl QuotaReader for AntigravityQuota {
    async fn read(
        &self,
        credential: &ProviderCredential,
        identity: &Value,
    ) -> Result<Option<Value>> {
        let project = identity
            .get("project_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        Ok(crate::cloud_code::read_quota(
            &self.http,
            &credential.api_key,
            project,
            &self.origins,
            None,
        )
        .await)
    }
}

#[cfg(test)]
impl MuseQuota {
    pub(super) fn for_test(url: &str) -> Self {
        Self {
            http: crate::auth_flows::http_client().unwrap(),
            url: url.to_owned(),
        }
    }
}

#[cfg(test)]
impl OpenCodeGoQuota {
    pub(super) fn for_test(url: &str) -> Self {
        Self {
            http: crate::auth_flows::http_client().unwrap(),
            url: url.to_owned(),
        }
    }
}

#[cfg(test)]
impl AntigravityQuota {
    pub(super) fn for_test(origin: &str) -> Self {
        Self {
            http: crate::auth_flows::http_client().unwrap(),
            origins: vec![origin.to_owned()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn credential(refresh: &str) -> ProviderCredential {
        ProviderCredential {
            api_key: "LLM|key".into(),
            refresh_token: refresh.into(),
            ..Default::default()
        }
    }

    fn windows(snapshot: &Value) -> &Vec<Value> {
        snapshot["windows"].as_array().unwrap()
    }

    #[test]
    fn unix_seconds_and_a_five_hour_window() {
        let snapshot = parse_muse_quota(&json!({
            "is_subs_active": true,
            "require_payment": false,
            "subs_usage": {
                "window": {"used_percent": 37.5, "window_duration_mins": 300, "resets_at": 1_770_000_000},
                "weekly": {"used_percent": 62.0, "resets_at": 1_770_500_000}
            }
        }))
        .unwrap();
        assert_eq!(snapshot["state"], "ok");
        assert_eq!(snapshot["source"], "muse");
        let windows = windows(&snapshot);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0]["label"], "5h");
        assert_eq!(windows[0]["used_percent"], 37.5);
        assert_eq!(windows[0]["remaining"], 0.625);
        assert_eq!(windows[0]["resets_at"], "2026-02-02T02:40:00Z");
        assert_eq!(windows[1]["label"], "week");
        assert_eq!(windows[1]["used_percent"], 62.0);
        assert_eq!(windows[1]["resets_at"], "2026-02-07T21:33:20Z");
    }

    #[test]
    fn iso_resets_string_percentages_and_odd_durations() {
        let snapshot = parse_muse_quota(&json!({
            "is_subs_active": true,
            "subs_usage": {
                "window": {"used_percent": "85.2", "window_duration_mins": 90, "resets_at": "2026-10-24T18:30:00Z"},
                "weekly": {"used_percent": 15.0, "resets_at": "2026-10-31T00:00:00Z"}
            }
        }))
        .unwrap();
        let windows = windows(&snapshot);
        assert_eq!(windows[0]["label"], "1.5h");
        assert_eq!(windows[0]["used_percent"], 85.2);
        assert_eq!(windows[0]["resets_at"], "2026-10-24T18:30:00Z");
        assert_eq!(windows[1]["resets_at"], "2026-10-31T00:00:00Z");

        // Milliseconds are told from seconds by size; both are the same instant.
        let seconds = reset_time(&json!(1_770_000_000)).unwrap();
        assert_eq!(
            reset_time(&json!(1_770_000_000_000_i64)),
            Some(seconds.clone())
        );
        assert_eq!(reset_time(&json!("1770000000")), Some(seconds));
        assert_eq!(reset_time(&json!("not a time")), None);
        assert_eq!(reset_time(&json!(null)), None);
        assert_eq!(duration_label(Some(45.0)), "45m");
        assert_eq!(duration_label(Some(60.0)), "1h");
        assert_eq!(duration_label(None), "window");
        // Percentages are clamped.
        let clamped = parse_muse_quota(&json!({
            "subs_usage": {"window": {"used_percent": 250}, "weekly": {"used_percent": -4}}
        }))
        .unwrap();
        assert_eq!(windows_of(&clamped)[0]["used_percent"], 100.0);
        assert_eq!(windows_of(&clamped)[0]["label"], "window");
        assert_eq!(windows_of(&clamped)[1]["used_percent"], 0.0);
    }

    fn windows_of(snapshot: &Value) -> &Vec<Value> {
        windows(snapshot)
    }

    #[test]
    fn an_inactive_or_unpaid_subscription_is_its_own_reading() {
        let inactive = parse_muse_quota(&json!({
            "is_subs_active": false,
            "subs_usage": {"window": {"used_percent": 10.0, "resets_at": 1_770_000_000}}
        }))
        .unwrap();
        assert_eq!(inactive["state"], "inactive");
        assert!(windows(&inactive).is_empty());
        let unpaid = parse_muse_quota(&json!({
            "is_subs_active": true,
            "require_payment": true,
            "subs_usage": {"window": {"used_percent": 25.0}}
        }))
        .unwrap();
        assert_eq!(unpaid["state"], "payment_required");
        assert!(windows(&unpaid).is_empty());
    }

    #[test]
    fn corrupt_or_empty_usage_is_no_reading() {
        for body in [
            json!(null),
            json!([]),
            json!({}),
            json!({"subs_usage": null}),
            json!({"subs_usage": {}}),
            json!({"subs_usage": {"window": {"resets_at": 5}}}),
        ] {
            assert!(parse_muse_quota(&body).is_none(), "{body}");
        }
    }

    #[tokio::test]
    async fn muse_quota_posts_an_empty_body_with_the_meta_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/muse-code/key"))
            .and(header("authorization", "Bearer meta-identity-token"))
            .and(header("x-api-version", "1.0.0"))
            // Quota never onboards.
            .and(body_json(json!({})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "is_subs_active": true,
                "subs_usage": {"weekly": {"used_percent": 20, "resets_at": 1_770_500_000}}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let reader = MuseQuota::for_test(&format!("{}/muse-code/key", server.uri()));
        let reading = reader
            .read(&credential("meta-identity-token"), &json!({}))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reading["windows"][0]["label"], "week");
        // No Meta token, or the legacy key stored as one: nothing is asked.
        for token in ["", "LLM|legacy-key"] {
            assert!(
                reader
                    .read(&credential(token), &json!({}))
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        // A refused call is "no reading", not an error.
        let refused = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&refused)
            .await;
        let reader = MuseQuota::for_test(&format!("{}/muse-code/key", refused.uri()));
        assert!(
            reader
                .read(&credential("expired"), &json!({}))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn opencode_go_usage_reads_the_three_meters() {
        let snapshot = parse_opencode_go_usage(&json!({"usage": {
            "rolling": {"status": "ok", "percent": 12.5, "resetsAt": "2026-11-16T15:00:00Z"},
            "weekly": {"percent": "40", "resetsAt": "2026-11-20T00:00:00Z"},
            "monthly": {"status": "rate_limited", "percent": 99, "resetsAt": "2026-12-01T00:00:00Z"}
        }}))
        .unwrap();
        assert_eq!(snapshot["source"], "opencode-go");
        let windows = windows(&snapshot);
        // The monthly meter is not "ok", so it is skipped.
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0]["label"], "5h");
        assert_eq!(windows[0]["remaining"], 0.875);
        assert_eq!(windows[0]["resets_at"], "2026-11-16T15:00:00Z");
        assert_eq!(windows[1]["label"], "week");
        assert_eq!(windows[1]["used_percent"], 40.0);
        for bad in [
            json!({}),
            json!({"usage": {}}),
            json!({"usage": {"rolling": {"status": "ok"}}}),
        ] {
            assert!(parse_opencode_go_usage(&bad).is_none(), "{bad}");
        }
    }

    #[tokio::test]
    async fn opencode_go_quota_sends_the_key_and_a_session_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/zen/go/v1/usage"))
            .and(header("authorization", "Bearer sk-go-synthetic"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"usage": {
                "rolling": {"percent": 10, "resetsAt": "2026-11-16T15:00:00Z"}
            }})))
            .expect(1)
            .mount(&server)
            .await;
        let reader = OpenCodeGoQuota::for_test(&format!("{}/zen/go/v1/usage", server.uri()));
        let credential = ProviderCredential {
            api_key: "sk-go-synthetic".into(),
            ..Default::default()
        };
        let reading = reader.read(&credential, &json!({})).await.unwrap().unwrap();
        assert_eq!(reading["windows"][0]["label"], "5h");
        let requests = server.received_requests().await.unwrap();
        let session = requests[0].headers.get("x-opencode-session").unwrap();
        assert!(session.to_str().unwrap().starts_with("ses_"));
        assert_eq!(session.to_str().unwrap().len(), 4 + 12 + 14);
        // No key, no request; a refusal is "no reading".
        assert!(
            reader
                .read(&ProviderCredential::default(), &json!({}))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        let refused = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&refused)
            .await;
        let reader = OpenCodeGoQuota::for_test(&format!("{}/usage", refused.uri()));
        assert!(
            reader
                .read(&credential, &json!({}))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn antigravity_quota_reads_the_summary_for_the_accounts_project() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:retrieveUserQuotaSummary"))
            .and(header("authorization", "Bearer LLM|key"))
            .and(body_json(json!({"project": "proj-synthetic"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"groups": [
                {"displayName": "Claude and GPT", "buckets": [
                    {"remainingFraction": 0.0, "window": "5h", "resetTime": "2026-11-16T10:00:00Z"}
                ]}
            ]})))
            .mount(&server)
            .await;
        let reader = AntigravityQuota::for_test(&server.uri());
        let reading = reader
            .read(&credential(""), &json!({"project_id": "proj-synthetic"}))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reading["source"], "summary");
        assert_eq!(reading["windows"][0]["remaining"], 0.0);
        assert_eq!(reading["windows"][0]["resets_at"], "2026-11-16T10:00:00Z");
    }

    #[test]
    fn the_standard_readers_cover_muse_and_antigravity() {
        let readers = standard_readers();
        assert!(readers.contains_key("muse-code"));
        assert!(readers.contains_key("google-antigravity"));
        assert!(readers.contains_key("opencode-go"));
        assert!(!readers.contains_key("opencode-zen"));
        assert!(!readers.contains_key("openai"));
    }
}
