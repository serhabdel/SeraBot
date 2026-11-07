//! Server health for the owner's Usage tab (0.21).
//!
//! Two parts, both read-only and both free of content:
//!
//! * a live host/process snapshot (memory, CPU, disk) from `sysinfo`;
//! * a storage breakdown of the SeraBot data directory by category, plus the
//!   bytes each member's own folders take.
//!
//! The directory walk is bounded: it stops after a time budget or an entry
//! budget (and reports `truncated`), never follows symlinks or Windows
//! junctions, never leaves the data directory, and the result is cached for
//! about a minute so the 30 second refresh of the desktop card stays cheap.
//! Nothing here reads file contents or credentials; only names, sizes and
//! counts are looked at.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sysinfo::{Disks, MINIMUM_CPU_UPDATE_INTERVAL, Pid, ProcessesToUpdate, System};

use crate::error::Result;
use crate::store::Store;

/// How long a storage scan is reused.
pub const SCAN_CACHE: Duration = Duration::from_secs(60);
/// Longest a single directory walk may run.
pub const SCAN_BUDGET: Duration = Duration::from_secs(8);
/// Most directory entries one walk visits.
pub const SCAN_MAX_ENTRIES: u64 = 2_000_000;
/// Deepest directory nesting the walk follows.
const MAX_DEPTH: usize = 64;
/// Audit the admin's status views at most this often (the card polls).
pub const AUDIT_EVERY: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Database,
    Documents,
    Artifacts,
    Media,
    Memory,
    Browser,
    Logs,
    Backups,
    Other,
}

impl Category {
    pub const ALL: [Category; 9] = [
        Category::Database,
        Category::Documents,
        Category::Artifacts,
        Category::Media,
        Category::Memory,
        Category::Browser,
        Category::Logs,
        Category::Backups,
        Category::Other,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Category::Database => "database",
            Category::Documents => "documents",
            Category::Artifacts => "artifacts",
            Category::Media => "media",
            Category::Memory => "memory",
            Category::Browser => "browser",
            Category::Logs => "logs",
            Category::Backups => "backups",
            Category::Other => "other",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::Database => "Database",
            Category::Documents => "Documents and uploads",
            Category::Artifacts => "Files the bots made",
            Category::Media => "Audio, images and video",
            Category::Memory => "Memory and search",
            Category::Browser => "Browser profiles",
            Category::Logs => "Logs",
            Category::Backups => "Backups",
            Category::Other => "Other",
        }
    }
}

/// Maps a file to a category from its top-level folder, name and extension.
#[derive(Clone, Debug)]
pub struct Rules {
    db_name: String,
    inbox: Option<String>,
    artifacts: Option<String>,
}

impl Rules {
    /// `inbox` and `artifacts` are the configured folders; they only count
    /// when they sit directly inside the data directory.
    pub fn new(data_dir: &Path, db_path: &Path, inbox: &Path, artifacts: &Path) -> Self {
        let top = |path: &Path| {
            path.strip_prefix(data_dir)
                .ok()
                .and_then(|rel| rel.components().next())
                .map(|part| part.as_os_str().to_string_lossy().to_ascii_lowercase())
        };
        Self {
            db_name: db_path
                .file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default(),
            inbox: top(inbox),
            artifacts: top(artifacts),
        }
    }

    /// `top` is the first folder under the data directory (`None` for a file
    /// directly in it).
    pub fn classify(&self, top: Option<&str>, file_name: &str) -> Category {
        let name = file_name.to_ascii_lowercase();
        if let Some(top) = top {
            let top = top.to_ascii_lowercase();
            if self.artifacts.as_deref() == Some(top.as_str()) || top == "artifacts" {
                return Category::Artifacts;
            }
            if self.inbox.as_deref() == Some(top.as_str())
                || matches!(
                    top.as_str(),
                    "inbox" | "documents" | "uploads" | "attachments"
                )
            {
                return Category::Documents;
            }
            match top.as_str() {
                "browser" | "browser-profiles" | "browser_profiles" => return Category::Browser,
                "logs" | "log" => return Category::Logs,
                "backups" | "backup" => return Category::Backups,
                "memory" | "models" | "embeddings" => return Category::Memory,
                _ => {}
            }
        }
        let extension = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
        if !self.db_name.is_empty() && name.starts_with(&self.db_name) {
            return Category::Database;
        }
        match extension {
            "db" | "sqlite" | "sqlite3" | "wal" | "shm" => Category::Database,
            "log" => Category::Logs,
            "bak" | "backup" | "tar" | "gz" | "tgz" | "zip" | "7z" => Category::Backups,
            "mp3" | "wav" | "ogg" | "m4a" | "flac" | "opus" | "png" | "jpg" | "jpeg" | "gif"
            | "webp" | "mp4" | "mov" | "webm" | "mkv" => Category::Media,
            "pptx" | "pdf" | "xlsx" | "docx" => Category::Artifacts,
            _ => Category::Other,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct StorageScan {
    pub categories: BTreeMap<Category, (u64, u64)>,
    /// Bytes under each member's own folders (`<inbox>/<user id>`,
    /// `browser/<user id>`), keyed by user id.
    pub user_bytes: HashMap<String, u64>,
    pub total_bytes: u64,
    pub total_files: u64,
    /// True when a budget stopped the walk early; the numbers are a floor.
    pub truncated: bool,
    pub scanned_at: Option<DateTime<Utc>>,
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// Walk `root` and total file sizes by category. Symlinks and junctions are
/// skipped, so the walk cannot leave `root`.
pub fn scan_data_dir(
    root: &Path,
    rules: &Rules,
    user_ids: &HashSet<String>,
    budget: Duration,
    max_entries: u64,
) -> StorageScan {
    let started = Instant::now();
    let mut scan = StorageScan {
        scanned_at: Some(Utc::now()),
        ..StorageScan::default()
    };
    let mut visited: u64 = 0;
    // (directory, depth, top folder, second folder)
    let mut stack: Vec<(PathBuf, usize, Option<String>, Option<String>)> =
        vec![(root.to_path_buf(), 0, None, None)];
    while let Some((dir, depth, top, second)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > max_entries || started.elapsed() > budget {
                scan.truncated = true;
                return scan;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if is_reparse_point(&metadata) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if file_type.is_dir() {
                if depth + 1 > MAX_DEPTH {
                    continue;
                }
                let (next_top, next_second) = match (&top, &second) {
                    (None, _) => (Some(name), None),
                    (Some(_), None) => (top.clone(), Some(name)),
                    _ => (top.clone(), second.clone()),
                };
                stack.push((entry.path(), depth + 1, next_top, next_second));
            } else if file_type.is_file() {
                let size = metadata.len();
                let category = rules.classify(top.as_deref(), &name);
                let slot = scan.categories.entry(category).or_insert((0, 0));
                slot.0 += size;
                slot.1 += 1;
                scan.total_bytes += size;
                scan.total_files += 1;
                if let Some(owner) = second.as_deref()
                    && matches!(category, Category::Documents | Category::Browser)
                    && user_ids.contains(owner)
                {
                    *scan.user_bytes.entry(owner.to_owned()).or_insert(0) += size;
                }
            }
        }
    }
    scan
}

#[derive(Clone, Debug, Serialize)]
pub struct DiskInfo {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct HostSnapshot {
    pub process_rss_bytes: u64,
    /// Share of the whole machine, 0..=100.
    pub process_cpu_percent: f32,
    pub host_memory_total_bytes: u64,
    pub host_memory_used_bytes: u64,
    pub host_cpu_percent: f32,
    pub cpu_cores: usize,
    /// The volume holding the data directory, when it can be found.
    pub disk: Option<DiskInfo>,
}

/// Strip the Windows `\\?\` prefix so mount points compare cleanly.
fn plain_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path.to_path_buf(),
    }
}

/// Free/used space of the disk whose mount point is the longest prefix of
/// `path`.
pub fn disk_for(path: &Path) -> Option<DiskInfo> {
    let target = plain_path(&path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
    let disks = Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|disk| target.starts_with(plain_path(disk.mount_point())))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| {
            let total = disk.total_space();
            let free = disk.available_space().min(total);
            DiskInfo {
                total_bytes: total,
                used_bytes: total - free,
                free_bytes: free,
            }
        })
}

struct Sampler {
    system: System,
    last: Instant,
}

/// Holds the long-lived sampler (CPU needs two readings) and the cached scan.
pub struct StatusMonitor {
    started: Instant,
    sampler: Mutex<Sampler>,
    scan: Mutex<Option<(Instant, StorageScan)>>,
    audited: Mutex<Option<Instant>>,
}

impl Default for StatusMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusMonitor {
    pub fn new() -> Self {
        let mut system = System::new();
        system.refresh_memory();
        system.refresh_cpu_usage();
        if let Ok(pid) = sysinfo::get_current_pid() {
            system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        }
        Self {
            started: Instant::now(),
            sampler: Mutex::new(Sampler {
                system,
                last: Instant::now(),
            }),
            scan: Mutex::new(None),
            audited: Mutex::new(None),
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// True when this call should write an audit row (rate-limited).
    pub fn should_audit(&self) -> bool {
        let mut last = self.audited.lock().unwrap_or_else(|e| e.into_inner());
        match *last {
            Some(at) if at.elapsed() < AUDIT_EVERY => false,
            _ => {
                *last = Some(Instant::now());
                true
            }
        }
    }

    /// Blocking: may sleep up to 200 ms so the CPU reading has a window.
    pub fn host_snapshot(&self, data_dir: &Path) -> HostSnapshot {
        let mut sampler = self.sampler.lock().unwrap_or_else(|e| e.into_inner());
        let wait = MINIMUM_CPU_UPDATE_INTERVAL.saturating_sub(sampler.last.elapsed());
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
        sampler.system.refresh_memory();
        sampler.system.refresh_cpu_usage();
        let pid = sysinfo::get_current_pid().ok().unwrap_or(Pid::from_u32(0));
        sampler
            .system
            .refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        sampler.last = Instant::now();
        let cores = sampler.system.cpus().len().max(1);
        let (rss, cpu) = sampler
            .system
            .process(pid)
            .map(|process| (process.memory(), process.cpu_usage() / cores as f32))
            .unwrap_or((0, 0.0));
        HostSnapshot {
            process_rss_bytes: rss,
            process_cpu_percent: cpu.clamp(0.0, 100.0),
            host_memory_total_bytes: sampler.system.total_memory(),
            host_memory_used_bytes: sampler.system.used_memory(),
            host_cpu_percent: sampler.system.global_cpu_usage().clamp(0.0, 100.0),
            cpu_cores: cores,
            disk: disk_for(data_dir),
        }
    }

    /// Blocking: the cached scan, or a fresh one when it is older than
    /// [`SCAN_CACHE`].
    pub fn storage(&self, root: &Path, rules: &Rules, user_ids: &HashSet<String>) -> StorageScan {
        let mut cache = self.scan.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, scan)) = cache.as_ref()
            && at.elapsed() < SCAN_CACHE
        {
            return scan.clone();
        }
        let scan = scan_data_dir(root, rules, user_ids, SCAN_BUDGET, SCAN_MAX_ENTRIES);
        *cache = Some((Instant::now(), scan.clone()));
        scan
    }
}

/// One member's totals: counts and bytes only, never content.
#[derive(Clone, Debug, Serialize)]
pub struct UserTotals {
    pub id: String,
    pub display_name: String,
    pub email: String,
    pub role: String,
    pub status: String,
    pub conversations: i64,
    pub messages: i64,
    pub documents: i64,
    pub memories: i64,
    pub artifacts: i64,
    /// Bytes of this member's own folders on disk.
    pub file_bytes: u64,
}

/// Counts per member from the database; `file_bytes` is filled by the caller.
pub fn user_totals(store: &Store) -> Result<Vec<UserTotals>> {
    let connection = store.connection()?;
    let mut statement = connection.prepare(
        "SELECT u.id, u.email, u.display_name, u.role, u.status,
            (SELECT COUNT(*) FROM conversations c WHERE c.user_id = u.id),
            (SELECT COUNT(*) FROM messages m WHERE m.user_id = u.id),
            (SELECT COUNT(*) FROM documents d WHERE d.user_id = u.id),
            (SELECT COUNT(*) FROM memories x WHERE x.user_id = u.id),
            (SELECT COUNT(*) FROM conversation_artifacts a WHERE a.user_id = u.id)
         FROM users u ORDER BY u.created_at, u.id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(UserTotals {
            id: row.get(0)?,
            email: row.get(1)?,
            display_name: row.get(2)?,
            role: row.get(3)?,
            status: row.get(4)?,
            conversations: row.get(5)?,
            messages: row.get(6)?,
            documents: row.get(7)?,
            memories: row.get(8)?,
            artifacts: row.get(9)?,
            file_bytes: 0,
        })
    })?;
    let mut users = Vec::new();
    for row in rows {
        users.push(row?);
    }
    Ok(users)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    fn rules(root: &Path) -> Rules {
        Rules::new(
            root,
            &root.join("serabot.db"),
            &root.join("inbox"),
            &root.join("artifacts"),
        )
    }

    #[test]
    fn files_are_sorted_into_categories() {
        let r = Rules::new(
            Path::new("/d"),
            Path::new("/d/serabot.db"),
            Path::new("/d/inbox"),
            Path::new("/d/artifacts"),
        );
        assert_eq!(r.classify(None, "serabot.db"), Category::Database);
        assert_eq!(r.classify(None, "serabot.db-wal"), Category::Database);
        assert_eq!(r.classify(None, "serabot.db-shm"), Category::Database);
        assert_eq!(r.classify(Some("inbox"), "a.pdf"), Category::Documents);
        assert_eq!(
            r.classify(Some("artifacts"), "deck.pptx"),
            Category::Artifacts
        );
        assert_eq!(r.classify(None, "report.xlsx"), Category::Artifacts);
        assert_eq!(r.classify(None, "voice.OGG"), Category::Media);
        assert_eq!(r.classify(Some("browser"), "Cookies.db"), Category::Browser);
        assert_eq!(r.classify(Some("memory"), "x"), Category::Memory);
        assert_eq!(r.classify(Some("models"), "m.onnx"), Category::Memory);
        assert_eq!(r.classify(Some("logs"), "a"), Category::Logs);
        assert_eq!(r.classify(None, "serabot.log"), Category::Logs);
        assert_eq!(r.classify(Some("backups"), "a"), Category::Backups);
        assert_eq!(r.classify(None, "old.tar.gz"), Category::Backups);
        assert_eq!(r.classify(None, "notes.txt"), Category::Other);
    }

    #[test]
    fn configured_folders_are_recognised_by_name() {
        let root = Path::new("/d");
        let r = Rules::new(
            root,
            &root.join("x.db"),
            &root.join("incoming"),
            &root.join("made"),
        );
        assert_eq!(r.classify(Some("incoming"), "a"), Category::Documents);
        assert_eq!(r.classify(Some("made"), "a"), Category::Artifacts);
        // A configured folder outside the data dir changes nothing.
        let outside = Rules::new(root, &root.join("x.db"), Path::new("/else/inbox"), root);
        assert_eq!(outside.classify(Some("else"), "a.bin"), Category::Other);
    }

    #[test]
    fn scan_totals_by_category_and_member() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("serabot.db"), 100);
        write(&root.join("serabot.db-wal"), 20);
        write(&root.join("inbox/u1/chat/a.pdf"), 1000);
        write(&root.join("inbox/u2/b.txt"), 10);
        write(&root.join("inbox/stranger/c.txt"), 7);
        write(&root.join("artifacts/h1/deck.pptx"), 300);
        write(&root.join("browser/u1/Default/Cookies"), 50);
        write(&root.join("logs/a.log"), 5);
        write(&root.join("voice.mp3"), 40);
        let users: HashSet<String> = ["u1".to_owned(), "u2".to_owned()].into();
        let scan = scan_data_dir(root, &rules(root), &users, Duration::from_secs(5), 10_000);
        assert!(!scan.truncated);
        assert_eq!(scan.categories[&Category::Database], (120, 2));
        assert_eq!(scan.categories[&Category::Documents], (1017, 3));
        assert_eq!(scan.categories[&Category::Artifacts], (300, 1));
        assert_eq!(scan.categories[&Category::Browser], (50, 1));
        assert_eq!(scan.categories[&Category::Logs], (5, 1));
        assert_eq!(scan.categories[&Category::Media], (40, 1));
        assert_eq!(scan.total_bytes, 120 + 1017 + 300 + 50 + 5 + 40);
        assert_eq!(scan.user_bytes["u1"], 1050);
        assert_eq!(scan.user_bytes["u2"], 10);
        assert!(!scan.user_bytes.contains_key("stranger"));
    }

    #[test]
    fn scan_stops_at_the_entry_budget_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..20 {
            write(&dir.path().join(format!("f{index}.txt")), 1);
        }
        let scan = scan_data_dir(
            dir.path(),
            &rules(dir.path()),
            &HashSet::new(),
            Duration::from_secs(5),
            5,
        );
        assert!(scan.truncated);
        assert!(scan.total_files <= 5);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("big.bin"), 5000);
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("inbox/ok.txt"), 3);
        std::os::unix::fs::symlink(outside.path(), dir.path().join("inbox/link")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("big.bin"),
            dir.path().join("big-link.bin"),
        )
        .unwrap();
        let scan = scan_data_dir(
            dir.path(),
            &rules(dir.path()),
            &HashSet::new(),
            Duration::from_secs(5),
            10_000,
        );
        assert_eq!(scan.total_bytes, 3);
    }

    #[test]
    fn missing_root_gives_an_empty_scan() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("nope");
        let scan = scan_data_dir(
            &gone,
            &rules(&gone),
            &HashSet::new(),
            Duration::from_secs(1),
            100,
        );
        assert_eq!(scan.total_bytes, 0);
        assert!(!scan.truncated);
    }

    #[test]
    fn scan_is_cached_between_calls() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.txt"), 10);
        let monitor = StatusMonitor::new();
        let r = rules(dir.path());
        let ids = HashSet::new();
        let first = monitor.storage(dir.path(), &r, &ids);
        write(&dir.path().join("b.txt"), 90);
        let second = monitor.storage(dir.path(), &r, &ids);
        assert_eq!(first.total_bytes, 10);
        assert_eq!(second.total_bytes, 10, "served from the cache");
    }

    #[test]
    fn audit_is_rate_limited() {
        let monitor = StatusMonitor::new();
        assert!(monitor.should_audit());
        assert!(!monitor.should_audit());
    }

    #[test]
    fn host_snapshot_reports_sane_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = StatusMonitor::new().host_snapshot(dir.path());
        assert!(snapshot.host_memory_total_bytes > 0);
        assert!(snapshot.host_memory_used_bytes <= snapshot.host_memory_total_bytes);
        assert!(snapshot.cpu_cores >= 1);
        assert!(snapshot.process_rss_bytes > 0);
        assert!((0.0..=100.0).contains(&snapshot.process_cpu_percent));
    }
}
