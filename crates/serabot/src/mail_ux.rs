//! Pure helpers for the 0.74.14 mail workspace (no I/O, std only).
//!
//! * Search operators (`from:`, `to:`, `subject:`, `has:attachment`, `is:unread`,
//!   `before:`, `after:`) parsed into a [`MailQuery`] and rendered as IMAP `SEARCH`
//!   criteria.
//! * Conversation threading from `Message-ID` / `In-Reply-To` / `References`, with a
//!   reply-prefix subject fallback ([`thread_keys`]).
//! * `List-Unsubscribe` parsing. Only `mailto:` and `https:` targets survive.
//! * List snippets, and the retry/staleness arithmetic for scheduled sends.

use std::collections::HashMap;

/// Longest search string the API accepts.
pub const MAX_QUERY_CHARS: usize = 256;
/// A scheduled send is attempted at most this many times.
pub const MAX_SEND_ATTEMPTS: i64 = 3;
/// A row stuck in `sending` for longer than this is treated as interrupted.
pub const STALE_SENDING_SECS: i64 = 600;
/// Characters kept for a list-row snippet.
pub const SNIPPET_CHARS: usize = 140;

// ------------------------------------------------------------ search ----

type Ymd = (i32, u32, u32);

/// A parsed search string.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailQuery {
    /// Free-text terms (a quoted phrase is one term).
    pub text: Vec<String>,
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub subject: Vec<String>,
    pub has_attachment: bool,
    /// `Some(true)` for `is:unread`, `Some(false)` for `is:read`.
    pub unread: Option<bool>,
    /// `Some(true)` for `is:starred`, `Some(false)` for `is:unstarred`.
    pub starred: Option<bool>,
    pub before: Option<Ymd>,
    pub after: Option<Ymd>,
}

fn tokenize(query: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut has_content = false;
    for c in query.chars() {
        if c == '"' {
            in_quote = !in_quote;
            has_content = true;
        } else if c.is_whitespace() && !in_quote {
            if has_content {
                out.push(std::mem::take(&mut current));
                has_content = false;
            }
        } else {
            current.push(c);
            has_content = true;
        }
    }
    if in_quote {
        return Err("search has an unclosed quote".into());
    }
    if has_content {
        out.push(current);
    }
    Ok(out)
}

fn parse_ymd(value: &str) -> Result<Ymd, String> {
    let bad = || format!("dates look like 2026-11-25, got '{value}'");
    let mut parts = value.split('-');
    let year: i32 = parts.next().and_then(|p| p.parse().ok()).ok_or_else(bad)?;
    let month: u32 = parts.next().and_then(|p| p.parse().ok()).ok_or_else(bad)?;
    let day: u32 = parts.next().and_then(|p| p.parse().ok()).ok_or_else(bad)?;
    if parts.next().is_some()
        || !(1970..=2100).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
    {
        return Err(bad());
    }
    Ok((year, month, day))
}

/// Parse a user search string. Unknown `key:value` pairs are kept as free text so
/// searching for an address like `john@x.com` or a time like `10:30` still works.
pub fn parse_query(query: &str) -> Result<MailQuery, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("query must be 1-256 characters".into());
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err("query must be 1-256 characters".into());
    }
    let mut out = MailQuery::default();
    for token in tokenize(query)? {
        let token = token.trim().to_owned();
        if token.is_empty() {
            continue;
        }
        let Some((key, value)) = token.split_once(':') else {
            out.text.push(token);
            continue;
        };
        let key = key.to_ascii_lowercase();
        let value = value.trim();
        match (key.as_str(), value) {
            ("from", v) if !v.is_empty() => out.from.push(v.to_owned()),
            ("to", v) if !v.is_empty() => out.to.push(v.to_owned()),
            ("subject", v) if !v.is_empty() => out.subject.push(v.to_owned()),
            ("has", v) if v.eq_ignore_ascii_case("attachment") => out.has_attachment = true,
            ("is", v) => match v.to_ascii_lowercase().as_str() {
                "unread" => out.unread = Some(true),
                "read" => out.unread = Some(false),
                "starred" | "flagged" => out.starred = Some(true),
                "unstarred" | "unflagged" => out.starred = Some(false),
                other => return Err(format!("unknown filter 'is:{other}'")),
            },
            ("before", v) => out.before = Some(parse_ymd(v)?),
            ("after", v) | ("since", v) => out.after = Some(parse_ymd(v)?),
            _ => out.text.push(token.clone()),
        }
    }
    Ok(out)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn imap_date((year, month, day): Ymd) -> String {
    format!("{day}-{}-{year}", MONTHS[(month - 1) as usize])
}

impl MailQuery {
    /// True when the query only needs a post-fetch attachment check on top of IMAP.
    pub fn needs_attachment_filter(&self) -> bool {
        self.has_attachment
    }

    /// IMAP `SEARCH` criteria. `quote` must produce a valid IMAP quoted string.
    /// A bare word searches subject, sender and body; operators narrow further.
    pub fn imap_criteria(&self, quote: &dyn Fn(&str) -> String) -> String {
        let mut parts: Vec<String> = Vec::new();
        for term in &self.text {
            let q = quote(term);
            parts.push(format!("OR OR SUBJECT {q} FROM {q} TEXT {q}"));
        }
        for term in &self.from {
            parts.push(format!("FROM {}", quote(term)));
        }
        for term in &self.to {
            parts.push(format!("TO {}", quote(term)));
        }
        for term in &self.subject {
            parts.push(format!("SUBJECT {}", quote(term)));
        }
        match self.unread {
            Some(true) => parts.push("UNSEEN".into()),
            Some(false) => parts.push("SEEN".into()),
            None => {}
        }
        match self.starred {
            Some(true) => parts.push("FLAGGED".into()),
            Some(false) => parts.push("UNFLAGGED".into()),
            None => {}
        }
        if let Some(date) = self.before {
            parts.push(format!("BEFORE {}", imap_date(date)));
        }
        if let Some(date) = self.after {
            parts.push(format!("SINCE {}", imap_date(date)));
        }
        if parts.is_empty() {
            "ALL".into()
        } else {
            parts.join(" ")
        }
    }
}

// --------------------------------------------------------- threading ----

/// The header fields threading needs.
#[derive(Debug, Clone, Copy)]
pub struct ThreadInput<'a> {
    pub message_id: &'a str,
    pub in_reply_to: &'a str,
    pub references: &'a [String],
    pub subject: &'a str,
}

fn strip_prefix_once(s: &str) -> Option<&str> {
    for prefix in ["re:", "fwd:", "fw:", "aw:", "wg:", "sv:", "tr:", "rv:"] {
        if let Some(head) = s.get(..prefix.len())
            && head.eq_ignore_ascii_case(prefix)
        {
            return Some(s[prefix.len()..].trim_start());
        }
    }
    // "Re[2]: subject"
    if let Some(head) = s.get(..3)
        && head.eq_ignore_ascii_case("re[")
        && let Some(close) = s.find("]:")
        && s[3..close].chars().all(|c| c.is_ascii_digit())
    {
        return Some(s[close + 2..].trim_start());
    }
    None
}

/// Lower-cased subject with reply/forward prefixes removed, and whether any was removed.
pub fn normalize_subject(subject: &str) -> (String, bool) {
    let mut rest = subject.trim();
    let mut had_prefix = false;
    while let Some(next) = strip_prefix_once(rest) {
        rest = next.trim();
        had_prefix = true;
    }
    let normalized = rest
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    (normalized, had_prefix)
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        // Keep the smaller index as root so keys are stable for a given list order.
        let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
        parent[hi] = lo;
    }
}

fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// One stable key per message; messages in the same conversation share a key.
///
/// Messages link when one cites another's `Message-ID` (or both cite the same id).
/// Messages with the same normalized subject also link when at least one carries a
/// reply/forward prefix, so a lone "Hello" from two senders is not merged.
pub fn thread_keys(items: &[ThreadInput<'_>]) -> Vec<String> {
    let n = items.len();
    let mut parent: Vec<usize> = (0..n).collect();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        let mut ids: Vec<&str> = Vec::new();
        if !item.message_id.trim().is_empty() {
            ids.push(item.message_id.trim());
        }
        if !item.in_reply_to.trim().is_empty() {
            ids.push(item.in_reply_to.trim());
        }
        ids.extend(
            item.references
                .iter()
                .map(|r| r.trim())
                .filter(|r| !r.is_empty()),
        );
        for id in ids {
            match by_id.get(id) {
                Some(&j) => union(&mut parent, i, j),
                None => {
                    by_id.insert(id.to_owned(), i);
                }
            }
        }
    }
    let mut by_subject: HashMap<String, Vec<(usize, bool)>> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        let (normalized, had_prefix) = normalize_subject(item.subject);
        if !normalized.is_empty() {
            by_subject
                .entry(normalized)
                .or_default()
                .push((i, had_prefix));
        }
    }
    for group in by_subject.values() {
        if group.len() > 1 && group.iter().any(|(_, prefixed)| *prefixed) {
            let first = group[0].0;
            for (i, _) in &group[1..] {
                union(&mut parent, first, *i);
            }
        }
    }
    // Key each component by its members' content so it survives re-ordering.
    let mut seeds: HashMap<usize, String> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        let root = find(&mut parent, i);
        let seed = if !item.message_id.trim().is_empty() {
            item.message_id.trim().to_owned()
        } else {
            normalize_subject(item.subject).0
        };
        let entry = seeds.entry(root).or_default();
        if entry.is_empty() || (!seed.is_empty() && seed < *entry) {
            *entry = seed;
        }
    }
    (0..n)
        .map(|i| {
            let root = find(&mut parent, i);
            let seed = seeds.get(&root).map(String::as_str).unwrap_or("");
            if seed.is_empty() {
                format!("t-solo-{i}")
            } else {
                format!("t-{:016x}", fnv1a(seed))
            }
        })
        .collect()
}

// -------------------------------------------------------- unsubscribe ----

/// Safe `List-Unsubscribe` targets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unsubscribe {
    /// `mailto:` target (address plus optional `?subject=`), without the scheme.
    pub mailto: Option<String>,
    /// `https:` link. Opened in the user's browser, never fetched by the server.
    pub url: Option<String>,
    /// RFC 8058 `List-Unsubscribe-Post: List-Unsubscribe=One-Click`.
    pub one_click: bool,
}

/// Parse `<mailto:...>, <https://...>` plus the optional one-click header.
pub fn parse_list_unsubscribe(header: &str, post_header: &str) -> Unsubscribe {
    let mut out = Unsubscribe {
        one_click: post_header
            .to_ascii_lowercase()
            .contains("list-unsubscribe=one-click"),
        ..Unsubscribe::default()
    };
    let mut rest = header;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        let target = rest[open + 1..open + close].trim();
        rest = &rest[open + close + 1..];
        if target.len() > 1000 || target.chars().any(|c| c.is_control() || c.is_whitespace()) {
            continue;
        }
        let lower = target.to_ascii_lowercase();
        if lower.starts_with("mailto:") && out.mailto.is_none() && target.len() > 7 {
            out.mailto = Some(target[7..].to_owned());
        } else if lower.starts_with("https://") && out.url.is_none() && target.len() > 8 {
            out.url = Some(target.to_owned());
        }
    }
    out
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Split a `mailto:` target (scheme already removed) into address, subject and body.
pub fn split_mailto(target: &str) -> Option<(String, String, String)> {
    let (addr, query) = match target.split_once('?') {
        Some((a, q)) => (a, q),
        None => (target, ""),
    };
    let addr = percent_decode(addr).trim().to_owned();
    if addr.is_empty() || !addr.contains('@') || addr.contains(['\r', '\n', ',', ' ', '<', '>']) {
        return None;
    }
    let mut subject = String::new();
    let mut body = String::new();
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            match k.to_ascii_lowercase().as_str() {
                "subject" => subject = percent_decode(v).replace(['\r', '\n'], " "),
                "body" => body = percent_decode(v),
                _ => {}
            }
        }
    }
    Some((addr, subject, body))
}

// ------------------------------------------------- ids and addresses ----

/// Every `<id@host>` token in a `Message-ID` / `In-Reply-To` / `References` value.
pub fn message_ids(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = value;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        let id = &rest[open..open + close + 1];
        rest = &rest[open + close + 1..];
        if id.len() > 2
            && id.len() <= 255
            && !id.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            out.push(id.to_owned());
        }
    }
    out
}

fn plausible_email(email: &str) -> bool {
    email.len() <= 254
        && !email.chars().any(|c| {
            c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';' | '"')
        })
        && matches!(email.split_once('@'), Some((local, domain)) if !local.is_empty() && !domain.is_empty() && !domain.contains('@'))
}

fn split_address_list(list: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut in_quote = false;
    let mut angle = false;
    for (i, c) in list.char_indices() {
        match c {
            '"' => in_quote = !in_quote,
            '<' if !in_quote => angle = true,
            '>' if !in_quote => angle = false,
            ',' | ';' if !in_quote && !angle => {
                out.push(&list[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&list[start..]);
    out
}

/// `(display name, lower-cased address)` pairs from an address header or a typed
/// recipient list. Entries without a plausible address are dropped.
pub fn parse_addresses(list: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for piece in split_address_list(list) {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        let (name, email) = match (piece.rfind('<'), piece.rfind('>')) {
            (Some(open), Some(close)) if open < close => (
                piece[..open].trim().trim_matches('"').trim().to_owned(),
                piece[open + 1..close].trim().to_owned(),
            ),
            _ => (String::new(), piece.to_owned()),
        };
        if plausible_email(&email) {
            out.push((name, email.to_ascii_lowercase()));
        }
        if out.len() >= 50 {
            break;
        }
    }
    out
}

/// The folder whose name contains `needle` (case-insensitive), preferring an exact
/// match on the last path segment. `None` when the server has no such folder.
pub fn pick_folder<'a>(folders: &'a [String], needle: &str) -> Option<&'a str> {
    let needle = needle.to_ascii_lowercase();
    let last = |f: &str| {
        f.rsplit(['/', '.'])
            .next()
            .unwrap_or(f)
            .to_ascii_lowercase()
    };
    folders
        .iter()
        .find(|f| last(f) == needle)
        .or_else(|| {
            folders
                .iter()
                .find(|f| f.to_ascii_lowercase().contains(&needle))
        })
        .map(String::as_str)
}

/// The UID from an `APPENDUID` response code (`a4 OK [APPENDUID 1699 42] done`).
pub fn parse_appenduid(line: &str) -> Option<u32> {
    let start = line.find("APPENDUID")?;
    let mut parts = line[start + "APPENDUID".len()..].split_whitespace();
    let _validity = parts.next()?;
    parts.next()?.trim_end_matches(']').parse().ok()
}

// ----------------------------------------------------------- snippets ----

/// First meaningful words of a plain body: quotes and blank lines skipped,
/// whitespace collapsed, clipped at [`SNIPPET_CHARS`].
pub fn snippet_from(body: &str) -> String {
    let mut words: Vec<&str> = Vec::new();
    let mut chars = 0usize;
    'lines: for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('>') {
            continue;
        }
        for word in line.split_whitespace() {
            chars += word.chars().count() + 1;
            words.push(word);
            if chars > SNIPPET_CHARS + 20 {
                break 'lines;
            }
        }
    }
    let joined = words.join(" ");
    if joined.chars().count() <= SNIPPET_CHARS {
        joined
    } else {
        let clipped: String = joined.chars().take(SNIPPET_CHARS).collect();
        format!("{}…", clipped.trim_end())
    }
}

// ---------------------------------------------------------- scheduling ----

/// Seconds to wait before the next attempt after `attempts` failed tries, or
/// `None` once the row should be marked failed for good.
pub fn retry_delay_secs(attempts: i64) -> Option<i64> {
    match attempts {
        a if a < 1 => Some(0),
        1 => Some(60),
        2 => Some(300),
        _ => None,
    }
}

/// Whether a row claimed at `claimed_at` has been `sending` too long.
pub fn is_stale_sending(claimed_at: Option<i64>, now: i64) -> bool {
    match claimed_at {
        Some(at) => now - at > STALE_SENDING_SECS,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> String {
        format!("\"{s}\"")
    }

    #[test]
    fn plain_query_keeps_the_legacy_or_shape() {
        let parsed = parse_query("invoice").unwrap();
        assert_eq!(
            parsed.imap_criteria(&q),
            "OR OR SUBJECT \"invoice\" FROM \"invoice\" TEXT \"invoice\""
        );
    }

    #[test]
    fn operators_become_imap_criteria() {
        let parsed = parse_query(
            "from:anna to:\"bob smith\" subject:report is:unread is:starred before:2026-11-25 after:2026-01-02 budget",
        )
        .unwrap();
        let c = parsed.imap_criteria(&q);
        assert!(c.contains("FROM \"anna\""), "{c}");
        assert!(c.contains("TO \"bob smith\""), "{c}");
        assert!(c.contains("SUBJECT \"report\""), "{c}");
        assert!(c.contains("UNSEEN") && c.contains("FLAGGED"), "{c}");
        assert!(c.contains("BEFORE 25-Nov-2026"), "{c}");
        assert!(c.contains("SINCE 2-Jan-2026"), "{c}");
        assert!(c.starts_with("OR OR SUBJECT \"budget\""), "{c}");
    }

    #[test]
    fn has_attachment_is_a_post_filter() {
        let parsed = parse_query("has:attachment invoice").unwrap();
        assert!(parsed.needs_attachment_filter());
        assert!(!parsed.imap_criteria(&q).contains("attachment"));
        let only = parse_query("has:attachment").unwrap();
        assert_eq!(only.imap_criteria(&q), "ALL");
    }

    #[test]
    fn unknown_keys_and_addresses_stay_free_text() {
        let parsed = parse_query("meeting 10:30 anna@example.example").unwrap();
        assert_eq!(
            parsed.text,
            vec!["meeting", "10:30", "anna@example.example"]
        );
        let parsed = parse_query("label:work").unwrap();
        assert_eq!(parsed.text, vec!["label:work"]);
    }

    #[test]
    fn bad_queries_are_rejected() {
        assert!(parse_query("   ").is_err());
        assert!(parse_query("\"open").is_err());
        assert!(parse_query("is:weird").is_err());
        assert!(parse_query("before:2026-13-01").is_err());
        assert!(parse_query("before:soon").is_err());
        assert!(parse_query(&"a".repeat(257)).is_err());
    }

    #[test]
    fn subjects_normalize() {
        assert_eq!(
            normalize_subject("Re: RE:  Fwd: Hello  World"),
            ("hello world".into(), true)
        );
        assert_eq!(normalize_subject("Re[2]: Hi"), ("hi".into(), true));
        assert_eq!(
            normalize_subject("Receipt for you"),
            ("receipt for you".into(), false)
        );
        assert_eq!(normalize_subject("Ré: é"), ("ré: é".into(), false));
    }

    fn input<'a>(
        id: &'a str,
        reply: &'a str,
        refs: &'a [String],
        subject: &'a str,
    ) -> ThreadInput<'a> {
        ThreadInput {
            message_id: id,
            in_reply_to: reply,
            references: refs,
            subject,
        }
    }

    #[test]
    fn threads_follow_references() {
        let r1 = vec!["<a@x>".to_owned()];
        let r2 = vec!["<a@x>".to_owned(), "<b@x>".to_owned()];
        let none: Vec<String> = vec![];
        let items = [
            input("<c@x>", "<b@x>", &r2, "Re: Plan"),
            input("<b@x>", "<a@x>", &r1, "Re: Plan"),
            input("<a@x>", "", &none, "Plan"),
            input("<z@x>", "", &none, "Other"),
        ];
        let keys = thread_keys(&items);
        assert_eq!(keys[0], keys[1]);
        assert_eq!(keys[1], keys[2]);
        assert_ne!(keys[0], keys[3]);
    }

    #[test]
    fn siblings_of_a_missing_parent_group_together() {
        let r = vec!["<gone@x>".to_owned()];
        let items = [
            input("<s1@x>", "<gone@x>", &r, "Re: Hi"),
            input("<s2@x>", "<gone@x>", &r, "Re: Hi"),
        ];
        let keys = thread_keys(&items);
        assert_eq!(keys[0], keys[1]);
    }

    #[test]
    fn plain_duplicate_subjects_do_not_merge_but_reply_prefixes_do() {
        let none: Vec<String> = vec![];
        let items = [
            input("<1@x>", "", &none, "Hello"),
            input("<2@x>", "", &none, "Hello"),
        ];
        let keys = thread_keys(&items);
        assert_ne!(keys[0], keys[1]);
        let items = [
            input("<1@x>", "", &none, "Hello"),
            input("<2@x>", "", &none, "Re: Hello"),
        ];
        let keys = thread_keys(&items);
        assert_eq!(keys[0], keys[1]);
    }

    #[test]
    fn keys_are_stable_across_reordering() {
        let none: Vec<String> = vec![];
        let r = vec!["<a@x>".to_owned()];
        let a = input("<a@x>", "", &none, "Plan");
        let b = input("<b@x>", "<a@x>", &r, "Re: Plan");
        let forward = thread_keys(&[a, b]);
        let backward = thread_keys(&[b, a]);
        assert_eq!(forward[0], backward[1]);
        assert_eq!(forward[1], backward[0]);
    }

    #[test]
    fn messages_without_ids_or_subjects_stay_alone() {
        let none: Vec<String> = vec![];
        let items = [input("", "", &none, ""), input("", "", &none, "")];
        let keys = thread_keys(&items);
        assert_ne!(keys[0], keys[1]);
    }

    #[test]
    fn unsubscribe_keeps_only_safe_targets() {
        let u = parse_list_unsubscribe(
            "<mailto:unsub@news.example?subject=unsubscribe>, <https://news.example/u?id=1>",
            "List-Unsubscribe=One-Click",
        );
        assert_eq!(
            u.mailto.as_deref(),
            Some("unsub@news.example?subject=unsubscribe")
        );
        assert_eq!(u.url.as_deref(), Some("https://news.example/u?id=1"));
        assert!(u.one_click);

        let u = parse_list_unsubscribe("<http://plain.example/u>, <javascript:alert(1)>", "");
        assert_eq!(u, Unsubscribe::default());

        let u = parse_list_unsubscribe("<https://a.example/x y>", "");
        assert!(u.url.is_none());
    }

    #[test]
    fn mailto_splits_into_parts() {
        let (addr, subject, body) =
            split_mailto("unsub@news.example?subject=Please%20remove%20me&body=bye").unwrap();
        assert_eq!(addr, "unsub@news.example");
        assert_eq!(subject, "Please remove me");
        assert_eq!(body, "bye");
        assert!(split_mailto("not-an-address").is_none());
        assert!(split_mailto("a@b.example,c@d.example").is_none());
        let (_, subject, _) = split_mailto("a@b.example?subject=x%0D%0Ay").unwrap();
        assert!(!subject.contains('\n') && !subject.contains('\r'));
    }

    #[test]
    fn message_ids_are_extracted() {
        assert_eq!(
            message_ids("<a@x> (comment) <b@y>\r\n <c@z>"),
            vec!["<a@x>", "<b@y>", "<c@z>"]
        );
        assert!(message_ids("<>").is_empty());
        assert!(message_ids("<has space@x>").is_empty());
        assert!(message_ids("no ids").is_empty());
    }

    #[test]
    fn addresses_parse_names_quotes_and_commas() {
        let parsed = parse_addresses(
            "\"Doe, Jane\" <Jane@Example.Example>, bob@example.example; Carol <carol@example.example>, junk, <>",
        );
        assert_eq!(
            parsed,
            vec![
                ("Doe, Jane".to_owned(), "jane@example.example".to_owned()),
                (String::new(), "bob@example.example".to_owned()),
                ("Carol".to_owned(), "carol@example.example".to_owned()),
            ]
        );
        assert!(parse_addresses("a@b@c").is_empty());
        assert!(parse_addresses("").is_empty());
    }

    #[test]
    fn special_folders_are_found() {
        let folders: Vec<String> = ["INBOX", "INBOX.Sent", "INBOX.Drafts", "Archive"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(pick_folder(&folders, "drafts"), Some("INBOX.Drafts"));
        assert_eq!(pick_folder(&folders, "sent"), Some("INBOX.Sent"));
        assert_eq!(pick_folder(&folders, "trash"), None);
        let nested: Vec<String> = vec!["[Gmail]/Drafts".into(), "Drafts Backup".into()];
        assert_eq!(pick_folder(&nested, "drafts"), Some("[Gmail]/Drafts"));
    }

    #[test]
    fn appenduid_is_parsed() {
        assert_eq!(
            parse_appenduid("a4 OK [APPENDUID 1699 42] Append completed."),
            Some(42)
        );
        assert_eq!(parse_appenduid("a4 OK [APPENDUID 1699 7]"), Some(7));
        assert_eq!(parse_appenduid("a4 OK Append completed."), None);
        assert_eq!(parse_appenduid("a4 OK [APPENDUID 1699"), None);
    }

    #[test]
    fn snippets_skip_quotes_and_clip() {
        assert_eq!(
            snippet_from("> old\n\nHello   there\n\nSecond line"),
            "Hello there Second line"
        );
        let long = "word ".repeat(100);
        let s = snippet_from(&long);
        assert!(
            s.chars().count() <= SNIPPET_CHARS + 1,
            "{}",
            s.chars().count()
        );
        assert!(s.ends_with('…'));
        assert_eq!(snippet_from(""), "");
    }

    #[test]
    fn retry_schedule_and_staleness() {
        assert_eq!(retry_delay_secs(1), Some(60));
        assert_eq!(retry_delay_secs(2), Some(300));
        assert_eq!(retry_delay_secs(MAX_SEND_ATTEMPTS), None);
        assert!(is_stale_sending(None, 1000));
        assert!(!is_stale_sending(Some(900), 1000));
        assert!(is_stale_sending(Some(100), 1000));
    }
}
