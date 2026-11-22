//! Job watcher data (0.70): the member's job profile, the built-in and
//! member-added job sources, postings already seen, profile matching and the
//! application pipeline.
//!
//! Every row is UID-scoped: each store method takes the acting user's id and
//! there is no unscoped path to another member's profile, sources, matches or
//! applications. Page fetching and parsing live in `job_parse` and
//! `job_watch`; this module is plain data and rules.

use chrono::{NaiveDate, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::new_id;
use crate::error::{Result, SeraError};
use crate::store::Store;
use crate::util::sha256_hex;

pub const MAX_LIST_ITEMS: usize = 20;
pub const MAX_TERM_CHARS: usize = 80;
pub const MAX_PATH_CHARS: usize = 300;
pub const MAX_CUSTOM_SOURCES: usize = 10;
/// Postings kept per member in the seen history.
pub const MAX_SEEN_PER_USER: usize = 1_000;
pub const MAX_APPLICATIONS: usize = 500;
pub const MAX_ARTIFACTS_PER_APPLICATION: usize = 10;

pub const SECTORS: [&str; 3] = ["public", "private", "internship"];
pub const SENIORITIES: [&str; 6] = ["any", "entry", "junior", "mid", "senior", "manager"];
pub const FREQUENCIES: [&str; 4] = ["daily", "twice_daily", "weekly", "off"];
pub const APPLICATION_STATUSES: [&str; 5] = ["saved", "applied", "interview", "offer", "rejected"];

// ---------------------------------------------------------------------------
// Profile
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct JobProfile {
    /// Target roles or fields ("data analyst", "comptable").
    #[serde(default)]
    pub roles: Vec<String>,
    /// Cities; "remote" means remote work is fine.
    #[serde(default)]
    pub cities: Vec<String>,
    /// Any of `public`, `private`, `internship`. Empty means all.
    #[serde(default)]
    pub sectors: Vec<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default = "default_seniority")]
    pub seniority: String,
    #[serde(default)]
    pub keywords_include: Vec<String>,
    #[serde(default)]
    pub keywords_exclude: Vec<String>,
    #[serde(default)]
    pub excluded_employers: Vec<String>,
    /// `daily`, `twice_daily`, `weekly` or `off`.
    #[serde(default = "default_frequency")]
    pub frequency: String,
    /// The member's own OneDrive Excel tracker (optional).
    #[serde(default)]
    pub onedrive_tracker_path: Option<String>,
    /// The member's OneDrive folder with CVs, letters and diplomas (optional).
    #[serde(default)]
    pub onedrive_materials_folder: Option<String>,
    /// Put deadlines of public concours in the member's tasks.
    #[serde(default)]
    pub deadlines_to_tasks: bool,
    #[serde(default)]
    pub updated_at: String,
}

impl Default for JobProfile {
    fn default() -> Self {
        Self {
            roles: Vec::new(),
            cities: Vec::new(),
            sectors: Vec::new(),
            languages: Vec::new(),
            seniority: default_seniority(),
            keywords_include: Vec::new(),
            keywords_exclude: Vec::new(),
            excluded_employers: Vec::new(),
            frequency: default_frequency(),
            onedrive_tracker_path: None,
            onedrive_materials_folder: None,
            deadlines_to_tasks: false,
            updated_at: String::new(),
        }
    }
}

fn default_seniority() -> String {
    "any".into()
}

fn default_frequency() -> String {
    "daily".into()
}

fn plain(value: &str) -> bool {
    !value.chars().any(|c| c.is_control())
}

/// Trim, drop empties and case-insensitive duplicates, bound count and size.
fn clean_list(label: &str, values: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if value.is_empty() {
            continue;
        }
        if !plain(&value) || value.chars().count() > MAX_TERM_CHARS {
            return Err(SeraError::Validation(format!(
                "{label}: each entry must be plain text of at most {MAX_TERM_CHARS} characters"
            )));
        }
        if out.iter().any(|seen| seen.eq_ignore_ascii_case(&value)) {
            continue;
        }
        out.push(value);
    }
    if out.len() > MAX_LIST_ITEMS {
        return Err(SeraError::Validation(format!(
            "{label}: at most {MAX_LIST_ITEMS} entries"
        )));
    }
    Ok(out)
}

fn clean_path(label: &str, value: &Option<String>) -> Result<Option<String>> {
    let Some(raw) = value else { return Ok(None) };
    let raw = raw.trim().replace('\\', "/");
    let trimmed = raw.trim_matches('/');
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_PATH_CHARS
        || !plain(trimmed)
        || trimmed
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || trimmed.contains([':', '*', '?', '<', '>', '|', '"'])
    {
        return Err(SeraError::Validation(format!(
            "{label}: a OneDrive path like Documents/Jobs/tracker.xlsx (no '..', at most {MAX_PATH_CHARS} characters)"
        )));
    }
    Ok(Some(trimmed.to_owned()))
}

impl JobProfile {
    /// Validate and normalise. Returns the cleaned copy.
    pub fn validated(&self) -> Result<JobProfile> {
        let mut sectors = Vec::new();
        for sector in clean_list("sectors", &self.sectors)? {
            let lower = sector.to_lowercase();
            if !SECTORS.contains(&lower.as_str()) {
                return Err(SeraError::Validation(
                    "sectors: use public, private or internship".into(),
                ));
            }
            if !sectors.contains(&lower) {
                sectors.push(lower);
            }
        }
        let seniority = self.seniority.trim().to_lowercase();
        if !SENIORITIES.contains(&seniority.as_str()) {
            return Err(SeraError::Validation(format!(
                "seniority: one of {}",
                SENIORITIES.join(", ")
            )));
        }
        let frequency = self.frequency.trim().to_lowercase();
        if !FREQUENCIES.contains(&frequency.as_str()) {
            return Err(SeraError::Validation(format!(
                "frequency: one of {}",
                FREQUENCIES.join(", ")
            )));
        }
        Ok(JobProfile {
            roles: clean_list("roles", &self.roles)?,
            cities: clean_list("cities", &self.cities)?,
            sectors,
            languages: clean_list("languages", &self.languages)?,
            seniority,
            keywords_include: clean_list("keywords_include", &self.keywords_include)?,
            keywords_exclude: clean_list("keywords_exclude", &self.keywords_exclude)?,
            excluded_employers: clean_list("excluded_employers", &self.excluded_employers)?,
            frequency,
            onedrive_tracker_path: clean_path(
                "onedrive_tracker_path",
                &self.onedrive_tracker_path,
            )?,
            onedrive_materials_folder: clean_path(
                "onedrive_materials_folder",
                &self.onedrive_materials_folder,
            )?,
            deadlines_to_tasks: self.deadlines_to_tasks,
            updated_at: String::new(),
        })
    }

    /// Minutes between scheduled runs; `None` when the watch is off.
    pub fn interval_minutes(&self) -> Option<i64> {
        match self.frequency.as_str() {
            "daily" => Some(22 * 60),
            "twice_daily" => Some(11 * 60),
            "weekly" => Some(6 * 24 * 60 + 20 * 60),
            _ => None,
        }
    }

    /// A short plain summary for chat and cards.
    pub fn summary(&self) -> String {
        let join = |v: &[String]| {
            if v.is_empty() {
                "any".to_owned()
            } else {
                v.join(", ")
            }
        };
        format!(
            "Roles: {} | Cities: {} | Sectors: {} | Languages: {} | Level: {} | Check: {}",
            join(&self.roles),
            join(&self.cities),
            join(&self.sectors),
            join(&self.languages),
            self.seniority,
            self.frequency.replace('_', " ")
        )
    }
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// A built-in Moroccan source. The listing URLs are the public listing pages
/// as best known; a member can always add a corrected one as their own.
pub struct BuiltinSource {
    pub key: &'static str,
    pub label: &'static str,
    pub url: &'static str,
    pub sectors: &'static [&'static str],
}

pub const BUILTIN_SOURCES: &[BuiltinSource] = &[
    BuiltinSource {
        key: "emploi-public",
        label: "emploi-public.ma (concours)",
        url: "https://www.emploi-public.ma/fr/concoursListe.asp",
        sectors: &["public"],
    },
    BuiltinSource {
        key: "alwadifa",
        label: "alwadifa-maroc.com",
        url: "https://www.alwadifa-maroc.com/",
        sectors: &["public"],
    },
    BuiltinSource {
        key: "anapec",
        label: "anapec.org",
        url: "https://anapec.org/home-page-o1/chercher-un-emploi/",
        sectors: &["public", "private"],
    },
    BuiltinSource {
        key: "rekrute",
        label: "rekrute.com",
        url: "https://www.rekrute.com/offres.html",
        sectors: &["private"],
    },
    BuiltinSource {
        key: "emploi-ma",
        label: "emploi.ma",
        url: "https://www.emploi.ma/offres-emploi-maroc",
        sectors: &["private"],
    },
    BuiltinSource {
        key: "dreamjob",
        label: "dreamjob.ma",
        url: "https://www.dreamjob.ma/",
        sectors: &["private", "internship"],
    },
    BuiltinSource {
        key: "stagiaire",
        label: "stagiaire.ma",
        url: "https://www.stagiaire.ma/",
        sectors: &["internship"],
    },
    BuiltinSource {
        key: "marocannonces",
        label: "marocannonces.com (jobs)",
        url: "https://www.marocannonces.com/categorie/309/Emploi/offres-emploi.html",
        sectors: &["private", "internship"],
    },
];

/// One source as a member sees it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct JobSource {
    pub key: String,
    pub label: String,
    pub url: String,
    pub builtin: bool,
    pub auto_watch: bool,
    /// Sectors this source covers; empty for member-added ones (any).
    pub sectors: Vec<String>,
}

/// Validate a member-added source URL: https only, a public-looking host.
pub fn validate_source_url(raw: &str) -> Result<url::Url> {
    let parsed = url::Url::parse(raw.trim())
        .map_err(|_| SeraError::Validation("source: not a valid web address".into()))?;
    if parsed.scheme() != "https" {
        return Err(SeraError::Validation(
            "source: use an https web address".into(),
        ));
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Err(SeraError::Validation(
            "source: no credentials in the address".into(),
        ));
    }
    match parsed.host_str() {
        Some(host) if host.contains('.') => {}
        _ => {
            return Err(SeraError::Validation(
                "source: the address needs a public host name".into(),
            ));
        }
    }
    if parsed.as_str().len() > 500 {
        return Err(SeraError::Validation("source: address too long".into()));
    }
    Ok(parsed)
}

fn custom_key(url: &url::Url) -> String {
    format!("c-{}", &sha256_hex(url.as_str().as_bytes())[..10])
}

impl Store {
    pub fn job_profile(&self, user_id: &str) -> Result<Option<JobProfile>> {
        let connection = self.connection()?;
        let row: Option<(String, String)> = connection
            .query_row(
                "SELECT profile, updated_at FROM job_profiles WHERE user_id = ?1",
                params![user_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((json, updated_at)) = row else {
            return Ok(None);
        };
        let mut profile: JobProfile = serde_json::from_str(&json)?;
        profile.updated_at = updated_at;
        Ok(Some(profile))
    }

    /// Validate and save the member's profile (replaces the previous one).
    pub fn set_job_profile(&self, user_id: &str, profile: &JobProfile) -> Result<JobProfile> {
        let mut clean = profile.validated()?;
        let now = Utc::now().to_rfc3339();
        let json = serde_json::to_string(&clean)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO job_profiles(user_id, profile, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(user_id) DO UPDATE SET profile = excluded.profile,
                updated_at = excluded.updated_at",
            params![user_id, json, now],
        )?;
        clean.updated_at = now;
        Ok(clean)
    }

    /// Members with a profile (the scheduler's work list).
    pub fn job_profile_owners(&self) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT user_id FROM job_profiles")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The member's effective sources: the built-in list with their own
    /// auto-watch switches, then the ones they added.
    pub fn job_sources(&self, user_id: &str) -> Result<Vec<JobSource>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT source_key, builtin, url, label, auto_watch FROM job_sources
             WHERE user_id = ?1 ORDER BY created_at, source_key",
        )?;
        let rows = statement
            .query_map(params![user_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? != 0,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)? != 0,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out: Vec<JobSource> = BUILTIN_SOURCES
            .iter()
            .map(|builtin| JobSource {
                key: builtin.key.into(),
                label: builtin.label.into(),
                url: builtin.url.into(),
                builtin: true,
                auto_watch: rows
                    .iter()
                    .find(|row| row.1 && row.0 == builtin.key)
                    .map(|row| row.4)
                    .unwrap_or(true),
                sectors: builtin.sectors.iter().map(|s| (*s).to_owned()).collect(),
            })
            .collect();
        for (key, builtin, url, label, auto_watch) in rows {
            if builtin {
                continue;
            }
            out.push(JobSource {
                label: if label.is_empty() {
                    url::Url::parse(&url)
                        .ok()
                        .and_then(|u| u.host_str().map(str::to_owned))
                        .unwrap_or_else(|| url.clone())
                } else {
                    label
                },
                key,
                url,
                builtin: false,
                auto_watch,
                sectors: Vec::new(),
            });
        }
        Ok(out)
    }

    /// Add a member source (bounded). Adding the same address again keeps
    /// one row and updates its label.
    pub fn add_job_source(
        &self,
        user_id: &str,
        raw_url: &str,
        label: Option<&str>,
    ) -> Result<JobSource> {
        let parsed = validate_source_url(raw_url)?;
        let label = label.map(str::trim).unwrap_or("");
        if label.chars().count() > 60 || !plain(label) {
            return Err(SeraError::Validation(
                "source label: at most 60 plain characters".into(),
            ));
        }
        let key = custom_key(&parsed);
        let existing = self.job_sources(user_id)?;
        let custom = existing.iter().filter(|s| !s.builtin).count();
        if !existing.iter().any(|s| s.key == key) && custom >= MAX_CUSTOM_SOURCES {
            return Err(SeraError::Validation(format!(
                "at most {MAX_CUSTOM_SOURCES} sources of your own; remove one first"
            )));
        }
        {
            let connection = self.connection()?;
            connection.execute(
                "INSERT INTO job_sources(user_id, source_key, builtin, url, label, auto_watch, created_at)
                 VALUES (?1, ?2, 0, ?3, ?4, 1, ?5)
                 ON CONFLICT(user_id, source_key) DO UPDATE SET label = excluded.label",
                params![user_id, key, parsed.as_str(), label, Utc::now().to_rfc3339()],
            )?;
        }
        self.job_sources(user_id)?
            .into_iter()
            .find(|source| source.key == key)
            .ok_or_else(|| SeraError::Internal("source insert lost".into()))
    }

    /// Remove one of the member's own sources. Built-in ones cannot be
    /// removed (switch auto-watch off instead).
    pub fn remove_job_source(&self, user_id: &str, key: &str) -> Result<bool> {
        if BUILTIN_SOURCES.iter().any(|b| b.key == key) {
            return Err(SeraError::Validation(
                "built-in sources cannot be removed; switch auto-watch off instead".into(),
            ));
        }
        let connection = self.connection()?;
        Ok(connection.execute(
            "DELETE FROM job_sources WHERE user_id = ?1 AND source_key = ?2 AND builtin = 0",
            params![user_id, key],
        )? > 0)
    }

    pub fn set_job_source_auto_watch(
        &self,
        user_id: &str,
        key: &str,
        auto_watch: bool,
    ) -> Result<()> {
        let builtin = BUILTIN_SOURCES.iter().any(|b| b.key == key);
        let connection = self.connection()?;
        if builtin {
            connection.execute(
                "INSERT INTO job_sources(user_id, source_key, builtin, auto_watch, created_at)
                 VALUES (?1, ?2, 1, ?3, ?4)
                 ON CONFLICT(user_id, source_key) DO UPDATE SET auto_watch = excluded.auto_watch",
                params![user_id, key, auto_watch as i64, Utc::now().to_rfc3339()],
            )?;
            return Ok(());
        }
        let changed = connection.execute(
            "UPDATE job_sources SET auto_watch = ?3 WHERE user_id = ?1 AND source_key = ?2 AND builtin = 0",
            params![user_id, key, auto_watch as i64],
        )?;
        if changed == 0 {
            return Err(SeraError::NotFound("job source".into()));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Items, matching, seen history
// ---------------------------------------------------------------------------

/// One posting found on a listing page.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct JobItem {
    pub title: String,
    pub employer: Option<String>,
    pub city: Option<String>,
    /// `YYYY-MM-DD` when the page states one.
    pub deadline: Option<String>,
    pub url: String,
    /// The listing's own words around the title, used for matching only.
    #[serde(default, skip_serializing)]
    pub summary: String,
    #[serde(default)]
    pub source: String,
}

/// Lowercase, strip the usual French/Arabic-transliteration accents.
pub fn fold(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ä' | 'À' | 'Â' | 'Ä' => 'a',
            'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' | 'Î' | 'Ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' | 'Ô' | 'Ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Û' | 'Ü' => 'u',
            'ç' | 'Ç' => 'c',
            other => other.to_ascii_lowercase(),
        })
        .collect()
}

fn contains_term(haystack: &str, term: &str) -> bool {
    let term = fold(term);
    !term.is_empty() && haystack.contains(&term)
}

/// Normalise a URL for deduplication: no fragment, no tracking parameters, no
/// trailing slash, lowercase host.
pub fn normalize_url(raw: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(raw) else {
        return raw.trim().to_owned();
    };
    parsed.set_fragment(None);
    let kept: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| {
            let key = key.to_lowercase();
            !(key.starts_with("utm_") || key == "fbclid" || key == "gclid" || key == "ref")
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        parsed.set_query(None);
    } else {
        parsed.query_pairs_mut().clear().extend_pairs(kept);
    }
    let mut text = parsed.to_string();
    while text.ends_with('/') && parsed.path().len() > 1 {
        text.pop();
    }
    text
}

pub fn url_hash(item: &JobItem) -> String {
    sha256_hex(normalize_url(&item.url).as_bytes())
}

pub fn title_hash(item: &JobItem) -> String {
    let key = format!(
        "{}|{}",
        fold(&item.title.split_whitespace().collect::<Vec<_>>().join(" ")),
        fold(item.employer.as_deref().unwrap_or(""))
    );
    sha256_hex(key.as_bytes())
}

/// Does `item` fit the profile? `source_sectors` are the sectors the source
/// covers (empty means any).
pub fn matches_profile(profile: &JobProfile, item: &JobItem, source_sectors: &[String]) -> bool {
    let title = fold(&item.title);
    let employer = fold(item.employer.as_deref().unwrap_or(""));
    let all = format!("{title} {employer} {}", fold(&item.summary));

    if profile
        .excluded_employers
        .iter()
        .any(|name| contains_term(&employer, name) || contains_term(&title, name))
    {
        return false;
    }
    if profile
        .keywords_exclude
        .iter()
        .any(|word| contains_term(&all, word))
    {
        return false;
    }
    if !profile.sectors.is_empty()
        && !source_sectors.is_empty()
        && !source_sectors.iter().any(|s| profile.sectors.contains(s))
    {
        return false;
    }
    if !profile.cities.is_empty() {
        let remote_ok = profile.cities.iter().any(|c| fold(c) == "remote");
        let is_remote = ["remote", "teletravail", "a distance"]
            .iter()
            .any(|word| all.contains(word));
        match item.city.as_deref() {
            Some(city) => {
                let city = fold(city);
                let listed = profile
                    .cities
                    .iter()
                    .any(|wanted| fold(wanted) != "remote" && contains_term(&city, wanted));
                if !(listed || remote_ok && is_remote) {
                    return false;
                }
            }
            None => {
                // City unknown: keep it unless the text names another place we
                // can recognise as not wanted — we cannot tell, so keep it.
            }
        }
    }
    if matches!(profile.seniority.as_str(), "entry" | "junior")
        && ["senior", "confirme", "directeur", "responsable", "manager"]
            .iter()
            .any(|word| title.contains(word))
    {
        return false;
    }
    let wanted: Vec<&String> = profile
        .roles
        .iter()
        .chain(profile.keywords_include.iter())
        .collect();
    wanted.is_empty() || wanted.iter().any(|term| contains_term(&all, term))
}

/// A posting kept in the member's history.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SeenJob {
    pub title: String,
    pub employer: String,
    pub city: String,
    pub deadline: String,
    pub url: String,
    pub source: String,
    pub matched: bool,
    pub first_seen: String,
}

impl Store {
    pub fn job_seen_exists(&self, user_id: &str, item: &JobItem) -> Result<bool> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT 1 FROM job_seen WHERE user_id = ?1 AND (url_hash = ?2 OR title_hash = ?3)",
                params![user_id, url_hash(item), title_hash(item)],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Remember a posting; false when it was already known. Keeps the history
    /// at `MAX_SEEN_PER_USER` rows, oldest first out.
    pub fn record_job_seen(&self, user_id: &str, item: &JobItem, matched: bool) -> Result<bool> {
        if self.job_seen_exists(user_id, item)? {
            return Ok(false);
        }
        let connection = self.connection()?;
        let inserted = connection.execute(
            "INSERT OR IGNORE INTO job_seen(user_id, url_hash, title_hash, source_key, title, employer,
                                            city, deadline, url, matched, first_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                user_id,
                url_hash(item),
                title_hash(item),
                item.source,
                item.title,
                item.employer.clone().unwrap_or_default(),
                item.city.clone().unwrap_or_default(),
                item.deadline.clone().unwrap_or_default(),
                item.url,
                matched as i64,
                Utc::now().to_rfc3339(),
            ],
        )?;
        connection.execute(
            "DELETE FROM job_seen WHERE user_id = ?1 AND url_hash NOT IN (
                SELECT url_hash FROM job_seen WHERE user_id = ?1
                ORDER BY first_seen DESC, rowid DESC LIMIT ?2)",
            params![user_id, MAX_SEEN_PER_USER as i64],
        )?;
        Ok(inserted > 0)
    }

    /// The member's latest matched postings, newest first.
    pub fn recent_job_matches(&self, user_id: &str, limit: usize) -> Result<Vec<SeenJob>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT title, employer, city, deadline, url, source_key, matched, first_seen
             FROM job_seen WHERE user_id = ?1 AND matched = 1
             ORDER BY first_seen DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![user_id, limit.clamp(1, 100) as i64], |row| {
            Ok(SeenJob {
                title: row.get(0)?,
                employer: row.get(1)?,
                city: row.get(2)?,
                deadline: row.get(3)?,
                url: row.get(4)?,
                source: row.get(5)?,
                matched: row.get::<_, i64>(6)? != 0,
                first_seen: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn job_seen_count(&self, user_id: &str) -> Result<i64> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT COUNT(*) FROM job_seen WHERE user_id = ?1",
            params![user_id],
            |row| row.get(0),
        )?)
    }

    pub fn job_watch_state(&self, user_id: &str) -> Result<Option<(String, String, Value)>> {
        let connection = self.connection()?;
        let row: Option<(String, String, String)> = connection
            .query_row(
                "SELECT last_run_at, last_mode, last_report FROM job_watch_state WHERE user_id = ?1",
                params![user_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        Ok(row.map(|(at, mode, report)| {
            (
                at,
                mode,
                serde_json::from_str(&report).unwrap_or(Value::Null),
            )
        }))
    }

    pub fn set_job_watch_state(&self, user_id: &str, mode: &str, report: &Value) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO job_watch_state(user_id, last_run_at, last_mode, last_report)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id) DO UPDATE SET last_run_at = excluded.last_run_at,
                last_mode = excluded.last_mode, last_report = excluded.last_report",
            params![
                user_id,
                Utc::now().to_rfc3339(),
                mode,
                serde_json::to_string(report)?
            ],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Applications
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct JobApplication {
    pub id: String,
    pub title: String,
    pub employer: String,
    pub city: String,
    pub url: String,
    pub deadline: String,
    pub status: String,
    pub notes: String,
    pub applied_on: String,
    pub interview_on: String,
    /// Hashes of the CV / letter artifacts prepared for this application.
    pub artifacts: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationInput {
    pub title: Option<String>,
    pub employer: Option<String>,
    pub city: Option<String>,
    pub url: Option<String>,
    pub deadline: Option<String>,
    pub status: Option<String>,
    pub notes: Option<String>,
    pub applied_on: Option<String>,
    pub interview_on: Option<String>,
    pub artifacts: Option<Vec<String>>,
}

fn rank(status: &str) -> usize {
    APPLICATION_STATUSES
        .iter()
        .position(|s| *s == status)
        .unwrap_or(0)
}

/// `saved → applied → interview → offer` forward (steps may be skipped, a
/// recruiter can call before any form), `rejected` from anywhere, and
/// `rejected` is final: add a new application to start again.
pub fn check_transition(from: &str, to: &str) -> Result<()> {
    if !APPLICATION_STATUSES.contains(&to) {
        return Err(SeraError::Validation(format!(
            "status: one of {}",
            APPLICATION_STATUSES.join(", ")
        )));
    }
    if from == to {
        return Ok(());
    }
    if from == "rejected" {
        return Err(SeraError::InvalidState(
            "this application is closed (rejected); add a new one to start again".into(),
        ));
    }
    if to == "rejected" || rank(to) > rank(from) {
        return Ok(());
    }
    Err(SeraError::InvalidState(format!(
        "an application cannot go back from {from} to {to}"
    )))
}

fn short(label: &str, value: &Option<String>, max: usize) -> Result<Option<String>> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim();
    if value.chars().count() > max || value.chars().any(|c| c.is_control() && c != '\n') {
        return Err(SeraError::Validation(format!(
            "{label}: plain text of at most {max} characters"
        )));
    }
    Ok(Some(value.to_owned()))
}

fn date(label: &str, value: &Option<String>) -> Result<Option<String>> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim();
    if value.is_empty() {
        return Ok(Some(String::new()));
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| SeraError::Validation(format!("{label}: use YYYY-MM-DD")))?;
    Ok(Some(value.to_owned()))
}

fn artifact_hashes(values: &Option<Vec<String>>) -> Result<Option<Vec<String>>> {
    let Some(values) = values else {
        return Ok(None);
    };
    if values.len() > MAX_ARTIFACTS_PER_APPLICATION {
        return Err(SeraError::Validation(format!(
            "at most {MAX_ARTIFACTS_PER_APPLICATION} files per application"
        )));
    }
    let mut out = Vec::new();
    for hash in values {
        let hash = hash.trim().to_lowercase();
        if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(SeraError::Validation("invalid artifact hash".into()));
        }
        if !out.contains(&hash) {
            out.push(hash);
        }
    }
    Ok(Some(out))
}

fn app_url(value: &Option<String>) -> Result<Option<String>> {
    let Some(value) = short("url", value, 500)? else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(Some(value));
    }
    validate_source_url(&value)?;
    Ok(Some(value))
}

fn read_application(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobApplication> {
    let artifacts: String = row.get(10)?;
    Ok(JobApplication {
        id: row.get(0)?,
        title: row.get(1)?,
        employer: row.get(2)?,
        city: row.get(3)?,
        url: row.get(4)?,
        deadline: row.get(5)?,
        status: row.get(6)?,
        notes: row.get(7)?,
        applied_on: row.get(8)?,
        interview_on: row.get(9)?,
        artifacts: serde_json::from_str(&artifacts).unwrap_or_default(),
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

const APPLICATION_COLUMNS: &str = "id, title, employer, city, url, deadline, status, notes,
    applied_on, interview_on, artifacts, created_at, updated_at";

impl Store {
    pub fn add_job_application(
        &self,
        user_id: &str,
        input: &ApplicationInput,
    ) -> Result<JobApplication> {
        let title = short("title", &input.title, 200)?
            .filter(|t| !t.is_empty())
            .ok_or_else(|| SeraError::Validation("title is required".into()))?;
        let status = input.status.clone().unwrap_or_else(|| "saved".into());
        check_transition("saved", &status)?;
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let mut applied_on = date("applied_on", &input.applied_on)?.unwrap_or_default();
        if status != "saved" && status != "rejected" && applied_on.is_empty() {
            applied_on = today;
        }
        let artifacts = artifact_hashes(&input.artifacts)?.unwrap_or_default();
        let count: i64 = self.connection()?.query_row(
            "SELECT COUNT(*) FROM job_applications WHERE user_id = ?1",
            params![user_id],
            |row| row.get(0),
        )?;
        if count as usize >= MAX_APPLICATIONS {
            return Err(SeraError::Validation(format!(
                "at most {MAX_APPLICATIONS} applications; remove old ones first"
            )));
        }
        let id = new_id().to_string();
        let now = Utc::now().to_rfc3339();
        {
            let connection = self.connection()?;
            connection.execute(
                "INSERT INTO job_applications(id, user_id, title, employer, city, url, deadline, status,
                    notes, applied_on, interview_on, artifacts, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
                params![
                    id,
                    user_id,
                    title,
                    short("employer", &input.employer, 120)?.unwrap_or_default(),
                    short("city", &input.city, 80)?.unwrap_or_default(),
                    app_url(&input.url)?.unwrap_or_default(),
                    date("deadline", &input.deadline)?.unwrap_or_default(),
                    status,
                    short("notes", &input.notes, 2_000)?.unwrap_or_default(),
                    applied_on,
                    date("interview_on", &input.interview_on)?.unwrap_or_default(),
                    serde_json::to_string(&artifacts)?,
                    now,
                ],
            )?;
        }
        self.job_application(user_id, &id)?
            .ok_or_else(|| SeraError::Internal("application insert lost".into()))
    }

    pub fn job_application(&self, user_id: &str, id: &str) -> Result<Option<JobApplication>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                &format!(
                    "SELECT {APPLICATION_COLUMNS} FROM job_applications WHERE id = ?1 AND user_id = ?2"
                ),
                params![id, user_id],
                read_application,
            )
            .optional()?)
    }

    /// Update one of the member's applications. A foreign or unknown id is
    /// `NotFound`.
    pub fn update_job_application(
        &self,
        user_id: &str,
        id: &str,
        input: &ApplicationInput,
    ) -> Result<JobApplication> {
        let current = self
            .job_application(user_id, id)?
            .ok_or_else(|| SeraError::NotFound("application".into()))?;
        let mut next = current.clone();
        if let Some(title) = short("title", &input.title, 200)? {
            if title.is_empty() {
                return Err(SeraError::Validation("title must not be empty".into()));
            }
            next.title = title;
        }
        if let Some(v) = short("employer", &input.employer, 120)? {
            next.employer = v;
        }
        if let Some(v) = short("city", &input.city, 80)? {
            next.city = v;
        }
        if let Some(v) = app_url(&input.url)? {
            next.url = v;
        }
        if let Some(v) = date("deadline", &input.deadline)? {
            next.deadline = v;
        }
        if let Some(v) = short("notes", &input.notes, 2_000)? {
            next.notes = v;
        }
        if let Some(v) = date("applied_on", &input.applied_on)? {
            next.applied_on = v;
        }
        if let Some(v) = date("interview_on", &input.interview_on)? {
            next.interview_on = v;
        }
        if let Some(v) = artifact_hashes(&input.artifacts)? {
            next.artifacts = v;
        }
        if let Some(status) = &input.status {
            check_transition(&current.status, status)?;
            next.status = status.clone();
            if status == "applied" && next.applied_on.is_empty() {
                next.applied_on = Utc::now().format("%Y-%m-%d").to_string();
            }
        }
        let connection = self.connection()?;
        connection.execute(
            "UPDATE job_applications SET title = ?3, employer = ?4, city = ?5, url = ?6,
                deadline = ?7, status = ?8, notes = ?9, applied_on = ?10, interview_on = ?11,
                artifacts = ?12, updated_at = ?13
             WHERE id = ?1 AND user_id = ?2",
            params![
                id,
                user_id,
                next.title,
                next.employer,
                next.city,
                next.url,
                next.deadline,
                next.status,
                next.notes,
                next.applied_on,
                next.interview_on,
                serde_json::to_string(&next.artifacts)?,
                Utc::now().to_rfc3339(),
            ],
        )?;
        drop(connection);
        self.job_application(user_id, id)?
            .ok_or_else(|| SeraError::Internal("application update lost".into()))
    }

    pub fn list_job_applications(
        &self,
        user_id: &str,
        status: Option<&str>,
    ) -> Result<Vec<JobApplication>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT {APPLICATION_COLUMNS} FROM job_applications
             WHERE user_id = ?1 AND (?2 IS NULL OR status = ?2)
             ORDER BY updated_at DESC LIMIT 200"
        ))?;
        let rows = statement.query_map(params![user_id, status], read_application)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Counts per status for the pipeline card.
    pub fn job_application_counts(&self, user_id: &str) -> Result<Vec<(String, i64)>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT status, COUNT(*) FROM job_applications WHERE user_id = ?1 GROUP BY status",
        )?;
        let found = statement
            .query_map(params![user_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(APPLICATION_STATUSES
            .iter()
            .map(|status| {
                let n = found
                    .iter()
                    .find(|(s, _)| s == status)
                    .map(|(_, n)| *n)
                    .unwrap_or(0);
                ((*status).to_owned(), n)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(dir.path().join("test.db"), 384, 5000).unwrap();
        (dir, store)
    }

    fn user(store: &Store, sub: &str) -> String {
        store
            .create_user(sub, &format!("{sub}@example.com"), sub)
            .unwrap()
            .id
    }

    fn item(title: &str, employer: &str, city: &str, url: &str) -> JobItem {
        JobItem {
            title: title.into(),
            employer: (!employer.is_empty()).then(|| employer.into()),
            city: (!city.is_empty()).then(|| city.into()),
            url: url.into(),
            ..Default::default()
        }
    }

    #[test]
    fn profile_validation_cleans_and_bounds() {
        let ok = JobProfile {
            roles: vec!["  Data   Analyst ".into(), "data analyst".into(), "".into()],
            cities: vec!["Rabat".into(), "remote".into()],
            sectors: vec!["Public".into(), "internship".into()],
            seniority: "Junior".into(),
            frequency: "Twice_Daily".into(),
            onedrive_tracker_path: Some("\\Documents\\Jobs/tracker.xlsx".into()),
            ..Default::default()
        };
        let clean = ok.validated().unwrap();
        assert_eq!(clean.roles, vec!["Data Analyst"]);
        assert_eq!(clean.sectors, vec!["public", "internship"]);
        assert_eq!(clean.seniority, "junior");
        assert_eq!(clean.frequency, "twice_daily");
        assert_eq!(
            clean.onedrive_tracker_path.as_deref(),
            Some("Documents/Jobs/tracker.xlsx")
        );
        assert_eq!(clean.interval_minutes(), Some(660));

        let mut bad = ok.clone();
        bad.sectors = vec!["government".into()];
        assert!(bad.validated().is_err());
        let mut bad = ok.clone();
        bad.frequency = "hourly".into();
        assert!(bad.validated().is_err());
        let mut bad = ok.clone();
        bad.roles = (0..21).map(|n| format!("role {n}")).collect();
        assert!(bad.validated().is_err());
        let mut bad = ok.clone();
        bad.keywords_include = vec!["x".repeat(81)];
        assert!(bad.validated().is_err());
        let mut bad = ok.clone();
        bad.cities = vec!["Ra\u{0}bat".into()];
        assert!(bad.validated().is_err());
        let mut bad = ok.clone();
        bad.onedrive_materials_folder = Some("Documents/../Secrets".into());
        assert!(bad.validated().is_err());
        let mut off = ok;
        off.frequency = "off".into();
        assert_eq!(off.validated().unwrap().interval_minutes(), None);
    }

    #[test]
    fn profile_is_scoped_to_the_member() {
        let (_dir, store) = store();
        let a = user(&store, "a");
        let b = user(&store, "b");
        let profile = JobProfile {
            roles: vec!["comptable".into()],
            seniority: "any".into(),
            frequency: "daily".into(),
            ..Default::default()
        };
        store.set_job_profile(&a, &profile).unwrap();
        assert_eq!(
            store.job_profile(&a).unwrap().unwrap().roles,
            vec!["comptable"]
        );
        assert!(store.job_profile(&b).unwrap().is_none());
        assert_eq!(store.job_profile_owners().unwrap(), vec![a]);
    }

    #[test]
    fn sources_builtin_toggle_custom_add_remove_and_scope() {
        let (_dir, store) = store();
        let a = user(&store, "a");
        let b = user(&store, "b");
        let sources = store.job_sources(&a).unwrap();
        assert_eq!(sources.len(), BUILTIN_SOURCES.len());
        assert!(sources.iter().all(|s| s.builtin && s.auto_watch));

        store
            .set_job_source_auto_watch(&a, "rekrute", false)
            .unwrap();
        assert!(
            !store
                .job_sources(&a)
                .unwrap()
                .iter()
                .find(|s| s.key == "rekrute")
                .unwrap()
                .auto_watch
        );
        assert!(
            store
                .job_sources(&b)
                .unwrap()
                .iter()
                .find(|s| s.key == "rekrute")
                .unwrap()
                .auto_watch
        );

        let custom = store
            .add_job_source(&a, "https://jobs.example.org/listing", Some("My board"))
            .unwrap();
        assert!(!custom.builtin);
        assert_eq!(custom.label, "My board");
        assert_eq!(store.job_sources(&b).unwrap().len(), BUILTIN_SOURCES.len());
        store
            .set_job_source_auto_watch(&a, &custom.key, false)
            .unwrap();
        assert!(
            store
                .set_job_source_auto_watch(&b, &custom.key, false)
                .is_err()
        );
        assert!(!store.remove_job_source(&b, &custom.key).unwrap());
        assert!(store.remove_job_source(&a, &custom.key).unwrap());
        assert!(store.remove_job_source(&a, "rekrute").is_err());

        assert!(
            store
                .add_job_source(&a, "ftp://x.example.org/", None)
                .is_err()
        );
        assert!(
            store
                .add_job_source(&a, "https://localhost/", None)
                .is_err()
        );
        assert!(
            store
                .add_job_source(&a, "https://u:p@x.example.org/", None)
                .is_err()
        );
        for n in 0..MAX_CUSTOM_SOURCES {
            store
                .add_job_source(&a, &format!("https://s{n}.example.org/"), None)
                .unwrap();
        }
        assert!(
            store
                .add_job_source(&a, "https://extra.example.org/", None)
                .is_err()
        );
    }

    #[test]
    fn matching_uses_keywords_cities_sector_and_exclusions() {
        let profile = JobProfile {
            roles: vec!["comptable".into()],
            cities: vec!["Casablanca".into(), "remote".into()],
            sectors: vec!["private".into()],
            keywords_exclude: vec!["stage".into()],
            excluded_employers: vec!["Acme".into()],
            seniority: "junior".into(),
            ..Default::default()
        };
        let private = vec!["private".to_owned()];
        let public = vec!["public".to_owned()];
        assert!(!matches_profile(
            &profile,
            &item(
                "Comptable confirmé(e)",
                "Beta",
                "Casablanca",
                "https://x.example/1"
            ),
            &private
        ));
        assert!(matches_profile(
            &profile,
            &item("Comptable H/F", "Beta", "Casablanca", "https://x.example/2"),
            &private
        ));
        assert!(matches_profile(
            &profile,
            &item("Comptable H/F", "Beta", "", "https://x.example/3"),
            &private
        ));
        assert!(!matches_profile(
            &profile,
            &item("Comptable H/F", "Beta", "Tanger", "https://x.example/4"),
            &private
        ));
        assert!(matches_profile(
            &profile,
            &item(
                "Comptable (télétravail)",
                "Beta",
                "Tanger",
                "https://x.example/5"
            ),
            &private
        ));
        assert!(!matches_profile(
            &profile,
            &item("Comptable H/F", "Beta", "Casablanca", "https://x.example/6"),
            &public
        ));
        assert!(!matches_profile(
            &profile,
            &item(
                "Comptable H/F",
                "ACME SA",
                "Casablanca",
                "https://x.example/7"
            ),
            &private
        ));
        assert!(!matches_profile(
            &profile,
            &item(
                "Stage comptable",
                "Beta",
                "Casablanca",
                "https://x.example/8"
            ),
            &private
        ));
        assert!(!matches_profile(
            &profile,
            &item(
                "Développeur Rust",
                "Beta",
                "Casablanca",
                "https://x.example/9"
            ),
            &private
        ));
        // Custom source (no sectors) is never filtered by sector; accents fold.
        assert!(matches_profile(
            &profile,
            &item("COMPTABLE", "Beta", "casablanca", "https://x.example/10"),
            &[]
        ));
        // Empty profile matches everything.
        assert!(matches_profile(
            &JobProfile::default(),
            &item("Anything", "", "", "https://x.example/11"),
            &public
        ));
    }

    #[test]
    fn dedupe_by_url_or_title_and_bounded_history() {
        let (_dir, store) = store();
        let a = user(&store, "a");
        let b = user(&store, "b");
        let first = item(
            "Comptable",
            "Beta",
            "Rabat",
            "https://x.example/job/1?utm_source=x#top",
        );
        assert!(store.record_job_seen(&a, &first, true).unwrap());
        // Same URL without tracking junk, trailing slash.
        let same_url = item("Other title", "", "", "https://x.example/job/1/");
        assert!(!store.record_job_seen(&a, &same_url, true).unwrap());
        // Same title + employer on a different site.
        let same_title = item("  comptable ", "BETA", "Rabat", "https://y.example/offer/9");
        assert!(!store.record_job_seen(&a, &same_title, true).unwrap());
        // Another member still sees it as new.
        assert!(store.record_job_seen(&b, &first, true).unwrap());
        assert_eq!(store.recent_job_matches(&a, 10).unwrap().len(), 1);

        for n in 0..(MAX_SEEN_PER_USER + 25) {
            store
                .record_job_seen(
                    &a,
                    &item(
                        &format!("Job {n}"),
                        "E",
                        "",
                        &format!("https://z.example/{n}"),
                    ),
                    false,
                )
                .unwrap();
        }
        assert_eq!(
            store.job_seen_count(&a).unwrap() as usize,
            MAX_SEEN_PER_USER
        );
        assert_eq!(store.job_seen_count(&b).unwrap(), 1);
    }

    #[test]
    fn application_status_transitions() {
        assert!(check_transition("saved", "applied").is_ok());
        assert!(check_transition("saved", "interview").is_ok());
        assert!(check_transition("applied", "interview").is_ok());
        assert!(check_transition("interview", "offer").is_ok());
        assert!(check_transition("offer", "rejected").is_ok());
        assert!(check_transition("applied", "applied").is_ok());
        assert!(check_transition("interview", "applied").is_err());
        assert!(check_transition("offer", "saved").is_err());
        assert!(check_transition("rejected", "applied").is_err());
        assert!(check_transition("saved", "hired").is_err());
    }

    #[test]
    fn applications_are_scoped_validated_and_dated() {
        let (_dir, store) = store();
        let a = user(&store, "a");
        let b = user(&store, "b");
        let hash = "a".repeat(64);
        let app = store
            .add_job_application(
                &a,
                &ApplicationInput {
                    title: Some("Comptable".into()),
                    employer: Some("Beta".into()),
                    url: Some("https://x.example/job/1".into()),
                    artifacts: Some(vec![hash.clone(), hash.to_uppercase()]),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(app.status, "saved");
        assert_eq!(app.artifacts, vec![hash]);

        let applied = store
            .update_job_application(
                &a,
                &app.id,
                &ApplicationInput {
                    status: Some("applied".into()),
                    notes: Some("sent CV".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(applied.status, "applied");
        assert_eq!(
            applied.applied_on,
            Utc::now().format("%Y-%m-%d").to_string()
        );
        assert!(
            store
                .update_job_application(
                    &a,
                    &app.id,
                    &ApplicationInput {
                        status: Some("saved".into()),
                        ..Default::default()
                    },
                )
                .is_err()
        );

        // Another member cannot see or change it.
        assert!(store.job_application(&b, &app.id).unwrap().is_none());
        assert!(store.list_job_applications(&b, None).unwrap().is_empty());
        assert!(matches!(
            store.update_job_application(
                &b,
                &app.id,
                &ApplicationInput {
                    notes: Some("x".into()),
                    ..Default::default()
                }
            ),
            Err(SeraError::NotFound(_))
        ));

        // Validation.
        for bad in [
            ApplicationInput {
                title: None,
                ..Default::default()
            },
            ApplicationInput {
                title: Some("T".into()),
                deadline: Some("12/12/2026".into()),
                ..Default::default()
            },
            ApplicationInput {
                title: Some("T".into()),
                artifacts: Some(vec!["nothex".into()]),
                ..Default::default()
            },
            ApplicationInput {
                title: Some("T".into()),
                url: Some("javascript:alert(1)".into()),
                ..Default::default()
            },
            ApplicationInput {
                title: Some("T".into()),
                status: Some("hired".into()),
                ..Default::default()
            },
        ] {
            assert!(store.add_job_application(&a, &bad).is_err());
        }
        let counts = store.job_application_counts(&a).unwrap();
        assert_eq!(counts.iter().find(|(s, _)| s == "applied").unwrap().1, 1);
        assert_eq!(
            store
                .list_job_applications(&a, Some("saved"))
                .unwrap()
                .len(),
            0
        );
    }
}
