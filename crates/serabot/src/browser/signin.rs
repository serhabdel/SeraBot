//! Sign-in refusals from identity providers, recognised from the page URL.
//!
//! A provider can refuse a browser it considers automated (Google answers with
//! `accounts.google.com/.../signin/rejected`, "This browser or app may not be
//! secure"). SeraBot does not try to get past that. It tells the owner and the
//! bot where the refusal happened and which supported route to use instead.
//! Another provider is one more row in [`RULES`].

use serde::Serialize;
use url::Url;

/// A recognised refusal page. `bot_note` is for the model and is not sent to
/// the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SigninBlock {
    pub provider: &'static str,
    /// Shown to the owner above the live view.
    pub message: &'static str,
    #[serde(skip)]
    pub bot_note: &'static str,
}

struct Rule {
    provider: &'static str,
    host: &'static str,
    /// The page path must contain one of these.
    path_markers: &'static [&'static str],
    message: &'static str,
    bot_note: &'static str,
}

const RULES: &[Rule] = &[Rule {
    provider: "google",
    host: "accounts.google.com",
    // Matches both `/signin/rejected` and `/v3/signin/rejected`.
    path_markers: &["/signin/rejected"],
    message: "Google refused sign-in from this automated server browser. Use the Google connector (Connectors → Google) for Gmail/Drive/Calendar, or sign in from a regular browser on your PC.",
    bot_note: "Google refused the sign-in from this automated browser (\"This browser or app may not be secure\"). Do not retry the login or look for another way past it. Tell the owner to use the Google connector (Connectors → Google) for Gmail, Drive and Calendar instead.",
}];

/// The refusal a page URL shows, if it is one of the known refusal pages.
pub fn signin_block_hint(url: &str) -> Option<SigninBlock> {
    let parsed = Url::parse(url).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?;
    let path = parsed.path();
    RULES
        .iter()
        .find(|rule| {
            host == rule.host && rule.path_markers.iter().any(|marker| path.contains(marker))
        })
        .map(|rule| SigninBlock {
            provider: rule.provider,
            message: rule.message,
            bot_note: rule.bot_note,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_google_rejected_sign_in_pages() {
        let block = signin_block_hint(
            "https://accounts.google.com/v3/signin/rejected?continue=https%3A%2F%2Fmail.google.com",
        )
        .expect("the v3 refusal page");
        assert_eq!(block.provider, "google");
        assert!(block.message.contains("Google refused sign-in"));
        assert!(block.bot_note.contains("Do not retry"));
        assert!(signin_block_hint("https://accounts.google.com/signin/rejected").is_some());
        assert!(signin_block_hint("https://ACCOUNTS.google.com/signin/rejected").is_some());
    }

    #[test]
    fn ignores_ordinary_sign_in_pages_and_look_alikes() {
        assert!(signin_block_hint("https://accounts.google.com/v3/signin/identifier").is_none());
        assert!(signin_block_hint("https://accounts.google.com/").is_none());
        assert!(signin_block_hint("https://evil.example/signin/rejected").is_none());
        assert!(
            signin_block_hint("https://accounts.google.com.evil.example/signin/rejected").is_none()
        );
        assert!(signin_block_hint("http://accounts.google.com/signin/rejected").is_none());
        assert!(signin_block_hint("about:blank").is_none());
        assert!(signin_block_hint("not a url").is_none());
    }

    #[test]
    fn the_owner_sees_only_provider_and_message() {
        let block = signin_block_hint("https://accounts.google.com/signin/rejected").unwrap();
        let value = serde_json::to_value(block).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 2);
        assert_eq!(value["provider"], "google");
        assert_eq!(value["message"], block.message);
    }
}
