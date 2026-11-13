//! Links that come from bot output are untrusted text. Only a plain https link
//! may be opened, and only through the app's own open path; anything else is
//! shown as text. This is the single rule for chat markdown.

/// Longest link the chat will open.
pub const MAX_LINK_CHARS: usize = 2_048;

/// The link to open, normalised, or `None` when it must not be opened.
///
/// Allowed: `https` with a host and no user name or password. Everything else
/// (`javascript:`, `data:`, `file:`, `vbscript:`, `mailto:`, plain `http`,
/// scheme-less and protocol-relative links, links with spaces or control
/// characters) is refused. The existing policy for opening a page is https
/// only, so plain `http` is not allowed here either.
pub fn safe_chat_link(raw: &str) -> Option<String> {
    let link = raw.trim();
    if link.is_empty()
        || link.chars().count() > MAX_LINK_CHARS
        || link.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    // The URL parser forgives odd spellings (`https:///host`, `https:\\host`);
    // the text itself must be a plain `https://host...`.
    let lower = link.to_ascii_lowercase();
    let after = lower.strip_prefix("https://")?;
    if after.is_empty() || after.starts_with(['/', '\\', '?', '#', '@']) {
        return None;
    }
    let parsed = url::Url::parse(link).ok()?;
    let ok = parsed.scheme() == "https"
        && parsed.host_str().is_some_and(|host| !host.is_empty())
        && parsed.username().is_empty()
        && parsed.password().is_none();
    ok.then(|| parsed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_https_link_is_allowed_and_normalised() {
        assert_eq!(
            safe_chat_link("https://example.com/a?b=1#c").as_deref(),
            Some("https://example.com/a?b=1#c")
        );
        assert_eq!(
            safe_chat_link("  HTTPS://Example.COM  ").as_deref(),
            Some("https://example.com/")
        );
    }

    #[test]
    fn script_and_data_and_file_links_are_refused() {
        for bad in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "  javascript:alert(1)",
            "java\tscript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "data:text/html;base64,PHNjcmlwdD4=",
            "file:///etc/passwd",
            "file://server/share/x",
            "vbscript:msgbox(1)",
            "mailto:a@example.com",
            "ftp://example.com/file",
            "blob:https://example.com/id",
            "about:blank",
        ] {
            assert_eq!(safe_chat_link(bad), None, "must refuse {bad}");
        }
    }

    #[test]
    fn credentialed_and_hostless_links_are_refused() {
        for bad in [
            "https://user:pass@example.com/",
            "https://user@example.com/",
            "https://:pass@example.com/",
            "https:///path-only",
            "https:\\\\example.com/x",
            "https://@example.com/",
            "https://?x=1",
            "https://",
            "//example.com/x",
            "example.com/x",
            "/relative/path",
        ] {
            assert_eq!(safe_chat_link(bad), None, "must refuse {bad}");
        }
    }

    #[test]
    fn plain_http_is_not_allowed() {
        assert_eq!(safe_chat_link("http://example.com/"), None);
    }

    #[test]
    fn spaces_control_characters_and_long_links_are_refused() {
        assert_eq!(safe_chat_link("https://exa mple.com/"), None);
        assert_eq!(safe_chat_link("https://example.com/a b"), None);
        assert_eq!(safe_chat_link("https://example.com/\u{0}"), None);
        assert_eq!(
            safe_chat_link("https://example.com/\n"),
            Some("https://example.com/".into())
        );
        assert_eq!(safe_chat_link(""), None);
        assert_eq!(safe_chat_link("   "), None);
        let long = format!("https://example.com/{}", "a".repeat(MAX_LINK_CHARS));
        assert_eq!(safe_chat_link(&long), None);
    }
}
