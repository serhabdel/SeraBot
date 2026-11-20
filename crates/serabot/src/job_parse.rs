//! Job listing pages: the small robots.txt check and the heuristics that turn
//! a listing page's HTML into postings (title, employer, city, deadline, URL).
//!
//! Page text is untrusted data: it is only ever parsed into plain fields,
//! never followed as instructions. Per-site selectors are inferred from the
//! public page structure and are best effort; when a site's selectors find
//! nothing (the site changed, or it is a member-added source) the generic
//! link extraction takes over.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::jobs::{JobItem, fold, normalize_url};

/// Most postings read from one listing page.
pub const MAX_ITEMS_PER_PAGE: usize = 80;

// ---------------------------------------------------------------------------
// robots.txt
// ---------------------------------------------------------------------------

/// The product token of a user agent string: `SeraBot/0.70 (+x)` → `serabot`.
pub fn agent_token(user_agent: &str) -> String {
    user_agent
        .split(['/', ' ', '(', ';'])
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

struct Rule {
    allow: bool,
    pattern: String,
}

/// `*` matches any run, a trailing `$` anchors the end; otherwise prefix.
fn rule_matches(pattern: &str, path: &str) -> bool {
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(rest) => (rest, true),
        None => (pattern, false),
    };
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut position = 0usize;
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if index == 0 {
            if !path.starts_with(part) {
                return false;
            }
            position = part.len();
        } else {
            match path[position..].find(part) {
                Some(found) => position += found + part.len(),
                None => return false,
            }
        }
    }
    if anchored {
        let last = parts.last().copied().unwrap_or("");
        if last.is_empty() {
            return pattern.contains('*') || position == path.len();
        }
        return path.ends_with(last) && position <= path.len();
    }
    true
}

/// May `agent` fetch `path_and_query` according to this robots.txt body?
///
/// A group naming our product token wins over `*`; inside the group the
/// longest matching rule wins and `Allow` wins a tie. Nothing matching, or no
/// robots rules at all, means allowed.
pub fn robots_allows(robots: &str, agent: &str, path_and_query: &str) -> bool {
    let agent = agent_token(agent);
    let mut groups: Vec<(Vec<String>, Vec<Rule>)> = Vec::new();
    let mut agents: Vec<String> = Vec::new();
    let mut rules: Vec<Rule> = Vec::new();
    let mut in_rules = false;
    for line in robots.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        match key.as_str() {
            "user-agent" => {
                if in_rules {
                    groups.push((std::mem::take(&mut agents), std::mem::take(&mut rules)));
                    in_rules = false;
                }
                agents.push(value.to_ascii_lowercase());
            }
            "allow" | "disallow" => {
                in_rules = true;
                if !value.is_empty() {
                    rules.push(Rule {
                        allow: key == "allow",
                        pattern: value.to_owned(),
                    });
                }
            }
            _ => {}
        }
    }
    if !agents.is_empty() || !rules.is_empty() {
        groups.push((agents, rules));
    }
    let named = |names: &Vec<String>| {
        !agent.is_empty()
            && names
                .iter()
                .any(|name| name != "*" && agent.contains(name.as_str()))
    };
    let selected: Vec<&Rule> = if groups.iter().any(|(names, _)| named(names)) {
        groups
            .iter()
            .filter(|(names, _)| named(names))
            .flat_map(|(_, rules)| rules.iter())
            .collect()
    } else {
        groups
            .iter()
            .filter(|(names, _)| names.iter().any(|name| name == "*"))
            .flat_map(|(_, rules)| rules.iter())
            .collect()
    };
    let mut best: Option<(usize, bool)> = None;
    for rule in selected {
        if rule_matches(&rule.pattern, path_and_query) {
            let length = rule.pattern.len();
            best = match best {
                Some((len, allow)) if len > length || (len == length && allow) => {
                    Some((len, allow))
                }
                _ => Some((length, rule.allow)),
            };
        }
    }
    best.map(|(_, allow)| allow).unwrap_or(true)
}

/// The robots.txt address for a page address.
pub fn robots_url(page: &Url) -> Option<String> {
    let host = page.host_str()?;
    let mut base = format!("{}://{}", page.scheme(), host);
    if let Some(port) = page.port() {
        base.push_str(&format!(":{port}"));
    }
    Some(format!("{base}/robots.txt"))
}

/// Path plus query of a page address, as robots rules are written.
pub fn path_and_query(page: &Url) -> String {
    match page.query() {
        Some(query) => format!("{}?{query}", page.path()),
        None => page.path().to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Dates, cities, employers
// ---------------------------------------------------------------------------

const MOROCCAN_CITIES: &[&str] = &[
    "Casablanca",
    "Rabat",
    "Marrakech",
    "Tanger",
    "Fès",
    "Agadir",
    "Meknès",
    "Oujda",
    "Kénitra",
    "Tétouan",
    "Témara",
    "Mohammedia",
    "El Jadida",
    "Nador",
    "Béni Mellal",
    "Settat",
    "Safi",
    "Khouribga",
    "Laâyoune",
    "Dakhla",
    "Essaouira",
    "Ouarzazate",
    "Errachidia",
    "Taza",
    "Larache",
    "Berkane",
    "Khémisset",
    "Guelmim",
    "Al Hoceima",
    "Ifrane",
    "Benguerir",
    "Skhirat",
    "Bouskoura",
    "Nouaceur",
    "Tiznit",
    "Taroudant",
    "Azrou",
    "Khénifra",
    "Chefchaouen",
    "Ksar El Kébir",
    "Tan-Tan",
    "Sidi Kacem",
    "Sidi Slimane",
    "Fnideq",
    "Martil",
    "Ben Guerir",
];

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// The first Moroccan city named in `text` (whole words, accent-insensitive).
pub fn find_city(text: &str) -> Option<String> {
    let folded = fold(text);
    let mut best: Option<(usize, &str)> = None;
    for city in MOROCCAN_CITIES {
        let needle = fold(city);
        let mut from = 0;
        while let Some(found) = folded[from..].find(&needle) {
            let start = from + found;
            let end = start + needle.len();
            let before_ok = folded[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !is_word_char(c));
            let after_ok = folded[end..]
                .chars()
                .next()
                .is_none_or(|c| !is_word_char(c));
            if before_ok && after_ok {
                if best.is_none_or(|(at, _)| start < at) {
                    best = Some((start, city));
                }
                break;
            }
            from = end;
        }
    }
    // "Salé" folds to a common French word; only the accented form counts.
    if let Some(at) = text.find("Salé").or_else(|| text.find("SALÉ"))
        && best.is_none_or(|(start, _)| at < start)
    {
        return Some("Salé".to_owned());
    }
    best.map(|(_, city)| (*city).to_owned())
}

fn date_regexes() -> &'static [Regex; 3] {
    static CELL: OnceLock<[Regex; 3]> = OnceLock::new();
    CELL.get_or_init(|| {
        [
            Regex::new(r"\b(\d{1,2})[/\-.](\d{1,2})[/\-.](\d{4})\b").expect("date regex"),
            Regex::new(r"\b(\d{4})-(\d{2})-(\d{2})\b").expect("date regex"),
            Regex::new(
                r"\b(\d{1,2})(?:er)?\s+(janvier|fevrier|mars|avril|mai|juin|juillet|aout|septembre|octobre|novembre|decembre|january|february|march|april|may|june|july|august|september|october|november|december)\s+(\d{4})\b",
            )
            .expect("date regex"),
        ]
    })
}

fn month_number(name: &str) -> Option<u32> {
    const NAMES: [(&str, u32); 24] = [
        ("janvier", 1),
        ("january", 1),
        ("fevrier", 2),
        ("february", 2),
        ("mars", 3),
        ("march", 3),
        ("avril", 4),
        ("april", 4),
        ("mai", 5),
        ("may", 5),
        ("juin", 6),
        ("june", 6),
        ("juillet", 7),
        ("july", 7),
        ("aout", 8),
        ("august", 8),
        ("septembre", 9),
        ("september", 9),
        ("octobre", 10),
        ("october", 10),
        ("novembre", 11),
        ("november", 11),
        ("decembre", 12),
        ("december", 12),
    ];
    NAMES.iter().find(|(n, _)| *n == name).map(|(_, m)| *m)
}

/// Every plausible date in `text` (accent-folded), as `(byte offset, YYYY-MM-DD)`.
fn dates_in(folded: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let [dmy, ymd, long] = date_regexes();
    let mut push = |at: usize, y: i32, m: u32, d: u32| {
        if (2020..=2100).contains(&y)
            && let Some(date) = chrono::NaiveDate::from_ymd_opt(y, m, d)
        {
            out.push((at, date.format("%Y-%m-%d").to_string()));
        }
    };
    for caps in dmy.captures_iter(folded) {
        let at = caps.get(0).map(|m| m.start()).unwrap_or(0);
        push(
            at,
            caps[3].parse().unwrap_or(0),
            caps[2].parse().unwrap_or(0),
            caps[1].parse().unwrap_or(0),
        );
    }
    for caps in ymd.captures_iter(folded) {
        let at = caps.get(0).map(|m| m.start()).unwrap_or(0);
        push(
            at,
            caps[1].parse().unwrap_or(0),
            caps[2].parse().unwrap_or(0),
            caps[3].parse().unwrap_or(0),
        );
    }
    for caps in long.captures_iter(folded) {
        let at = caps.get(0).map(|m| m.start()).unwrap_or(0);
        if let Some(month) = month_number(&caps[2]) {
            push(
                at,
                caps[3].parse().unwrap_or(0),
                month,
                caps[1].parse().unwrap_or(0),
            );
        }
    }
    out.sort();
    out
}

const DEADLINE_WORDS: &[&str] = &[
    "date limite",
    "dernier delai",
    "delai de depot",
    "limite de depot",
    "deadline",
    "avant le",
    "cloture",
    "jusqu'au",
    "jusqu au",
    "apply before",
    "closing date",
    "date de cloture",
];

/// The deadline a text states: the first date after a deadline word. With
/// `latest_if_unlabelled` (table rows that list a publication date and a
/// closing date) the latest date in the text is used when no word is found.
pub fn find_deadline(text: &str, latest_if_unlabelled: bool) -> Option<String> {
    let folded = fold(text);
    let dates = dates_in(&folded);
    if dates.is_empty() {
        return None;
    }
    for word in DEADLINE_WORDS {
        if let Some(at) = folded.find(word)
            && let Some((_, date)) = dates
                .iter()
                .find(|(pos, _)| *pos >= at && *pos <= at + word.len() + 60)
        {
            return Some(date.clone());
        }
    }
    if latest_if_unlabelled {
        return dates.iter().map(|(_, d)| d.clone()).max();
    }
    None
}

fn employer_in_title(title: &str) -> Option<String> {
    static CELL: OnceLock<Regex> = OnceLock::new();
    let re = CELL.get_or_init(|| {
        Regex::new(r"(?i)\b(?:chez|recrute|recrutement\s+(?:de|d')\s+[^,]{0,40}?\bchez)\s+([^\-–—|,(\n]{2,60})")
            .expect("employer regex")
    });
    re.captures(title)
        .map(|caps| caps[1].trim().to_owned())
        .filter(|name| !name.is_empty())
}

// ---------------------------------------------------------------------------
// Sites
// ---------------------------------------------------------------------------

struct Site {
    host: &'static str,
    containers: &'static str,
    title: &'static str,
    /// Selector and optional attribute to read instead of the text.
    employer: &'static [(&'static str, Option<&'static str>)],
    city: &'static str,
    deadline: &'static str,
    /// Table rows list several dates; the latest is the closing date.
    row_dates: bool,
}

const SITES: &[Site] = &[
    // Concours table: one row per concours (organisme, intitulé, dates, lieu).
    Site {
        host: "emploi-public.ma",
        containers: "table tr",
        title: "a[href]",
        employer: &[("td:first-child", None)],
        city: "td",
        deadline: "td",
        row_dates: true,
    },
    // WordPress theme: one <article> per announcement.
    Site {
        host: "alwadifa-maroc.com",
        containers: "article, div.post, li.post",
        title: "h2 a, h3 a, .entry-title a, .post-title a",
        employer: &[],
        city: ".location, .post-meta",
        deadline: ".entry-content, .excerpt, .post-excerpt, p",
        row_dates: false,
    },
    Site {
        host: "anapec.org",
        containers: "tr, article, li.offre, div.offre",
        title: "a[href]",
        employer: &[("td:nth-child(2)", None)],
        city: "td",
        deadline: "td",
        row_dates: false,
    },
    // Each offer is a block with a `titreJob` link; the logo's alt is the employer.
    Site {
        host: "rekrute.com",
        containers: "li.post-id, div.post-id, div.section",
        title: "a.titreJob",
        employer: &[("img[alt]", Some("alt"))],
        city: ".info, .holder",
        deadline: ".info, .holder",
        row_dates: false,
    },
    Site {
        host: "emploi.ma",
        containers: ".card-job, article.node, .job-description-wrapper, div.views-row",
        title: "h3 a, h2 a, .card-job-title a, a[href*='/offre-emploi']",
        employer: &[
            (".card-job-company a", None),
            (".card-job-company", None),
            (".company-name", None),
        ],
        city: ".card-job-detail-location, .job-location, .location",
        deadline: ".card-job-detail, .job-details",
        row_dates: false,
    },
    Site {
        host: "dreamjob.ma",
        containers: "article",
        title: "h2 a, h3 a, .entry-title a",
        employer: &[],
        city: ".entry-meta, .post-meta",
        deadline: ".entry-content, .excerpt, p",
        row_dates: false,
    },
    Site {
        host: "stagiaire.ma",
        containers: "article",
        title: "h2 a, h3 a, .entry-title a",
        employer: &[],
        city: ".entry-meta, .post-meta",
        deadline: ".entry-content, .excerpt, p",
        row_dates: false,
    },
    Site {
        host: "marocannonces.com",
        containers: "ul.cars-list li, li.annonce, div.annonce",
        title: "h3 a, a[href*='/annonce']",
        employer: &[],
        city: ".location, .annonce-city",
        deadline: "",
        row_dates: false,
    },
];

fn site_for(page: &Url) -> Option<&'static Site> {
    let host = page.host_str()?.to_ascii_lowercase();
    SITES
        .iter()
        .find(|site| host == site.host || host.ends_with(&format!(".{}", site.host)))
}

fn text_of(element: ElementRef<'_>) -> String {
    element
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn selector(css: &str) -> Option<Selector> {
    if css.trim().is_empty() {
        return None;
    }
    Selector::parse(css).ok()
}

fn resolve(base: &Url, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty()
        || href.starts_with('#')
        || href.starts_with("javascript:")
        || href.starts_with("mailto:")
        || href.starts_with("tel:")
    {
        return None;
    }
    let mut joined = base.join(href).ok()?;
    if !matches!(joined.scheme(), "http" | "https") {
        return None;
    }
    joined.set_fragment(None);
    Some(joined.to_string())
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Build an item from the pieces found around one link.
#[allow(clippy::too_many_arguments)]
fn make_item(
    title_text: &str,
    url: String,
    context: &str,
    employer: Option<String>,
    city_text: &str,
    deadline_text: &str,
    row_dates: bool,
    source: &str,
) -> Option<JobItem> {
    let mut title = title_text.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.chars().count() < 6 {
        return None;
    }
    let mut city = find_city(city_text);
    // "Comptable | Casablanca (Maroc)": the part after the bar is the place.
    if let Some((head, tail)) = title.split_once(" | ")
        && let Some(found) = find_city(tail)
    {
        city.get_or_insert(found);
        title = head.trim().to_owned();
    }
    if city.is_none() {
        city = find_city(&title).or_else(|| find_city(context));
    }
    let deadline = find_deadline(deadline_text, row_dates)
        .or_else(|| find_deadline(context, row_dates))
        .or_else(|| find_deadline(&title, false));
    let employer = employer
        .map(|e| e.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|e| !e.is_empty() && e.chars().count() <= 100 && e != &title)
        .or_else(|| employer_in_title(&title));
    Some(JobItem {
        title: truncate(&title, 200),
        employer,
        city,
        deadline,
        url,
        summary: truncate(context, 300),
        source: source.to_owned(),
    })
}

fn parse_with_site(site: &Site, base: &Url, doc: &Html, source: &str) -> Vec<JobItem> {
    let (Some(containers), Some(title_sel)) = (selector(site.containers), selector(site.title))
    else {
        return Vec::new();
    };
    let city_sel = selector(site.city);
    let deadline_sel = selector(site.deadline);
    let anchor = selector("a[href]");
    let mut out = Vec::new();
    for container in doc.select(&containers) {
        let link = container
            .select(&title_sel)
            .find(|el| el.value().attr("href").is_some())
            .or_else(|| {
                // The selector matched a heading: take the link inside it.
                container
                    .select(&title_sel)
                    .find_map(|el| anchor.as_ref().and_then(|a| el.select(a).next()))
            });
        let Some(link) = link else { continue };
        let Some(url) = link.value().attr("href").and_then(|h| resolve(base, h)) else {
            continue;
        };
        let context = text_of(container);
        let title_text = text_of(link);
        let employer = site.employer.iter().find_map(|(css, attr)| {
            let sel = selector(css)?;
            container.select(&sel).find_map(|el| {
                let value = match attr {
                    Some(name) => el.value().attr(name).unwrap_or("").to_owned(),
                    None => text_of(el),
                };
                (!value.trim().is_empty()).then_some(value)
            })
        });
        let join_texts = |sel: &Option<Selector>| {
            sel.as_ref()
                .map(|s| {
                    container
                        .select(s)
                        .map(text_of)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        };
        let city_text = join_texts(&city_sel);
        let deadline_text = join_texts(&deadline_sel);
        if let Some(item) = make_item(
            &title_text,
            url,
            &context,
            employer,
            &city_text,
            &deadline_text,
            site.row_dates,
            source,
        ) {
            out.push(item);
        }
    }
    out
}

const NAV_WORDS: &[&str] = &[
    "accueil",
    "contact",
    "connexion",
    "login",
    "inscription",
    "mon compte",
    "politique",
    "mentions legales",
    "cookies",
    "newsletter",
    "facebook",
    "twitter",
    "linkedin",
    "instagram",
    "youtube",
    "a propos",
    "plan du site",
    "lire la suite",
    "voir plus",
    "voir tout",
    "suivant",
    "precedent",
    "next",
    "previous",
    "read more",
    "publier",
    "deposer",
    "cgu",
    "conditions",
    "faq",
    "aide",
    "home",
    "privacy",
    "terms",
];

const JOB_WORDS: &[&str] = &[
    "recrut",
    "concours",
    "offre",
    "emploi",
    "stage",
    "stagiaire",
    "poste",
    "h/f",
    "f/h",
    "cdi",
    "cdd",
    "ingenieur",
    "technicien",
    "comptable",
    "assistant",
    "responsable",
    "directeur",
    "charge de",
    "developpeur",
    "analyste",
    "consultant",
    "alternance",
    "job",
    "career",
    "vacancy",
    "opportunit",
    "annonce",
    "profil",
];

fn looks_like_job_link(href: &str, text: &str) -> bool {
    let text_folded = fold(text);
    let count = text.chars().count();
    if !(8..=180).contains(&count) {
        return false;
    }
    if NAV_WORDS
        .iter()
        .any(|word| text_folded == *word || text_folded.starts_with(word))
    {
        return false;
    }
    if text
        .chars()
        .all(|c| c.is_ascii_digit() || c.is_whitespace())
    {
        return false;
    }
    let href_folded = fold(href);
    JOB_WORDS
        .iter()
        .any(|word| href_folded.contains(word) || text_folded.contains(word))
}

/// Links plus the text around them: works on any listing page.
fn parse_generic(base: &Url, doc: &Html, source: &str) -> Vec<JobItem> {
    let Some(anchors) = selector("a[href]") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for link in doc.select(&anchors) {
        let Some(url) = link.value().attr("href").and_then(|h| resolve(base, h)) else {
            continue;
        };
        let title = text_of(link);
        let title = if title.is_empty() {
            link.value().attr("title").unwrap_or("").trim().to_owned()
        } else {
            title
        };
        if !looks_like_job_link(&url, &title) || normalize_url(&url) == normalize_url(base.as_str())
        {
            continue;
        }
        if !seen.insert(normalize_url(&url)) {
            continue;
        }
        // The block around the link: up to two levels of li/tr/article/div.
        let mut context = String::new();
        let mut node = link.parent().and_then(ElementRef::wrap);
        for _ in 0..3 {
            let Some(element) = node else { break };
            if matches!(
                element.value().name(),
                "li" | "tr" | "article" | "div" | "section"
            ) {
                let text = text_of(element);
                if text.chars().count() <= 600 {
                    context = text;
                } else {
                    break;
                }
            }
            node = element.parent().and_then(ElementRef::wrap);
        }
        if let Some(item) = make_item(
            &title, url, &context, None, &context, &context, false, source,
        ) {
            out.push(item);
        }
        if out.len() >= MAX_ITEMS_PER_PAGE {
            break;
        }
    }
    out
}

/// Parse one listing page. `page_url` is the address the page came from (it
/// resolves relative links and picks the per-site rules).
pub fn parse_listing(source: &str, page_url: &str, html: &str) -> Vec<JobItem> {
    let Ok(base) = Url::parse(page_url) else {
        return Vec::new();
    };
    let doc = Html::parse_document(html);
    let mut items = site_for(&base)
        .map(|site| parse_with_site(site, &base, &doc, source))
        .unwrap_or_default();
    if items.is_empty() {
        items = parse_generic(&base, &doc, source);
    }
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(normalize_url(&item.url)));
    items.truncate(MAX_ITEMS_PER_PAGE);
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn robots_group_selection_and_longest_match() {
        let body = "\
# comment
User-agent: *
Disallow: /private/
Disallow: /offres?
Allow: /private/open

User-agent: badbot
Disallow: /
";
        let ua = "SeraBot/0.70 (+https://example.org)";
        assert!(robots_allows(body, ua, "/offres.html"));
        assert!(!robots_allows(body, ua, "/offres?page=2"));
        assert!(!robots_allows(body, ua, "/private/x"));
        assert!(robots_allows(body, ua, "/private/open/y"));
        assert!(!robots_allows(body, "BadBot/1", "/anything"));
        // A group for our own token replaces the * group.
        let own = "User-agent: *\nDisallow: /\n\nUser-agent: serabot\nDisallow: /secret\n";
        assert!(robots_allows(own, ua, "/offres"));
        assert!(!robots_allows(own, ua, "/secret/a"));
        assert!(!robots_allows(own, "Other/1", "/offres"));
        // Empty Disallow, empty file, wildcards and anchors.
        assert!(robots_allows("User-agent: *\nDisallow:\n", ua, "/x"));
        assert!(robots_allows("", ua, "/x"));
        let wild = "User-agent: *\nDisallow: /*.pdf$\nDisallow: /a/*/b\n";
        assert!(!robots_allows(wild, ua, "/doc/cv.pdf"));
        assert!(robots_allows(wild, ua, "/doc/cv.pdf?x=1"));
        assert!(!robots_allows(wild, ua, "/a/zzz/b/c"));
        assert!(robots_allows(wild, ua, "/a/zzz/c"));
        // Several agents sharing one group.
        let shared = "User-agent: foo\nUser-agent: serabot\nDisallow: /x\n";
        assert!(!robots_allows(shared, ua, "/x"));
        assert_eq!(agent_token(ua), "serabot");
    }

    #[test]
    fn robots_url_and_path() {
        let page = Url::parse("https://www.example.ma:8443/a/b?c=1#f").unwrap();
        assert_eq!(
            robots_url(&page).unwrap(),
            "https://www.example.ma:8443/robots.txt"
        );
        assert_eq!(path_and_query(&page), "/a/b?c=1");
    }

    #[test]
    fn deadlines_cities_and_employers() {
        assert_eq!(
            find_deadline("Date limite : 15/12/2026", false).as_deref(),
            Some("2026-12-15")
        );
        assert_eq!(
            find_deadline("avant le 3 décembre 2026 svp", false).as_deref(),
            Some("2026-12-03")
        );
        assert_eq!(
            find_deadline("Deadline 2026-01-31", false).as_deref(),
            Some("2026-01-31")
        );
        assert_eq!(find_deadline("Publié le 01/11/2026", false), None);
        assert_eq!(
            find_deadline("Publié le 01/11/2026 clôture 20/11/2026", true).as_deref(),
            Some("2026-11-20")
        );
        assert_eq!(find_deadline("Date limite 31/02/2026", false), None);
        assert_eq!(
            find_city("Poste basé à Kénitra (Maroc)").as_deref(),
            Some("Kénitra")
        );
        assert_eq!(find_city("Salé médina").as_deref(), Some("Salé"));
        assert_eq!(find_city("vente sale et propre"), None);
        assert_eq!(
            find_city("Recrutement Fès et Rabat").as_deref(),
            Some("Fès")
        );
        assert_eq!(
            employer_in_title("Recrutement chez Maroc Telecom - Rabat").as_deref(),
            Some("Maroc Telecom")
        );
    }

    const REKRUTE: &str = r#"<html><body><ul>
      <li class="post-id"><div class="holder"><img alt="Banque Alpha" src="/l.png">
        <a class="titreJob" href="/offre-emploi-comptable-123.html">Comptable Senior | Casablanca (Maroc)</a>
        <div class="info">Publication 10/11/2026 - Postes: 2</div></div></li>
      <li class="post-id"><div class="holder">
        <a class="titreJob" href="/offre-emploi-data-456.html?utm_source=x">Data Analyst | Rabat (Maroc)</a></div></li>
      <li><a href="/contact">Contact</a></li></ul></body></html>"#;

    #[test]
    fn parses_rekrute_style_listing() {
        let items = parse_listing("rekrute", "https://www.rekrute.com/offres.html", REKRUTE);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Comptable Senior");
        assert_eq!(items[0].employer.as_deref(), Some("Banque Alpha"));
        assert_eq!(items[0].city.as_deref(), Some("Casablanca"));
        assert_eq!(
            items[0].url,
            "https://www.rekrute.com/offre-emploi-comptable-123.html"
        );
        assert_eq!(items[1].city.as_deref(), Some("Rabat"));
        assert_eq!(items[1].employer, None);
    }

    #[test]
    fn parses_emploi_public_table_with_row_dates() {
        let html = r#"<table>
          <tr><th>Organisme</th><th>Concours</th><th>Dates</th></tr>
          <tr><td>Ministère de la Santé</td>
              <td><a href="/fr/concours/777">Concours de recrutement de 20 infirmiers à Agadir</a></td>
              <td>Publié 02/11/2026</td><td>Dernier délai 30/11/2026</td></tr>
          <tr><td>Commune de Salé</td>
              <td><a href="https://www.emploi-public.ma/fr/concours/778">Concours rédacteur</a></td>
              <td>05/11/2026</td><td>12/12/2026</td></tr></table>"#;
        let items = parse_listing(
            "emploi-public",
            "https://www.emploi-public.ma/fr/concoursListe.asp",
            html,
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].employer.as_deref(), Some("Ministère de la Santé"));
        assert_eq!(items[0].city.as_deref(), Some("Agadir"));
        assert_eq!(items[0].deadline.as_deref(), Some("2026-11-30"));
        assert_eq!(items[1].deadline.as_deref(), Some("2026-12-12"));
        assert_eq!(items[1].city.as_deref(), Some("Salé"));
    }

    #[test]
    fn parses_wordpress_article_sites() {
        let html = r#"<main>
          <article><h2 class="entry-title"><a href="/recrutement-chez-ocp-2026/">Recrutement chez OCP – Benguerir</a></h2>
            <div class="entry-content">Date limite : 25 novembre 2026. Profil ingénieur.</div></article>
          <article><h3><a href="/stage-pfe-marketing/">Stage PFE Marketing Digital</a></h3>
            <p>Casablanca - 6 mois</p></article></main>"#;
        for (key, url) in [
            ("dreamjob", "https://www.dreamjob.ma/"),
            ("alwadifa", "https://www.alwadifa-maroc.com/"),
            ("stagiaire", "https://www.stagiaire.ma/"),
        ] {
            let items = parse_listing(key, url, html);
            assert_eq!(items.len(), 2, "{key}");
            assert_eq!(items[0].employer.as_deref(), Some("OCP"), "{key}");
            assert_eq!(items[0].city.as_deref(), Some("Benguerir"), "{key}");
            assert_eq!(items[0].deadline.as_deref(), Some("2026-11-25"), "{key}");
            assert_eq!(items[1].city.as_deref(), Some("Casablanca"), "{key}");
            assert!(items[1].url.ends_with("/stage-pfe-marketing/"), "{key}");
        }
    }

    #[test]
    fn parses_emploi_ma_cards_and_marocannonces_list() {
        let cards = r#"<div class="card-job"><h3><a href="/offre-emploi-maroc/chef-projet-1">Chef de projet IT</a></h3>
            <div class="card-job-company"><a href="/e/1">Société Gamma</a></div>
            <div class="card-job-detail-location">Tanger</div></div>"#;
        let items = parse_listing(
            "emploi-ma",
            "https://www.emploi.ma/offres-emploi-maroc",
            cards,
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].employer.as_deref(), Some("Société Gamma"));
        assert_eq!(items[0].city.as_deref(), Some("Tanger"));

        let classifieds = r#"<ul class="cars-list">
            <li><a href="/annonce/12345/vendeur"><h3>Vendeur H/F</h3></a><span class="location">Marrakech</span></li>
            <li><a href="/annonce/12346/comptable"><h3>Comptable expérimenté</h3></a><span class="location">Oujda</span></li></ul>"#;
        let items = parse_listing(
            "marocannonces",
            "https://www.marocannonces.com/categorie/309/Emploi/offres-emploi.html",
            classifieds,
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].city.as_deref(), Some("Oujda"));
        assert_eq!(
            items[0].url,
            "https://www.marocannonces.com/annonce/12345/vendeur"
        );
    }

    #[test]
    fn generic_fallback_for_unknown_sites_and_changed_layouts() {
        let html = r##"<html><body>
          <nav><a href="/">Accueil</a><a href="/login">Connexion</a><a href="#top">Haut de page</a>
               <a href="mailto:a@b.c">Nous écrire par mail</a></nav>
          <div class="row"><a href="/jobs/45">Ingénieur logiciel confirmé</a> <span>Tétouan</span>
              <span>Date limite : 01/12/2026</span></div>
          <div class="row"><a href="/jobs/45#again">Ingénieur logiciel confirmé</a></div>
          <div class="row"><a href="https://other.example.org/offre/9">Technicien maintenance (CDI)</a></div>
          <p><a href="/blog/hello">Un article sans rapport</a></p></body></html>"##;
        let items = parse_listing("c-abc", "https://boards.example.org/list", html);
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(items[0].title, "Ingénieur logiciel confirmé");
        assert_eq!(items[0].city.as_deref(), Some("Tétouan"));
        assert_eq!(items[0].deadline.as_deref(), Some("2026-12-01"));
        assert_eq!(items[1].url, "https://other.example.org/offre/9");

        // A known site whose layout changed falls back to links.
        let changed = r#"<div><a href="/offre-emploi-chef-comptable-9.html">Chef comptable - Casablanca</a></div>"#;
        let items = parse_listing("rekrute", "https://www.rekrute.com/offres.html", changed);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].city.as_deref(), Some("Casablanca"));
        assert!(parse_listing("x", "not a url", changed).is_empty());
        assert!(parse_listing("x", "https://a.example.org/", "").is_empty());
    }
}
