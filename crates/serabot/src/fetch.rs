//! Bounded, SSRF-resistant HTTP fetching.
//!
//! The fetcher deliberately avoids reqwest's automatic redirect handling and
//! revalidates every hop: scheme, credentials, port, host name and *all* DNS
//! answers are checked before each request. Response bodies are streamed and
//! capped so a hostile or broken origin cannot exhaust memory.
//!
//! Residual limitation: DNS is resolved here for validation and then resolved
//! again by reqwest when it connects. A hostile resolver can therefore answer
//! "public" for the validation lookup and "private" for the connection lookup
//! (DNS rebinding TOCTOU). reqwest does not expose a hook to pin or verify the
//! address actually connected to, so this check is best-effort, not a
//! guarantee. Origins are nevertheless limited to plain public HTTPS in normal
//! operation, which keeps the practical risk small.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use futures::StreamExt;
use reqwest::header::{CONTENT_TYPE, LOCATION};
use reqwest::{Client, Url};
use serde::Serialize;

use crate::error::{Result, SeraError};

/// Maximum number of redirects followed before giving up.
const MAX_REDIRECTS: usize = 3;

/// Status codes that are handled as redirects when a `Location` header exists.
const REDIRECT_STATUSES: [u16; 5] = [301, 302, 303, 307, 308];

/// A successfully retrieved HTTP response.
#[derive(Debug, Clone, Serialize)]
pub struct FetchedPage {
    /// URL after all manually followed redirects, with any fragment removed.
    pub final_url: String,
    /// HTTP status code of the final response.
    pub status: u16,
    /// `Content-Type` header of the final response, when present and valid.
    pub content_type: Option<String>,
    /// Decoded body. Invalid UTF-8 is replaced rather than rejected.
    pub body: String,
}

/// HTTP client that refuses non-public targets before issuing a request.
#[derive(Debug, Clone)]
pub struct SafeFetcher {
    client: Client,
    max_bytes: usize,
    allow_insecure_localhost: bool,
    allowed_hosts: Option<std::collections::HashSet<String>>,
}

impl SafeFetcher {
    /// Build a fetcher. `timeout_secs` applies to each individual request
    /// (connect, headers and body). `max_bytes` caps the response body.
    pub fn new(user_agent: impl Into<String>, timeout_secs: u64, max_bytes: usize) -> Result<Self> {
        let user_agent = user_agent.into();
        if user_agent.trim().is_empty() {
            return Err(SeraError::Config(
                "fetch user agent must not be empty".into(),
            ));
        }
        if timeout_secs == 0 {
            return Err(SeraError::Config(
                "fetch timeout must be greater than zero".into(),
            ));
        }
        if max_bytes == 0 {
            return Err(SeraError::Config(
                "fetch response byte limit must be greater than zero".into(),
            ));
        }
        let client = Client::builder()
            .user_agent(user_agent)
            .timeout(Duration::from_secs(timeout_secs))
            // Redirects are followed by hand so every hop is revalidated.
            .redirect(reqwest::redirect::Policy::none())
            // Environment proxies would bypass the address checks below.
            .no_proxy()
            // reqwest does not enable a cookie store unless asked to, so no
            // cookies are ever sent or retained.
            .build()
            .map_err(|error| SeraError::External(format!("cannot build HTTP client: {error}")))?;
        Ok(Self {
            client,
            max_bytes,
            allow_insecure_localhost: false,
            allowed_hosts: None,
        })
    }

    /// Restrict every request and redirect to these hostnames. An empty set
    /// denies all hosts; callers should pass the configured source hosts.
    pub fn allow_hosts<I, S>(mut self, hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let hosts = hosts
            .into_iter()
            .map(|host| host.into().to_ascii_lowercase())
            .collect::<std::collections::HashSet<_>>();
        self.allowed_hosts = Some(hosts);
        self
    }

    /// Explicitly opt into plain HTTP to `localhost` (including `*.localhost`).
    /// This exists for local integration tests such as wiremock. IP-literal
    /// hosts are still rejected, and every resolved address must be loopback.
    pub fn allow_insecure_localhost(mut self) -> Self {
        self.allow_insecure_localhost = true;
        self
    }

    /// Fetch `url` and return the bounded response.
    ///
    /// Non-success status codes are returned rather than treated as transport
    /// errors; callers decide whether a 404/500 page is useful.
    pub async fn fetch(&self, url: &str) -> Result<FetchedPage> {
        let mut current = self.validate(url).await?;
        let mut redirects = 0usize;

        loop {
            let response = self
                .client
                .get(current.clone())
                .send()
                .await
                .map_err(|error| SeraError::External(format!("fetch {current}: {error}")))?;
            let status = response.status();

            if REDIRECT_STATUSES.contains(&status.as_u16()) {
                if redirects >= MAX_REDIRECTS {
                    return Err(SeraError::External(format!(
                        "too many redirects while fetching {url}"
                    )));
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        SeraError::External(format!(
                            "redirect without a usable Location header from {current}"
                        ))
                    })?
                    .to_owned();
                let next = current.join(&location).map_err(|error| {
                    SeraError::External(format!("invalid redirect target from {current}: {error}"))
                })?;
                current = self.validate_parsed(next).await?;
                redirects += 1;
                continue;
            }

            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);

            if response
                .content_length()
                .is_some_and(|length| length > self.max_bytes as u64)
            {
                return Err(SeraError::External(format!(
                    "response from {current} exceeds {} bytes",
                    self.max_bytes
                )));
            }

            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| {
                    SeraError::External(format!("read body from {current}: {error}"))
                })?;
                if body.len().saturating_add(chunk.len()) > self.max_bytes {
                    return Err(SeraError::External(format!(
                        "response from {current} exceeds {} bytes",
                        self.max_bytes
                    )));
                }
                body.extend_from_slice(&chunk);
            }

            return Ok(FetchedPage {
                final_url: current.to_string(),
                status: status.as_u16(),
                content_type,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }
    }

    async fn validate(&self, raw: &str) -> Result<Url> {
        let parsed = Url::parse(raw)
            .map_err(|error| SeraError::Validation(format!("invalid URL {raw:?}: {error}")))?;
        self.validate_parsed(parsed).await
    }

    async fn validate_parsed(&self, mut url: Url) -> Result<Url> {
        // Fragments never reach the origin and must not survive in final_url.
        url.set_fragment(None);

        if !url.username().is_empty() || url.password().is_some() {
            return Err(SeraError::Forbidden(
                "URLs with embedded userinfo are not allowed".into(),
            ));
        }

        let scheme = url.scheme().to_ascii_lowercase();
        let raw_host = url
            .host_str()
            .ok_or_else(|| SeraError::Validation("URL has no host".into()))?;
        // `host_str` serializes IPv6 literals with brackets; strip them so the
        // IP test below also catches bracketed forms.
        let host = raw_host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase();
        if host.is_empty() {
            return Err(SeraError::Validation("URL has an empty host".into()));
        }
        if let Some(allowed) = &self.allowed_hosts {
            if !allowed.contains(&host) {
                return Err(SeraError::Forbidden(format!(
                    "host {host} is not in the configured source allowlist"
                )));
            }
        }
        if host.parse::<IpAddr>().is_ok() {
            return Err(SeraError::Forbidden(
                "IP-literal URLs are not allowed; use a hostname".into(),
            ));
        }

        let is_localhost_name = host == "localhost" || host.ends_with(".localhost");
        match scheme.as_str() {
            "https" => {}
            "http" if self.allow_insecure_localhost && is_localhost_name => {}
            "http" => {
                return Err(SeraError::Forbidden(
                    "plain HTTP is only allowed for explicit localhost tests".into(),
                ));
            }
            other => {
                return Err(SeraError::Forbidden(format!(
                    "unsupported URL scheme: {other}"
                )));
            }
        }

        let port = url
            .port_or_known_default()
            .ok_or_else(|| SeraError::Validation("URL has no usable port".into()))?;
        if scheme == "https" && port != 443 {
            return Err(SeraError::Forbidden(format!(
                "unsupported port {port} for HTTPS"
            )));
        }
        // In insecure-localhost mode the test server port is arbitrary, so
        // only the loopback requirement below applies.

        let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|error| {
                SeraError::External(format!("DNS resolution failed for {host}: {error}"))
            })?
            .collect();
        if addresses.is_empty() {
            return Err(SeraError::External(format!(
                "DNS resolution returned no addresses for {host}"
            )));
        }

        if self.allow_insecure_localhost
            && addresses.iter().any(|address| !address.ip().is_loopback())
        {
            return Err(SeraError::Forbidden(format!(
                "{host} does not resolve exclusively to loopback addresses"
            )));
        } else if !self.allow_insecure_localhost
            && addresses
                .iter()
                .any(|address| is_forbidden_ip(address.ip()))
        {
            return Err(SeraError::Forbidden(format!(
                "{host} resolves to a non-public address"
            )));
        }

        Ok(url)
    }
}

/// Reject addresses that must never be reached by a watcher: loopback,
/// private, link-local, unique-local, unspecified, multicast and the common
/// reserved/benchmark blocks.
fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_v4(v4),
        IpAddr::V6(v6) => is_forbidden_v6(v6),
    }
}

fn is_forbidden_v4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        || octets[0] == 0
        // Carrier-grade NAT 100.64.0.0/10.
        || (octets[0] == 100 && (octets[1] & 0xc0) == 64)
        // IETF protocol assignments 192.0.0.0/24.
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        // TEST-NET-1..3.
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
        // Benchmarking 198.18.0.0/15.
        || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
        // Reserved 240.0.0.0/4.
        || octets[0] >= 240
}

fn is_forbidden_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    // Link-local fe80::/10.
    if (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    // Unique-local fc00::/7.
    if (segments[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // Documentation 2001:db8::/32.
    if segments[0] == 0x2001 && segments[1] == 0x0db8 {
        return true;
    }
    // IPv4-mapped addresses inherit the IPv4 policy.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_forbidden_v4(v4);
    }
    // Deprecated IPv4-compatible `::/96` addresses are never public targets.
    if segments[0..6].iter().all(|segment| *segment == 0) {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fetcher(max_bytes: usize) -> SafeFetcher {
        SafeFetcher::new("SeraBot-test/0.1", 5, max_bytes)
            .unwrap()
            .allow_insecure_localhost()
    }

    /// wiremock listens on 127.0.0.1; IP literals are always rejected, so
    /// tests address it through the allowed `localhost` hostname instead.
    fn localhost_url(server: &MockServer, path: &str) -> String {
        let uri = Url::parse(&server.uri()).unwrap();
        let port = uri.port().unwrap();
        format!("http://localhost:{port}{path}")
    }

    #[tokio::test]
    async fn fetches_through_localhost_opt_in_and_strips_fragment() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<html>ok</html>", "text/html"))
            .mount(&server)
            .await;

        let page = fetcher(1024)
            .fetch(&format!("{}#section", localhost_url(&server, "/page")))
            .await
            .unwrap();

        assert_eq!(page.status, 200);
        assert_eq!(page.body, "<html>ok</html>");
        assert!(page.final_url.ends_with("/page"));
        assert!(!page.final_url.contains('#'));
        assert_eq!(page.content_type.as_deref(), Some("text/html"));
    }

    #[tokio::test]
    async fn follows_safe_redirects_and_returns_final_url() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/start"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", localhost_url(&server, "/next")),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/next"))
            .respond_with(ResponseTemplate::new(200).set_body_string("done"))
            .mount(&server)
            .await;

        let page = fetcher(1024)
            .fetch(&localhost_url(&server, "/start"))
            .await
            .unwrap();
        assert_eq!(page.status, 200);
        assert_eq!(page.body, "done");
        assert!(page.final_url.ends_with("/next"));
    }

    #[tokio::test]
    async fn redirect_targets_are_revalidated() {
        let server = MockServer::start().await;
        // The initial URL uses `localhost`, the redirect points at the raw
        // loopback IP literal, which must be rejected on the second hop.
        let target = format!("{}/final", server.uri());
        Mock::given(method("GET"))
            .and(path("/start"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", target.as_str()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/final"))
            .respond_with(ResponseTemplate::new(200).set_body_string("must not be reached"))
            .mount(&server)
            .await;

        let error = fetcher(1024)
            .fetch(&localhost_url(&server, "/start"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("IP-literal"), "{error}");
    }

    #[tokio::test]
    async fn rejects_unsafe_urls_without_network_access() {
        let fetcher = fetcher(1024);
        assert!(fetcher.fetch("file:///etc/passwd").await.is_err());
        assert!(
            fetcher
                .fetch("https://user:secret@example.com/")
                .await
                .is_err()
        );
        assert!(fetcher.fetch("https://10.0.0.1/").await.is_err());
        assert!(fetcher.fetch("https://127.0.0.1/").await.is_err());
        assert!(
            fetcher
                .fetch("https://169.254.169.254/latest/meta-data")
                .await
                .is_err()
        );
        assert!(fetcher.fetch("https://example.com:8443/").await.is_err());
        assert!(fetcher.fetch("ftp://example.com/file").await.is_err());
    }

    #[tokio::test]
    async fn host_allowlist_blocks_unlisted_hosts() {
        let fetcher = fetcher(1024).allow_hosts(["example.com"]);
        let error = fetcher.fetch("https://other.example/").await.unwrap_err();
        assert!(error.to_string().contains("allowlist"), "{error}");
    }

    #[tokio::test]
    async fn host_allowlist_permits_configured_localhost() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ok"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        let page = fetcher(1024)
            .allow_hosts(["localhost"])
            .fetch(&localhost_url(&server, "/ok"))
            .await
            .unwrap();
        assert_eq!(page.body, "ok");
    }

    #[tokio::test]
    async fn enforces_response_size_cap() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/large"))
            .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(4096)))
            .mount(&server)
            .await;

        let error = fetcher(512)
            .fetch(&localhost_url(&server, "/large"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeds"), "{error}");
    }
}
