//! Per-user project folders in the server inbox.
//!
//! Every user's files live under `<inbox>/<user_id>/`. Chat attachments land in
//! `chat/<conversation>/`; projects live in `projects/<slug>/`. A bot that sees
//! a file belongs to a project moves it there with `project_file`, so the
//! user's documents end up organised by project instead of by chat.
//!
//! The model never chooses a server path: sources go through
//! [`crate::user_inbox::resolve`] (the acting user's own directory only, no
//! links out), project names become a fixed `[a-z0-9-]` slug, and a move never
//! overwrites an existing file. Moves stay inside the user's own area, so they
//! are not on the approval list; they are audited by the gateway like any
//! other call.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::config::Config;
use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolSpec};

pub const PROJECT_LIST_TOOL: &str = "project_list";
pub const PROJECT_CREATE_TOOL: &str = "project_create";
pub const PROJECT_FILE_TOOL: &str = "project_file";
pub const PROJECT_CHECK_TOOL: &str = "project_check";

const PROJECTS_DIR: &str = "projects";
const MAX_SLUG: usize = 64;
const MAX_PROJECTS: usize = 200;
const MAX_LISTED_FILES: usize = 50;

/// A project name as a folder name: lowercase ASCII letters, digits and `-`.
pub fn slugify(name: &str) -> Result<String> {
    let mut slug = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug
        .trim_end_matches('-')
        .chars()
        .take(MAX_SLUG)
        .collect::<String>();
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        return Err(SeraError::Validation(
            "a project name needs at least one letter or digit".into(),
        ));
    }
    Ok(slug)
}

fn safe_subfolder(value: &str) -> Result<Vec<String>> {
    let mut parts = Vec::new();
    for part in value.split(['/', '\\']).filter(|part| !part.is_empty()) {
        parts.push(slugify(part)?);
    }
    if parts.len() > 3 {
        return Err(SeraError::Validation(
            "a subfolder can be at most three levels deep".into(),
        ));
    }
    Ok(parts)
}

#[derive(Clone)]
struct Inbox {
    config: Arc<Config>,
}

impl Inbox {
    fn root(&self) -> PathBuf {
        self.config.path(&self.config.documents.watch_dir)
    }

    /// `<inbox>/<user>/projects`, created on demand as a real directory.
    fn projects_dir(&self, user_id: &str) -> Result<PathBuf> {
        if !crate::user_inbox::safe_user_segment(user_id) {
            return Err(SeraError::Forbidden(
                "the account id is not a safe inbox directory name".into(),
            ));
        }
        let dir = self.root().join(user_id).join(PROJECTS_DIR);
        std::fs::create_dir_all(&dir)?;
        let meta = std::fs::symlink_metadata(&dir)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(SeraError::Forbidden(
                "the projects folder is not a plain folder".into(),
            ));
        }
        Ok(dir)
    }

    fn list(&self, user_id: &str) -> Result<Vec<String>> {
        let dir = self.projects_dir(user_id)?;
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir()
                && !kind.is_symlink()
                && let Some(name) = entry.file_name().to_str()
            {
                names.push(name.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    fn create(&self, user_id: &str, name: &str) -> Result<(String, bool)> {
        let slug = slugify(name)?;
        let dir = self.projects_dir(user_id)?;
        let target = dir.join(&slug);
        if target.is_dir() {
            return Ok((slug, false));
        }
        if self.list(user_id)?.len() >= MAX_PROJECTS {
            return Err(SeraError::Validation("too many projects".into()));
        }
        std::fs::create_dir(&target)?;
        Ok((slug, true))
    }
}

fn rel(user_id: &str, parts: &[&str]) -> String {
    let mut out = format!("{user_id}/{PROJECTS_DIR}");
    for part in parts {
        out.push('/');
        out.push_str(part);
    }
    out
}

fn files_in(dir: &Path, prefix: &str, out: &mut Vec<String>, depth: usize) -> Result<()> {
    if depth > 3 || out.len() >= MAX_LISTED_FILES {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let kind = entry.file_type()?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        let path = format!("{prefix}/{name}");
        if kind.is_dir() {
            files_in(&entry.path(), &path, out, depth + 1)?;
        } else if out.len() < MAX_LISTED_FILES {
            out.push(path);
        }
    }
    Ok(())
}

use serde::Serialize;

use crate::store::Store;

/// Summary of one user project folder, returned by `GET /v1/projects`.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectSummary {
    pub slug: String,
    pub file_count: usize,
    pub total_bytes: u64,
}

/// One file inside a user project folder, returned by `GET /v1/projects/{slug}/files`.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectFileEntry {
    pub name: String,
    /// Relative path inside the project (e.g. `invoices/2026/a.pdf`).
    pub path: String,
    /// Inbox path usable with bot document tools (`<user>/projects/<slug>/...`).
    pub inbox_path: String,
    pub size_bytes: u64,
    pub mime: String,
    pub modified_at: Option<String>,
}

fn collect_entries(
    dir: &Path,
    user_id: &str,
    slug: &str,
    sub_prefix: &str,
    out: &mut Vec<ProjectFileEntry>,
    depth: usize,
) -> Result<()> {
    if depth > 3 || out.len() >= MAX_LISTED_FILES * 4 {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let rel_in_project = if sub_prefix.is_empty() {
            name.clone()
        } else {
            format!("{sub_prefix}/{name}")
        };
        if kind.is_dir() {
            collect_entries(
                &entry.path(),
                user_id,
                slug,
                &rel_in_project,
                out,
                depth + 1,
            )?;
        } else if kind.is_file() {
            let meta = entry.metadata()?;
            let modified_at = meta.modified().ok().map(|t| {
                let dt: chrono::DateTime<chrono::Utc> = t.into();
                dt.to_rfc3339()
            });
            let inbox_path = format!("{user_id}/{PROJECTS_DIR}/{slug}/{rel_in_project}");
            out.push(ProjectFileEntry {
                mime: crate::util::mime_for_filename(&name).to_owned(),
                name,
                path: rel_in_project,
                inbox_path,
                size_bytes: meta.len(),
                modified_at,
            });
        }
    }
    Ok(())
}

/// List all projects owned by `user_id` with file counts and sizes.
pub fn list_projects_with_stats(config: &Config, user_id: &str) -> Result<Vec<ProjectSummary>> {
    let inbox = Inbox {
        config: Arc::new(config.clone()),
    };
    let slugs = inbox.list(user_id)?;
    let base = inbox.projects_dir(user_id)?;
    let mut out = Vec::with_capacity(slugs.len());
    for slug in slugs {
        let mut entries = Vec::new();
        let dir = base.join(&slug);
        if dir.is_dir() {
            collect_entries(&dir, user_id, &slug, "", &mut entries, 0)?;
        }
        let total_bytes = entries.iter().map(|e| e.size_bytes).sum();
        out.push(ProjectSummary {
            slug,
            file_count: entries.len(),
            total_bytes,
        });
    }
    Ok(out)
}

/// Create a project for `user_id`. Returns `(slug, created)`.
pub fn create_project(config: &Config, user_id: &str, name: &str) -> Result<(String, bool)> {
    let inbox = Inbox {
        config: Arc::new(config.clone()),
    };
    inbox.create(user_id, name)
}

/// List files inside one project owned by `user_id`.
pub fn list_project_files(
    config: &Config,
    user_id: &str,
    project: &str,
) -> Result<(String, Vec<ProjectFileEntry>)> {
    let slug = slugify(project)?;
    let inbox = Inbox {
        config: Arc::new(config.clone()),
    };
    let dir = inbox.projects_dir(user_id)?.join(&slug);
    if !dir.is_dir() {
        return Err(SeraError::NotFound(format!("no project named {slug}")));
    }
    let mut entries = Vec::new();
    collect_entries(&dir, user_id, &slug, "", &mut entries, 0)?;
    Ok((slug, entries))
}

/// Save a validated uploaded file directly into a user's project folder.
pub fn save_project_upload(
    config: &Config,
    user_id: &str,
    project: &str,
    subfolder: Option<&str>,
    filename: &str,
    bytes: &[u8],
) -> Result<ProjectFileEntry> {
    crate::attachments::validate_upload(filename, bytes)?;
    let safe_name = crate::telegram::safe_upload_name(filename);
    let sub_parts = safe_subfolder(subfolder.unwrap_or(""))?;
    let inbox = Inbox {
        config: Arc::new(config.clone()),
    };
    let (slug, _) = inbox.create(user_id, project)?;
    let projects_root = inbox.projects_dir(user_id)?.canonicalize()?;
    let mut target_dir = inbox.projects_dir(user_id)?.join(&slug);
    for part in &sub_parts {
        target_dir.push(part);
    }
    std::fs::create_dir_all(&target_dir)?;
    if !target_dir.canonicalize()?.starts_with(&projects_root) {
        return Err(SeraError::Forbidden(
            "the target is outside the projects folder".into(),
        ));
    }
    let target = target_dir.join(&safe_name);
    if target.exists() {
        return Err(SeraError::Validation(format!(
            "{safe_name} already exists in that project; rename or choose a subfolder"
        )));
    }
    std::fs::write(&target, bytes)?;
    let mut path_parts = sub_parts;
    path_parts.push(safe_name.clone());
    let rel_in_project = path_parts.join("/");
    let inbox_path = format!("{user_id}/{PROJECTS_DIR}/{slug}/{rel_in_project}");
    Ok(ProjectFileEntry {
        mime: crate::util::mime_for_filename(&safe_name).to_owned(),
        name: safe_name,
        path: rel_in_project,
        inbox_path,
        size_bytes: bytes.len() as u64,
        modified_at: Some(chrono::Utc::now().to_rfc3339()),
    })
}

/// Resolve a relative file path inside `<user>/projects/<slug>/` safely.
fn resolve_project_file(
    config: &Config,
    user_id: &str,
    project: &str,
    rel_file: &str,
) -> Result<(String, PathBuf)> {
    let slug = slugify(project)?;
    if rel_file.trim().is_empty()
        || rel_file.contains("..")
        || rel_file.starts_with('/')
        || rel_file.starts_with('\\')
    {
        return Err(SeraError::Validation("invalid project file path".into()));
    }
    let inbox = Inbox {
        config: Arc::new(config.clone()),
    };
    let project_dir = inbox.projects_dir(user_id)?.join(&slug);
    if !project_dir.is_dir() {
        return Err(SeraError::NotFound(format!("no project named {slug}")));
    }
    let project_canon = project_dir.canonicalize()?;
    let mut candidate = project_dir;
    for part in rel_file.split(['/', '\\']).filter(|p| !p.is_empty()) {
        if part == "." || part == ".." {
            return Err(SeraError::Validation("invalid project file path".into()));
        }
        candidate.push(part);
    }
    let meta = std::fs::symlink_metadata(&candidate)
        .map_err(|_| SeraError::NotFound("project file".into()))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(SeraError::NotFound("project file".into()));
    }
    let canon = candidate.canonicalize()?;
    if !canon.starts_with(&project_canon) {
        return Err(SeraError::Forbidden(
            "the file is outside the project folder".into(),
        ));
    }
    let filename = canon
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    Ok((filename, canon))
}

/// Read one file inside a user's project folder. Returns `(filename, bytes)`.
pub fn read_project_file(
    config: &Config,
    user_id: &str,
    project: &str,
    rel_file: &str,
) -> Result<(String, Vec<u8>)> {
    let (filename, path) = resolve_project_file(config, user_id, project, rel_file)?;
    let bytes = std::fs::read(&path)?;
    Ok((filename, bytes))
}

/// Delete one file inside a user's project folder. Returns the deleted filename.
pub fn delete_project_file(
    config: &Config,
    user_id: &str,
    project: &str,
    rel_file: &str,
) -> Result<String> {
    let (filename, path) = resolve_project_file(config, user_id, project, rel_file)?;
    std::fs::remove_file(&path)?;
    Ok(filename)
}

/// Maximum size of a text file the workspace editor can save (1 MiB).
pub const MAX_PROJECT_TEXT_BYTES: usize = 1024 * 1024;

/// Write a text file inside a user's project folder (the workspace editor's
/// save). Creates missing parent folders, never follows symlinks, and keeps
/// the target inside the project folder (canonicalized containment).
pub fn write_project_file(
    config: &Config,
    user_id: &str,
    project: &str,
    rel_file: &str,
    content: &str,
) -> Result<ProjectFileEntry> {
    if content.len() > MAX_PROJECT_TEXT_BYTES {
        return Err(SeraError::Validation(
            "the file is larger than 1 MiB; split it first".into(),
        ));
    }
    let slug = slugify(project)?;
    if rel_file.trim().is_empty()
        || rel_file.contains("..")
        || rel_file.starts_with('/')
        || rel_file.starts_with('\\')
    {
        return Err(SeraError::Validation("invalid project file path".into()));
    }
    let inbox = Inbox {
        config: Arc::new(config.clone()),
    };
    let project_dir = inbox.projects_dir(user_id)?.join(&slug);
    if !project_dir.is_dir() {
        return Err(SeraError::NotFound(format!("no project named {slug}")));
    }
    let projects_root = inbox.projects_dir(user_id)?.canonicalize()?;
    let project_canon = project_dir.canonicalize()?;
    if !project_canon.starts_with(&projects_root) {
        return Err(SeraError::Forbidden(
            "the project folder is outside the projects directory".into(),
        ));
    }
    let mut target = project_dir.clone();
    for part in rel_file.split(['/', '\\']).filter(|p| !p.is_empty()) {
        if part == "." || part == ".." {
            return Err(SeraError::Validation("invalid project file path".into()));
        }
        target.push(part);
    }
    if let Ok(meta) = std::fs::symlink_metadata(&target)
        && (!meta.is_file() || meta.file_type().is_symlink())
    {
        return Err(SeraError::Validation("the path is not a plain file".into()));
    }
    let parent = target
        .parent()
        .ok_or_else(|| SeraError::Validation("invalid project file path".into()))?;
    std::fs::create_dir_all(parent)?;
    let parent_canon = parent.canonicalize()?;
    if !parent_canon.starts_with(&project_canon) {
        return Err(SeraError::Forbidden(
            "the file is outside the project folder".into(),
        ));
    }
    let filename = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    std::fs::write(&target, content.as_bytes())?;
    let rel_in_project = target
        .strip_prefix(&project_canon)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| filename.clone());
    let inbox_path = format!("{user_id}/{PROJECTS_DIR}/{slug}/{rel_in_project}");
    Ok(ProjectFileEntry {
        mime: crate::util::mime_for_filename(&filename).to_owned(),
        name: filename,
        path: rel_in_project,
        inbox_path,
        size_bytes: content.len() as u64,
        modified_at: Some(chrono::Utc::now().to_rfc3339()),
    })
}

pub struct ProjectListTool {
    inbox: Inbox,
}

pub struct ProjectCreateTool {
    inbox: Inbox,
}

pub struct ProjectFileTool {
    inbox: Inbox,
    store: Option<Arc<Store>>,
}

/// 0.73: run the project's configured check command and report the compiler
/// diagnostics. Approval-gated (`DESTRUCTIVE_TOOLS`): it starts a process, so
/// the model can only ask.
pub struct ProjectCheckTool {
    inbox: Inbox,
    store: Option<Arc<Store>>,
}

pub fn register(registry: &mut crate::gateway::ToolRegistry, config: Arc<Config>) -> Result<()> {
    register_with_store(registry, config, None)
}

pub fn register_with_store(
    registry: &mut crate::gateway::ToolRegistry,
    config: Arc<Config>,
    store: Option<Arc<Store>>,
) -> Result<()> {
    let inbox = Inbox { config };
    registry.register(ProjectListTool {
        inbox: inbox.clone(),
    })?;
    registry.register(ProjectCreateTool {
        inbox: inbox.clone(),
    })?;
    registry.register(ProjectFileTool {
        inbox: inbox.clone(),
        store: store.clone(),
    })?;
    registry.register(ProjectCheckTool { inbox, store })?;
    Ok(())
}

#[async_trait]
impl Tool for ProjectListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: PROJECT_LIST_TOOL.into(),
            description: "List the user's project folders on the server, or the files in one \
                          project. Paths returned are inbox paths usable with document tools."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": "Project name; omit to list all projects"}
                },
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let user = &context.user_id;
        match args.get("project").and_then(Value::as_str).map(str::trim) {
            Some(project) if !project.is_empty() => {
                let slug = slugify(project)?;
                let dir = self.inbox.projects_dir(user)?.join(&slug);
                if !dir.is_dir() {
                    return Err(SeraError::NotFound(format!("no project named {slug}")));
                }
                let mut files = Vec::new();
                files_in(&dir, &rel(user, &[&slug]), &mut files, 0)?;
                let content = if files.is_empty() {
                    format!("Project {slug} is empty.")
                } else {
                    format!("Project {slug}:\n{}", files.join("\n"))
                };
                Ok(ToolOutput {
                    content,
                    data: Some(json!({"project": slug, "files": files})),
                    ..ToolOutput::default()
                })
            }
            _ => {
                let projects = self.inbox.list(user)?;
                let content = if projects.is_empty() {
                    "No projects yet. Create one with project_create.".to_string()
                } else {
                    format!("Projects: {}", projects.join(", "))
                };
                Ok(ToolOutput {
                    content,
                    data: Some(json!({"projects": projects})),
                    ..ToolOutput::default()
                })
            }
        }
    }
}

#[async_trait]
impl Tool for ProjectCreateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: PROJECT_CREATE_TOOL.into(),
            description: "Create a project folder on the server for the user's documents. \
                          Creating one that exists is not an error."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {"name": {"type": "string", "description": "Project name"}},
                "required": ["name"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("name is required".into()))?;
        let (slug, created) = self.inbox.create(&context.user_id, name)?;
        let content = if created {
            format!("Created project {slug}.")
        } else {
            format!("Project {slug} already exists.")
        };
        Ok(ToolOutput {
            content,
            data: Some(json!({"project": slug, "created": created})),
            ..ToolOutput::default()
        })
    }
}

#[async_trait]
impl Tool for ProjectFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: PROJECT_FILE_TOOL.into(),
            description: "Move a file from the user's inbox (for example a chat attachment) into \
                          a project folder, creating the project if needed. Never overwrites a \
                          file. Returns the new inbox path; use it from then on."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Inbox path of the file to move"},
                    "project": {"type": "string", "description": "Project name"},
                    "subfolder": {"type": "string", "description": "Optional folder inside the project, e.g. invoices/2026"}
                },
                "required": ["path", "project"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let user = &context.user_id;
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("path is required".into()))?;
        let project = args
            .get("project")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("project is required".into()))?;
        let subfolder =
            safe_subfolder(args.get("subfolder").and_then(Value::as_str).unwrap_or(""))?;

        let source = crate::user_inbox::resolve(&self.inbox.root(), user, path)?;
        if !std::fs::symlink_metadata(&source)?.is_file() {
            return Err(SeraError::Validation(
                "only a single file can be moved".into(),
            ));
        }
        let filename = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| SeraError::Validation("the file name is not valid text".into()))?
            .to_string();
        let (slug, _) = self.inbox.create(user, project)?;
        let mut target_dir = self.inbox.projects_dir(user)?.join(&slug);
        for part in &subfolder {
            target_dir.push(part);
        }
        std::fs::create_dir_all(&target_dir)?;
        let target = target_dir.join(&filename);
        let projects = self.inbox.projects_dir(user)?.canonicalize()?;
        if !target_dir.canonicalize()?.starts_with(&projects) {
            return Err(SeraError::Forbidden(
                "the target is outside the projects folder".into(),
            ));
        }
        if source == target {
            return Err(SeraError::Validation("the file is already there".into()));
        }
        if target.exists() {
            return Err(SeraError::Validation(format!(
                "{filename} already exists in that project; rename or choose a subfolder"
            )));
        }
        let old_rel = source
            .strip_prefix(self.inbox.root())
            .ok()
            .and_then(|p| p.to_str())
            .map(|s| s.replace('\\', "/"));
        std::fs::rename(&source, &target)?;
        let mut parts: Vec<&str> = vec![&slug];
        parts.extend(subfolder.iter().map(String::as_str));
        parts.push(&filename);
        let new_path = rel(user, &parts);
        if let (Some(store), Some(old_rel)) = (&self.store, old_rel.as_deref()) {
            let _ = store.relocate_chat_attachment(user, old_rel, &new_path);
        }
        Ok(ToolOutput {
            content: format!("Moved {filename} to {new_path}."),
            data: Some(json!({"project": slug, "path": new_path})),
            ..ToolOutput::default()
        })
    }
}

#[async_trait]
impl Tool for ProjectCheckTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: PROJECT_CHECK_TOOL.into(),
            description: "Run the check command configured for one of the user's project folders \
                          (default: cargo check --message-format=json) and report the compiler \
                          diagnostics: errors and warnings with file, line and column. Starts a \
                          process, so it always asks for approval first."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": "Project name (slug)"}
                },
                "required": ["project"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let project = args
            .get("project")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("project is required".into()))?;
        let Some(store) = &self.store else {
            return Err(SeraError::Internal(
                "the project check store is unavailable".into(),
            ));
        };
        let summary =
            crate::diagnostics::run_check(&self.inbox.config, store, &context.user_id, project)
                .await?;
        let content = format!(
            "Check `{}` finished in {}ms: {} errors, {} warnings, {} notes. Full rows are stored \
             for project `{}` (run {}); list them with the project diagnostics endpoint.",
            summary.command.join(" "),
            summary.duration_ms,
            summary.errors,
            summary.warnings,
            summary.notes,
            summary.project,
            summary.run_id
        );
        Ok(ToolOutput {
            content,
            data: Some(json!({
                "project": summary.project,
                "run_id": summary.run_id,
                "errors": summary.errors,
                "warnings": summary.warnings,
                "notes": summary.notes,
                "duration_ms": summary.duration_ms,
            })),
            ..ToolOutput::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Arc<Config>) {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            base_dir: dir.path().to_path_buf(),
            ..Config::default()
        };
        let root = config.path(&config.documents.watch_dir);
        std::fs::create_dir_all(root.join("alice/chat/dir-main")).unwrap();
        std::fs::create_dir_all(root.join("bob/chat/dir-main")).unwrap();
        std::fs::write(root.join("alice/chat/dir-main/invoice.pdf"), b"%PDF").unwrap();
        std::fs::write(root.join("bob/chat/dir-main/theirs.pdf"), b"%PDF").unwrap();
        (dir, Arc::new(config))
    }

    fn ctx(user: &str) -> ToolContext {
        ToolContext {
            user_id: user.into(),
            actor: user.into(),
            bot: "comptable".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: Default::default(),
            require_approval: Default::default(),
        }
    }

    #[test]
    fn write_project_file_stays_inside_the_project() {
        let (_dir, config) = setup();
        let (slug, _) = create_project(&config, "alice", "demo").unwrap();
        let entry =
            write_project_file(&config, "alice", "demo", "src/main.rs", "fn main() {}\n").unwrap();
        assert_eq!(entry.path, "src/main.rs");
        assert_eq!(entry.size_bytes, 13);
        let (name, bytes) = read_project_file(&config, "alice", &slug, "src/main.rs").unwrap();
        assert_eq!(name, "main.rs");
        assert_eq!(bytes, b"fn main() {}\n");
        // overwrite keeps containment
        write_project_file(&config, "alice", "demo", "src/main.rs", "fn main() { }\n").unwrap();
        // escapes are refused
        assert!(write_project_file(&config, "alice", "demo", "../evil.rs", "x").is_err());
        assert!(write_project_file(&config, "alice", "demo", "/abs.rs", "x").is_err());
        assert!(write_project_file(&config, "alice", "demo", "a/../../b.rs", "x").is_err());
        // another user cannot see or write it
        assert!(read_project_file(&config, "bob", &slug, "src/main.rs").is_err());
    }

    #[test]
    fn slugs_are_folder_safe() {
        assert_eq!(slugify("  Compta 2026 / Q1 ").unwrap(), "compta-2026-q1");
        assert_eq!(slugify("../../etc").unwrap(), "etc");
        assert!(slugify("..").is_err());
        assert!(slugify("").is_err());
    }

    #[tokio::test]
    async fn files_an_attachment_into_a_project_without_overwriting() {
        let (_dir, config) = setup();
        let inbox = Inbox {
            config: config.clone(),
        };
        let tool = ProjectFileTool {
            inbox: inbox.clone(),
            store: None,
        };
        let out = tool
            .execute(
                json!({"path": "alice/chat/dir-main/invoice.pdf", "project": "Compta 2026", "subfolder": "invoices"}),
                &ctx("alice"),
            )
            .await
            .unwrap();
        let new_path = "alice/projects/compta-2026/invoices/invoice.pdf";
        assert_eq!(out.data.unwrap()["path"], new_path);
        let root = config.path(&config.documents.watch_dir);
        assert!(root.join(new_path).is_file());
        assert!(!root.join("alice/chat/dir-main/invoice.pdf").exists());

        // A second file with the same name is refused, not overwritten.
        std::fs::write(root.join("alice/chat/dir-main/invoice.pdf"), b"%PDF2").unwrap();
        assert!(
            tool.execute(
                json!({"path": "alice/chat/dir-main/invoice.pdf", "project": "compta-2026", "subfolder": "invoices"}),
                &ctx("alice"),
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read(root.join(new_path)).unwrap(), b"%PDF");

        let list = ProjectListTool { inbox }
            .execute(json!({"project": "compta-2026"}), &ctx("alice"))
            .await
            .unwrap();
        assert!(list.content.contains(new_path), "{}", list.content);
    }

    #[tokio::test]
    async fn another_users_file_is_never_reachable() {
        let (_dir, config) = setup();
        let tool = ProjectFileTool {
            inbox: Inbox {
                config: config.clone(),
            },
            store: None,
        };
        assert!(
            tool.execute(
                json!({"path": "bob/chat/dir-main/theirs.pdf", "project": "x"}),
                &ctx("alice"),
            )
            .await
            .is_err()
        );
        assert!(
            tool.execute(
                json!({"path": "alice/../bob/chat/dir-main/theirs.pdf", "project": "x"}),
                &ctx("alice"),
            )
            .await
            .is_err()
        );
        let root = config.path(&config.documents.watch_dir);
        assert!(root.join("bob/chat/dir-main/theirs.pdf").is_file());
    }
}
