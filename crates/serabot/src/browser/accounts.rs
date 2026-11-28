//! Which external accounts a browser profile is signed into, derived from the
//! profile's cookies. Only cookie **names and domains** are ever read — never
//! values — so this cannot leak a session token, and the result is metadata
//! (which account is connected), not a credential.
//!
//! Used by the owner console and by the read-only `browser_account_status`
//! bot tool, so a bot picks a profile that is already signed in instead of
//! walking into a captcha on a logged-out one.

use serde::Serialize;

/// One supported account provider and the cookie names that mean "signed in".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AccountProvider {
    pub id: &'static str,
    pub name: &'static str,
    /// A cookie on this domain (or a parent of it) marks the account signed in.
    pub domain: &'static str,
    /// Any of these cookie names on that domain counts as signed in.
    pub markers: &'static [&'static str],
}

pub const ACCOUNT_PROVIDERS: &[AccountProvider] = &[
    AccountProvider {
        id: "google",
        name: "Google",
        domain: "google.com",
        markers: &[
            "SAPISID",
            "SSID",
            "HSID",
            "SID",
            "__Secure-1PSID",
            "__Secure-3PSID",
        ],
    },
    AccountProvider {
        id: "youtube",
        name: "YouTube",
        domain: "youtube.com",
        // A Google sign-in also covers YouTube; these are set on YouTube itself.
        markers: &["SAPISID", "SSID", "SID", "VISITOR_INFO1_LIVE", "YSC"],
    },
    AccountProvider {
        id: "linkedin",
        name: "LinkedIn",
        domain: "linkedin.com",
        markers: &["li_at", "liap"],
    },
    AccountProvider {
        id: "instagram",
        name: "Instagram",
        domain: "instagram.com",
        markers: &["sessionid", "ds_user_id"],
    },
    AccountProvider {
        id: "x",
        name: "X",
        domain: "x.com",
        markers: &["auth_token", "twid"],
    },
    AccountProvider {
        id: "facebook",
        name: "Facebook",
        domain: "facebook.com",
        markers: &["c_user", "xs"],
    },
];

/// Per-provider sign-in state for one profile. Metadata only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountStatus {
    pub id: &'static str,
    pub name: &'static str,
    pub signed_in: bool,
}

/// Does `domain` equal `provider.domain` or sit under it (`.google.com`)?
pub fn domain_matches(cookie_domain: &str, provider_domain: &str) -> bool {
    let cookie = cookie_domain.trim_start_matches('.').to_ascii_lowercase();
    let provider = provider_domain.to_ascii_lowercase();
    cookie == provider || cookie.ends_with(&format!(".{provider}"))
}

/// Signed-in if any (domain, name) cookie sits on the provider's domain under
/// one of its marker names. `cookies` are (domain, name) pairs only.
pub fn provider_signed_in(cookies: &[(String, String)], provider: &AccountProvider) -> bool {
    cookies.iter().any(|(domain, name)| {
        domain_matches(domain, provider.domain) && provider.markers.contains(&name.as_str())
    })
}

/// The full per-provider status list for one profile's cookies.
pub fn account_statuses(cookies: &[(String, String)]) -> Vec<AccountStatus> {
    ACCOUNT_PROVIDERS
        .iter()
        .map(|provider| AccountStatus {
            id: provider.id,
            name: provider.name,
            signed_in: provider_signed_in(cookies, provider),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(domain: &str, name: &str) -> (String, String) {
        (domain.to_owned(), name.to_owned())
    }

    #[test]
    fn google_session_cookies_mark_google_and_youtube_signed_in() {
        let cookies = vec![
            cookie(".google.com", "SAPISID"),
            cookie("accounts.google.com", "HSID"),
            cookie(".youtube.com", "VISITOR_INFO1_LIVE"),
        ];
        let statuses = account_statuses(&cookies);
        let google = statuses.iter().find(|s| s.id == "google").unwrap();
        let youtube = statuses.iter().find(|s| s.id == "youtube").unwrap();
        assert!(google.signed_in);
        assert!(youtube.signed_in);
        assert!(!statuses.iter().find(|s| s.id == "x").unwrap().signed_in);
    }

    #[test]
    fn social_markers_are_per_platform() {
        let cookies = vec![
            cookie(".linkedin.com", "li_at"),
            cookie(".instagram.com", "sessionid"),
            cookie(".x.com", "auth_token"),
            cookie(".facebook.com", "c_user"),
        ];
        let statuses = account_statuses(&cookies);
        for id in ["linkedin", "instagram", "x", "facebook"] {
            assert!(
                statuses.iter().find(|s| s.id == id).unwrap().signed_in,
                "{id} should be signed in"
            );
        }
        assert!(
            !statuses
                .iter()
                .find(|s| s.id == "google")
                .unwrap()
                .signed_in
        );
    }

    #[test]
    fn look_alike_domains_and_unrelated_cookies_do_not_count() {
        let cookies = vec![
            cookie("evil-google.com", "SAPISID"),
            cookie("google.com.evil.example", "SAPISID"),
            cookie(".google.com", "NID"), // real cookie, not a session marker
            cookie("notlinkedin.com", "li_at"),
        ];
        let statuses = account_statuses(&cookies);
        assert!(statuses.iter().all(|s| !s.signed_in));
    }

    #[test]
    fn domain_matching_handles_leading_dot_and_case() {
        assert!(domain_matches(".Google.COM", "google.com"));
        assert!(domain_matches("accounts.google.com", "google.com"));
        assert!(domain_matches("google.com", "google.com"));
        assert!(!domain_matches("google.com.evil.example", "google.com"));
        assert!(!domain_matches("evilgoogle.com", "google.com"));
        assert!(!domain_matches("googlecom", "google.com"));
    }
}
