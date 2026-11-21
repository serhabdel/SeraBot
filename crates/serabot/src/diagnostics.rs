//! Project check harness (0.73): run an owner-configured check command inside
//! one of the user's own project folders and store the diagnostics it emits, so
//! the desktop can show a real Problems panel (errors with file/line/column)
//! instead of asking the model to read compiler output.
//!
//! Safety rules, all enforced here:
//! - the command is set only through the acting user's own API route, never by
//!   a model: argv[0] must be a bare name on the allowlist (`cargo`, `rustc`),
//!   no absolute paths, no `..` segments, bounded argument count and length;
//! - the process runs with the project folder as cwd, never with a shell, and
//!   the project folder is resolved with symlink + canonicalize containment
//!   inside the user's projects directory;
//! - output is size-capped and the run has a hard timeout;
//! - every run is audit-logged; the bot tool `project_check` is approval-gated
//!   (DESTRUCTIVE_TOOLS) so a model can only ask, never run silently.
//!
//! Tests use synthetic rustc JSON fixtures; nothing runs a real compiler here.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::Config;
use crate::domain::{new_id, utc_now};
use crate::error::{Result, SeraError};
use crate::projects::slugify;
use crate::store::Store;

/// Programs a project check command may run. Bare names only: resolved through
/// PATH by the child process, never a path.
pub const ALLOWED_PROGRAMS: &[&str] = &["cargo", "rustc"];
/// Default check for a Rust project; other projects get their command set
/// through `PUT /v1/projects/{slug}/check`.
pub const DEFAULT_COMMAND: &[&str] = &["cargo", "check", "--message-format=json"];
/// Hard limits for one check run.
pub const MAX_ARGS: usize = 8;
pub const MAX_ARG_LEN: usize = 256;
pub const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
pub const RUN_TIMEOUT: Duration = Duration::from_secs(120);
pub const MAX_DIAGNOSTICS: usize = 500;

/// One compiler diagnostic, normalised from rustc JSON (or a synthetic row for
/// commands that do not emit JSON).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnostic {
    pub file: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub severity: String,
    pub code: Option<String>,
    pub message: String,
}

/// Validate an owner-supplied check command. Rejects anything that could
/// escape the project folder or run an arbitrary program.
pub fn validate_command(argv: &[String]) -> Result<()> {
    if argv.is_empty() || argv.len() > MAX_ARGS {
        return Err(SeraError::Validation(
            "the check command needs 1 to 8 arguments".into(),
        ));
    }
    let program = &argv[0];
    if program.contains('/') || program.contains('\\') || program.contains("..") {
        return Err(SeraError::Validation(
            "the check program must be a bare name (cargo or rustc), not a path".into(),
        ));
    }
    if !ALLOWED_PROGRAMS.contains(&program.as_str()) {
        return Err(SeraError::Validation(format!(
            "the check program must be one of: {}",
            ALLOWED_PROGRAMS.join(", ")
        )));
    }
    for arg in argv.iter().skip(1) {
        if arg.chars().count() > MAX_ARG_LEN || arg.chars().any(|c| c.is_control()) {
            return Err(SeraError::Validation(
                "a check argument is too long or not plain text".into(),
            ));
        }
        let trimmed = arg.trim();
        let windows_drive = trimmed
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic())
            && trimmed.as_bytes().get(1) == Some(&b':');
        if trimmed.starts_with('/')
            || trimmed.starts_with('\\')
            || windows_drive
            || trimmed.split(['/', '\\']).any(|part| part == "..")
        {
            return Err(SeraError::Validation(
                "check arguments must stay inside the project folder (no absolute paths)".into(),
            ));
        }
    }
    Ok(())
}

/// Parse `cargo check --message-format=json` stdout into diagnostics. Only
/// `compiler-message` reasons are read; the primary span (or the first span)
/// gives file/line/column. Anything else on the line is ignored, so a
/// malformed line never fails the whole run.
pub fn parse_rustc_json(stdout: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("reason").and_then(Value::as_str) != Some("compiler-message") {
            continue;
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        let text = message
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned();
        if text.is_empty() {
            continue;
        }
        let level = message
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or("note");
        let severity = match level {
            "error" | "failure-note" => "error",
            "warning" => "warning",
            _ => "note",
        };
        let code = message
            .get("code")
            .and_then(|c| c.get("code"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let span = message
            .get("spans")
            .and_then(Value::as_array)
            .and_then(|spans| {
                spans
                    .iter()
                    .find(|s| s.get("is_primary").and_then(Value::as_bool) == Some(true))
                    .or_else(|| spans.first())
            });
        let (file, line, column) = match span {
            Some(span) => (
                span.get("file_name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                span.get("line_start")
                    .and_then(Value::as_u64)
                    .map(|n| n as u32),
                span.get("column_start")
                    .and_then(Value::as_u64)
                    .map(|n| n as u32),
            ),
            None => (String::new(), None, None),
        };
        out.push(Diagnostic {
            file,
            line,
            column,
            severity: severity.to_owned(),
            code,
            message: text,
        });
        if out.len() >= MAX_DIAGNOSTICS {
            break;
        }
    }
    out
}

/// The project folder for `(user, project)`, resolved with containment inside
/// the user's projects directory.
fn project_dir(config: &Config, user_id: &str, project: &str) -> Result<PathBuf> {
    let slug = slugify(project)?;
    if !crate::user_inbox::safe_user_segment(user_id) {
        return Err(SeraError::Forbidden(
            "the account id is not a safe inbox directory name".into(),
        ));
    }
    let root = config.path(&config.documents.watch_dir);
    let projects = root.join(user_id).join("projects");
    std::fs::create_dir_all(&projects)?;
    let projects_canon = projects.canonicalize()?;
    let dir = projects.join(&slug);
    if !dir.is_dir() {
        return Err(SeraError::NotFound(format!("no project named {slug}")));
    }
    let canon = dir.canonicalize()?;
    if !canon.starts_with(&projects_canon) {
        return Err(SeraError::Forbidden(
            "the project folder is outside the projects directory".into(),
        ));
    }
    Ok(canon)
}

#[derive(Serialize)]
/// How one check run ended, written to `project_check_runs`.
pub struct CheckOutcome<'a> {
    pub status: &'a str,
    pub errors: u32,
    pub warnings: u32,
    pub notes: u32,
    pub error: Option<&'a str>,
}

#[derive(Serialize)]
pub struct CheckSummary {
    pub run_id: String,
    pub project: String,
    pub errors: u32,
    pub warnings: u32,
    pub notes: u32,
    pub duration_ms: u64,
    pub command: Vec<String>,
}

/// Run the project's check command, parse its output, and store the run plus
/// its diagnostics (replacing the project's previous rows).
pub async fn run_check(
    config: &Arc<Config>,
    store: &Store,
    user_id: &str,
    project: &str,
) -> Result<CheckSummary> {
    let slug = slugify(project)?;
    let dir = project_dir(config, user_id, &slug)?;
    let argv: Vec<String> = store
        .get_project_check(user_id, &slug)?
        .map(|stored| {
            serde_json::from_str::<Vec<String>>(&stored)
                .unwrap_or_else(|_| DEFAULT_COMMAND.iter().map(|s| s.to_string()).collect())
        })
        .unwrap_or_else(|| DEFAULT_COMMAND.iter().map(|s| s.to_string()).collect());
    validate_command(&argv)?;

    let run_id = new_id().to_string();
    let started = std::time::Instant::now();
    store.insert_check_run(&run_id, user_id, &slug)?;

    let result = run_process(&argv, &dir).await;
    let duration_ms = started.elapsed().as_millis() as u64;
    let (diagnostics, run_error) = match result {
        Ok((code, stdout, stderr)) => {
            let mut diags = parse_rustc_json(&stdout);
            if diags.is_empty() && code != 0 {
                let tail = tail_lines(&format!("{stdout}\n{stderr}"), 20);
                diags.push(Diagnostic {
                    file: String::new(),
                    line: None,
                    column: None,
                    severity: "error".into(),
                    code: None,
                    message: format!("check command exited with code {code}: {tail}"),
                });
            }
            (diags, None)
        }
        Err(error) => {
            let message = error.to_string();
            (
                vec![Diagnostic {
                    file: String::new(),
                    line: None,
                    column: None,
                    severity: "error".into(),
                    code: None,
                    message: message.clone(),
                }],
                Some(message),
            )
        }
    };
    let errors = diagnostics.iter().filter(|d| d.severity == "error").count() as u32;
    let warnings = diagnostics
        .iter()
        .filter(|d| d.severity == "warning")
        .count() as u32;
    let notes = diagnostics.iter().filter(|d| d.severity == "note").count() as u32;
    store.replace_project_diagnostics(user_id, &slug, &run_id, &diagnostics)?;
    store.finish_check_run(
        &run_id,
        user_id,
        &CheckOutcome {
            status: if run_error.is_some() {
                "failed"
            } else {
                "done"
            },
            errors,
            warnings,
            notes,
            error: run_error.as_deref(),
        },
    )?;
    store.record_audit(
        user_id,
        "project.check",
        Some("project_check"),
        None,
        None,
        None,
        None,
        Some(&json!({
            "project": slug,
            "run_id": run_id,
            "errors": errors,
            "warnings": warnings,
            "notes": notes,
            "duration_ms": duration_ms,
        })),
    )?;
    Ok(CheckSummary {
        run_id,
        project: slug,
        errors,
        warnings,
        notes,
        duration_ms,
        command: argv,
    })
}

/// Spawn the command with the project dir as cwd, no shell, capped output and a
/// hard timeout. `kill_on_drop` makes the timeout kill the child.
async fn run_process(argv: &[String], dir: &std::path::Path) -> Result<(i32, String, String)> {
    use tokio::process::Command;
    let child = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| SeraError::External(format!("could not start {}: {e}", argv[0])))?;
    let wait = tokio::time::timeout(RUN_TIMEOUT, child.wait_with_output()).await;
    match wait {
        Ok(Ok(output)) => {
            let code = output.status.code().unwrap_or(-1);
            let stdout = cap(&String::from_utf8_lossy(&output.stdout));
            let stderr = cap(&String::from_utf8_lossy(&output.stderr));
            Ok((code, stdout, stderr))
        }
        Ok(Err(error)) => Err(SeraError::External(format!(
            "the check command failed to run: {error}"
        ))),
        Err(_) => Err(SeraError::External(format!(
            "the check command timed out after {}s",
            RUN_TIMEOUT.as_secs()
        ))),
    }
}

fn cap(text: &str) -> String {
    if text.len() <= MAX_OUTPUT_BYTES {
        text.to_owned()
    } else {
        text.chars().take(MAX_OUTPUT_BYTES).collect()
    }
}

fn tail_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(max);
    lines[start..].join("\n").chars().take(2000).collect()
}

/// Store side of the harness. Every method takes the acting user's id; there
/// is no unscoped path to these rows.
impl Store {
    pub fn set_project_check(&self, user_id: &str, project: &str, argv: &[String]) -> Result<()> {
        let command = serde_json::to_string(argv)
            .map_err(|e| SeraError::Internal(format!("command serialise: {e}")))?;
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO project_checks (user_id, project, command, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id, project) DO UPDATE SET
                command = excluded.command, updated_at = excluded.updated_at",
            rusqlite::params![user_id, project, command, utc_now().to_string()],
        )?;
        Ok(())
    }

    pub fn get_project_check(&self, user_id: &str, project: &str) -> Result<Option<String>> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT command FROM project_checks WHERE user_id = ?1 AND project = ?2",
            rusqlite::params![user_id, project],
            |row| row.get(0),
        )
        .optional()
        .map_err(SeraError::from)
    }

    pub fn insert_check_run(&self, run_id: &str, user_id: &str, project: &str) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO project_check_runs (id, user_id, project, status, started_at)
             VALUES (?1, ?2, ?3, 'running', ?4)",
            rusqlite::params![run_id, user_id, project, utc_now().to_string()],
        )?;
        Ok(())
    }

    pub fn finish_check_run(
        &self,
        run_id: &str,
        user_id: &str,
        outcome: &CheckOutcome,
    ) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "UPDATE project_check_runs
             SET status = ?3, finished_at = ?4, errors = ?5, warnings = ?6, notes = ?7, error = ?8
             WHERE id = ?1 AND user_id = ?2",
            rusqlite::params![
                run_id,
                user_id,
                outcome.status,
                utc_now().to_string(),
                outcome.errors,
                outcome.warnings,
                outcome.notes,
                outcome.error
            ],
        )?;
        Ok(())
    }

    pub fn replace_project_diagnostics(
        &self,
        user_id: &str,
        project: &str,
        run_id: &str,
        diagnostics: &[Diagnostic],
    ) -> Result<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM project_diagnostics WHERE user_id = ?1 AND project = ?2",
            rusqlite::params![user_id, project],
        )?;
        for diag in diagnostics {
            tx.execute(
                "INSERT INTO project_diagnostics
                    (id, user_id, project, run_id, file, line, column, severity, code, message, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    new_id(),
                    user_id,
                    project,
                    run_id,
                    diag.file,
                    diag.line,
                    diag.column,
                    diag.severity,
                    diag.code,
                    diag.message,
                    utc_now().to_string()
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The project's latest stored diagnostics plus the run that produced them.
    pub fn list_project_diagnostics(
        &self,
        user_id: &str,
        project: &str,
    ) -> Result<(Option<Value>, Vec<Diagnostic>)> {
        let conn = self.connection()?;
        let run = conn
            .query_row(
                "SELECT id, status, started_at, finished_at, errors, warnings, notes, error
                 FROM project_check_runs
                 WHERE user_id = ?1 AND project = ?2
                 ORDER BY started_at DESC, id DESC LIMIT 1",
                rusqlite::params![user_id, project],
                |row| {
                    Ok(json!({
                        "run_id": row.get::<_, String>(0)?,
                        "status": row.get::<_, String>(1)?,
                        "started_at": row.get::<_, String>(2)?,
                        "finished_at": row.get::<_, Option<String>>(3)?,
                        "errors": row.get::<_, u32>(4)?,
                        "warnings": row.get::<_, u32>(5)?,
                        "notes": row.get::<_, u32>(6)?,
                        "error": row.get::<_, Option<String>>(7)?,
                    }))
                },
            )
            .optional()?;
        let mut stmt = conn.prepare(
            "SELECT file, line, column, severity, code, message
             FROM project_diagnostics
             WHERE user_id = ?1 AND project = ?2
             ORDER BY severity DESC, file, line, column",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![user_id, project], |row| {
                Ok(Diagnostic {
                    file: row.get(0)?,
                    line: row.get(1)?,
                    column: row.get(2)?,
                    severity: row.get(3)?,
                    code: row.get(4)?,
                    message: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok((run, rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"reason":"compiler-artifact","package_id":"x","target":{"name":"x"}}
{"reason":"compiler-message","message":{"level":"error","message":"mismatched types","code":{"code":"E0308","explanation":"..."},"spans":[{"file_name":"src/main.rs","line_start":3,"column_start":9,"is_primary":true}],"children":[]}}
{"reason":"compiler-message","message":{"level":"warning","message":"unused variable: `x`","code":null,"spans":[{"file_name":"src/lib.rs","line_start":7,"column_start":5,"is_primary":true}]}}
{"reason":"build-finished","success":false}
not json at all
"#;

    #[test]
    fn parses_rustc_json_into_diagnostics() {
        let diags = parse_rustc_json(SAMPLE);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].severity, "error");
        assert_eq!(diags[0].file, "src/main.rs");
        assert_eq!(diags[0].line, Some(3));
        assert_eq!(diags[0].column, Some(9));
        assert_eq!(diags[0].code.as_deref(), Some("E0308"));
        assert_eq!(diags[0].message, "mismatched types");
        assert_eq!(diags[1].severity, "warning");
        assert_eq!(diags[1].code, None);
    }

    #[test]
    fn command_validation_blocks_paths_and_strangers() {
        assert!(validate_command(&["cargo".into(), "check".into()]).is_ok());
        assert!(validate_command(&["/bin/sh".into(), "-c".into()]).is_err());
        assert!(
            validate_command(&["cargo".into(), "--manifest-path".into(), "/etc/x".into()]).is_err()
        );
        assert!(validate_command(&["cargo".into(), "../outside".into()]).is_err());
        assert!(validate_command(&["node".into()]).is_err());
        assert!(validate_command(&[]).is_err());
        assert!(validate_command(&vec!["cargo".into(); MAX_ARGS + 1]).is_err());
    }

    #[test]
    fn store_roundtrip_scoped_per_user() {
        let store = Store::open_in_memory(384).unwrap();
        store
            .set_project_check("u1", "demo", &["cargo".into(), "check".into()])
            .unwrap();
        assert_eq!(
            store.get_project_check("u1", "demo").unwrap().as_deref(),
            Some(r#"["cargo","check"]"#)
        );
        assert!(store.get_project_check("u2", "demo").unwrap().is_none());
        let diags = vec![Diagnostic {
            file: "src/main.rs".into(),
            line: Some(1),
            column: Some(2),
            severity: "error".into(),
            code: Some("E0308".into()),
            message: "boom".into(),
        }];
        store.insert_check_run("r1", "u1", "demo").unwrap();
        store
            .replace_project_diagnostics("u1", "demo", "r1", &diags)
            .unwrap();
        store
            .finish_check_run(
                "r1",
                "u1",
                &CheckOutcome {
                    status: "done",
                    errors: 1,
                    warnings: 0,
                    notes: 0,
                    error: None,
                },
            )
            .unwrap();
        let (run, rows) = store.list_project_diagnostics("u1", "demo").unwrap();
        assert_eq!(rows, diags);
        assert_eq!(run.unwrap()["errors"], json!(1));
        let (_, other) = store.list_project_diagnostics("u2", "demo").unwrap();
        assert!(other.is_empty());
    }
}
