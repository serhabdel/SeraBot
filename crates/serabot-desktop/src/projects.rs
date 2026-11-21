//! Blocking API helpers behind the desktop Projects workspace (0.59).
//! Each call opens a short-lived `ApiClient` from the session credentials and
//! runs off the UI thread.

use serde_json::Value;

use crate::api::ApiClient;
use crate::worker::Credentials;

fn client(credentials: &Credentials) -> Result<ApiClient, String> {
    ApiClient::new(credentials.endpoint.clone(), credentials.token.clone())
        .map_err(|error| error.to_string())
}

pub fn list(credentials: &Credentials) -> Result<Value, String> {
    client(credentials)?
        .projects_list()
        .map_err(|error| error.to_string())
}

pub fn create(credentials: &Credentials, name: &str) -> Result<Value, String> {
    client(credentials)?
        .projects_create(name)
        .map_err(|error| error.to_string())
}

pub fn list_files(credentials: &Credentials, slug: &str) -> Result<Value, String> {
    client(credentials)?
        .projects_files(slug)
        .map_err(|error| error.to_string())
}

pub fn upload(
    credentials: &Credentials,
    slug: &str,
    filename: &str,
    subfolder: Option<&str>,
    bytes: &[u8],
) -> Result<Value, String> {
    client(credentials)?
        .projects_upload(slug, filename, subfolder, bytes)
        .map_err(|error| error.to_string())
}

pub fn download(credentials: &Credentials, slug: &str, rel_path: &str) -> Result<Vec<u8>, String> {
    client(credentials)?
        .projects_download(slug, rel_path)
        .map_err(|error| error.to_string())
}

pub fn delete_file(credentials: &Credentials, slug: &str, rel_path: &str) -> Result<Value, String> {
    client(credentials)?
        .projects_delete_file(slug, rel_path)
        .map_err(|error| error.to_string())
}

/// `GET /v1/projects/{slug}/check` (0.73): the configured check command.
pub fn check_get(credentials: &Credentials, slug: &str) -> Result<Value, String> {
    client(credentials)?
        .projects_check_get(slug)
        .map_err(|error| error.to_string())
}

/// `PUT /v1/projects/{slug}/check` (0.73): set the check command (argv array).
pub fn check_put(
    credentials: &Credentials,
    slug: &str,
    command: &[String],
) -> Result<Value, String> {
    client(credentials)?
        .projects_check_put(slug, command)
        .map_err(|error| error.to_string())
}

/// `POST /v1/projects/{slug}/check/run` (0.73): run the check now.
pub fn check_run(credentials: &Credentials, slug: &str) -> Result<Value, String> {
    client(credentials)?
        .projects_check_run(slug)
        .map_err(|error| error.to_string())
}

/// `GET /v1/projects/{slug}/diagnostics` (0.73): latest run plus diagnostics.
pub fn diagnostics(credentials: &Credentials, slug: &str) -> Result<Value, String> {
    client(credentials)?
        .projects_diagnostics(slug)
        .map_err(|error| error.to_string())
}

/// `PUT /v1/projects/{slug}/files/{*path}` (0.74): save a text file from the
/// workspace editor.
pub fn write_file(
    credentials: &Credentials,
    slug: &str,
    rel_path: &str,
    content: &str,
) -> Result<Value, String> {
    client(credentials)?
        .projects_write_file(slug, rel_path, content)
        .map_err(|error| error.to_string())
}

/// Summary row for a project folder (`GET /v1/projects`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRow {
    pub slug: String,
    pub file_count: usize,
    pub total_bytes: u64,
}

/// One file inside a project folder (`GET /v1/projects/{slug}/files`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    pub name: String,
    pub path: String,
    pub inbox_path: String,
    pub size_bytes: u64,
    pub mime: String,
    pub modified_at: Option<String>,
}

/// One diagnostic line from `GET /v1/projects/{slug}/diagnostics`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagRow {
    pub file: String,
    pub line: Option<u64>,
    pub column: Option<u64>,
    pub severity: String,
    pub code: Option<String>,
    pub message: String,
}

/// Last check status badge derived from a project's latest `run` payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckStatus {
    Unchecked,
    Running,
    Passing,
    Warnings,
    Failing,
    Errored,
}

impl CheckStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unchecked => "Not checked",
            Self::Running => "Checking",
            Self::Passing => "Passing",
            Self::Warnings => "Warnings",
            Self::Failing => "Failing",
            Self::Errored => "Check error",
        }
    }

    pub fn css_modifier(self) -> &'static str {
        match self {
            Self::Unchecked => "muted",
            Self::Running => "acc",
            Self::Passing => "ok",
            Self::Warnings => "warn",
            Self::Failing | Self::Errored => "err",
        }
    }
}

/// Severity breakdown and run status from `GET /v1/projects/{slug}/diagnostics`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagSummary {
    pub status: CheckStatus,
    pub errors: u64,
    pub warnings: u64,
    pub notes: u64,
    pub finished_at: Option<String>,
    pub error_message: Option<String>,
}

/// One flattened entry in the project file tree (directories and files).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectTreeEntry {
    pub key: String,
    pub label: String,
    pub depth: usize,
    pub is_dir: bool,
    pub file: Option<FileRow>,
}

/// Parse `GET /v1/projects` JSON into `ProjectRow` values.
pub fn parse_projects(value: &Value) -> Vec<ProjectRow> {
    value["projects"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(ProjectRow {
                        slug: item["slug"].as_str()?.to_owned(),
                        file_count: item["file_count"].as_u64().unwrap_or(0) as usize,
                        total_bytes: item["total_bytes"].as_u64().unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse `GET /v1/projects/{slug}/files` JSON into `FileRow` values.
pub fn parse_files(value: &Value) -> Vec<FileRow> {
    value["files"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(FileRow {
                        name: item["name"].as_str()?.to_owned(),
                        path: item["path"].as_str()?.to_owned(),
                        inbox_path: item["inbox_path"].as_str()?.to_owned(),
                        size_bytes: item["size_bytes"].as_u64().unwrap_or(0),
                        mime: item["mime"].as_str().unwrap_or("").to_owned(),
                        modified_at: item["modified_at"]
                            .as_str()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Split `GET /v1/projects/{slug}/diagnostics` into the run summary and rows.
pub fn parse_diagnostics(value: &Value) -> (Option<Value>, Vec<DiagRow>) {
    let run = value.get("run").filter(|r| !r.is_null()).cloned();
    let rows = value["diagnostics"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| DiagRow {
                    file: item["file"].as_str().unwrap_or("").to_owned(),
                    line: item["line"].as_u64(),
                    column: item["column"].as_u64(),
                    severity: item["severity"].as_str().unwrap_or("note").to_owned(),
                    code: item["code"].as_str().map(str::to_owned),
                    message: item["message"].as_str().unwrap_or("").to_owned(),
                })
                .collect()
        })
        .unwrap_or_default();
    (run, rows)
}

/// Derive a structured status & severity summary for a project's check run.
pub fn summarize_diagnostics(run: Option<&Value>, rows: &[DiagRow], checking: bool) -> DiagSummary {
    if checking {
        return DiagSummary {
            status: CheckStatus::Running,
            errors: 0,
            warnings: 0,
            notes: 0,
            finished_at: None,
            error_message: None,
        };
    }
    let Some(run) = run else {
        return DiagSummary {
            status: CheckStatus::Unchecked,
            errors: 0,
            warnings: 0,
            notes: 0,
            finished_at: None,
            error_message: None,
        };
    };
    let run_errors = run["errors"].as_u64();
    let run_warnings = run["warnings"].as_u64();
    let run_notes = run["notes"].as_u64();
    let errors =
        run_errors.unwrap_or_else(|| rows.iter().filter(|d| d.severity == "error").count() as u64);
    let warnings = run_warnings
        .unwrap_or_else(|| rows.iter().filter(|d| d.severity == "warning").count() as u64);
    let notes = run_notes.unwrap_or_else(|| {
        rows.iter()
            .filter(|d| d.severity != "error" && d.severity != "warning")
            .count() as u64
    });
    let run_status = run["status"].as_str().unwrap_or("");
    let error_message = run["error"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let finished_at = run["finished_at"]
        .as_str()
        .or_else(|| run["started_at"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let status = if run_status == "running" {
        CheckStatus::Running
    } else if run_status == "error" || (error_message.is_some() && errors == 0) {
        CheckStatus::Errored
    } else if errors > 0 {
        CheckStatus::Failing
    } else if warnings > 0 {
        CheckStatus::Warnings
    } else {
        CheckStatus::Passing
    };
    DiagSummary {
        status,
        errors,
        warnings,
        notes,
        finished_at,
        error_message,
    }
}

/// Whether a project file opens in the workspace editor (0.74).
pub fn is_editable_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        ".rs", ".toml", ".md", ".txt", ".json", ".csv", ".yaml", ".yml", ".py", ".js", ".ts",
        ".tsx", ".jsx", ".html", ".css", ".xml", ".svg", ".log", ".sh", ".bat", ".sql", ".env",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

pub fn is_editable_file(mime: &str, name: &str) -> bool {
    mime.starts_with("text/")
        || mime.contains("json")
        || mime.contains("xml")
        || is_editable_name(name)
}

/// Human-readable byte size formatting.
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Latest modification timestamp across a project's file list.
pub fn latest_modified_stamp(files: &[FileRow]) -> Option<String> {
    files
        .iter()
        .filter_map(|f| f.modified_at.as_deref())
        .max()
        .map(str::to_owned)
}

/// Format an RFC 3339 timestamp as a compact relative time (`5m ago`, `2d ago`),
/// falling back to trimmed input if unparseable.
pub fn format_relative_stamp(raw: &str, now_secs: i64) -> String {
    crate::workflows::relative_time(raw, now_secs)
}

/// Build an ordered directory + file tree list from flat relative file paths.
/// Directories appear before sibling files at each level.
pub fn build_project_tree(files: &[FileRow]) -> Vec<ProjectTreeEntry> {
    use std::collections::BTreeSet;

    let mut seen_dirs: BTreeSet<String> = BTreeSet::new();
    let mut entries = Vec::new();
    let mut sorted = files.to_vec();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));

    for file in sorted {
        let parts: Vec<&str> = file.path.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            continue;
        }
        let mut prefix = String::new();
        for (idx, dir_name) in parts[..parts.len() - 1].iter().enumerate() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(dir_name);
            if seen_dirs.insert(prefix.clone()) {
                entries.push(ProjectTreeEntry {
                    key: format!("dir:{prefix}"),
                    label: (*dir_name).to_owned(),
                    depth: idx,
                    is_dir: true,
                    file: None,
                });
            }
        }
        let label = parts
            .last()
            .copied()
            .unwrap_or(file.name.as_str())
            .to_owned();
        entries.push(ProjectTreeEntry {
            key: format!("file:{}", file.path),
            label,
            depth: parts.len().saturating_sub(1),
            is_dir: false,
            file: Some(file),
        });
    }
    entries
}

/// Breadcrumb trail segments for an active project and optional open file path.
pub fn project_breadcrumbs(slug: Option<&str>, open_path: Option<&str>) -> Vec<String> {
    let mut crumbs = vec!["Projects".to_owned()];
    if let Some(slug) = slug.map(str::trim).filter(|s| !s.is_empty()) {
        crumbs.push(slug.to_owned());
        if let Some(path) = open_path.map(str::trim).filter(|s| !s.is_empty()) {
            for part in path.split('/').filter(|s| !s.is_empty()) {
                crumbs.push(part.to_owned());
            }
        }
    }
    crumbs
}

/// Parse user-entered check command input (JSON argv array or whitespace-separated words).
pub fn parse_check_command(raw: &str) -> Vec<String> {
    let text = raw.trim();
    if text.is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<String>>(text)
        .ok()
        .filter(|parts| !parts.is_empty() && parts.iter().all(|p| !p.trim().is_empty()))
        .unwrap_or_else(|| text.split_whitespace().map(str::to_owned).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats_byte_sizes_cleanly() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2.0 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn parses_projects_files_and_diagnostics() {
        let projects = parse_projects(&json!({
            "projects": [
                { "slug": "demo-app", "file_count": 4, "total_bytes": 4096 }
            ]
        }));
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].slug, "demo-app");
        assert_eq!(projects[0].file_count, 4);
        assert_eq!(projects[0].total_bytes, 4096);

        let files = parse_files(&json!({
            "files": [
                {
                    "name": "main.rs",
                    "path": "src/main.rs",
                    "inbox_path": "u1/projects/demo-app/src/main.rs",
                    "size_bytes": 512,
                    "mime": "text/plain",
                    "modified_at": "2026-11-10T10:00:00Z"
                }
            ]
        }));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(
            files[0].modified_at.as_deref(),
            Some("2026-11-10T10:00:00Z")
        );
        assert_eq!(
            latest_modified_stamp(&files).as_deref(),
            Some("2026-11-10T10:00:00Z")
        );

        let (run, diags) = parse_diagnostics(&json!({
            "run": {
                "status": "done",
                "errors": 1,
                "warnings": 2,
                "notes": 0,
                "finished_at": "2026-11-10T10:05:00Z"
            },
            "diagnostics": [
                {
                    "file": "src/main.rs",
                    "line": 12,
                    "column": 5,
                    "severity": "error",
                    "code": "E0308",
                    "message": "mismatched types"
                }
            ]
        }));
        assert!(run.is_some());
        assert_eq!(diags.len(), 1);

        let summary = summarize_diagnostics(run.as_ref(), &diags, false);
        assert_eq!(summary.status, CheckStatus::Failing);
        assert_eq!(summary.errors, 1);
        assert_eq!(summary.warnings, 2);
        assert_eq!(summary.notes, 0);
        assert_eq!(summary.status.label(), "Failing");

        let running = summarize_diagnostics(run.as_ref(), &diags, true);
        assert_eq!(running.status, CheckStatus::Running);
        assert_eq!(
            summarize_diagnostics(None, &[], false).status,
            CheckStatus::Unchecked
        );
    }

    #[test]
    fn builds_nested_file_tree_and_breadcrumbs() {
        let files = vec![
            FileRow {
                name: "Cargo.toml".into(),
                path: "Cargo.toml".into(),
                inbox_path: "u/projects/demo/Cargo.toml".into(),
                size_bytes: 120,
                mime: "text/plain".into(),
                modified_at: None,
            },
            FileRow {
                name: "main.rs".into(),
                path: "src/main.rs".into(),
                inbox_path: "u/projects/demo/src/main.rs".into(),
                size_bytes: 200,
                mime: "text/plain".into(),
                modified_at: None,
            },
            FileRow {
                name: "util.rs".into(),
                path: "src/nested/util.rs".into(),
                inbox_path: "u/projects/demo/src/nested/util.rs".into(),
                size_bytes: 80,
                mime: "text/plain".into(),
                modified_at: None,
            },
        ];
        let tree = build_project_tree(&files);
        assert_eq!(tree.len(), 5);
        assert_eq!(
            (tree[0].label.as_str(), tree[0].is_dir, tree[0].depth),
            ("Cargo.toml", false, 0)
        );
        assert_eq!(
            (tree[1].label.as_str(), tree[1].is_dir, tree[1].depth),
            ("src", true, 0)
        );
        assert_eq!(
            (tree[2].label.as_str(), tree[2].is_dir, tree[2].depth),
            ("main.rs", false, 1)
        );
        assert_eq!(
            (tree[3].label.as_str(), tree[3].is_dir, tree[3].depth),
            ("nested", true, 1)
        );
        assert_eq!(
            (tree[4].label.as_str(), tree[4].is_dir, tree[4].depth),
            ("util.rs", false, 2)
        );

        assert_eq!(
            project_breadcrumbs(Some("demo"), Some("src/nested/util.rs")),
            vec!["Projects", "demo", "src", "nested", "util.rs"]
        );
        assert_eq!(project_breadcrumbs(None, None), vec!["Projects"]);
        assert_eq!(
            parse_check_command(r#"["cargo","check"]"#),
            vec!["cargo", "check"]
        );
        assert_eq!(
            parse_check_command("cargo test --no-run"),
            vec!["cargo", "test", "--no-run"]
        );
        assert!(parse_check_command("   ").is_empty());
    }
}
