//! The job watcher (0.70): fetch a member's job sources, parse the listing
//! pages, match the postings against their profile, remember what was seen
//! and report what is new. Also the `job_*` chat tools.
//!
//! Pages go through the shared [`SafeFetcher`] (public addresses only, size
//! and time bounded). One fetch per source per run, a few seconds apart. A
//! source whose robots.txt disallows its listing path is skipped on a
//! schedule and only fetched when the member explicitly asks. Everything read
//! from a page is data, never an instruction.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use url::Url;

use crate::config::Config;
use crate::error::{Result, SeraError};
use crate::fetch::SafeFetcher;
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolRegistry, ToolSpec};
use crate::job_parse::{parse_listing, path_and_query, robots_allows, robots_url};
use crate::jobs::{
    APPLICATION_STATUSES, ApplicationInput, JobItem, JobProfile, JobSource, matches_profile,
};
use crate::personal_tasks::{self, NewTask};
use crate::store::Store;

/// Pause between two sources of one run.
pub const SOURCE_DELAY: Duration = Duration::from_secs(4);
/// New matches in one message.
pub const MAX_PER_MESSAGE: usize = 8;
/// A manual run is refused when the last run ended less than this ago.
const MANUAL_COOLDOWN_SECS: i64 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    /// The schedule: auto-watch sources only, robots.txt respected.
    Scheduled,
    /// The member asked: every source, robots.txt noted but not blocking.
    Manual,
}

impl RunMode {
    fn name(self) -> &'static str {
        match self {
            RunMode::Scheduled => "scheduled",
            RunMode::Manual => "manual",
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct JobHit {
    pub title: String,
    pub employer: Option<String>,
    pub city: Option<String>,
    pub deadline: Option<String>,
    pub url: String,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SourceReport {
    pub key: String,
    pub label: String,
    /// `ok`, `robots` (skipped), `error`.
    pub status: String,
    pub items: usize,
    pub new_matches: usize,
    pub note: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct WatchReport {
    pub checked_at: String,
    pub mode: String,
    pub sources: Vec<SourceReport>,
    pub new_matches: Vec<JobHit>,
    pub tasks_created: usize,
}

/// A fetcher for job pages: any public host (members add their own sources).
pub fn job_fetcher(config: &Config) -> Result<SafeFetcher> {
    SafeFetcher::new(
        config.watcher.user_agent.clone(),
        config.watcher.timeout_secs,
        config.watcher.max_response_bytes,
    )
}

fn clean_text(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

async fn robots_decision(
    fetcher: &SafeFetcher,
    agent: &str,
    page: &Url,
    cache: &mut HashMap<String, Option<String>>,
) -> RobotsDecision {
    let Some(robots) = robots_url(page) else {
        return RobotsDecision::Unknown;
    };
    if !cache.contains_key(&robots) {
        let body = match fetcher.fetch(&robots).await {
            Ok(page) if page.status == 200 => Some(page.body),
            // No robots.txt (4xx) means no rules.
            Ok(page) if (400..500).contains(&page.status) => Some(String::new()),
            _ => None,
        };
        cache.insert(robots.clone(), body);
    }
    match cache.get(&robots).and_then(|body| body.as_deref()) {
        Some(body) if robots_allows(body, agent, &path_and_query(page)) => RobotsDecision::Allowed,
        Some(_) => RobotsDecision::Disallowed,
        None => RobotsDecision::Unknown,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RobotsDecision {
    Allowed,
    Disallowed,
    Unknown,
}

/// Run one member's watch.
pub async fn run_watch(
    store: &Store,
    fetcher: &SafeFetcher,
    agent: &str,
    user_id: &str,
    mode: RunMode,
    only: Option<&str>,
    delay: Duration,
) -> Result<WatchReport> {
    let profile = store.job_profile(user_id)?.ok_or_else(|| {
        SeraError::Validation("no job profile yet; set one up first (job_profile_set)".into())
    })?;
    let mut sources: Vec<JobSource> = store.job_sources(user_id)?;
    if let Some(key) = only {
        sources.retain(|source| source.key == key);
        if sources.is_empty() {
            return Err(SeraError::NotFound("job source".into()));
        }
    } else if mode == RunMode::Scheduled {
        sources.retain(|source| source.auto_watch);
    }
    let today = Utc::now().date_naive();
    let mut report = WatchReport {
        checked_at: Utc::now().to_rfc3339(),
        mode: mode.name().into(),
        ..Default::default()
    };
    let mut robots_cache: HashMap<String, Option<String>> = HashMap::new();
    let mut first = true;
    for source in sources {
        let mut line = SourceReport {
            key: source.key.clone(),
            label: source.label.clone(),
            status: "ok".into(),
            items: 0,
            new_matches: 0,
            note: String::new(),
        };
        let Ok(page_url) = Url::parse(&source.url) else {
            line.status = "error".into();
            line.note = "not a valid address".into();
            report.sources.push(line);
            continue;
        };
        if !first && !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        first = false;
        match robots_decision(fetcher, agent, &page_url, &mut robots_cache).await {
            RobotsDecision::Allowed => {}
            RobotsDecision::Disallowed if mode == RunMode::Scheduled => {
                line.status = "robots".into();
                line.note =
                    "robots.txt asks for no automatic checks; ask me to look when you want".into();
                report.sources.push(line);
                continue;
            }
            RobotsDecision::Unknown if mode == RunMode::Scheduled => {
                line.status = "robots".into();
                line.note = "robots.txt could not be read; skipped on the schedule".into();
                report.sources.push(line);
                continue;
            }
            RobotsDecision::Disallowed => {
                line.note =
                    "robots.txt asks for no automatic checks; fetched once because you asked"
                        .into();
            }
            RobotsDecision::Unknown => {}
        }
        let page = match fetcher.fetch(&source.url).await {
            Ok(page) if page.status == 200 => page,
            Ok(page) => {
                line.status = "error".into();
                line.note = format!("the site answered HTTP {}", page.status);
                report.sources.push(line);
                continue;
            }
            Err(error) => {
                line.status = "error".into();
                line.note = clean_text(&error.to_string(), 160);
                report.sources.push(line);
                continue;
            }
        };
        let items = parse_listing(&source.key, &page.final_url, &page.body);
        line.items = items.len();
        if items.is_empty() && line.note.is_empty() {
            line.note = "no postings recognised on the page".into();
        }
        for mut item in items {
            item.source = source.key.clone();
            let matched = matches_profile(&profile, &item, &source.sectors);
            let fresh = store.record_job_seen(user_id, &item, matched)?;
            if fresh && matched {
                line.new_matches += 1;
                report.new_matches.push(hit(&item, &source.label));
                if profile.deadlines_to_tasks
                    && source.sectors.iter().any(|s| s == "public")
                    && deadline_task(store, user_id, &item, today)
                {
                    report.tasks_created += 1;
                }
            }
        }
        report.sources.push(line);
    }
    store.set_job_watch_state(user_id, mode.name(), &serde_json::to_value(&report)?)?;
    let _ = store.record_audit(
        user_id,
        "jobs.watch",
        None,
        None,
        None,
        None,
        None,
        Some(&json!({
            "mode": mode.name(),
            "sources": report.sources.len(),
            "new_matches": report.new_matches.len(),
            "tasks_created": report.tasks_created,
        })),
    );
    Ok(report)
}

fn hit(item: &JobItem, source_label: &str) -> JobHit {
    JobHit {
        title: clean_text(&item.title, 160),
        employer: item.employer.as_deref().map(|e| clean_text(e, 100)),
        city: item.city.clone(),
        deadline: item.deadline.clone(),
        url: item.url.clone(),
        source: source_label.to_owned(),
    }
}

/// A personal task for a public concours deadline that is still ahead.
fn deadline_task(store: &Store, user_id: &str, item: &JobItem, today: NaiveDate) -> bool {
    let Some(deadline) = item
        .deadline
        .as_deref()
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
    else {
        return false;
    };
    if deadline < today {
        return false;
    }
    let title: String = format!("Deadline: {}", clean_text(&item.title, 180))
        .chars()
        .take(200)
        .collect();
    let notes = format!(
        "{}{} {}",
        item.employer.as_deref().unwrap_or(""),
        item.city
            .as_deref()
            .map(|c| format!(" — {c}"))
            .unwrap_or_default(),
        item.url
    );
    personal_tasks::create(
        store,
        user_id,
        &NewTask {
            title,
            notes: Some(notes.chars().take(1_000).collect()),
            status: None,
            due: Some(deadline.format("%Y-%m-%d").to_string()),
        },
    )
    .is_ok()
}

/// The message for new matches: at most [`MAX_PER_MESSAGE`], one per line.
pub fn format_matches(hits: &[JobHit]) -> String {
    let mut text = format!("New job matches ({}):", hits.len());
    for (index, hit) in hits.iter().take(MAX_PER_MESSAGE).enumerate() {
        let mut parts = vec![hit.title.clone()];
        if let Some(employer) = hit.employer.as_deref().filter(|e| !e.is_empty()) {
            parts.push(employer.to_owned());
        }
        if let Some(city) = hit.city.as_deref() {
            parts.push(city.to_owned());
        }
        if let Some(deadline) = hit.deadline.as_deref() {
            parts.push(format!("deadline {deadline}"));
        }
        parts.push(hit.url.clone());
        text.push_str(&format!("\n{}. {}", index + 1, parts.join(" — ")));
    }
    if hits.len() > MAX_PER_MESSAGE {
        text.push_str(&format!(
            "\n…and {} more. Ask JobFinder to show them.",
            hits.len() - MAX_PER_MESSAGE
        ));
    }
    text
}

/// Is a scheduled run due for this member?
pub fn watch_due(profile: &JobProfile, last_run: Option<&str>, now: DateTime<Utc>) -> bool {
    let Some(interval) = profile.interval_minutes() else {
        return false;
    };
    match last_run.and_then(|at| DateTime::parse_from_rfc3339(at).ok()) {
        Some(last) => now - last.with_timezone(&Utc) >= chrono::Duration::minutes(interval),
        None => true,
    }
}

/// Members whose scheduled watch is due now.
pub fn due_members(store: &Store, now: DateTime<Utc>) -> Result<Vec<String>> {
    let mut due = Vec::new();
    for user_id in store.job_profile_owners()? {
        let Some(profile) = store.job_profile(&user_id)? else {
            continue;
        };
        let last = store.job_watch_state(&user_id)?.map(|(at, _, _)| at);
        if watch_due(&profile, last.as_deref(), now) {
            due.push(user_id);
        }
    }
    Ok(due)
}

fn report_text(report: &WatchReport) -> String {
    let mut lines = vec![format!(
        "Checked {} source(s): {} new match(es).",
        report.sources.len(),
        report.new_matches.len()
    )];
    for source in &report.sources {
        let mut line = format!(
            "- {} [{}]: {} posting(s) read",
            source.label, source.status, source.items
        );
        if source.new_matches > 0 {
            line.push_str(&format!(", {} new match(es)", source.new_matches));
        }
        if !source.note.is_empty() {
            line.push_str(&format!(" ({})", source.note));
        }
        lines.push(line);
    }
    if !report.new_matches.is_empty() {
        lines.push(String::new());
        lines.push(format_matches(&report.new_matches));
    }
    if report.tasks_created > 0 {
        lines.push(format!(
            "{} deadline(s) added to your tasks.",
            report.tasks_created
        ));
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

pub fn register_job_watch_tools(
    registry: &mut ToolRegistry,
    store: Store,
    config: Arc<Config>,
) -> Result<()> {
    let fetcher = job_fetcher(&config)?;
    registry.register(JobProfileGetTool {
        store: store.clone(),
    })?;
    registry.register(JobProfileSetTool {
        store: store.clone(),
    })?;
    registry.register(JobWatchNowTool {
        store: store.clone(),
        fetcher,
        agent: config.watcher.user_agent.clone(),
        delay: SOURCE_DELAY,
    })?;
    registry.register(JobSourcesListTool {
        store: store.clone(),
    })?;
    registry.register(JobSourcesSetTool {
        store: store.clone(),
    })?;
    registry.register(JobApplicationAddTool {
        store: store.clone(),
    })?;
    registry.register(JobApplicationUpdateTool {
        store: store.clone(),
    })?;
    registry.register(JobApplicationsListTool { store })?;
    Ok(())
}

fn text_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

struct JobProfileGetTool {
    store: Store,
}

#[async_trait]
impl Tool for JobProfileGetTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "job_profile_get".into(),
            description: "Read the acting member's job profile (roles, cities, sectors, languages, level, keywords, excluded employers, check frequency, their own OneDrive tracker and materials folder). Returns 'no profile yet' on their first conversation.".into(),
            parameters: json!({"type": "object", "properties": {}, "additionalProperties": false}),
            external_effect: false,
        }
    }

    async fn execute(&self, _args: Value, context: &ToolContext) -> Result<ToolOutput> {
        match self.store.job_profile(&context.user_id)? {
            None => Ok(ToolOutput {
                content: "No job profile yet. Run the short interview, then save it with job_profile_set.".into(),
                data: Some(json!({"profile": null})),
                ..Default::default()
            }),
            Some(profile) => Ok(ToolOutput {
                content: format!(
                    "{}\nKeywords to include: {}\nKeywords to exclude: {}\nExcluded employers: {}\nOneDrive tracker: {}\nOneDrive materials folder: {}\nDeadlines to tasks: {}",
                    profile.summary(),
                    profile.keywords_include.join(", "),
                    profile.keywords_exclude.join(", "),
                    profile.excluded_employers.join(", "),
                    profile.onedrive_tracker_path.as_deref().unwrap_or("none"),
                    profile.onedrive_materials_folder.as_deref().unwrap_or("none"),
                    if profile.deadlines_to_tasks { "yes" } else { "no" },
                ),
                data: Some(json!({"profile": profile})),
                ..Default::default()
            }),
        }
    }
}

struct JobProfileSetTool {
    store: Store,
}

fn string_list() -> Value {
    json!({"type": "array", "maxItems": 20, "items": {"type": "string", "maxLength": 80}})
}

#[async_trait]
impl Tool for JobProfileSetTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "job_profile_set".into(),
            description: "Create or update the acting member's job profile. Only the fields you pass change; lists replace the old list. Use it after the first-chat interview and whenever the member asks to change something.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "roles": string_list(),
                    "cities": string_list(),
                    "sectors": {"type": "array", "items": {"type": "string", "enum": ["public", "private", "internship"]}},
                    "languages": string_list(),
                    "seniority": {"type": "string", "enum": ["any", "entry", "junior", "mid", "senior", "manager"]},
                    "keywords_include": string_list(),
                    "keywords_exclude": string_list(),
                    "excluded_employers": string_list(),
                    "frequency": {"type": "string", "enum": ["daily", "twice_daily", "weekly", "off"]},
                    "onedrive_tracker_path": {"type": ["string", "null"], "maxLength": 300},
                    "onedrive_materials_folder": {"type": ["string", "null"], "maxLength": 300},
                    "deadlines_to_tasks": {"type": "boolean"}
                },
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let Value::Object(changes) = args else {
            return Err(SeraError::Validation("arguments must be an object".into()));
        };
        let current = self
            .store
            .job_profile(&context.user_id)?
            .unwrap_or_default();
        let mut merged = match serde_json::to_value(&current)? {
            Value::Object(map) => map,
            _ => Default::default(),
        };
        merged.remove("updated_at");
        for (key, value) in changes {
            merged.insert(key, value);
        }
        let profile: JobProfile = serde_json::from_value(Value::Object(merged))
            .map_err(|error| SeraError::Validation(format!("job profile: {error}")))?;
        let saved = self.store.set_job_profile(&context.user_id, &profile)?;
        Ok(ToolOutput {
            content: format!("Saved your job profile.\n{}", saved.summary()),
            data: Some(json!({"profile": saved})),
            ..Default::default()
        })
    }
}

struct JobWatchNowTool {
    store: Store,
    fetcher: SafeFetcher,
    agent: String,
    delay: Duration,
}

#[async_trait]
impl Tool for JobWatchNowTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "job_watch_now".into(),
            description: "Run the acting member's job watch right now: fetch their job sources (one page each, a few seconds apart), match the postings against their profile and return the NEW matches. Pass `source` (a key from job_sources_list) to check just one. Only real postings found on the pages are returned.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"source": {"type": "string", "maxLength": 40}},
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        if let Some((at, _, _)) = self.store.job_watch_state(&context.user_id)?
            && let Ok(at) = DateTime::parse_from_rfc3339(&at)
            && (Utc::now() - at.with_timezone(&Utc)).num_seconds() < MANUAL_COOLDOWN_SECS
        {
            return Err(SeraError::InvalidState(
                "the watch ran a moment ago; wait a minute and ask again".into(),
            ));
        }
        let report = run_watch(
            &self.store,
            &self.fetcher,
            &self.agent,
            &context.user_id,
            RunMode::Manual,
            text_arg(&args, "source"),
            self.delay,
        )
        .await?;
        Ok(ToolOutput {
            content: report_text(&report),
            data: Some(serde_json::to_value(&report)?),
            ..Default::default()
        })
    }
}

struct JobSourcesListTool {
    store: Store,
}

#[async_trait]
impl Tool for JobSourcesListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "job_sources_list".into(),
            description: "List the acting member's job sources: the built-in Moroccan sites and the ones they added, with the auto-watch switch of each.".into(),
            parameters: json!({"type": "object", "properties": {}, "additionalProperties": false}),
            external_effect: false,
        }
    }

    async fn execute(&self, _args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let sources = self.store.job_sources(&context.user_id)?;
        let lines = sources
            .iter()
            .map(|s| {
                format!(
                    "- {} (key={}, auto-watch {}{}): {}",
                    s.label,
                    s.key,
                    if s.auto_watch { "on" } else { "off" },
                    if s.builtin { "" } else { ", yours" },
                    s.url
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput {
            content: format!("Job sources ({}):\n{lines}", sources.len()),
            data: Some(json!({"sources": sources})),
            ..Default::default()
        })
    }
}

struct JobSourcesSetTool {
    store: Store,
}

#[async_trait]
impl Tool for JobSourcesSetTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "job_sources_set".into(),
            description: "Change the acting member's job sources: `add` a listing page (url, optional label; at most 10 of their own), `remove` one of their own (key), or switch `auto_watch` on or off for any source (key, auto_watch).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["add", "remove", "auto_watch"]},
                    "url": {"type": "string", "maxLength": 500},
                    "label": {"type": "string", "maxLength": 60},
                    "key": {"type": "string", "maxLength": 40},
                    "auto_watch": {"type": "boolean"}
                },
                "required": ["action"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let user = &context.user_id;
        let content = match text_arg(&args, "action") {
            Some("add") => {
                let url = text_arg(&args, "url")
                    .ok_or_else(|| SeraError::Validation("url is required".into()))?;
                let source = self
                    .store
                    .add_job_source(user, url, text_arg(&args, "label"))?;
                format!("Added {} (key={}).", source.label, source.key)
            }
            Some("remove") => {
                let key = text_arg(&args, "key")
                    .ok_or_else(|| SeraError::Validation("key is required".into()))?;
                if self.store.remove_job_source(user, key)? {
                    format!("Removed {key}.")
                } else {
                    return Err(SeraError::NotFound("job source".into()));
                }
            }
            Some("auto_watch") => {
                let key = text_arg(&args, "key")
                    .ok_or_else(|| SeraError::Validation("key is required".into()))?;
                let on = args
                    .get("auto_watch")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| SeraError::Validation("auto_watch is required".into()))?;
                self.store.set_job_source_auto_watch(user, key, on)?;
                format!(
                    "Auto-watch for {key} is now {}.",
                    if on { "on" } else { "off" }
                )
            }
            _ => {
                return Err(SeraError::Validation(
                    "action must be add, remove or auto_watch".into(),
                ));
            }
        };
        Ok(ToolOutput {
            content,
            data: Some(json!({"sources": self.store.job_sources(user)?})),
            ..Default::default()
        })
    }
}

fn application_properties(with_id: bool) -> Value {
    let mut props = json!({
        "title": {"type": "string", "maxLength": 200},
        "employer": {"type": "string", "maxLength": 120},
        "city": {"type": "string", "maxLength": 80},
        "url": {"type": "string", "maxLength": 500},
        "deadline": {"type": "string", "description": "YYYY-MM-DD"},
        "status": {"type": "string", "enum": APPLICATION_STATUSES},
        "notes": {"type": "string", "maxLength": 2000},
        "applied_on": {"type": "string", "description": "YYYY-MM-DD"},
        "interview_on": {"type": "string", "description": "YYYY-MM-DD"},
        "artifacts": {"type": "array", "maxItems": 10, "items": {"type": "string"},
            "description": "artifact hashes of the CV / letter prepared for this job"}
    });
    if with_id && let Value::Object(map) = &mut props {
        map.insert("id".into(), json!({"type": "string", "maxLength": 128}));
    }
    props
}

fn application_input(args: &Value) -> Result<ApplicationInput> {
    let mut map = match args {
        Value::Object(map) => map.clone(),
        _ => return Err(SeraError::Validation("arguments must be an object".into())),
    };
    map.remove("id");
    serde_json::from_value(Value::Object(map))
        .map_err(|error| SeraError::Validation(format!("application: {error}")))
}

fn application_line(app: &crate::jobs::JobApplication) -> String {
    format!(
        "- [{}] {}{}{} (id={}{}{})",
        app.status,
        app.title,
        if app.employer.is_empty() {
            String::new()
        } else {
            format!(" — {}", app.employer)
        },
        if app.city.is_empty() {
            String::new()
        } else {
            format!(" — {}", app.city)
        },
        app.id,
        if app.deadline.is_empty() {
            String::new()
        } else {
            format!(", deadline {}", app.deadline)
        },
        if app.interview_on.is_empty() {
            String::new()
        } else {
            format!(", interview {}", app.interview_on)
        },
    )
}

struct JobApplicationAddTool {
    store: Store,
}

#[async_trait]
impl Tool for JobApplicationAddTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "job_application_add".into(),
            description: "Add a job to the acting member's application pipeline (saved, applied, interview, offer, rejected) with notes, dates and the hashes of the CV or letter artifacts prepared for it.".into(),
            parameters: json!({
                "type": "object",
                "properties": application_properties(false),
                "required": ["title"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let app = self
            .store
            .add_job_application(&context.user_id, &application_input(&args)?)?;
        Ok(ToolOutput {
            content: format!("Added to your pipeline:\n{}", application_line(&app)),
            data: Some(json!({"application": app})),
            ..Default::default()
        })
    }
}

struct JobApplicationUpdateTool {
    store: Store,
}

#[async_trait]
impl Tool for JobApplicationUpdateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "job_application_update".into(),
            description: "Update one of the acting member's applications by id: status (forward only: saved, applied, interview, offer; rejected from any step and then closed), notes, dates, linked artifacts.".into(),
            parameters: json!({
                "type": "object",
                "properties": application_properties(true),
                "required": ["id"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let id = text_arg(&args, "id")
            .ok_or_else(|| SeraError::Validation("id is required".into()))?
            .to_owned();
        let app =
            self.store
                .update_job_application(&context.user_id, &id, &application_input(&args)?)?;
        Ok(ToolOutput {
            content: format!("Updated:\n{}", application_line(&app)),
            data: Some(json!({"application": app})),
            ..Default::default()
        })
    }
}

struct JobApplicationsListTool {
    store: Store,
}

#[async_trait]
impl Tool for JobApplicationsListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "job_applications_list".into(),
            description: "List the acting member's job applications and the count per status, optionally only one status.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"status": {"type": "string", "enum": APPLICATION_STATUSES}},
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let apps = self
            .store
            .list_job_applications(&context.user_id, text_arg(&args, "status"))?;
        let counts = self.store.job_application_counts(&context.user_id)?;
        let summary = counts
            .iter()
            .map(|(s, n)| format!("{s}: {n}"))
            .collect::<Vec<_>>()
            .join(", ");
        let lines = apps
            .iter()
            .map(application_line)
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput {
            content: if apps.is_empty() {
                format!("No applications yet. ({summary})")
            } else {
                format!("Applications ({summary}):\n{lines}")
            },
            data: Some(json!({"applications": apps, "counts": counts})),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const AGENT: &str = "SeraBot/0.70 (test)";

    fn setup() -> (tempfile::TempDir, Store) {
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

    fn fetcher() -> SafeFetcher {
        SafeFetcher::new(AGENT, 5, 1_000_000)
            .unwrap()
            .allow_insecure_localhost()
    }

    fn ctx(user_id: &str) -> ToolContext {
        ToolContext {
            user_id: user_id.into(),
            actor: user_id.into(),
            bot: "jobfinder".into(),
            turn_id: Some("t1".into()),
            task_id: Some("task1".into()),
            conversation_id: Some("dir:jobfinder".into()),
            allowed_tools: HashSet::new(),
            require_approval: HashSet::new(),
        }
    }

    fn listing(deadline: &str) -> String {
        format!(
            r#"<main>
              <div><a href="/jobs/1">Comptable confirmé — Casablanca</a> Date limite : {deadline}</div>
              <div><a href="/jobs/2">Développeur Rust (CDI) Tanger</a></div>
              <div><a href="/jobs/3">Stage comptable Casablanca</a></div>
              <div><a href="/jobs/4">Comptable chez Acme SA Casablanca</a></div></main>"#
        )
    }

    async fn only_custom(store: &Store, user: &str, server: &MockServer) -> String {
        for builtin in crate::jobs::BUILTIN_SOURCES {
            store
                .set_job_source_auto_watch(user, builtin.key, false)
                .unwrap();
        }
        // The fetcher only reaches the loopback mock by its host name, which
        // member input validation (https, public host) would refuse.
        let url = server.uri().replace("127.0.0.1", "localhost");
        let key = "c-local".to_owned();
        store
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO job_sources(user_id, source_key, builtin, url, label, auto_watch, created_at)
                 VALUES (?1, ?2, 0, ?3, 'Local board', 1, 'now')",
                rusqlite::params![user, key, format!("{url}/list")],
            )
            .unwrap();
        key
    }

    fn profile() -> JobProfile {
        JobProfile {
            roles: vec!["comptable".into()],
            cities: vec!["Casablanca".into()],
            keywords_exclude: vec!["stage".into()],
            excluded_employers: vec!["Acme".into()],
            deadlines_to_tasks: true,
            seniority: "any".into(),
            frequency: "daily".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn watch_matches_dedupes_and_creates_deadline_tasks_per_member() {
        let server = MockServer::start().await;
        let future = (Utc::now() + chrono::Duration::days(20))
            .format("%d/%m/%Y")
            .to_string();
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /admin\n"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/list"))
            .respond_with(ResponseTemplate::new(200).set_body_string(listing(&future)))
            .mount(&server)
            .await;
        let (_dir, store) = setup();
        let alice = user(&store, "alice");
        let bob = user(&store, "bob");
        store.set_job_profile(&alice, &profile()).unwrap();
        only_custom(&store, &alice, &server).await;
        // The custom source is not a public-sector one, so no task; mark by
        // checking matching and dedupe first.
        let first = run_watch(
            &store,
            &fetcher(),
            AGENT,
            &alice,
            RunMode::Scheduled,
            None,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert_eq!(first.sources.len(), 1);
        assert_eq!(first.sources[0].status, "ok");
        assert_eq!(first.sources[0].items, 4);
        let titles: Vec<&str> = first.new_matches.iter().map(|h| h.title.as_str()).collect();
        assert_eq!(titles, vec!["Comptable confirmé — Casablanca"], "{first:?}");
        assert_eq!(first.new_matches[0].city.as_deref(), Some("Casablanca"));
        assert!(first.new_matches[0].deadline.is_some());
        assert_eq!(first.tasks_created, 0);

        let second = run_watch(
            &store,
            &fetcher(),
            AGENT,
            &alice,
            RunMode::Scheduled,
            None,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(second.new_matches.is_empty());
        assert_eq!(store.recent_job_matches(&alice, 10).unwrap().len(), 1);
        // Bob has no profile: nothing runs, nothing of Alice's is visible.
        assert!(
            run_watch(
                &store,
                &fetcher(),
                AGENT,
                &bob,
                RunMode::Manual,
                None,
                Duration::ZERO
            )
            .await
            .is_err()
        );
        assert!(store.recent_job_matches(&bob, 10).unwrap().is_empty());
        assert!(store.job_watch_state(&bob).unwrap().is_none());
        assert!(store.job_watch_state(&alice).unwrap().is_some());
    }

    #[tokio::test]
    async fn robots_disallow_blocks_schedule_but_not_a_member_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/robots.txt"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /list\n"),
            )
            .mount(&server)
            .await;
        let page = Mock::given(method("GET"))
            .and(path("/list"))
            .respond_with(ResponseTemplate::new(200).set_body_string(listing("01/01/2099")))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let (_dir, store) = setup();
        let alice = user(&store, "alice");
        store.set_job_profile(&alice, &profile()).unwrap();
        let key = only_custom(&store, &alice, &server).await;

        let scheduled = run_watch(
            &store,
            &fetcher(),
            AGENT,
            &alice,
            RunMode::Scheduled,
            None,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert_eq!(scheduled.sources[0].status, "robots");
        assert_eq!(scheduled.sources[0].items, 0);
        assert!(scheduled.new_matches.is_empty());

        let manual = run_watch(
            &store,
            &fetcher(),
            AGENT,
            &alice,
            RunMode::Manual,
            Some(&key),
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert_eq!(manual.sources[0].status, "ok");
        assert!(manual.sources[0].note.contains("robots.txt"));
        assert_eq!(manual.new_matches.len(), 1);
        drop(page);
    }

    #[tokio::test]
    async fn auto_watch_off_and_http_errors_are_reported() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/list"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let (_dir, store) = setup();
        let alice = user(&store, "alice");
        store.set_job_profile(&alice, &profile()).unwrap();
        let key = only_custom(&store, &alice, &server).await;
        // Nothing to watch on a schedule once auto-watch is off.
        store
            .set_job_source_auto_watch(&alice, &key, false)
            .unwrap();
        let none = run_watch(
            &store,
            &fetcher(),
            AGENT,
            &alice,
            RunMode::Scheduled,
            None,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(none.sources.is_empty());
        // A manual run still checks it; robots.txt 404 means allowed; HTTP 503 is an error line.
        let manual = run_watch(
            &store,
            &fetcher(),
            AGENT,
            &alice,
            RunMode::Manual,
            Some(&key),
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert_eq!(manual.sources[0].status, "error");
        assert!(manual.sources[0].note.contains("503"));
        assert!(
            run_watch(
                &store,
                &fetcher(),
                AGENT,
                &alice,
                RunMode::Manual,
                Some("nope"),
                Duration::ZERO
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn schedule_follows_the_chosen_frequency() {
        let now = Utc::now();
        let mut profile = profile();
        assert!(watch_due(&profile, None, now));
        let hours_ago = |h: i64| (now - chrono::Duration::hours(h)).to_rfc3339();
        assert!(!watch_due(&profile, Some(&hours_ago(5)), now));
        assert!(watch_due(&profile, Some(&hours_ago(23)), now));
        profile.frequency = "twice_daily".into();
        assert!(watch_due(&profile, Some(&hours_ago(12)), now));
        assert!(!watch_due(&profile, Some(&hours_ago(6)), now));
        profile.frequency = "weekly".into();
        assert!(!watch_due(&profile, Some(&hours_ago(24 * 3)), now));
        assert!(watch_due(&profile, Some(&hours_ago(24 * 7)), now));
        profile.frequency = "off".into();
        assert!(!watch_due(&profile, None, now));

        let (_dir, store) = setup();
        let a = user(&store, "a");
        let b = user(&store, "b");
        store
            .set_job_profile(
                &a,
                &JobProfile {
                    frequency: "daily".into(),
                    seniority: "any".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .set_job_profile(
                &b,
                &JobProfile {
                    frequency: "off".into(),
                    seniority: "any".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(due_members(&store, now).unwrap(), vec![a.clone()]);
        store
            .set_job_watch_state(&a, "scheduled", &json!({}))
            .unwrap();
        assert!(due_members(&store, now).unwrap().is_empty());
    }

    #[test]
    fn message_has_at_most_eight_lines_with_the_fields() {
        let hits: Vec<JobHit> = (0..11)
            .map(|n| JobHit {
                title: format!("Job {n}"),
                employer: (n % 2 == 0).then(|| "Beta".to_owned()),
                city: Some("Rabat".into()),
                deadline: (n == 0).then(|| "2026-12-01".to_owned()),
                url: format!("https://x.example/{n}"),
                source: "s".into(),
            })
            .collect();
        let text = format_matches(&hits);
        assert!(text.starts_with("New job matches (11):"));
        assert!(
            text.contains("1. Job 0 — Beta — Rabat — deadline 2026-12-01 — https://x.example/0")
        );
        assert!(text.contains("2. Job 1 — Rabat — https://x.example/1"));
        assert!(text.contains("8. Job 7"));
        assert!(!text.contains("9. Job 8"));
        assert!(text.contains("and 3 more"));
    }

    #[tokio::test]
    async fn public_deadline_becomes_a_personal_task_once() {
        let (_dir, store) = setup();
        let alice = user(&store, "alice");
        let future = (Utc::now() + chrono::Duration::days(10)).date_naive();
        let item = JobItem {
            title: "Concours rédacteur".into(),
            employer: Some("Commune de Rabat".into()),
            city: Some("Rabat".into()),
            deadline: Some(future.format("%Y-%m-%d").to_string()),
            url: "https://www.emploi-public.ma/fr/concours/1".into(),
            ..Default::default()
        };
        assert!(deadline_task(
            &store,
            &alice,
            &item,
            Utc::now().date_naive()
        ));
        let tasks = personal_tasks::list(&store, &alice, None, None, None, 10).unwrap();
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].title.starts_with("Deadline: Concours"));
        let past = JobItem {
            deadline: Some("2020-01-01".into()),
            ..item.clone()
        };
        assert!(!deadline_task(
            &store,
            &alice,
            &past,
            Utc::now().date_naive()
        ));
        let none = JobItem {
            deadline: None,
            ..item
        };
        assert!(!deadline_task(
            &store,
            &alice,
            &none,
            Utc::now().date_naive()
        ));
    }

    #[tokio::test]
    async fn tools_are_member_scoped_and_validated() {
        let (_dir, store) = setup();
        let alice = user(&store, "alice");
        let bob = user(&store, "bob");
        let mut registry = ToolRegistry::default();
        register_job_watch_tools(&mut registry, store.clone(), Arc::new(Config::default()))
            .unwrap();
        let call = |name: &'static str, args: Value, who: String| {
            let tool = registry.get(name).unwrap();
            async move { tool.execute(args, &ctx(&who)).await }
        };

        let none = call("job_profile_get", json!({}), alice.clone())
            .await
            .unwrap();
        assert!(none.content.contains("No job profile yet"));
        let saved = call(
            "job_profile_set",
            json!({"roles": ["comptable"], "cities": ["Rabat"], "frequency": "weekly"}),
            alice.clone(),
        )
        .await
        .unwrap();
        assert!(saved.content.contains("Saved"));
        // A partial update keeps the rest.
        call(
            "job_profile_set",
            json!({"seniority": "junior"}),
            alice.clone(),
        )
        .await
        .unwrap();
        let got = call("job_profile_get", json!({}), alice.clone())
            .await
            .unwrap();
        assert!(got.content.contains("comptable") && got.content.contains("junior"));
        assert!(
            call("job_profile_get", json!({}), bob.clone())
                .await
                .unwrap()
                .content
                .contains("No job profile yet")
        );
        assert!(
            call(
                "job_profile_set",
                json!({"frequency": "hourly"}),
                alice.clone()
            )
            .await
            .is_err()
        );
        assert!(
            call("job_profile_set", json!({"bogus": 1}), alice.clone())
                .await
                .is_err()
        );

        let added = call(
            "job_sources_set",
            json!({"action": "add", "url": "https://jobs.example.org/list", "label": "Mine"}),
            alice.clone(),
        )
        .await
        .unwrap();
        assert!(added.content.contains("Added Mine"));
        let key = added.data.unwrap()["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["label"] == "Mine")
            .unwrap()["key"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            !call("job_sources_list", json!({}), bob.clone())
                .await
                .unwrap()
                .content
                .contains("Mine")
        );
        assert!(
            call(
                "job_sources_set",
                json!({"action": "remove", "key": key}),
                bob.clone()
            )
            .await
            .is_err()
        );
        call(
            "job_sources_set",
            json!({"action": "auto_watch", "key": "rekrute", "auto_watch": false}),
            alice.clone(),
        )
        .await
        .unwrap();
        assert!(
            call("job_sources_list", json!({}), alice.clone())
                .await
                .unwrap()
                .content
                .contains("(key=rekrute, auto-watch off")
        );
        assert!(
            call(
                "job_sources_set",
                json!({"action": "add", "url": "https://localhost/"}),
                alice.clone()
            )
            .await
            .is_err()
        );

        let app = call(
            "job_application_add",
            json!({"title": "Comptable", "employer": "Beta"}),
            alice.clone(),
        )
        .await
        .unwrap();
        let id = app.data.unwrap()["application"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        call(
            "job_application_update",
            json!({"id": id, "status": "interview", "interview_on": "2026-12-02"}),
            alice.clone(),
        )
        .await
        .unwrap();
        assert!(
            call(
                "job_application_update",
                json!({"id": id, "status": "saved"}),
                alice.clone()
            )
            .await
            .is_err()
        );
        assert!(
            call(
                "job_application_update",
                json!({"id": id, "notes": "x"}),
                bob.clone()
            )
            .await
            .is_err()
        );
        let list = call("job_applications_list", json!({}), alice.clone())
            .await
            .unwrap();
        assert!(list.content.contains("interview: 1") && list.content.contains("Comptable"));
        assert!(
            call("job_applications_list", json!({}), bob)
                .await
                .unwrap()
                .content
                .contains("No applications")
        );
    }
}
