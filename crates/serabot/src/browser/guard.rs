//! Navigation guard: public http(s) only.
//!
//! A URL is checked twice: statically (scheme, credentials, literal IPs and
//! well-known internal names) and after DNS resolution (every returned address
//! must be public). The same check runs from the CDP `Fetch.requestPaused`
//! filter for subresources and redirects.
//!
//! Limits (documented in `docs/browser.md`): the browser resolves names again
//! on its own, so a rebinding DNS server can answer differently the second
//! time; WebSocket, WebRTC and `Service Worker` traffic are not seen by the
//! Fetch domain. The VPS firewall must also drop browser-user egress to
//! private ranges for defence in depth.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

use crate::error::{Result, SeraError};

pub const MAX_URL_LEN: usize = 2048;

fn denied(reason: &str) -> SeraError {
    SeraError::Validation(format!("browser navigation refused: {reason}"))
}

/// True for any address that is not a public unicast address.
pub fn ip_is_forbidden(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => ipv4_is_forbidden(v4),
        IpAddr::V6(v6) => ipv6_is_forbidden(v6),
    }
}

fn ipv4_is_forbidden(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local() // 169.254/16, includes 169.254.169.254 metadata
        || ip.is_broadcast()
        || ip.is_multicast()
        || a == 0 // 0.0.0.0/8 "this network"
        || (a == 100 && (64..=127).contains(&b)) // 100.64/10 CGNAT
        || (a == 192 && b == 0 && c == 0) // 192.0.0/24 IETF protocol assignments
        || (a == 192 && b == 0 && c == 2) // TEST-NET-1
        || (a == 198 && (b == 18 || b == 19)) // 198.18/15 benchmarking
        || (a == 198 && b == 51 && c == 100) // TEST-NET-2
        || (a == 203 && b == 0 && c == 113) // TEST-NET-3
        || a >= 240 // 240/4 reserved
}

fn ipv6_is_forbidden(ip: Ipv6Addr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return true;
    }
    let seg = ip.segments();
    // fc00::/7 unique local (includes the fd00:ec2::254 cloud metadata address).
    if seg[0] & 0xfe00 == 0xfc00 {
        return true;
    }
    // fe80::/10 link local, fec0::/10 deprecated site local.
    if seg[0] & 0xffc0 == 0xfe80 || seg[0] & 0xffc0 == 0xfec0 {
        return true;
    }
    // 2001:db8::/32 documentation.
    if seg[0] == 0x2001 && seg[1] == 0x0db8 {
        return true;
    }
    // Embedded IPv4: ::ffff:a.b.c.d, ::a.b.c.d, 64:ff9b::/96 (NAT64), 2002::/16 (6to4).
    let octets = ip.octets();
    let low_embedded = (seg[..5] == [0, 0, 0, 0, 0] && (seg[5] == 0xffff || seg[5] == 0))
        || (seg[0] == 0x0064 && seg[1] == 0xff9b && seg[2..6] == [0, 0, 0, 0]);
    let embedded = if low_embedded {
        Some(Ipv4Addr::new(
            octets[12], octets[13], octets[14], octets[15],
        ))
    } else if seg[0] == 0x2002 {
        Some(Ipv4Addr::new(octets[2], octets[3], octets[4], octets[5]))
    } else {
        None
    };
    embedded.is_some_and(ipv4_is_forbidden)
}

fn hostname_is_forbidden(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".localdomain")
        || host.ends_with(".home.arpa")
        || host == "metadata"
        || !host.contains('.')
}

/// Static validation. Returns the parsed URL. Does not touch the network.
pub fn validate_navigation_url(raw: &str) -> Result<Url> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_URL_LEN {
        return Err(SeraError::Validation(
            "url must be 1..2048 characters".into(),
        ));
    }
    let url = Url::parse(raw).map_err(|_| SeraError::Validation("url must be absolute".into()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(denied("only http and https URLs are allowed"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SeraError::Validation(
            "url must not embed credentials".into(),
        ));
    }
    match url.host() {
        None => return Err(denied("url has no host")),
        Some(Host::Ipv4(ip)) if ipv4_is_forbidden(ip) => {
            return Err(denied("address is not public"));
        }
        Some(Host::Ipv6(ip)) if ipv6_is_forbidden(ip) => {
            return Err(denied("address is not public"));
        }
        Some(Host::Domain(name)) if hostname_is_forbidden(name) => {
            return Err(denied("host name is internal"));
        }
        Some(_) => {}
    }
    Ok(url)
}

/// Every resolved address must be public; an empty answer is refused.
pub fn check_resolved(host: &str, addrs: &[IpAddr]) -> Result<()> {
    if addrs.is_empty() {
        return Err(denied(&format!("{host} did not resolve")));
    }
    if addrs.iter().any(|ip| ip_is_forbidden(*ip)) {
        return Err(denied("host resolves to a non-public address"));
    }
    Ok(())
}

/// Static check plus DNS resolution through the system resolver.
pub async fn validate_resolved(raw: &str) -> Result<Url> {
    let url = validate_navigation_url(raw)?;
    if let Some(Host::Domain(name)) = url.host() {
        let port = url.port_or_known_default().unwrap_or(443);
        let addrs: Vec<IpAddr> = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::net::lookup_host((name, port))
                .await
                .map(|iter| iter.map(|addr| addr.ip()).collect::<Vec<_>>())
        })
        .await
        .map_err(|_| denied("DNS lookup timed out"))?
        .map_err(|_| denied("DNS lookup failed"))?;
        check_resolved(name, &addrs)?;
    }
    Ok(url)
}

/// Decision for a subresource or redirect hop seen by `Fetch.requestPaused`.
/// `data:`, `blob:` and `about:blank` are inline and allowed; everything else
/// must pass [`validate_resolved`].
pub async fn allow_subrequest(raw: &str) -> bool {
    if raw.starts_with("data:") || raw.starts_with("blob:") || raw == "about:blank" {
        return true;
    }
    validate_resolved(raw).await.is_ok()
}

/// Host (lowercase, no port) of a URL for approvals and vault matching.
pub fn host_of(raw: &str) -> Option<String> {
    Url::parse(raw)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_public_http_and_https() {
        assert!(validate_navigation_url("https://example.com/a?b=1").is_ok());
        assert!(validate_navigation_url("http://example.com").is_ok());
        assert!(validate_navigation_url("https://8.8.8.8/").is_ok());
        assert!(validate_navigation_url("https://[2606:4700:4700::1111]/").is_ok());
    }

    #[test]
    fn rejects_other_schemes_and_credentials() {
        for url in [
            "file:///etc/passwd",
            "ftp://example.com",
            "javascript:alert(1)",
            "data:text/html,hi",
            "chrome://settings",
            "view-source:https://example.com",
            "https://user:pw@example.com",
            "example.com",
            "",
        ] {
            assert!(validate_navigation_url(url).is_err(), "{url}");
        }
        assert!(validate_navigation_url(&format!("https://e.com/{}", "a".repeat(2100))).is_err());
    }

    #[test]
    fn rejects_non_public_literals_in_every_spelling() {
        for url in [
            "http://127.0.0.1/",
            "http://127.1/",
            "http://2130706433/",
            "http://0x7f000001/",
            "http://0177.0.0.1/",
            "http://0.0.0.0/",
            "http://10.1.2.3/",
            "http://172.16.0.1/",
            "http://172.31.255.255/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://100.64.0.1/",
            "http://100.127.255.255/",
            "http://198.18.0.1/",
            "http://224.0.0.1/",
            "http://255.255.255.255/",
            "http://[::1]/",
            "http://[::]/",
            "http://[fe80::1]/",
            "http://[fd00:ec2::254]/",
            "http://[fc00::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:169.254.169.254]/",
            "http://[64:ff9b::7f00:1]/",
            "http://[2002:7f00:1::]/",
            "http://[2001:db8::1]/",
        ] {
            assert!(validate_navigation_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn rejects_internal_names() {
        for url in [
            "http://localhost/",
            "http://LOCALHOST:8080/",
            "http://foo.localhost/",
            "http://printer.local/",
            "http://metadata.google.internal/",
            "http://intranet/",
            "http://localhost./",
        ] {
            assert!(validate_navigation_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn public_neighbours_of_private_ranges_pass() {
        for url in [
            "http://100.63.255.255/",
            "http://100.128.0.1/",
            "http://172.15.0.1/",
            "http://172.32.0.1/",
            "http://198.20.0.1/",
        ] {
            assert!(validate_navigation_url(url).is_ok(), "{url}");
        }
    }

    #[test]
    fn resolved_addresses_are_all_checked() {
        let public: IpAddr = "93.184.216.34".parse().unwrap();
        let private: IpAddr = "10.0.0.7".parse().unwrap();
        assert!(check_resolved("a.example", &[public]).is_ok());
        assert!(check_resolved("a.example", &[public, private]).is_err());
        assert!(check_resolved("a.example", &[]).is_err());
    }

    #[tokio::test]
    async fn subrequests_allow_inline_and_block_private() {
        assert!(allow_subrequest("data:text/plain,hi").await);
        assert!(allow_subrequest("blob:https://example.com/x").await);
        assert!(!allow_subrequest("http://127.0.0.1:9222/json").await);
        assert!(!allow_subrequest("file:///etc/passwd").await);
        assert!(!allow_subrequest("http://169.254.169.254/").await);
    }

    #[test]
    fn host_of_lowercases() {
        assert_eq!(
            host_of("https://Login.Example.com:8443/x").as_deref(),
            Some("login.example.com")
        );
        assert_eq!(host_of("nonsense"), None);
    }
}
