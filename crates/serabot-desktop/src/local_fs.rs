//! Large-folder engine for the local folders harness (0.21).
//!
//! Everything here runs on this PC for a request the server relayed. The
//! allowlist and the path rules live in [`crate::local_folders`]; this module
//! adds the bounded operations on top of them:
//!
//! * a resumable, depth-limited walk that never follows links, junctions or
//!   reparse points, skips hidden/system entries by default, and stops at a
//!   time budget with a cursor instead of freezing on a 100k-file tree;
//! * `list` (recursive, filtered, sorted, paginated, or summary counts only),
//!   `search` (names and optionally text content), `stat`, and ranged `read`.
//!
//! Cursors are opaque to the model. They only say where to resume and carry
//! summary counts; they are never joined into a path.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::Value;

use crate::local_folders::{is_link, resolve, sensitive_name};

/// Per-call output ceiling for reads, in bytes.
pub const MAX_READ_BYTES: usize = 256 * 1024;
/// Entries per list page (default / hard cap).
pub const DEFAULT_PAGE: usize = 100;
pub const MAX_PAGE: usize = 500;
/// Deepest recursion any walk will go.
pub const MAX_DEPTH: usize = 12;
/// Entries collected for a size/date sort before the sort is declared partial.
pub const MAX_SORT_SCAN: usize = 20_000;
/// Time a single list/search/stat call may spend walking.
pub const WALK_BUDGET: Duration = Duration::from_secs(8);
/// Search results per call (default / hard cap).
pub const DEFAULT_RESULTS: usize = 50;
pub const MAX_RESULTS: usize = 200;
/// Per-file and per-call content scan limits for `search`.
pub const DEFAULT_SCAN_FILE_BYTES: u64 = 1024 * 1024;
pub const MAX_SCAN_FILE_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_SCAN_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
/// Bytes of a file inspected to tell text from binary.
const SNIFF_BYTES: usize = 8192;
/// Longest stored line in a line-range read.
const MAX_LINE_BYTES: usize = 32 * 1024;
/// Upper bound on bytes scanned to reach a line range.
const MAX_LINE_SCAN_BYTES: u64 = 256 * 1024 * 1024;

/// Time and cancellation limit shared by one request.
#[derive(Clone)]
pub struct Budget {
    deadline: Instant,
    cancel: Arc<AtomicBool>,
}

impl Budget {
    pub fn new(limit: Duration) -> Self {
        Self {
            deadline: Instant::now() + limit,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_cancel(limit: Duration, cancel: Arc<AtomicBool>) -> Self {
        Self {
            deadline: Instant::now() + limit,
            cancel,
        }
    }

    pub fn expired(&self) -> bool {
        Instant::now() >= self.deadline || self.cancel.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Names, globs, dates
// ---------------------------------------------------------------------------

const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Entry names the harness can later address (the request path rules refuse
/// everything else), so listings never show what could not be opened:
/// alternate-stream colons, reserved device names, trailing dot/space,
/// control or wildcard characters.
pub fn name_addressable(name: &str) -> bool {
    if name.is_empty() || name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        return false;
    }
    !name.chars().any(|c| {
        c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*' | ':' | '/' | '\\')
    })
}

/// Hidden by dot-name everywhere, and by the hidden/system attribute on Windows.
fn is_hidden(name: &str, meta: &std::fs::Metadata) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const HIDDEN: u32 = 0x2;
        const SYSTEM: u32 = 0x4;
        if meta.file_attributes() & (HIDDEN | SYSTEM) != 0 {
            return true;
        }
    }
    #[cfg(not(windows))]
    let _ = meta;
    false
}

/// Case-insensitive match of a file name against `pattern`. `*` and `?` are
/// wildcards; a pattern without them matches as a substring. Iterative, so a
/// hostile pattern cannot blow up.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let text: Vec<char> = text.to_lowercase().chars().collect();
    if !pattern.iter().any(|c| matches!(c, '*' | '?')) {
        if pattern.is_empty() {
            return true;
        }
        return text.windows(pattern.len()).any(|w| w == pattern.as_slice());
    }
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// `YYYY-MM-DD` for Unix seconds (UTC).
pub fn iso_date(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `YYYY-MM-DDTHH:MM:SSZ` for Unix seconds (UTC).
pub fn iso_datetime(secs: u64) -> String {
    let rem = secs % 86_400;
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        iso_date(secs),
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn modified_secs(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

/// `1.5 MiB` style size for summaries.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

// ---------------------------------------------------------------------------
// Containment
// ---------------------------------------------------------------------------

/// Windows paths compare without case and without the `\\?\` prefix.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn normalize_windows(path: &str) -> String {
    let trimmed = path
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .or_else(|| path.strip_prefix(r"\\?\").map(str::to_owned))
        .unwrap_or_else(|| path.to_owned());
    trimmed
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// True when `path` is `root` or inside it.
pub(crate) fn path_within(root: &Path, path: &Path) -> bool {
    #[cfg(windows)]
    {
        let root = normalize_windows(&root.to_string_lossy());
        let path = normalize_windows(&path.to_string_lossy());
        path == root || path.starts_with(&format!("{root}\\"))
    }
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
}

// ---------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Cursor {
    /// Resume after this entry (components relative to the listed folder).
    after: Vec<String>,
    /// Offset into a sorted collection (size/date sorts).
    offset: usize,
    /// Summary counts carried between calls.
    files: u64,
    folders: u64,
    bytes: u64,
}

impl Cursor {
    fn encode(&self) -> String {
        let value = serde_json::json!({
            "a": self.after, "o": self.offset, "f": self.files, "d": self.folders, "b": self.bytes
        });
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
    }

    fn decode(text: &str) -> Result<Self, String> {
        if text.len() > 4096 {
            return Err("invalid cursor".into());
        }
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| "invalid cursor".to_string())?;
        let value: Value =
            serde_json::from_slice(&raw).map_err(|_| "invalid cursor".to_string())?;
        let after: Vec<String> = value
            .get("a")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| item.as_str().unwrap_or_default().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        if after.len() > MAX_DEPTH + 1
            || after.iter().any(|part| {
                part.is_empty()
                    || part.len() > 255
                    || part == ".."
                    || part.contains(['/', '\\', '\0'])
            })
        {
            return Err("invalid cursor".into());
        }
        let number = |key: &str| value.get(key).and_then(Value::as_u64).unwrap_or(0);
        Ok(Self {
            after,
            offset: number("o").min(10_000_000) as usize,
            files: number("f"),
            folders: number("d"),
            bytes: number("b"),
        })
    }
}

// ---------------------------------------------------------------------------
// Walk
// ---------------------------------------------------------------------------

struct Entry {
    rel: Vec<String>,
    is_dir: bool,
    size: u64,
    modified: Option<u64>,
}

impl Entry {
    fn name(&self) -> &str {
        self.rel.last().map(String::as_str).unwrap_or_default()
    }

    fn rel_text(&self) -> String {
        self.rel.join("/")
    }

    fn extension(&self) -> String {
        Path::new(self.name())
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
    }
}

enum Flow {
    Continue,
    Stop,
}

struct WalkEnd {
    /// The walk ran out of entries.
    complete: bool,
    /// Last entry handed to the visitor (resume point).
    last: Option<Vec<String>>,
    /// Entries refused (links, unaddressable names, unreadable).
    skipped: u64,
    /// Entries looked at, including filtered ones.
    scanned: u64,
}

struct Walker<'a> {
    root: &'a Path,
    max_depth: usize,
    include_hidden: bool,
    budget: &'a Budget,
    visit: &'a mut dyn FnMut(&Entry) -> Flow,
    last: Option<Vec<String>>,
    skipped: u64,
    scanned: u64,
    stopped: bool,
}

fn sort_key(name: &str) -> (String, String) {
    (name.to_lowercase(), name.to_owned())
}

impl Walker<'_> {
    fn dir(&mut self, dir: &Path, rel: &mut Vec<String>, after: Option<&[String]>) {
        // Re-check containment of every folder we are about to enter, so a
        // swap between the entry check and the read cannot leave the root.
        match dir.canonicalize() {
            Ok(real) if path_within(self.root, &real) => {}
            _ => {
                self.skipped += 1;
                return;
            }
        }
        let Ok(read) = std::fs::read_dir(dir) else {
            self.skipped += 1;
            return;
        };
        let mut names: Vec<String> = Vec::new();
        for item in read {
            let Ok(item) = item else { continue };
            let Some(name) = item.file_name().to_str().map(str::to_owned) else {
                self.skipped += 1;
                continue;
            };
            names.push(name);
        }
        names.sort_by_key(|name| sort_key(name));
        let resume = after.and_then(|parts| parts.first());
        let resume_key = resume.map(|name| sort_key(name));
        for name in names {
            if self.stopped {
                return;
            }
            let mut descend_after: Option<&[String]> = None;
            let mut emit = true;
            if let (Some(key), Some(parts)) = (&resume_key, after) {
                let this = sort_key(&name);
                if this < *key {
                    continue;
                }
                if this == *key {
                    emit = false;
                    if parts.len() > 1 {
                        descend_after = Some(&parts[1..]);
                    }
                }
            }
            if self.budget.expired() {
                self.stopped = true;
                return;
            }
            self.scanned += 1;
            if !name_addressable(&name) {
                self.skipped += 1;
                continue;
            }
            let path = dir.join(&name);
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                self.skipped += 1;
                continue;
            };
            if is_link(&meta) {
                self.skipped += 1;
                continue;
            }
            if (!self.include_hidden && is_hidden(&name, &meta)) || sensitive_name(&name) {
                continue;
            }
            let is_dir = meta.is_dir();
            if !is_dir && !meta.is_file() {
                continue;
            }
            rel.push(name);
            if emit {
                let entry = Entry {
                    rel: rel.clone(),
                    is_dir,
                    size: if is_dir { 0 } else { meta.len() },
                    modified: modified_secs(&meta),
                };
                self.last = Some(entry.rel.clone());
                if let Flow::Stop = (self.visit)(&entry) {
                    self.stopped = true;
                    rel.pop();
                    return;
                }
            }
            if is_dir && rel.len() < self.max_depth {
                self.dir(&path, rel, descend_after);
            }
            rel.pop();
        }
    }
}

/// Walk `start` (inside `root`) in a stable order: names sorted without case,
/// folders before their children.
fn walk(
    root: &Path,
    start: &Path,
    max_depth: usize,
    include_hidden: bool,
    after: &[String],
    budget: &Budget,
    visit: &mut dyn FnMut(&Entry) -> Flow,
) -> WalkEnd {
    let mut walker = Walker {
        root,
        max_depth,
        include_hidden,
        budget,
        visit,
        last: None,
        skipped: 0,
        scanned: 0,
        stopped: false,
    };
    let mut rel = Vec::new();
    let after = if after.is_empty() { None } else { Some(after) };
    walker.dir(start, &mut rel, after);
    WalkEnd {
        complete: !walker.stopped,
        last: walker.last,
        skipped: walker.skipped,
        scanned: walker.scanned,
    }
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn arg_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn arg_usize(args: &Value, key: &str, default: usize, min: usize, max: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .map_or(default, |value| value as usize)
        .clamp(min, max)
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn arg_extensions(args: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |text: &str| {
        let ext = text.trim().trim_start_matches('.').to_ascii_lowercase();
        if !ext.is_empty() && ext.len() <= 16 && out.len() < 16 {
            out.push(ext);
        }
    };
    match args.get("extensions") {
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).for_each(&mut push),
        Some(Value::String(text)) => text.split([',', ' ']).for_each(&mut push),
        _ => {}
    }
    out
}

fn arg_pattern(args: &Value, key: &str) -> Result<Option<String>, String> {
    match arg_str(args, key) {
        Some(pattern) if pattern.len() > 128 => Err(format!("{key} is too long")),
        Some(pattern) => Ok(Some(pattern.to_owned())),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Name,
    Size,
    Modified,
    Type,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Any,
    File,
    Dir,
}

struct Filter {
    pattern: Option<String>,
    extensions: Vec<String>,
    kind: Kind,
}

impl Filter {
    fn from_args(args: &Value) -> Result<Self, String> {
        let kind = match arg_str(args, "kind") {
            None | Some("any") => Kind::Any,
            Some("file") | Some("files") => Kind::File,
            Some("dir") | Some("folder") | Some("directory") => Kind::Dir,
            Some(_) => return Err("kind must be any, file or dir".into()),
        };
        Ok(Self {
            pattern: arg_pattern(args, "pattern")?,
            extensions: arg_extensions(args),
            kind,
        })
    }

    fn matches(&self, entry: &Entry) -> bool {
        match self.kind {
            Kind::File if entry.is_dir => return false,
            Kind::Dir if !entry.is_dir => return false,
            _ => {}
        }
        if !self.extensions.is_empty()
            && (entry.is_dir || !self.extensions.contains(&entry.extension()))
        {
            return false;
        }
        if let Some(pattern) = &self.pattern {
            return glob_match(pattern, entry.name());
        }
        true
    }
}

fn describe(entry: &Entry) -> String {
    let date = entry
        .modified
        .map(|secs| format!(", {}", iso_date(secs)))
        .unwrap_or_default();
    if entry.is_dir {
        format!("{}/", entry.rel_text())
    } else {
        format!("{} ({} bytes{date})", entry.rel_text(), entry.size)
    }
}

/// `local_list`: one page of a (possibly recursive) listing, or only counts.
pub fn list(root: &Path, rel: &str, args: &Value, budget: &Budget) -> Result<String, String> {
    let root_canon = root
        .canonicalize()
        .map_err(|_| "the allowed folder is no longer available".to_string())?;
    let dir = if rel == "." {
        root_canon.clone()
    } else {
        resolve(root, rel, true)?
    };
    let meta = std::fs::symlink_metadata(&dir).map_err(|_| "could not be opened".to_string())?;
    if !meta.is_dir() || is_link(&meta) {
        return Err("not a folder".into());
    }
    let recursive = arg_bool(args, "recursive");
    let max_depth = if recursive {
        arg_usize(args, "max_depth", 3, 1, MAX_DEPTH)
    } else {
        1
    };
    let filter = Filter::from_args(args)?;
    let include_hidden = arg_bool(args, "include_hidden");
    let page = arg_usize(args, "page_size", DEFAULT_PAGE, 1, MAX_PAGE);
    let cursor = match arg_str(args, "cursor") {
        Some(text) => Cursor::decode(text)?,
        None => Cursor::default(),
    };
    if arg_bool(args, "summary") {
        return list_summary(
            &root_canon,
            &dir,
            rel,
            recursive,
            args,
            &filter,
            include_hidden,
            cursor,
            budget,
        );
    }
    let sort = match arg_str(args, "sort") {
        None | Some("name") => Sort::Name,
        Some("size") => Sort::Size,
        Some("modified") | Some("date") => Sort::Modified,
        Some("type") | Some("extension") => Sort::Type,
        Some(_) => return Err("sort must be name, size, modified or type".into()),
    };
    let descending = matches!(arg_str(args, "order"), Some("desc") | Some("descending"));

    let mut lines: Vec<String> = Vec::new();
    let header = format!(
        "{} {}{}",
        if recursive {
            "Recursive listing of"
        } else {
            "Listing of"
        },
        rel,
        if recursive {
            format!(" (depth {max_depth})")
        } else {
            String::new()
        }
    );

    if sort == Sort::Name && !descending {
        let mut next: Option<Cursor> = None;
        let mut last_shown: Option<Vec<String>> = None;
        let mut overflow = false;
        let mut visit = |entry: &Entry| {
            if !filter.matches(entry) {
                return Flow::Continue;
            }
            if lines.len() >= page {
                // One entry past the page proves there is more.
                overflow = true;
                return Flow::Stop;
            }
            lines.push(describe(entry));
            last_shown = Some(entry.rel.clone());
            Flow::Continue
        };
        let end = walk(
            &root_canon,
            &dir,
            max_depth,
            include_hidden,
            &cursor.after,
            budget,
            &mut visit,
        );
        let mut text = header;
        text.push('\n');
        if lines.is_empty() {
            text.push_str("(no matching entries)");
        } else {
            text.push_str(&lines.join("\n"));
        }
        if !end.complete {
            let after = if overflow {
                last_shown.clone().unwrap_or_default()
            } else {
                end.last.clone().unwrap_or(cursor.after.clone())
            };
            next = Some(Cursor {
                after,
                ..Cursor::default()
            });
        }
        text.push_str(&format!(
            "\n-- {} shown, {} scanned",
            lines.len(),
            end.scanned
        ));
        if end.skipped > 0 {
            text.push_str(&format!(", {} not shown (links, odd names)", end.skipped));
        }
        match next {
            Some(next) => {
                let reason = if overflow {
                    "page full"
                } else {
                    "time budget reached; partial result"
                };
                text.push_str(&format!(
                    "\nmore: true ({reason})\nnext_cursor: {}",
                    next.encode()
                ));
            }
            None => text.push_str("\nmore: false"),
        }
        return Ok(text);
    }

    // Size / date / type sorts (and descending name) need the whole set.
    let mut collected: Vec<Entry> = Vec::new();
    let mut capped = false;
    let mut visit = |entry: &Entry| {
        if !filter.matches(entry) {
            return Flow::Continue;
        }
        if collected.len() >= MAX_SORT_SCAN {
            capped = true;
            return Flow::Stop;
        }
        collected.push(Entry {
            rel: entry.rel.clone(),
            is_dir: entry.is_dir,
            size: entry.size,
            modified: entry.modified,
        });
        Flow::Continue
    };
    let end = walk(
        &root_canon,
        &dir,
        max_depth,
        include_hidden,
        &[],
        budget,
        &mut visit,
    );
    collected.sort_by(|a, b| {
        let order = match sort {
            Sort::Name => sort_key(&a.rel_text()).cmp(&sort_key(&b.rel_text())),
            Sort::Size => a.size.cmp(&b.size),
            Sort::Modified => a.modified.cmp(&b.modified),
            Sort::Type => a
                .extension()
                .cmp(&b.extension())
                .then_with(|| sort_key(&a.rel_text()).cmp(&sort_key(&b.rel_text()))),
        };
        if descending { order.reverse() } else { order }
    });
    let total = collected.len();
    let start = cursor.offset.min(total);
    let slice = &collected[start..(start + page).min(total)];
    let mut text = header;
    text.push('\n');
    if slice.is_empty() {
        text.push_str("(no matching entries)");
    } else {
        text.push_str(&slice.iter().map(describe).collect::<Vec<_>>().join("\n"));
    }
    text.push_str(&format!(
        "\n-- {} shown of {} collected",
        slice.len(),
        total
    ));
    if capped || !end.complete {
        text.push_str(&format!(
            "\nnote: sorted among the first {total} matches only (limit {MAX_SORT_SCAN} or time budget); narrow with path, pattern or extensions for a complete order"
        ));
    }
    if start + slice.len() < total {
        let next = Cursor {
            offset: start + slice.len(),
            ..Cursor::default()
        };
        text.push_str(&format!("\nmore: true\nnext_cursor: {}", next.encode()));
    } else {
        text.push_str("\nmore: false");
    }
    Ok(text)
}

#[allow(clippy::too_many_arguments)]
fn list_summary(
    root_canon: &Path,
    dir: &Path,
    rel: &str,
    recursive: bool,
    args: &Value,
    filter: &Filter,
    include_hidden: bool,
    cursor: Cursor,
    budget: &Budget,
) -> Result<String, String> {
    let max_depth = if recursive {
        arg_usize(args, "max_depth", MAX_DEPTH, 1, MAX_DEPTH)
    } else {
        1
    };
    let (mut files, mut folders, mut bytes) = (cursor.files, cursor.folders, cursor.bytes);
    let mut visit = |entry: &Entry| {
        if filter.matches(entry) {
            if entry.is_dir {
                folders += 1;
            } else {
                files += 1;
                bytes += entry.size;
            }
        }
        Flow::Continue
    };
    let end = walk(
        root_canon,
        dir,
        max_depth,
        include_hidden,
        &cursor.after,
        budget,
        &mut visit,
    );
    let mut text = format!(
        "Summary of {rel}{}: files={files}, folders={folders}, total size={} ({bytes} bytes)",
        if recursive {
            format!(" (recursive, depth {max_depth})")
        } else {
            String::new()
        },
        human_size(bytes)
    );
    if end.skipped > 0 {
        text.push_str(&format!(
            "\n{} entries not counted (links, odd names)",
            end.skipped
        ));
    }
    if end.complete {
        text.push_str("\ncomplete: true");
    } else {
        let next = Cursor {
            after: end.last.unwrap_or(cursor.after),
            files,
            folders,
            bytes,
            offset: 0,
        };
        text.push_str(&format!(
            "\ncomplete: false (time budget reached; counts so far)\nnext_cursor: {}",
            next.encode()
        ));
    }
    Ok(text)
}

/// Whole-folder counts for the settings screen. Same walk, longer budget.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FolderStats {
    pub files: u64,
    pub folders: u64,
    pub bytes: u64,
    /// False when the budget ran out: the numbers are lower bounds.
    pub complete: bool,
}

impl FolderStats {
    /// `1,234 files, 56 folders, 2.3 GiB`; a lower bound is marked with `+`.
    pub fn label(&self) -> String {
        let plus = if self.complete { "" } else { "+" };
        format!(
            "{}{plus} files, {}{plus} folders, {}{plus}",
            group_digits(self.files),
            group_digits(self.folders),
            human_size(self.bytes)
        )
    }
}

fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn folder_stats(root: &Path, budget: &Budget) -> FolderStats {
    let Ok(root_canon) = root.canonicalize() else {
        return FolderStats::default();
    };
    let mut stats = FolderStats::default();
    let mut visit = |entry: &Entry| {
        if entry.is_dir {
            stats.folders += 1;
        } else {
            stats.files += 1;
            stats.bytes += entry.size;
        }
        Flow::Continue
    };
    let end = walk(&root_canon, &root_canon, 64, false, &[], budget, &mut visit);
    stats.complete = end.complete;
    stats
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

/// Text sniff: no NUL and valid UTF-8 (a cut multi-byte tail is fine).
fn looks_like_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(error) => error.error_len().is_none(),
    }
}

fn sniff_file(path: &Path) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = vec![0u8; SNIFF_BYTES];
    let read = file.read(&mut head).ok()?;
    head.truncate(read);
    Some(head)
}

/// `local_search`: find files by name pattern and optionally by text content.
pub fn search(root: &Path, rel: &str, args: &Value, budget: &Budget) -> Result<String, String> {
    let root_canon = root
        .canonicalize()
        .map_err(|_| "the allowed folder is no longer available".to_string())?;
    let dir = if rel == "." {
        root_canon.clone()
    } else {
        resolve(root, rel, true)?
    };
    let meta = std::fs::symlink_metadata(&dir).map_err(|_| "could not be opened".to_string())?;
    if !meta.is_dir() || is_link(&meta) {
        return Err("not a folder".into());
    }
    let name = arg_pattern(args, "name")?;
    let text = arg_pattern(args, "text")?.map(|needle| needle.to_lowercase());
    if name.is_none() && text.is_none() && arg_extensions(args).is_empty() {
        return Err("give a name pattern, an extension list or text to search for".into());
    }
    let extensions = arg_extensions(args);
    let max_depth = arg_usize(args, "max_depth", 8, 1, MAX_DEPTH);
    let max_results = arg_usize(args, "max_results", DEFAULT_RESULTS, 1, MAX_RESULTS);
    let scan_cap = args
        .get("max_file_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_SCAN_FILE_BYTES)
        .clamp(1024, MAX_SCAN_FILE_BYTES);
    let include_hidden = arg_bool(args, "include_hidden");
    let cursor = match arg_str(args, "cursor") {
        Some(text) => Cursor::decode(text)?,
        None => Cursor::default(),
    };

    let mut found: Vec<String> = Vec::new();
    let mut files_seen = 0u64;
    let mut scanned_bytes = 0u64;
    let (mut skipped_binary, mut skipped_large) = (0u64, 0u64);
    let mut budget_hit = false;
    let mut overflow = false;
    let mut last_shown: Option<Vec<String>> = None;
    let (mut previous, mut current): (Option<Vec<String>>, Option<Vec<String>>) = (None, None);
    let mut visit = |entry: &Entry| {
        previous = current.take();
        current = Some(entry.rel.clone());
        if entry.is_dir {
            return Flow::Continue;
        }
        files_seen += 1;
        if let Some(pattern) = &name
            && !glob_match(pattern, entry.name())
        {
            return Flow::Continue;
        }
        if !extensions.is_empty() && !extensions.contains(&entry.extension()) {
            return Flow::Continue;
        }
        let summary = format!("{} ({} bytes)", entry.rel_text(), entry.size);
        let Some(needle) = &text else {
            if found.len() >= max_results {
                overflow = true;
                return Flow::Stop;
            }
            found.push(summary);
            last_shown = Some(entry.rel.clone());
            return Flow::Continue;
        };
        if entry.size > scan_cap {
            skipped_large += 1;
            return Flow::Continue;
        }
        if scanned_bytes + entry.size > MAX_SCAN_TOTAL_BYTES {
            budget_hit = true;
            return Flow::Stop;
        }
        let path = dir.join(entry.rel.join(std::path::MAIN_SEPARATOR_STR));
        // The walk checked this entry a moment ago; check again right before
        // opening so a swapped-in link is not followed.
        if !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file() && !is_link(&meta)) {
            return Flow::Continue;
        }
        let Some(head) = sniff_file(&path) else {
            return Flow::Continue;
        };
        if !looks_like_text(&head) {
            skipped_binary += 1;
            return Flow::Continue;
        }
        scanned_bytes += entry.size;
        let hits = match_lines(&path, needle);
        if hits.is_empty() {
            return Flow::Continue;
        }
        if found.len() >= max_results {
            overflow = true;
            return Flow::Stop;
        }
        found.push(format!("{summary}\n{}", hits.join("\n")));
        last_shown = Some(entry.rel.clone());
        Flow::Continue
    };
    let end = walk(
        &root_canon,
        &dir,
        max_depth,
        include_hidden,
        &cursor.after,
        budget,
        &mut visit,
    );
    let mut out = format!("Search in {rel}: {} match(es)\n", found.len());
    if found.is_empty() {
        out.push_str("(nothing found)\n");
    } else {
        out.push_str(&found.join("\n"));
        out.push('\n');
    }
    out.push_str(&format!(
        "-- {files_seen} files checked, {} content bytes scanned",
        scanned_bytes
    ));
    if skipped_binary + skipped_large > 0 {
        out.push_str(&format!(
            ", skipped {skipped_binary} binary and {skipped_large} over the {} cap",
            human_size(scan_cap)
        ));
    }
    if end.skipped > 0 {
        out.push_str(&format!(", {} not shown (links, odd names)", end.skipped));
    }
    if end.complete && !budget_hit {
        out.push_str("\nmore: false");
    } else {
        let after = if overflow {
            last_shown.unwrap_or_default()
        } else if budget_hit {
            // The entry that tripped the scan limit was not scanned.
            previous.unwrap_or(cursor.after)
        } else {
            end.last.unwrap_or(cursor.after)
        };
        let next = Cursor {
            after,
            ..Cursor::default()
        };
        let reason = if overflow {
            "result limit reached"
        } else if budget_hit {
            "content scan limit reached; partial result"
        } else {
            "time budget reached; partial result"
        };
        out.push_str(&format!(
            "\nmore: true ({reason})\nnext_cursor: {}",
            next.encode()
        ));
    }
    Ok(out)
}

/// Up to three matching lines of a text file, trimmed to a snippet.
fn match_lines(path: &Path, needle_lower: &str) -> Vec<String> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut reader = BufReader::new(file);
    let mut hits = Vec::new();
    let mut buf = Vec::new();
    let mut number = 0usize;
    loop {
        buf.clear();
        match reader
            .by_ref()
            .take(MAX_LINE_BYTES as u64)
            .read_until(b'\n', &mut buf)
        {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        number += 1;
        let line = String::from_utf8_lossy(&buf);
        if line.to_lowercase().contains(needle_lower) {
            let snippet: String = line.trim().chars().take(160).collect();
            hits.push(format!("  L{number}: {snippet}"));
            if hits.len() >= 3 {
                break;
            }
        }
    }
    hits
}

// ---------------------------------------------------------------------------
// stat
// ---------------------------------------------------------------------------

/// `local_stat`: type, size, modified time and whether it reads as text.
pub fn stat(root: &Path, rel: &str) -> Result<String, String> {
    let path = if rel == "." {
        root.canonicalize()
            .map_err(|_| "the allowed folder is no longer available".to_string())?
    } else {
        resolve(root, rel, true)?
    };
    let meta = std::fs::symlink_metadata(&path).map_err(|_| "not found".to_string())?;
    if is_link(&meta) {
        return Err("links and junctions are not followed".into());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if sensitive_name(name) {
        return Err("credential-like files are never shared".into());
    }
    let modified = modified_secs(&meta)
        .map(iso_datetime)
        .unwrap_or_else(|| "unknown".into());
    let mut out = format!("path: {rel}\n");
    if meta.is_dir() {
        out.push_str(&format!("type: folder\nmodified: {modified}\n"));
    } else if meta.is_file() {
        let extension = Path::new(name)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let content = sniff_file(&path)
            .map(|head| {
                if looks_like_text(&head) {
                    "text"
                } else {
                    "binary"
                }
            })
            .unwrap_or("unreadable");
        out.push_str(&format!(
            "type: file\nsize: {} bytes ({})\nmodified: {modified}\nextension: {}\ncontent: {content}\nread_only: {}\n",
            meta.len(),
            human_size(meta.len()),
            if extension.is_empty() { "none" } else { &extension },
            meta.permissions().readonly()
        ));
    } else {
        return Err("not a regular file or folder".into());
    }
    out.push_str(&format!("hidden: {}", is_hidden(name, &meta)));
    Ok(out)
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

/// `local_read`: UTF-8 text by byte range or line range, never more than
/// [`MAX_READ_BYTES`] per call, binary refused.
pub fn read(root: &Path, rel: &str, args: &Value, budget: &Budget) -> Result<String, String> {
    let path = resolve(root, rel, true)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if sensitive_name(name) {
        return Err("credential-like files are never shared".into());
    }
    let meta = std::fs::symlink_metadata(&path).map_err(|_| "could not be opened".to_string())?;
    if !meta.is_file() || is_link(&meta) {
        return Err("not a regular file".into());
    }
    let size = meta.len();
    let head = sniff_file(&path).ok_or_else(|| "could not be read".to_string())?;
    if !looks_like_text(&head) {
        return Err(format!(
            "binary or non-UTF-8 file ({size} bytes), not shown as text. Use local_stat for details; \
             for Word, Excel, PowerPoint, PDF or image files use local_import and the document tools"
        ));
    }
    let limit = arg_usize(args, "length", MAX_READ_BYTES, 1, MAX_READ_BYTES);
    let line_mode = ["start_line", "end_line", "max_lines"]
        .iter()
        .any(|key| args.get(*key).is_some());
    if line_mode {
        return read_lines(&path, size, args, limit, budget);
    }
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
    if offset > size || (offset == size && size > 0) {
        return Err(format!("offset is past the end of the file ({size} bytes)"));
    }
    let mut file = std::fs::File::open(&path).map_err(|_| "could not be opened".to_string())?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| "could not be read".to_string())?;
    // Read a few extra bytes so a cut multi-byte character can be completed.
    let mut buf = Vec::new();
    file.take(limit as u64 + 4)
        .read_to_end(&mut buf)
        .map_err(|_| "could not be read".to_string())?;
    // Start on a character boundary.
    let skip = buf
        .iter()
        .take(3)
        .take_while(|b| (**b & 0xC0) == 0x80)
        .count();
    let skip = if offset == 0 { 0 } else { skip };
    let mut chunk = &buf[skip..];
    if chunk.len() > limit {
        chunk = &chunk[..limit];
    }
    // End on a character boundary.
    let text = match std::str::from_utf8(chunk) {
        Ok(text) => text,
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&chunk[..error.valid_up_to()]).unwrap_or_default()
        }
        Err(_) => return Err("only UTF-8 text files are shared".into()),
    };
    let start = offset + skip as u64;
    let end = start + text.len() as u64;
    if start == 0 && end == size {
        return Ok(text.to_owned());
    }
    let more = if end < size {
        format!("next_offset={end}")
    } else {
        "end of file".to_owned()
    };
    Ok(format!("[bytes {start}-{end} of {size}; {more}]\n{text}"))
}

fn read_lines(
    path: &Path,
    size: u64,
    args: &Value,
    byte_limit: usize,
    budget: &Budget,
) -> Result<String, String> {
    let start = arg_usize(args, "start_line", 1, 1, 100_000_000);
    let max_lines = arg_usize(args, "max_lines", 2000, 1, 5000);
    let end_line = args
        .get("end_line")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(usize::MAX);
    if end_line < start {
        return Err("end_line must not be before start_line".into());
    }
    let last_wanted = end_line.min(start.saturating_add(max_lines - 1));
    let file = std::fs::File::open(path).map_err(|_| "could not be opened".to_string())?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut line_no = 0usize;
    let mut scanned = 0u64;
    let mut out = String::new();
    let mut shown = 0usize;
    let mut next_line = None;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        // Cap each stored line; the rest of an over-long line is skipped.
        let read = reader
            .by_ref()
            .take(MAX_LINE_BYTES as u64)
            .read_until(b'\n', &mut buf)
            .map_err(|_| "could not be read".to_string())?;
        if read == 0 {
            break;
        }
        scanned += read as u64;
        let mut truncated = false;
        if buf.last() != Some(&b'\n') && read == MAX_LINE_BYTES {
            truncated = true;
            let mut sink = Vec::new();
            let skipped = reader
                .read_until(b'\n', &mut sink)
                .map_err(|_| "could not be read".to_string())?;
            scanned += skipped as u64;
        }
        line_no += 1;
        if scanned > MAX_LINE_SCAN_BYTES || budget.expired() {
            next_line = Some(line_no);
            break;
        }
        if line_no < start {
            continue;
        }
        if line_no > last_wanted {
            next_line = Some(line_no);
            break;
        }
        let text = String::from_utf8_lossy(&buf);
        let text = text.trim_end_matches(['\n', '\r']);
        if out.len() + text.len() + 1 > byte_limit && shown > 0 {
            next_line = Some(line_no);
            break;
        }
        out.push_str(text);
        if truncated {
            out.push_str(" …[line truncated]");
        }
        out.push('\n');
        shown += 1;
    }
    if shown == 0 {
        return Ok(format!(
            "[no lines at {start}; the file has {line_no} line(s), {size} bytes]"
        ));
    }
    let last = start + shown - 1;
    let header = match next_line {
        Some(next) => format!("[lines {start}-{last}; next_line={next}]"),
        None => format!("[lines {start}-{last} of {line_no}; end of file]"),
    };
    Ok(format!("{header}\n{out}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(base: &Path, files: usize, per_dir: usize) {
        let mut made = 0;
        let mut d = 0;
        while made < files {
            let dir = base.join(format!("d{:03}", d / 10)).join(format!("s{d}"));
            std::fs::create_dir_all(&dir).unwrap();
            for i in 0..per_dir.min(files - made) {
                std::fs::write(
                    dir.join(format!("f{i:03}.txt")),
                    format!("file {made} needle{i}"),
                )
                .unwrap();
                made += 1;
            }
            d += 1;
        }
    }

    fn temp() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("serabot-fs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        base.canonicalize().unwrap()
    }

    fn cursor_of(text: &str) -> Option<String> {
        text.lines()
            .find_map(|line| line.strip_prefix("next_cursor: "))
            .map(str::to_owned)
    }

    #[test]
    fn glob_cases_dates_and_sizes() {
        assert!(glob_match("*.TXT", "notes.txt"));
        assert!(glob_match("rep?rt*", "Report-2024.pdf"));
        assert!(glob_match("budget", "My-Budget-2024.xlsx"));
        assert!(!glob_match("*.md", "a.txt"));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b*c", "aXXbYY"));
        // A hostile pattern stays fast.
        assert!(!glob_match(
            &"a*".repeat(60),
            &"a".repeat(2000).replace('a', "b")
        ));
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(iso_datetime(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert!(name_addressable("report.pdf"));
        for bad in ["a:b", "con", "nul.txt", "x.", "x ", "a*b", ""] {
            assert!(!name_addressable(bad), "{bad:?}");
        }
    }

    #[test]
    fn windows_paths_compare_without_case_or_prefix() {
        assert_eq!(
            normalize_windows(r"\\?\C:\Users\Me\Docs\"),
            r"c:\users\me\docs"
        );
        assert_eq!(normalize_windows(r"\\?\UNC\srv\share"), r"\\srv\share");
        #[cfg(windows)]
        {
            assert!(path_within(
                Path::new(r"\\?\C:\Docs"),
                Path::new(r"c:\docs\a\b")
            ));
            assert!(!path_within(
                Path::new(r"C:\Docs"),
                Path::new(r"C:\Docs2\a")
            ));
        }
        #[cfg(not(windows))]
        {
            assert!(path_within(Path::new("/a/b"), Path::new("/a/b/c")));
            assert!(!path_within(Path::new("/a/b"), Path::new("/a/bc")));
        }
    }

    #[test]
    fn cursors_round_trip_and_reject_hostile_parts() {
        let cursor = Cursor {
            after: vec!["a".into(), "b c".into()],
            offset: 7,
            files: 3,
            folders: 2,
            bytes: 99,
        };
        assert_eq!(Cursor::decode(&cursor.encode()).unwrap(), cursor);
        assert!(Cursor::decode("not base64 !!").is_err());
        let evil = Cursor {
            after: vec!["..".into()],
            ..Cursor::default()
        };
        assert!(Cursor::decode(&evil.encode()).is_err());
        let slash = Cursor {
            after: vec!["a/b".into()],
            ..Cursor::default()
        };
        assert!(Cursor::decode(&slash.encode()).is_err());
        assert!(Cursor::decode(&"A".repeat(5000)).is_err());
    }

    #[test]
    fn thousands_of_files_paginate_without_gaps_or_repeats() {
        let base = temp();
        tree(&base, 2500, 25);
        let budget = Budget::new(Duration::from_secs(60));
        let mut seen = std::collections::HashSet::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut args = serde_json::json!({"recursive": true, "max_depth": 6, "kind": "file", "page_size": 500});
            if let Some(cursor) = &cursor {
                args["cursor"] = Value::String(cursor.clone());
            }
            let out = list(&base, ".", &args, &budget).unwrap();
            for line in out.lines().filter(|l| l.contains(".txt (")) {
                assert!(seen.insert(line.to_owned()), "repeat: {line}");
            }
            pages += 1;
            cursor = cursor_of(&out);
            if cursor.is_none() {
                assert!(out.contains("more: false"));
                break;
            }
        }
        assert_eq!(seen.len(), 2500);
        assert_eq!(pages, 5);
        // Page size is capped.
        let big = list(
            &base,
            ".",
            &serde_json::json!({"recursive": true, "max_depth": 6, "kind": "file", "page_size": 100000}),
            &budget,
        )
        .unwrap();
        assert_eq!(
            big.lines().filter(|l| l.contains(".txt (")).count(),
            MAX_PAGE
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn summary_counts_without_listing_and_resumes_after_the_budget() {
        let base = temp();
        tree(&base, 600, 20);
        let full = list(
            &base,
            ".",
            &serde_json::json!({"summary": true, "recursive": true}),
            &Budget::new(Duration::from_secs(60)),
        )
        .unwrap();
        assert!(
            full.contains("files=600") && full.contains("complete: true"),
            "{full}"
        );
        assert!(!full.contains("f000.txt"));
        // An already expired budget yields a partial answer with a cursor, and
        // continuing from it never loses ground.
        let expired = Budget::new(Duration::ZERO);
        let partial = list(
            &base,
            ".",
            &serde_json::json!({"summary": true, "recursive": true}),
            &expired,
        )
        .unwrap();
        assert!(
            partial.contains("complete: false") && cursor_of(&partial).is_some(),
            "{partial}"
        );
        // Finish in slices of at most 40 entries via a counting budget trick:
        // resume with a fresh budget from the carried cursor.
        let resumed = list(
            &base,
            ".",
            &serde_json::json!({"summary": true, "recursive": true, "cursor": cursor_of(&partial).unwrap()}),
            &Budget::new(Duration::from_secs(60)),
        )
        .unwrap();
        assert!(resumed.contains("files=600"), "{resumed}");
        let stats = folder_stats(&base, &Budget::new(Duration::from_secs(60)));
        assert_eq!((stats.files, stats.complete), (600, true));
        assert!(
            stats.label().starts_with("600 files, "),
            "{}",
            stats.label()
        );
        assert_eq!(group_digits(1_234_567), "1,234,567");
        let partial = folder_stats(&base, &Budget::new(Duration::ZERO));
        assert!(
            !partial.complete && partial.label().contains("+ files"),
            "{}",
            partial.label()
        );
        assert!(stats.bytes > 0 && stats.folders > 0);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn filters_sorting_depth_and_hidden_rules() {
        let base = temp();
        std::fs::create_dir_all(base.join("a/b/c/d")).unwrap();
        std::fs::write(base.join("one.md"), "1").unwrap();
        std::fs::write(base.join("a/two.txt"), "22").unwrap();
        std::fs::write(base.join("a/b/three.txt"), "333").unwrap();
        std::fs::write(base.join("a/b/c/d/deep.txt"), "4444").unwrap();
        std::fs::write(base.join(".hidden.txt"), "h").unwrap();
        std::fs::write(base.join(".env"), "SECRET=x").unwrap();
        let budget = Budget::new(Duration::from_secs(30));
        let out = list(
            &base,
            ".",
            &serde_json::json!({"recursive": true, "max_depth": 3, "extensions": ["txt"]}),
            &budget,
        )
        .unwrap();
        assert!(
            out.contains("a/two.txt") && out.contains("a/b/three.txt"),
            "{out}"
        );
        assert!(
            !out.contains("deep.txt") && !out.contains("one.md"),
            "{out}"
        );
        assert!(!out.contains(".hidden") && !out.contains(".env"), "{out}");
        let all = list(
            &base,
            ".",
            &serde_json::json!({"recursive": true, "max_depth": 6, "include_hidden": true}),
            &budget,
        )
        .unwrap();
        assert!(
            all.contains(".hidden.txt") && all.contains("deep.txt"),
            "{all}"
        );
        assert!(
            !all.contains(".env"),
            "credential-like names stay hidden: {all}"
        );
        let by_size = list(&base, ".", &serde_json::json!({"recursive": true, "max_depth": 6, "kind": "file", "sort": "size", "order": "desc"}), &budget).unwrap();
        let first = by_size.lines().nth(1).unwrap();
        assert!(first.contains("deep.txt"), "{by_size}");
        let by_pattern = list(
            &base,
            ".",
            &serde_json::json!({"recursive": true, "pattern": "T*E?.txt"}),
            &budget,
        )
        .unwrap();
        assert!(by_pattern.contains("three.txt"), "{by_pattern}");
        // Sorted pagination walks the whole set.
        let page1 = list(&base, ".", &serde_json::json!({"recursive": true, "max_depth": 6, "kind": "file", "sort": "size", "page_size": 2}), &budget).unwrap();
        let page2 = list(&base, ".", &serde_json::json!({"recursive": true, "max_depth": 6, "kind": "file", "sort": "size", "page_size": 2, "cursor": cursor_of(&page1).unwrap()}), &budget).unwrap();
        assert!(
            page2.contains("more: false") && page2.contains("deep.txt"),
            "{page2}"
        );
        assert!(list(&base, ".", &serde_json::json!({"sort": "bogus"}), &budget).is_err());
        assert!(list(&base, ".", &serde_json::json!({"cursor": "@@"}), &budget).is_err());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn search_by_name_and_text_with_caps_and_binary_skip() {
        let base = temp();
        tree(&base, 300, 30);
        std::fs::write(base.join("budget-2024.csv"), "a,b\nrow,UniqueMarker here\n").unwrap();
        std::fs::write(base.join("blob.bin"), [0u8, 159, 146, 150, 0]).unwrap();
        std::fs::write(base.join("big.txt"), "x".repeat(5 * 1024 * 1024)).unwrap();
        let budget = Budget::new(Duration::from_secs(60));
        let by_name = search(&base, ".", &serde_json::json!({"name": "budget"}), &budget).unwrap();
        assert!(
            by_name.contains("budget-2024.csv") && by_name.contains("more: false"),
            "{by_name}"
        );
        let by_text = search(
            &base,
            ".",
            &serde_json::json!({"text": "uniquemarker"}),
            &budget,
        )
        .unwrap();
        assert!(by_text.contains("L2: row,UniqueMarker here"), "{by_text}");
        assert!(by_text.contains("skipped 1 binary and 1 over"), "{by_text}");
        // Result cap and resume.
        let first = search(
            &base,
            ".",
            &serde_json::json!({"name": "f0", "max_results": 40}),
            &budget,
        )
        .unwrap();
        assert!(
            first.contains("40 match") && first.contains("result limit reached"),
            "{first}"
        );
        let second = search(&base, ".", &serde_json::json!({"name": "f0", "max_results": 200, "cursor": cursor_of(&first).unwrap()}), &budget).unwrap();
        assert!(
            second.contains("260 match") || second.contains("match(es)"),
            "{second}"
        );
        assert!(search(&base, ".", &serde_json::json!({}), &budget).is_err());
        let timed = search(
            &base,
            ".",
            &serde_json::json!({"name": "f"}),
            &Budget::new(Duration::ZERO),
        )
        .unwrap();
        assert!(timed.contains("time budget"), "{timed}");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn stat_reports_type_size_time_and_refuses_secrets() {
        let base = temp();
        std::fs::write(base.join("a.txt"), "hello").unwrap();
        std::fs::write(base.join("b.dat"), [0u8, 1, 2]).unwrap();
        std::fs::write(base.join(".env"), "S=1").unwrap();
        std::fs::create_dir(base.join("sub")).unwrap();
        let a = stat(&base, "a.txt").unwrap();
        assert!(
            a.contains("type: file") && a.contains("size: 5 bytes") && a.contains("content: text"),
            "{a}"
        );
        assert!(a.contains("modified: 20"), "{a}");
        assert!(stat(&base, "b.dat").unwrap().contains("content: binary"));
        assert!(stat(&base, "sub").unwrap().contains("type: folder"));
        assert!(stat(&base, ".").unwrap().contains("type: folder"));
        assert!(stat(&base, ".env").is_err());
        assert!(stat(&base, "../x").is_err());
        assert!(stat(&base, "missing").is_err());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn reads_by_byte_and_line_range_with_a_hard_cap() {
        let base = temp();
        let budget = Budget::new(Duration::from_secs(30));
        let body: String = (1..=1000).map(|n| format!("line {n}\n")).collect();
        std::fs::write(base.join("l.txt"), &body).unwrap();
        std::fs::write(base.join("small.txt"), "tiny").unwrap();
        std::fs::write(base.join("bin.dat"), [0u8, 1]).unwrap();
        let multibyte = "é".repeat(300_000);
        std::fs::write(base.join("m.txt"), &multibyte).unwrap();
        assert_eq!(
            read(&base, "small.txt", &serde_json::json!({}), &budget).unwrap(),
            "tiny"
        );
        let ranged = read(
            &base,
            "l.txt",
            &serde_json::json!({"offset": 5, "length": 12}),
            &budget,
        )
        .unwrap();
        assert!(
            ranged.starts_with("[bytes 5-17 of ") && ranged.contains("next_offset=17"),
            "{ranged}"
        );
        let lines = read(
            &base,
            "l.txt",
            &serde_json::json!({"start_line": 10, "end_line": 12}),
            &budget,
        )
        .unwrap();
        assert_eq!(
            lines,
            "[lines 10-12; next_line=13]\nline 10\nline 11\nline 12\n"
        );
        let tail = read(
            &base,
            "l.txt",
            &serde_json::json!({"start_line": 999}),
            &budget,
        )
        .unwrap();
        assert!(
            tail.contains("line 999") && tail.contains("end of file"),
            "{tail}"
        );
        let past = read(
            &base,
            "l.txt",
            &serde_json::json!({"start_line": 5000}),
            &budget,
        )
        .unwrap();
        assert!(past.contains("the file has 1000 line"), "{past}");
        assert!(
            read(
                &base,
                "l.txt",
                &serde_json::json!({"start_line": 5, "end_line": 2}),
                &budget
            )
            .is_err()
        );
        assert!(
            read(&base, "bin.dat", &serde_json::json!({}), &budget)
                .unwrap_err()
                .contains("binary")
        );
        // Hard cap per call, on a character boundary at both ends, and the
        // whole file is reachable by following next_offset.
        let mut offset = 0u64;
        let mut collected = String::new();
        loop {
            let out = read(
                &base,
                "m.txt",
                &serde_json::json!({"offset": offset, "length": 100_000_000}),
                &budget,
            )
            .unwrap();
            let (head, text) = out.split_once('\n').unwrap();
            assert!(text.len() <= MAX_READ_BYTES, "cap exceeded");
            collected.push_str(text);
            match head.split("next_offset=").nth(1) {
                Some(next) => offset = next.trim_end_matches(']').parse().unwrap(),
                None => break,
            }
        }
        assert_eq!(collected, multibyte);
        assert!(
            read(
                &base,
                "l.txt",
                &serde_json::json!({"offset": 999_999_999}),
                &budget
            )
            .is_err()
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_pointing_out_are_never_walked_or_read() {
        let base = temp();
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "OUTSIDE").unwrap();
        std::fs::write(root.join("ok.txt"), "inside").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link.txt")).unwrap();
        let budget = Budget::new(Duration::from_secs(30));
        let out = list(&root, ".", &serde_json::json!({"recursive": true}), &budget).unwrap();
        assert!(
            out.contains("ok.txt")
                && !out.contains("secret")
                && !out.contains("escape")
                && !out.contains("link.txt"),
            "{out}"
        );
        let found = search(&root, ".", &serde_json::json!({"text": "outside"}), &budget).unwrap();
        assert!(
            !found.contains("secret") && found.contains("nothing found"),
            "{found}"
        );
        assert!(read(&root, "link.txt", &serde_json::json!({}), &budget).is_err());
        assert!(read(&root, "escape/secret.txt", &serde_json::json!({}), &budget).is_err());
        assert!(stat(&root, "escape").is_err());
        let stats = folder_stats(&root, &budget);
        assert_eq!(stats.files, 1);
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(windows)]
    #[test]
    fn junctions_ads_and_reserved_names_are_not_walked_or_shown() {
        let base = temp();
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "OUTSIDE").unwrap();
        std::fs::write(root.join("ok.txt"), "inside").unwrap();
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.join("jn"))
            .arg(&outside)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        let budget = Budget::new(Duration::from_secs(30));
        if made {
            let out = list(&root, ".", &serde_json::json!({"recursive": true}), &budget).unwrap();
            assert!(!out.contains("secret") && !out.contains("jn"), "{out}");
            let found =
                search(&root, ".", &serde_json::json!({"text": "outside"}), &budget).unwrap();
            assert!(found.contains("nothing found"), "{found}");
            assert_eq!(folder_stats(&root, &budget).files, 1);
            let _ = std::process::Command::new("cmd")
                .args(["/C", "rmdir"])
                .arg(root.join("jn"))
                .output();
        }
        // Alternate data streams, reserved names and a different-case path.
        assert!(read(&root, "ok.txt:stream", &serde_json::json!({}), &budget).is_err());
        assert!(read(&root, "NUL", &serde_json::json!({}), &budget).is_err());
        assert!(read(&root, "OK.TXT", &serde_json::json!({}), &budget).is_ok());
        assert!(list(&root, "..", &serde_json::json!({}), &budget).is_err());
        // Hidden attribute hides a file without a dot name.
        std::fs::write(root.join("quiet.txt"), "q").unwrap();
        let _ = std::process::Command::new("attrib")
            .arg("+H")
            .arg(root.join("quiet.txt"))
            .output();
        let out = list(&root, ".", &serde_json::json!({}), &budget).unwrap();
        assert!(!out.contains("quiet.txt"), "{out}");
        let shown = list(
            &root,
            ".",
            &serde_json::json!({"include_hidden": true}),
            &budget,
        )
        .unwrap();
        assert!(shown.contains("quiet.txt"), "{shown}");
        std::fs::remove_dir_all(&base).ok();
    }
}
