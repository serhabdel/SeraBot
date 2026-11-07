//! "This PC" numbers for the Usage page (0.21): what this app uses on the
//! computer it runs on.
//!
//! * the app's own memory and CPU,
//! * the app's local data (settings, update downloads, logs),
//! * the allowed local folders: how many, and how much they hold,
//! * free space on the disk the app keeps its data on.
//!
//! Everything runs on a background thread, never the UI thread. Walking a
//! huge folder can be slow, so the page first shows the quick numbers and
//! "Counting..." for folders; the walk has a time and entry budget, skips
//! symlinks and junctions, and its result is cached for two minutes.
//! Only sizes are read. File names and contents are never looked at.

use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use sysinfo::{Disks, MINIMUM_CPU_UPDATE_INTERVAL, Pid, ProcessesToUpdate, System};

/// How long the folder total is reused.
pub const FOLDER_CACHE: Duration = Duration::from_secs(120);
/// Longest the folder walk may run, all folders together.
pub const WALK_BUDGET: Duration = Duration::from_secs(10);
/// Most entries the folder walk visits, all folders together.
pub const WALK_MAX_ENTRIES: u64 = 1_000_000;
const MAX_DEPTH: usize = 64;

/// The app's own folders on this PC.
#[derive(Clone, Debug, Default)]
pub struct PcDirs {
    /// `%APPDATA%\SeraBot`: settings and the allowed-folder list. Kept.
    pub settings: Option<PathBuf>,
    /// `%LOCALAPPDATA%\SeraBot\updates`: downloaded installers. Regenerable.
    pub updates: Option<PathBuf>,
}

impl PcDirs {
    /// The folders this app really uses.
    pub fn current() -> Self {
        Self {
            settings: crate::creds::settings_dir(),
            updates: crate::updater::downloads_dir(),
        }
    }
}

/// Bytes the app keeps on this PC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalData {
    /// Settings and the folder list. Not cache: never cleared from here.
    pub settings_bytes: u64,
    /// Downloaded update installers. Safe to clear: they download again.
    pub updates_bytes: u64,
    /// The updater's log file.
    pub log_bytes: u64,
}

impl LocalData {
    pub fn total(&self) -> u64 {
        self.settings_bytes + self.updates_bytes + self.log_bytes
    }

    /// What "Clear cache" would free.
    pub fn clearable(&self) -> u64 {
        self.updates_bytes
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quick {
    pub app_memory_bytes: u64,
    /// Share of the whole computer, 0..=100.
    pub app_cpu_percent: f32,
    pub data: LocalData,
    /// (total, free) of the disk holding the app's data.
    pub disk: Option<(u64, u64)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoldersSize {
    /// The walk is running and nothing earlier is known.
    Counting,
    Done {
        count: usize,
        bytes: u64,
        /// The walk hit its budget: the size is "at least".
        truncated: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PcView {
    pub quick: Option<Quick>,
    pub folders: Option<FoldersSize>,
}

struct Sampler {
    system: System,
    last: Instant,
}

#[derive(Default)]
struct Shared {
    view: Mutex<PcView>,
    folders_at: Mutex<Option<Instant>>,
    sampler: Mutex<Option<Sampler>>,
    running: AtomicBool,
    /// A refresh was asked for while one was running: go round once more.
    again: AtomicBool,
}

/// Handle to the background sampler. Cheap to clone.
#[derive(Clone, Default)]
pub struct PcMonitor {
    shared: Arc<Shared>,
}

impl PcMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    /// What is known right now. Never blocks on a walk.
    pub fn view(&self) -> PcView {
        *self.shared.view.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start a refresh on a background thread. While one runs, the request
    /// is remembered and the running one goes round once more.
    pub fn refresh(&self, dirs: PcDirs, folders: Vec<PathBuf>) {
        if self.shared.running.swap(true, Ordering::AcqRel) {
            self.shared.again.store(true, Ordering::Release);
            return;
        }
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("serabot-pc-stats".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || {
                    loop {
                        run_refresh(&shared, &dirs, &folders);
                        if !shared.again.swap(false, Ordering::AcqRel) {
                            break;
                        }
                    }
                    shared.running.store(false, Ordering::Release);
                }
            });
        if spawned.is_err() {
            shared.running.store(false, Ordering::Release);
        }
    }

    /// Forget the cached folder total so the next refresh recounts.
    pub fn invalidate(&self) {
        *self
            .shared
            .folders_at
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }
}

fn run_refresh(shared: &Shared, dirs: &PcDirs, folders: &[PathBuf]) {
    let quick = quick_numbers(shared, dirs);
    shared.view.lock().unwrap_or_else(|e| e.into_inner()).quick = Some(quick);

    if folders.is_empty() {
        shared
            .view
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .folders = Some(FoldersSize::Done {
            count: 0,
            bytes: 0,
            truncated: false,
        });
        return;
    }
    let fresh = shared
        .folders_at
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some_and(|at| at.elapsed() < FOLDER_CACHE);
    let known = shared
        .view
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .folders
        .is_some_and(
            |size| matches!(size, FoldersSize::Done { count, .. } if count == folders.len()),
        );
    if fresh && known {
        return;
    }
    {
        let mut view = shared.view.lock().unwrap_or_else(|e| e.into_inner());
        if !known {
            view.folders = Some(FoldersSize::Counting);
        }
    }
    let (bytes, truncated) = folders_size(folders, WALK_BUDGET, WALK_MAX_ENTRIES);
    shared
        .view
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .folders = Some(FoldersSize::Done {
        count: folders.len(),
        bytes,
        truncated,
    });
    *shared.folders_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
}

fn quick_numbers(shared: &Shared, dirs: &PcDirs) -> Quick {
    let pid = sysinfo::get_current_pid().unwrap_or(Pid::from_u32(0));
    let (memory, cpu) = {
        let mut guard = shared.sampler.lock().unwrap_or_else(|e| e.into_inner());
        let sampler = guard.get_or_insert_with(|| {
            let mut system = System::new();
            system.refresh_cpu_usage();
            system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
            Sampler {
                system,
                last: Instant::now(),
            }
        });
        let wait = MINIMUM_CPU_UPDATE_INTERVAL.saturating_sub(sampler.last.elapsed());
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
        sampler.system.refresh_cpu_usage();
        sampler
            .system
            .refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        sampler.last = Instant::now();
        let cores = sampler.system.cpus().len().max(1) as f32;
        sampler
            .system
            .process(pid)
            .map(|process| {
                (
                    process.memory(),
                    (process.cpu_usage() / cores).clamp(0.0, 100.0),
                )
            })
            .unwrap_or((0, 0.0))
    };
    let data = local_data(dirs);
    let disk_dir = dirs.settings.as_deref().or(dirs.updates.as_deref());
    Quick {
        app_memory_bytes: memory,
        app_cpu_percent: cpu,
        data,
        disk: disk_dir.and_then(disk_space),
    }
}

/// (total, free) of the disk whose mount point is the longest prefix of `path`.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    fn plain(path: &Path) -> PathBuf {
        let text = path.to_string_lossy();
        match text.strip_prefix(r"\\?\") {
            Some(rest) => PathBuf::from(rest),
            None => path.to_path_buf(),
        }
    }
    // The folder may not exist yet: use the nearest parent that does.
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        if !probe.pop() {
            return None;
        }
    }
    let target = plain(&probe.canonicalize().unwrap_or(probe));
    let disks = Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|disk| target.starts_with(plain(disk.mount_point())))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| {
            (
                disk.total_space(),
                disk.available_space().min(disk.total_space()),
            )
        })
}

/// The updater's log keeps its own line in the breakdown.
const UPDATE_LOG: &str = "update.log";

/// Sizes of the app's own folders (small, so no budget games).
pub fn local_data(dirs: &PcDirs) -> LocalData {
    let mut data = LocalData::default();
    if let Some(dir) = &dirs.settings {
        data.settings_bytes =
            walk_size(dir, Instant::now() + Duration::from_secs(2), &mut 100_000).0;
    }
    if let Some(dir) = &dirs.updates {
        let total = walk_size(dir, Instant::now() + Duration::from_secs(2), &mut 100_000).0;
        let log = std::fs::symlink_metadata(dir.join(UPDATE_LOG))
            .map(|meta| if meta.is_file() { meta.len() } else { 0 })
            .unwrap_or(0);
        data.log_bytes = log;
        data.updates_bytes = total.saturating_sub(log);
    }
    data
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

/// Total file bytes under `root`. Symlinks and junctions are not followed.
/// Stops at `deadline` or when `budget` entries are used up; the flag says
/// whether it stopped early.
pub fn walk_size(root: &Path, deadline: Instant, budget: &mut u64) -> (u64, bool) {
    let mut total = 0u64;
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if *budget == 0 || Instant::now() > deadline {
                return (total, true);
            }
            *budget -= 1;
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
            if file_type.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((entry.path(), depth + 1));
                }
            } else if file_type.is_file() {
                total += metadata.len();
            }
        }
    }
    (total, false)
}

/// Total of several folders under one shared time and entry budget.
pub fn folders_size(folders: &[PathBuf], time: Duration, max_entries: u64) -> (u64, bool) {
    let deadline = Instant::now() + time;
    let mut budget = max_entries;
    let mut total = 0u64;
    for folder in folders {
        let (bytes, truncated) = walk_size(folder, deadline, &mut budget);
        total += bytes;
        if truncated {
            return (total, true);
        }
    }
    (total, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("serabot-pc-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    #[test]
    fn walk_adds_up_nested_files() {
        let dir = temp("walk");
        write(&dir.join("a.txt"), 10);
        write(&dir.join("sub/b.txt"), 20);
        write(&dir.join("sub/deeper/c.txt"), 30);
        let (bytes, truncated) =
            walk_size(&dir, Instant::now() + Duration::from_secs(5), &mut 1000);
        assert_eq!((bytes, truncated), (60, false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_stops_at_its_entry_budget_and_says_so() {
        let dir = temp("budget");
        for index in 0..30 {
            write(&dir.join(format!("f{index}.txt")), 1);
        }
        let (bytes, truncated) = walk_size(&dir, Instant::now() + Duration::from_secs(5), &mut 5);
        assert!(truncated);
        assert!(bytes <= 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_folder_counts_as_zero() {
        let gone = std::env::temp_dir().join(format!("serabot-pc-gone-{}", uuid::Uuid::new_v4()));
        assert_eq!(
            folders_size(&[gone], Duration::from_secs(1), 100),
            (0, false)
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let outside = temp("outside");
        write(&outside.join("big.bin"), 9000);
        let dir = temp("links");
        write(&dir.join("ok.txt"), 4);
        std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();
        let (bytes, _) = walk_size(&dir, Instant::now() + Duration::from_secs(5), &mut 1000);
        assert_eq!(bytes, 4);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn local_data_separates_settings_downloads_and_log() {
        let settings = temp("settings");
        let updates = temp("updates");
        write(&settings.join("settings.json"), 100);
        write(&updates.join("serabot-desktop-9.9.9.exe"), 5000);
        write(&updates.join("update.log"), 40);
        let data = local_data(&PcDirs {
            settings: Some(settings.clone()),
            updates: Some(updates.clone()),
        });
        assert_eq!(data.settings_bytes, 100);
        assert_eq!(data.updates_bytes, 5000);
        assert_eq!(data.log_bytes, 40);
        assert_eq!(data.total(), 5140);
        assert_eq!(data.clearable(), 5000);
        let _ = std::fs::remove_dir_all(&settings);
        let _ = std::fs::remove_dir_all(&updates);
    }

    #[test]
    fn refresh_fills_quick_numbers_then_the_folder_total() {
        let settings = temp("r-settings");
        let folder = temp("r-folder");
        write(&folder.join("a.bin"), 700);
        write(&settings.join("settings.json"), 10);
        let monitor = PcMonitor::new();
        assert!(monitor.view().quick.is_none());
        monitor.refresh(
            PcDirs {
                settings: Some(settings.clone()),
                updates: None,
            },
            vec![folder.clone()],
        );
        let started = Instant::now();
        loop {
            let view = monitor.view();
            if let (
                Some(quick),
                Some(FoldersSize::Done {
                    count,
                    bytes,
                    truncated,
                }),
            ) = (view.quick, view.folders)
            {
                assert_eq!((count, bytes, truncated), (1, 700, false));
                assert!(quick.app_memory_bytes > 0);
                assert_eq!(quick.data.settings_bytes, 10);
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "refresh never finished"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(&settings);
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn no_allowed_folders_is_an_immediate_zero() {
        let monitor = PcMonitor::new();
        monitor.refresh(PcDirs::default(), Vec::new());
        let started = Instant::now();
        loop {
            if let Some(FoldersSize::Done {
                count: 0, bytes: 0, ..
            }) = monitor.view().folders
            {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
