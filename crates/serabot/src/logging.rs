//! Server logging (0.28.0, phase C4): a daily-rotating file writer under
//! `data/logs/` plus an in-memory ring buffer that the owner console reads
//! through `GET /v1/logs`.
//!
//! Wiring is deliberately split: [`install_default`] installs the tracing
//! layers (call once at process start) and the `logs` route is served by
//! `crate::api::logs` (merged into the protected API router). This module
//! owns only the writers and the ring buffer — no config or main changes.
//!
//! Retention: one `serabot-YYYY-MM-DD.log` file per UTC day, at most
//! [`LOG_RETENTION_DAYS`] files; older files are pruned on rotation and at
//! install. The ring buffer keeps the last [`RING_CAPACITY`] events.
//!
//! Secrets never reach the ring buffer unredacted: every message passes
//! through the redactor installed with [`set_redactor`] (the wiring should
//! pass the gateway/vault redactor), and when none is installed a built-in
//! scrub of bearer tokens and long key-like strings still applies.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use axum::extract::{Extension, Query};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{Event, Metadata, Subscriber, field::Visit};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::error::{Result, SeraError};

/// Events kept in the in-memory ring buffer.
pub const RING_CAPACITY: usize = 2000;
/// Daily log files kept on disk.
pub const LOG_RETENTION_DAYS: i64 = 7;
/// Log file name prefix: `serabot-YYYY-MM-DD.log`.
pub const LOG_FILE_PREFIX: &str = "serabot";

/// One captured log event, as served by `GET /v1/logs`. The message is
/// already redacted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEvent {
    pub ts: String,
    pub level: String,
    pub target: String,
    pub message: String,
}

/// The process-wide ring buffer.
#[derive(Debug, Default)]
pub struct RingBuffer {
    events: Mutex<VecDeque<LogEvent>>,
}

impl RingBuffer {
    fn push(&self, event: LogEvent) {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while events.len() >= RING_CAPACITY {
            events.pop_front();
        }
        events.push_back(event);
    }

    /// Snapshot newest first, filtered by minimum `level` (when given) and a
    /// case-insensitive substring `q` over message and target.
    pub fn snapshot(&self, level: Option<&str>, q: Option<&str>, limit: usize) -> Vec<LogEvent> {
        let minimum = level.and_then(parse_level);
        let needle = q
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase);
        let events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        events
            .iter()
            .rev()
            .filter(|event| minimum.is_none_or(|min| level_rank(&event.level) >= min))
            .filter(|event| {
                needle.as_ref().is_none_or(|needle| {
                    event.message.to_ascii_lowercase().contains(needle)
                        || event.target.to_ascii_lowercase().contains(needle)
                })
            })
            .take(limit.clamp(1, RING_CAPACITY))
            .cloned()
            .collect()
    }
}

fn ring() -> &'static RingBuffer {
    static RING: OnceLock<RingBuffer> = OnceLock::new();
    RING.get_or_init(RingBuffer::default)
}

fn parse_level(text: &str) -> Option<u8> {
    match text.trim().to_ascii_lowercase().as_str() {
        "trace" => Some(0),
        "debug" => Some(1),
        "info" => Some(2),
        "warn" | "warning" => Some(3),
        "error" => Some(4),
        _ => None,
    }
}

fn level_rank(level: &str) -> u8 {
    parse_level(level).unwrap_or(2)
}

type Redactor = Arc<dyn Fn(&str) -> String + Send + Sync>;

fn redactor() -> &'static RwLock<Option<Redactor>> {
    static REDACTOR: OnceLock<RwLock<Option<Redactor>>> = OnceLock::new();
    REDACTOR.get_or_init(|| RwLock::new(None))
}

/// Install the secret redactor the ring buffer applies to every message.
/// The intended wiring passes a closure over the gateway's vault redactor
/// (`Policy::redact_text` / `vault::Redactor`); until one is installed a
/// built-in scrub of bearer tokens and long key-like strings still runs.
pub fn set_redactor(redact: Redactor) {
    let slot = redactor();
    *slot
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(redact);
}

/// Best-effort built-in scrub for when no redactor is installed: bearer
/// tokens and long hex/base64-like runs never reach the ring buffer.
fn builtin_scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("Bearer ") {
        out.push_str(&rest[..at]);
        out.push_str("Bearer [redacted]");
        let after = &rest[at + "Bearer ".len()..];
        let end = after
            .find(|c: char| c.is_whitespace())
            .unwrap_or(after.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    let mut scrubbed = String::with_capacity(out.len());
    let mut run = String::new();
    for c in out.chars() {
        if c.is_ascii_alphanumeric() && run.chars().count() < 64 {
            run.push(c);
        } else {
            if run.len() >= 32 && run.chars().any(|c| c.is_ascii_digit()) {
                scrubbed.push_str("[redacted]");
            } else {
                scrubbed.push_str(&run);
            }
            run.clear();
            scrubbed.push(c);
        }
    }
    if run.len() >= 32 && run.chars().any(|c| c.is_ascii_digit()) {
        scrubbed.push_str("[redacted]");
    } else {
        scrubbed.push_str(&run);
    }
    scrubbed
}

fn redact(text: &str) -> String {
    let slot = redactor();
    let guard = slot.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    let once = guard
        .as_ref()
        .map(|redact| redact(text))
        .unwrap_or_default();
    let base = if guard.is_some() {
        once
    } else {
        text.to_owned()
    };
    builtin_scrub(&base)
}

/// Collects an event's fields into one readable line: the `message` field
/// verbatim, every other field as `name=value`.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
            // `record_debug` for message arrives as a quoted string; strip
            // the surrounding quotes so the log reads like the text itself.
            if self.message.len() >= 2
                && self.message.starts_with('"')
                && self.message.ends_with('"')
            {
                self.message = self.message[1..self.message.len() - 1].to_owned();
            }
        } else {
            self.fields
                .push_str(&format!(" {}={:?}", field.name(), value));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
        } else {
            self.fields
                .push_str(&format!(" {}={}", field.name(), value));
        }
    }
}

/// Event formatter that redacts before anything is written, so both the
/// rolling files and stdout stay secret-free. Format:
/// `2026-11-08T10:00:00.000Z WARN target: message fields`.
struct RedactingFormatter;

impl<S, N> tracing_subscriber::fmt::FormatEvent<S, N> for RedactingFormatter
where
    S: Subscriber + for<'writer> tracing_subscriber::registry::LookupSpan<'writer>,
    N: for<'writer> tracing_subscriber::fmt::FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        _ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let meta = event.metadata();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let mut message = visitor.message;
        if !visitor.fields.is_empty() {
            message.push_str(&visitor.fields);
        }
        let stamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
        writeln!(
            writer,
            "{stamp} {} {}: {}",
            meta.level(),
            meta.target(),
            redact(&message)
        )
    }
}

/// Ring-buffer layer: every event is formatted, redacted, and kept.
struct RingLayer;

impl<S: Subscriber> Layer<S> for RingLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        let meta: &Metadata<'_> = event.metadata();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let mut message = visitor.message;
        if !visitor.fields.is_empty() {
            message.push_str(&visitor.fields);
        }
        ring().push(LogEvent {
            ts: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            level: meta.level().as_str().to_ascii_lowercase(),
            target: meta.target().to_owned(),
            message: redact(&message),
        });
    }
}

/// Daily-rotating file writer: appends to `serabot-YYYY-MM-DD.log` under the
/// log directory, rolling at UTC midnight and pruning files older than
/// [`LOG_RETENTION_DAYS`] days.
#[derive(Clone)]
struct DailyWriter {
    dir: Arc<PathBuf>,
    state: Arc<Mutex<WriterState>>,
}

struct WriterState {
    date: Option<NaiveDate>,
    file: Option<std::fs::File>,
}

impl DailyWriter {
    fn new(dir: PathBuf) -> Self {
        Self {
            dir: Arc::new(dir),
            state: Arc::new(Mutex::new(WriterState {
                date: None,
                file: None,
            })),
        }
    }

    fn log_path(dir: &Path, date: NaiveDate) -> PathBuf {
        dir.join(format!("{LOG_FILE_PREFIX}-{date}.log"))
    }

    /// Remove `serabot-*.log` files older than the retention window.
    fn prune(dir: &Path, today: NaiveDate) -> io::Result<()> {
        let cutoff = today - chrono::Duration::days(LOG_RETENTION_DAYS);
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(date) = name
                .strip_prefix(&format!("{LOG_FILE_PREFIX}-"))
                .and_then(|rest| rest.strip_suffix(".log"))
                .and_then(|rest| NaiveDate::parse_from_str(rest, "%Y-%m-%d").ok())
            else {
                continue;
            };
            if date < cutoff {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(())
    }
}

impl Write for DailyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let today = Utc::now().date_naive();
        if state.date != Some(today) {
            std::fs::create_dir_all(self.dir.as_ref())?;
            let path = Self::log_path(&self.dir, today);
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            state.file = Some(file);
            state.date = Some(today);
            let _ = Self::prune(&self.dir, today);
        }
        match state.file.as_mut() {
            Some(file) => file.write_all(buf).map(|_| buf.len()),
            None => Err(io::Error::other("log file is not open")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match state.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for DailyWriter {
    type Writer = DailyWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Install the default tracing pipeline: stdout plus the daily-rotating file
/// writer under `log_dir`, and the redacting ring buffer. Idempotent: a
/// second call (as in tests) is a no-op.
pub fn install_default(log_dir: impl Into<PathBuf>) -> Result<()> {
    let dir = log_dir.into();
    std::fs::create_dir_all(&dir).map_err(SeraError::from)?;
    let _ = DailyWriter::prune(&dir, Utc::now().date_naive());

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,serabot=debug"));
    let file_writer = DailyWriter::new(dir);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(file_writer)
        .with_ansi(false)
        .event_format(RedactingFormatter)
        .with_filter(filter.clone());
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_writer(io::stdout)
        .event_format(RedactingFormatter)
        .with_filter(filter);

    // `try_init` fails only when a dispatcher is already installed (tests);
    // that is the idempotent path, not an error.
    let _ = tracing_subscriber::registry()
        .with(file_layer)
        .with(stdout_layer)
        .with(RingLayer)
        .try_init();
    Ok(())
}

/// The log query behind `GET /v1/logs?level=warn&q=...`.
#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    /// Minimum level: trace, debug, info, warn, error.
    #[serde(default)]
    pub level: Option<String>,
    /// Case-insensitive substring over message and target.
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `GET /v1/logs`: the ring buffer, owner only. Every event is already
/// redacted before it entered the buffer.
pub async fn logs_handler(
    Extension(user): Extension<crate::api::ActingUser>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<Value>> {
    if !user.is_admin() {
        return Err(SeraError::Forbidden(
            "the logs endpoint requires the admin role".into(),
        ));
    }
    let limit = query.limit.unwrap_or(200);
    let events = ring().snapshot(
        query.level.as_deref(),
        query.q.as_deref(),
        limit.clamp(1, RING_CAPACITY),
    );
    let count = events.len();
    Ok(Json(json!({
        "logs": events,
        "count": count,
        "capacity": RING_CAPACITY,
    })))
}

/// The logs route as a free-standing router, for wiring outside `api`.
pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new().route("/v1/logs", get(logs_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(level: &str, message: &str) -> LogEvent {
        LogEvent {
            ts: "2026-11-08T10:00:00.000Z".into(),
            level: level.into(),
            target: "serabot::test".into(),
            message: message.into(),
        }
    }

    #[test]
    fn ring_buffer_caps_and_filters() {
        let ring = RingBuffer::default();
        for index in 0..(RING_CAPACITY + 50) {
            ring.push(event("info", &format!("line {index}")));
        }
        assert_eq!(ring.events.lock().unwrap().len(), RING_CAPACITY);
        // Newest first, level floor, substring filter.
        let all = ring.snapshot(None, None, 10);
        assert_eq!(all.len(), 10);
        assert!(all[0].message.contains(&(RING_CAPACITY + 49).to_string()));
        let warns = ring.snapshot(Some("warn"), None, 100);
        assert!(warns.iter().all(|event| level_rank(&event.level) >= 3));
        let hits = ring.snapshot(None, Some("line 199"), 100);
        assert!(hits.iter().all(|event| event.message.contains("line 199")));
        assert!(!hits.is_empty());
    }

    #[test]
    fn builtin_scrub_hides_tokens_and_long_keys() {
        let scrubbed = builtin_scrub("auth: Bearer abcdef123456 tail");
        assert!(scrubbed.contains("Bearer [redacted]"));
        assert!(!scrubbed.contains("abcdef123456"));
        let key = "k".to_owned() + &"a1".repeat(20);
        let scrubbed = builtin_scrub(&format!("leaked {key} here"));
        assert!(!scrubbed.contains(&key));
        // Short words pass through untouched.
        assert_eq!(builtin_scrub("hello world"), "hello world");
    }

    #[test]
    fn daily_writer_rotates_and_prunes() {
        let dir = std::env::temp_dir().join(format!("serabot-log-test-{}", uuid::Uuid::new_v4()));
        let writer = DailyWriter::new(dir.clone());
        // A file from 30 days ago is pruned on the first write.
        let old = DailyWriter::log_path(&dir, Utc::now().date_naive() - chrono::Duration::days(30));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&old, b"old").unwrap();
        let mut writer = writer;
        writer.write_all(b"hello").unwrap();
        writer.flush().unwrap();
        let today = DailyWriter::log_path(&dir, Utc::now().date_naive());
        assert_eq!(std::fs::read(&today).unwrap(), b"hello");
        assert!(!old.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn logs_handler_requires_admin() {
        use crate::api::ActingUser;
        use crate::store::UserRole;
        let member = ActingUser {
            id: "member".into(),
            role: UserRole::Member,
            session_id: None,
            token_hash: "x".repeat(64),
        };
        let result = logs_handler(
            Extension(member),
            Query(LogsQuery {
                level: None,
                q: None,
                limit: None,
            }),
        )
        .await;
        assert!(matches!(result, Err(SeraError::Forbidden(_))));
    }
}
