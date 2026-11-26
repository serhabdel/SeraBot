//! AWS Kiro (Amazon Q / CodeWhisperer) member sign-in: AWS SSO OIDC device
//! authorization grant with dynamic client registration.
//!
//! Flow:
//! 1. `POST https://oidc.<region>.amazonaws.com/client/register` registers a
//!    public client (`clientId`, `clientSecret`).
//! 2. `POST https://oidc.<region>.amazonaws.com/device_authorization` starts
//!    the device flow with `startUrl: "https://view.awsapps.com/start"`.
//! 3. `POST https://oidc.<region>.amazonaws.com/token` is polled per RFC 8628
//!    until approved, denied, or expired.
//! 4. `finish_device` stores the `access_token` in `ProviderCredential.api_key`
//!    and packs `{refresh_token, client_id, client_secret, region}` as JSON in
//!    `ProviderCredential.refresh_token` so [`refresh_kiro_token`] can refresh
//!    against the pinned regional OIDC endpoint.
//!
//! Security:
//! - Regions are strictly allowlisted by [`KiroRegion`]; raw region strings are
//!   never interpolated into URLs (preventing SSRF).
//! - Verification URIs are pinned to `view.awsapps.com`, `*.awsapps.com`, or
//!   `signin.aws.amazon.com` over HTTPS.
//! - Secrets and device codes never appear in errors or `Debug` output.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::accounts::{AuthKind, RefreshError, RefreshGrant, identity_fingerprint};
use crate::error::{Result, SeraError};
use crate::user_secrets::ProviderCredential;

use super::device::{DevicePoll, DeviceStart, MAX_INTERVAL_SECS, MAX_WAIT_SECS};
use super::{
    FlowKind, PinnedEndpoint, SignInProvider, SignedIn, TokenSet, credential_ok, read_capped,
};

const CLIENT_NAME: &str = "SeraBot Kiro";
const CLIENT_TYPE: &str = "public";
const START_URL: &str = "https://view.awsapps.com/start";
const SCOPES: &[&str] = &[
    "codewhisperer:completions",
    "codewhisperer:analysis",
    "codewhisperer:conversations",
];
const GRANT_TYPES: &[&str] = &[
    "urn:ietf:params:oauth:grant-type:device_code",
    "refresh_token",
];
const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
const MAX_PENDING_CLIENTS: usize = 64;

/// Strictly allowlisted AWS regions for Kiro OIDC and Q endpoints.
///
/// Hosts are returned as compile-time static strings so raw user input is
/// never interpolated into URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum KiroRegion {
    #[default]
    #[serde(rename = "us-east-1")]
    UsEast1,
    #[serde(rename = "us-west-2")]
    UsWest2,
    #[serde(rename = "eu-central-1")]
    EuCentral1,
    #[serde(rename = "eu-west-1")]
    EuWest1,
    #[serde(rename = "ap-southeast-1")]
    ApSoutheast1,
    #[serde(rename = "ap-northeast-1")]
    ApNortheast1,
}

impl KiroRegion {
    /// Parse a region string against the strict allowlist.
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim() {
            "us-east-1" => Ok(Self::UsEast1),
            "us-west-2" => Ok(Self::UsWest2),
            "eu-central-1" => Ok(Self::EuCentral1),
            "eu-west-1" => Ok(Self::EuWest1),
            "ap-southeast-1" => Ok(Self::ApSoutheast1),
            "ap-northeast-1" => Ok(Self::ApNortheast1),
            _ => Err(SeraError::Validation("unsupported AWS Kiro region".into())),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UsEast1 => "us-east-1",
            Self::UsWest2 => "us-west-2",
            Self::EuCentral1 => "eu-central-1",
            Self::EuWest1 => "eu-west-1",
            Self::ApSoutheast1 => "ap-southeast-1",
            Self::ApNortheast1 => "ap-northeast-1",
        }
    }

    /// Pinned static OIDC host for this region.
    pub const fn oidc_host(self) -> &'static str {
        match self {
            Self::UsEast1 => "oidc.us-east-1.amazonaws.com",
            Self::UsWest2 => "oidc.us-west-2.amazonaws.com",
            Self::EuCentral1 => "oidc.eu-central-1.amazonaws.com",
            Self::EuWest1 => "oidc.eu-west-1.amazonaws.com",
            Self::ApSoutheast1 => "oidc.ap-southeast-1.amazonaws.com",
            Self::ApNortheast1 => "oidc.ap-northeast-1.amazonaws.com",
        }
    }

    /// Pinned static Amazon Q / Kiro inference host for this region.
    pub const fn q_host(self) -> &'static str {
        match self {
            Self::UsEast1 => "q.us-east-1.amazonaws.com",
            Self::UsWest2 => "q.us-west-2.amazonaws.com",
            Self::EuCentral1 => "q.eu-central-1.amazonaws.com",
            Self::EuWest1 => "q.eu-west-1.amazonaws.com",
            Self::ApSoutheast1 => "q.ap-southeast-1.amazonaws.com",
            Self::ApNortheast1 => "q.ap-northeast-1.amazonaws.com",
        }
    }
}

/// Refresh material packed as JSON inside `ProviderCredential.refresh_token`.
#[derive(Clone, Serialize, Deserialize)]
pub struct KiroRefreshMaterial {
    pub refresh_token: String,
    pub client_id: String,
    pub client_secret: String,
    pub region: KiroRegion,
}

impl std::fmt::Debug for KiroRefreshMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KiroRefreshMaterial")
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct RegisteredClient {
    client_id: String,
    client_secret: String,
}

/// AWS Kiro sign-in provider (`FlowKind::Device`).
pub struct KiroSignIn {
    region: KiroRegion,
    register: PinnedEndpoint,
    device_auth: PinnedEndpoint,
    token: PinnedEndpoint,
    /// Maps `device_code` -> registered client for `device_poll`.
    clients_by_device: Mutex<HashMap<String, RegisteredClient>>,
    /// Maps `access_token` -> registered client for `finish_device`.
    clients_by_token: Mutex<HashMap<String, RegisteredClient>>,
}

impl KiroSignIn {
    pub fn new() -> Self {
        Self::with_region(KiroRegion::UsEast1)
    }

    pub fn with_region(region: KiroRegion) -> Self {
        let host = region.oidc_host();
        Self {
            region,
            register: PinnedEndpoint::https(host, "/client/register"),
            device_auth: PinnedEndpoint::https(host, "/device_authorization"),
            token: PinnedEndpoint::https(host, "/token"),
            clients_by_device: Mutex::new(HashMap::new()),
            clients_by_token: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(base_url: &str) -> Self {
        Self::for_test_with_region(base_url, KiroRegion::UsEast1)
    }

    #[cfg(test)]
    pub(crate) fn for_test_with_region(base_url: &str, region: KiroRegion) -> Self {
        Self {
            region,
            register: PinnedEndpoint::loopback(&format!("{base_url}/client/register")),
            device_auth: PinnedEndpoint::loopback(&format!("{base_url}/device_authorization")),
            token: PinnedEndpoint::loopback(&format!("{base_url}/token")),
            clients_by_device: Mutex::new(HashMap::new()),
            clients_by_token: Mutex::new(HashMap::new()),
        }
    }

    fn store_device_client(&self, device_code: &str, client: RegisteredClient) {
        let mut map = self
            .clients_by_device
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if map.len() >= MAX_PENDING_CLIENTS {
            map.clear();
        }
        map.insert(device_code.to_owned(), client);
    }

    fn get_device_client(&self, device_code: &str) -> Option<RegisteredClient> {
        self.clients_by_device
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(device_code)
            .cloned()
    }

    fn promote_to_token(&self, device_code: &str, access_token: &str) {
        let client = self
            .clients_by_device
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(device_code);
        if let Some(client) = client {
            let mut by_token = self
                .clients_by_token
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if by_token.len() >= MAX_PENDING_CLIENTS {
                by_token.clear();
            }
            by_token.insert(access_token.to_owned(), client);
        }
    }

    fn take_token_client(&self, access_token: &str) -> Option<RegisteredClient> {
        self.clients_by_token
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(access_token)
    }

    async fn register_client(&self, http: &reqwest::Client) -> Result<RegisteredClient> {
        let url = self.register.checked_url()?;
        let response = http
            .post(url)
            .header("accept", "application/json")
            .json(&json!({
                "clientName": CLIENT_NAME,
                "clientType": CLIENT_TYPE,
                "scopes": SCOPES,
                "grantTypes": GRANT_TYPES,
            }))
            .send()
            .await
            .map_err(|_| SeraError::External("AWS Kiro OIDC could not be reached".into()))?;
        let status = response.status();
        let body = read_capped(response).await?;
        if !status.is_success() {
            return Err(SeraError::Rejected {
                code: "signin_unavailable",
                message: "AWS Kiro could not register a sign-in client. Try again later.".into(),
            });
        }
        let value: Value = serde_json::from_slice(&body)
            .map_err(|_| SeraError::External("AWS Kiro sent an invalid register reply".into()))?;
        let client_id = json_text(&value, &["clientId", "client_id"])
            .filter(|v| credential_ok(v))
            .ok_or_else(|| SeraError::External("AWS Kiro did not return a clientId".into()))?;
        let client_secret = json_text(&value, &["clientSecret", "client_secret"])
            .filter(|v| credential_ok(v))
            .ok_or_else(|| SeraError::External("AWS Kiro did not return a clientSecret".into()))?;
        Ok(RegisteredClient {
            client_id,
            client_secret,
        })
    }
}

impl Default for KiroSignIn {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SignInProvider for KiroSignIn {
    fn flows(&self) -> &'static [FlowKind] {
        &[FlowKind::Device]
    }

    async fn device_start(&self, http: &reqwest::Client) -> Result<DeviceStart> {
        let client = self.register_client(http).await?;
        let url = self.device_auth.checked_url()?;
        let response = http
            .post(url)
            .header("accept", "application/json")
            .json(&json!({
                "clientId": client.client_id,
                "clientSecret": client.client_secret,
                "startUrl": START_URL,
            }))
            .send()
            .await
            .map_err(|_| SeraError::External("AWS Kiro OIDC could not be reached".into()))?;
        let status = response.status();
        let body = read_capped(response).await?;
        if !status.is_success() {
            return Err(SeraError::Rejected {
                code: "signin_unavailable",
                message: "AWS Kiro could not start a sign-in. Try again later.".into(),
            });
        }
        let value: Value = serde_json::from_slice(&body).map_err(|_| {
            SeraError::External("AWS Kiro sent an invalid device authorization reply".into())
        })?;
        let incomplete =
            || SeraError::External("AWS Kiro sent an incomplete device authorization reply".into());
        let device_code = json_text(&value, &["deviceCode", "device_code"])
            .filter(|v| credential_ok(v) && v.len() <= 256)
            .ok_or_else(incomplete)?;
        let user_code = json_text(&value, &["userCode", "user_code"])
            .filter(|v| credential_ok(v) && v.len() <= 128)
            .ok_or_else(incomplete)?;
        let raw_uri = json_text(
            &value,
            &[
                "verificationUriComplete",
                "verification_uri_complete",
                "verificationUri",
                "verification_uri",
            ],
        )
        .ok_or_else(incomplete)?;
        let verification_uri = pin_verification_uri(&raw_uri)?;
        let interval = json_u64(&value, &["interval"])
            .unwrap_or(5)
            .clamp(1, MAX_INTERVAL_SECS);
        let expires_in = json_u64(&value, &["expiresIn", "expires_in"])
            .unwrap_or(600)
            .clamp(1, MAX_WAIT_SECS);

        self.store_device_client(&device_code, client);
        Ok(DeviceStart {
            device_code,
            user_code,
            verification_uri,
            interval,
            expires_in,
        })
    }

    async fn device_poll(&self, http: &reqwest::Client, device_code: &str) -> Result<DevicePoll> {
        let Some(client) = self.get_device_client(device_code) else {
            return Ok(DevicePoll::Failed("missing_client_registration"));
        };
        let url = self.token.checked_url()?;
        let response = http
            .post(url)
            .header("accept", "application/json")
            .json(&json!({
                "clientId": client.client_id,
                "clientSecret": client.client_secret,
                "deviceCode": device_code,
                "grantType": DEVICE_GRANT_TYPE,
            }))
            .send()
            .await
            .map_err(|_| SeraError::External("AWS Kiro OIDC could not be reached".into()))?;
        let status = response.status();
        if status.is_server_error() {
            return Err(SeraError::External(
                "AWS Kiro OIDC is temporarily unavailable".into(),
            ));
        }
        let body = read_capped(response).await?;
        let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if status.is_success() {
            return match parse_token_set(&value) {
                Ok(tokens) => {
                    self.promote_to_token(device_code, &tokens.access_token);
                    Ok(DevicePoll::Done(tokens))
                }
                Err(_) => Ok(DevicePoll::Failed("bad_token_reply")),
            };
        }
        Ok(classify_poll_error(status.as_u16(), &value))
    }

    async fn finish_device(&self, _http: &reqwest::Client, tokens: TokenSet) -> Result<SignedIn> {
        let client = self
            .take_token_client(&tokens.access_token)
            .ok_or_else(|| {
                SeraError::External("AWS Kiro sign-in lost its client registration".into())
            })?;
        let raw_refresh = tokens
            .refresh_token
            .as_deref()
            .ok_or_else(|| SeraError::External("AWS Kiro did not return a refresh token".into()))?;
        let material = KiroRefreshMaterial {
            refresh_token: raw_refresh.to_owned(),
            client_id: client.client_id.clone(),
            client_secret: client.client_secret,
            region: self.region,
        };
        let packed_refresh = serde_json::to_string(&material)?;
        let fingerprint = identity_fingerprint(
            "aws-kiro",
            &format!("{}:{}", self.region.as_str(), client.client_id),
        );
        let expires_at = tokens.expires_at();
        Ok(SignedIn {
            label: "AWS Kiro".into(),
            auth_kind: AuthKind::Device,
            fingerprint: Some(fingerprint),
            identity: json!({
                "region": self.region.as_str(),
            }),
            credential: ProviderCredential {
                api_key: tokens.access_token,
                refresh_token: packed_refresh,
                expires_at,
                generation: None,
            },
        })
    }
}

/// Refresh an AWS Kiro access token using the packed [`KiroRefreshMaterial`]
/// JSON stored in `ProviderCredential.refresh_token`.
pub async fn refresh_kiro_token(
    http: &reqwest::Client,
    refresh_material: &str,
) -> std::result::Result<RefreshGrant, RefreshError> {
    let material: KiroRefreshMaterial = serde_json::from_str(refresh_material).map_err(|_| {
        RefreshError::Failed(SeraError::Config(
            "stored AWS Kiro refresh material is invalid".into(),
        ))
    })?;
    let endpoint = PinnedEndpoint::https(material.region.oidc_host(), "/token");
    let url = endpoint.checked_url().map_err(RefreshError::Failed)?;
    refresh_kiro_token_at(http, url, &material).await
}

async fn refresh_kiro_token_at(
    http: &reqwest::Client,
    url: url::Url,
    material: &KiroRefreshMaterial,
) -> std::result::Result<RefreshGrant, RefreshError> {
    if !credential_ok(&material.refresh_token)
        || !credential_ok(&material.client_id)
        || !credential_ok(&material.client_secret)
    {
        return Err(RefreshError::InvalidGrant);
    }
    let response = http
        .post(url)
        .header("accept", "application/json")
        .json(&json!({
            "clientId": material.client_id,
            "clientSecret": material.client_secret,
            "grantType": "refresh_token",
            "refreshToken": material.refresh_token,
        }))
        .send()
        .await
        .map_err(|_| {
            RefreshError::Failed(SeraError::External(
                "AWS Kiro OIDC could not be reached".into(),
            ))
        })?;
    let status = response.status();
    let body = read_capped(response).await.map_err(RefreshError::Failed)?;
    let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !status.is_success() {
        let err_code = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if status.as_u16() == 401
            || matches!(
                err_code,
                "invalid_grant" | "InvalidGrantException" | "ExpiredTokenException"
            )
        {
            return Err(RefreshError::InvalidGrant);
        }
        return Err(RefreshError::Failed(SeraError::External(
            "AWS Kiro token refresh was refused".into(),
        )));
    }
    let tokens = parse_token_set(&value).map_err(RefreshError::Failed)?;
    let expires_at = tokens.expires_at();
    let rotated_refresh = match tokens.refresh_token {
        Some(new_refresh) => {
            let updated = KiroRefreshMaterial {
                refresh_token: new_refresh,
                client_id: material.client_id.clone(),
                client_secret: material.client_secret.clone(),
                region: material.region,
            };
            Some(
                serde_json::to_string(&updated)
                    .map_err(|e| RefreshError::Failed(SeraError::from(e)))?,
            )
        }
        None => None,
    };
    Ok(RefreshGrant {
        access_token: tokens.access_token,
        refresh_token: rotated_refresh,
        expires_at,
    })
}

/// Pin verification URIs to `view.awsapps.com`, `*.awsapps.com`, or
/// `signin.aws.amazon.com` over HTTPS.
pub fn pin_verification_uri(raw: &str) -> Result<String> {
    let refuse = || SeraError::External("the AWS Kiro sign-in page was refused".into());
    let url = url::Url::parse(raw).map_err(|_| refuse())?;
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let host_ok = host == "view.awsapps.com"
        || host == "signin.aws.amazon.com"
        || (host.ends_with(".awsapps.com") && host.len() > ".awsapps.com".len());
    if url.scheme() != "https"
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || !host_ok
    {
        return Err(refuse());
    }
    Ok(url.to_string())
}

fn json_text(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
}

fn json_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
}

fn parse_token_set(value: &Value) -> Result<TokenSet> {
    let bad = || SeraError::External("AWS Kiro sent an invalid token reply".into());
    let access_token = json_text(value, &["accessToken", "access_token"])
        .filter(|t| credential_ok(t))
        .ok_or_else(bad)?;
    let refresh_token =
        json_text(value, &["refreshToken", "refresh_token"]).filter(|t| credential_ok(t));
    let expires_in = json_u64(value, &["expiresIn", "expires_in"]).filter(|s| *s > 0);
    let id_token = json_text(value, &["idToken", "id_token"]).filter(|t| credential_ok(t));
    Ok(TokenSet {
        access_token,
        refresh_token,
        expires_in,
        id_token,
        refresh_expires_in: None,
    })
}

fn classify_poll_error(status: u16, value: &Value) -> DevicePoll {
    match value.get("error").and_then(Value::as_str) {
        Some("authorization_pending") => DevicePoll::Pending,
        Some("slow_down") => DevicePoll::SlowDown,
        Some("access_denied") => DevicePoll::Denied,
        Some("expired_token") => DevicePoll::Expired,
        None if status == 400 => DevicePoll::Pending,
        _ if status == 429 => DevicePoll::SlowDown,
        _ if status >= 500 => DevicePoll::Failed("provider_error"),
        _ => DevicePoll::Failed("provider_refused"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[test]
    fn region_allowlist_accepts_valid_and_rejects_invalid() {
        for valid in [
            "us-east-1",
            "us-west-2",
            "eu-central-1",
            "eu-west-1",
            "ap-southeast-1",
            "ap-northeast-1",
        ] {
            let parsed = KiroRegion::parse(valid).unwrap();
            assert_eq!(parsed.as_str(), valid);
            assert!(parsed.oidc_host().ends_with(".amazonaws.com"));
            assert!(parsed.q_host().ends_with(".amazonaws.com"));
        }

        for invalid in [
            "",
            "us-east-2",
            "us-east-1.evil.com",
            "us-east-1/../../admin",
            "169.254.169.254",
            "localhost",
        ] {
            assert!(
                KiroRegion::parse(invalid).is_err(),
                "expected invalid region to be rejected: {invalid}"
            );
        }
    }

    #[test]
    fn verification_uri_pinning() {
        assert!(
            pin_verification_uri("https://view.awsapps.com/start#/device?user_code=ABCD").is_ok()
        );
        assert!(pin_verification_uri("https://d-123456.awsapps.com/start").is_ok());
        assert!(pin_verification_uri("https://signin.aws.amazon.com/device").is_ok());

        for bad in [
            "http://view.awsapps.com/start",
            "https://evil.com/start",
            "https://awsapps.com.evil.com/start",
            "https://view.awsapps.com:8443/start",
            "https://user@view.awsapps.com/start",
        ] {
            assert!(
                pin_verification_uri(bad).is_err(),
                "expected unpinned verification URI to be rejected: {bad}"
            );
        }
    }

    #[tokio::test]
    async fn happy_path_register_device_poll_and_finish() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/client/register"))
            .and(body_json(json!({
                "clientName": "SeraBot Kiro",
                "clientType": "public",
                "scopes": [
                    "codewhisperer:completions",
                    "codewhisperer:analysis",
                    "codewhisperer:conversations"
                ],
                "grantTypes": [
                    "urn:ietf:params:oauth:grant-type:device_code",
                    "refresh_token"
                ]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "clientId": "cid-kiro-test",
                "clientSecret": "csec-kiro-test"
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/device_authorization"))
            .and(body_json(json!({
                "clientId": "cid-kiro-test",
                "clientSecret": "csec-kiro-test",
                "startUrl": "https://view.awsapps.com/start"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "deviceCode": "dc-kiro-test",
                "userCode": "ABCD-EFGH",
                "verificationUri": "https://view.awsapps.com/start",
                "verificationUriComplete": "https://view.awsapps.com/start#/device?user_code=ABCD-EFGH",
                "expiresIn": 600,
                "interval": 1
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "authorization_pending"
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accessToken": "at-kiro-synthetic",
                "refreshToken": "rt-kiro-synthetic",
                "expiresIn": 3600
            })))
            .mount(&server)
            .await;

        let provider = KiroSignIn::for_test(&server.uri());
        let http = crate::auth_flows::http_client().unwrap();

        let started = provider.device_start(&http).await.unwrap();
        assert_eq!(started.user_code, "ABCD-EFGH");
        assert_eq!(
            started.verification_uri,
            "https://view.awsapps.com/start#/device?user_code=ABCD-EFGH"
        );

        let poll1 = provider
            .device_poll(&http, &started.device_code)
            .await
            .unwrap();
        assert!(matches!(poll1, DevicePoll::Pending));

        let poll2 = provider
            .device_poll(&http, &started.device_code)
            .await
            .unwrap();
        let tokens = match poll2 {
            DevicePoll::Done(tokens) => tokens,
            other => panic!("expected DevicePoll::Done, got {other:?}"),
        };

        let signed_in = provider.finish_device(&http, tokens).await.unwrap();
        assert_eq!(signed_in.label, "AWS Kiro");
        assert_eq!(signed_in.auth_kind, AuthKind::Device);
        assert_eq!(signed_in.identity["region"], "us-east-1");
        assert_eq!(signed_in.credential.api_key, "at-kiro-synthetic");

        let material: KiroRefreshMaterial =
            serde_json::from_str(&signed_in.credential.refresh_token).unwrap();
        assert_eq!(material.refresh_token, "rt-kiro-synthetic");
        assert_eq!(material.client_id, "cid-kiro-test");
        assert_eq!(material.client_secret, "csec-kiro-test");
        assert_eq!(material.region, KiroRegion::UsEast1);
    }

    #[tokio::test]
    async fn unpinned_verification_uri_is_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/client/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "clientId": "cid-kiro-test",
                "clientSecret": "csec-kiro-test"
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/device_authorization"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "deviceCode": "dc-kiro-test",
                "userCode": "ABCD-EFGH",
                "verificationUri": "https://evil.example.com/phish",
                "expiresIn": 600,
                "interval": 5
            })))
            .mount(&server)
            .await;

        let provider = KiroSignIn::for_test(&server.uri());
        let http = crate::auth_flows::http_client().unwrap();
        let err = provider.device_start(&http).await.unwrap_err();
        assert!(err.to_string().contains("refused"));
    }

    #[tokio::test]
    async fn refresh_token_rotates_and_handles_invalid_grant() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_json(json!({
                "clientId": "cid-kiro",
                "clientSecret": "csec-kiro",
                "grantType": "refresh_token",
                "refreshToken": "rt-old"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accessToken": "at-new",
                "refreshToken": "rt-new",
                "expiresIn": 1800
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let http = crate::auth_flows::http_client().unwrap();
        let material = KiroRefreshMaterial {
            refresh_token: "rt-old".into(),
            client_id: "cid-kiro".into(),
            client_secret: "csec-kiro".into(),
            region: KiroRegion::EuWest1,
        };
        let url = url::Url::parse(&format!("{}/token", server.uri())).unwrap();
        let grant = refresh_kiro_token_at(&http, url.clone(), &material)
            .await
            .unwrap();
        assert_eq!(grant.access_token, "at-new");
        let rotated: KiroRefreshMaterial =
            serde_json::from_str(grant.refresh_token.as_deref().unwrap()).unwrap();
        assert_eq!(rotated.refresh_token, "rt-new");
        assert_eq!(rotated.region, KiroRegion::EuWest1);

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "invalid_grant"
            })))
            .mount(&server)
            .await;
        let err = refresh_kiro_token_at(&http, url, &rotated)
            .await
            .unwrap_err();
        assert!(matches!(err, RefreshError::InvalidGrant));
    }
}
