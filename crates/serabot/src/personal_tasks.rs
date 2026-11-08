//! Personal tasks: the Tasks tab's per-user todo list, calendar and Kanban.
//!
//! Every row is UID-scoped — the acting user's id is always passed and there
//! is no unscoped path. Status doubles as the Kanban column: `todo`, `doing`,
//! `done`. `due` is an RFC 3339 timestamp (or a bare date) used by the
//! calendar view. `remote_provider`/`remote_id` link a row to its Google
//! Tasks / Microsoft To Do counterpart (see `task_sync`).

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::domain::new_id;
use crate::error::{Result, SeraError};
use crate::store::{Store, now_string};

pub const STATUS_TODO: &str = "todo";
pub const STATUS_DOING: &str = "doing";
pub const STATUS_DONE: &str = "done";
pub const STATUSES: [&str; 3] = [STATUS_TODO, STATUS_DOING, STATUS_DONE];

pub const PROVIDER_NONE: &str = "none";
pub const PROVIDER_GOOGLE_TASKS: &str = "google_tasks";
pub const PROVIDER_MICROSOFT_TODO: &str = "microsoft_todo";
pub const PROVIDER_GOOGLE_CALENDAR: &str = "google_calendar";
pub const PROVIDER_OUTLOOK_CALENDAR: &str = "outlook_calendar";
/// Providers the user can pick for task sync; the two calendar providers are
/// read-only event sources for the calendar view.
pub const SYNC_PROVIDERS: [&str; 5] = [
    PROVIDER_NONE,
    PROVIDER_GOOGLE_TASKS,
    PROVIDER_MICROSOFT_TODO,
    PROVIDER_GOOGLE_CALENDAR,
    PROVIDER_OUTLOOK_CALENDAR,
];

pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_NOTES_CHARS: usize = 4_000;
pub const MAX_LIST_LIMIT: usize = 500;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonalTask {
    pub id: String,
    pub title: String,
    pub notes: String,
    pub status: String,
    /// RFC 3339 timestamp or `YYYY-MM-DD`, when set.
    pub due: Option<String>,
    pub position: i64,
    pub remote_provider: Option<String>,
    pub remote_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPatch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    /// `null` clears the due date.
    #[serde(default)]
    pub due: Option<Option<String>>,
    #[serde(default)]
    pub position: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskPrefs {
    pub sync_provider: String,
    pub updated_at: String,
}

pub fn validate_title(title: &str) -> Result<String> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        return Err(SeraError::Validation(format!(
            "title must be 1-{MAX_TITLE_CHARS} characters"
        )));
    }
    if title.chars().any(char::is_control) {
        return Err(SeraError::Validation(
            "title must not contain control characters".into(),
        ));
    }
    Ok(title.to_owned())
}

pub fn validate_notes(notes: &str) -> Result<String> {
    if notes.chars().count() > MAX_NOTES_CHARS || notes.chars().any(char::is_control) {
        return Err(SeraError::Validation(format!(
            "notes must be at most {MAX_NOTES_CHARS} characters without control characters"
        )));
    }
    Ok(notes.to_owned())
}

pub fn validate_status(status: &str) -> Result<()> {
    if !STATUSES.contains(&status) {
        return Err(SeraError::Validation(
            "status must be todo, doing or done".into(),
        ));
    }
    Ok(())
}

/// `due` accepts RFC 3339 (`2026-11-08T10:00:00Z`) or a bare date
/// (`2026-11-08`); it is normalized to RFC 3339 UTC. `None` clears it.
pub fn normalize_due(due: &str) -> Result<String> {
    let due = due.trim();
    if due.is_empty() || due.eq_ignore_ascii_case("null") {
        return Err(SeraError::Validation(
            "due must be an RFC 3339 timestamp or date".into(),
        ));
    }
    if let Ok(date) = NaiveDate::parse_from_str(due, "%Y-%m-%d") {
        return Ok(date
            .and_hms_opt(0, 0, 0)
            .expect("midnight is valid")
            .and_utc()
            .to_rfc3339_opts(SecondsFormat::Secs, true));
    }
    let parsed = DateTime::parse_from_rfc3339(due)
        .map_err(|_| SeraError::Validation("due must be an RFC 3339 timestamp or date".into()))?;
    Ok(parsed
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Secs, true))
}

pub fn validate_task_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SeraError::Validation("invalid task id".into()));
    }
    Ok(())
}

pub fn validate_sync_provider(provider: &str) -> Result<()> {
    if !SYNC_PROVIDERS.contains(&provider) {
        return Err(SeraError::Validation(
            "sync_provider must be none, google_tasks, microsoft_todo, google_calendar or outlook_calendar"
                .into(),
        ));
    }
    Ok(())
}

fn map_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<PersonalTask> {
    Ok(PersonalTask {
        id: row.get(0)?,
        title: row.get(1)?,
        notes: row.get(2)?,
        status: row.get(3)?,
        due: row.get(4)?,
        position: row.get(5)?,
        remote_provider: row.get(6)?,
        remote_id: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

const TASK_COLUMNS: &str =
    "id, title, notes, status, due, position, remote_provider, remote_id, created_at, updated_at";

/// The acting user's tasks, newest first. `status` filters; `from`/`to` bound
/// the due range (inclusive) for the calendar view.
pub fn list(
    store: &Store,
    user_id: &str,
    status: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    limit: usize,
) -> Result<Vec<PersonalTask>> {
    if let Some(status) = status {
        validate_status(status)?;
    }
    let mut sql = format!("SELECT {TASK_COLUMNS} FROM user_tasks WHERE user_id = ?1");
    let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(user_id.to_owned())];
    if let Some(status) = status {
        sql.push_str(" AND status = ?2");
        args.push(Box::new(status.to_owned()));
    }
    if let Some(from) = from {
        sql.push_str(" AND due IS NOT NULL AND due >= ?");
        args.push(Box::new(normalize_due(from)?));
    }
    if let Some(to) = to {
        sql.push_str(" AND due IS NOT NULL AND due <= ?");
        args.push(Box::new(normalize_due(to)?));
    }
    sql.push_str(" ORDER BY position ASC, created_at DESC LIMIT ?");
    args.push(Box::new((limit.clamp(1, MAX_LIST_LIMIT)) as i64));
    let connection = store.connection()?;
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(
        rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
        map_task,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The acting user's task by id; another user's task is indistinguishable
/// from a missing one.
pub fn get(store: &Store, user_id: &str, id: &str) -> Result<Option<PersonalTask>> {
    validate_task_id(id)?;
    let connection = store.connection()?;
    connection
        .query_row(
            &format!("SELECT {TASK_COLUMNS} FROM user_tasks WHERE id = ?1 AND user_id = ?2"),
            params![id, user_id],
            map_task,
        )
        .optional()
        .map_err(SeraError::from)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewTask {
    pub title: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub due: Option<String>,
}

pub fn create(store: &Store, user_id: &str, request: &NewTask) -> Result<PersonalTask> {
    let title = validate_title(&request.title)?;
    let notes = validate_notes(request.notes.as_deref().unwrap_or(""))?;
    let status = match &request.status {
        Some(status) => {
            validate_status(status)?;
            status.clone()
        }
        None => STATUS_TODO.to_owned(),
    };
    let due = request
        .due
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(normalize_due)
        .transpose()?;
    let id = new_id().to_string();
    let now = now_string();
    {
        // Scoped: the in-memory test pool has one connection, and `get` below
        // needs it back.
        let connection = store.connection()?;
        connection.execute(
            "INSERT INTO user_tasks(id, user_id, title, notes, status, due, position,
                                     created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?7)",
            params![id, user_id, title, notes, status, due, now],
        )?;
    }
    get(store, user_id, &id)?.ok_or_else(|| SeraError::Internal("task insert lost".into()))
}

/// Patch one of the acting user's tasks. Absent fields are unchanged.
pub fn update(
    store: &Store,
    user_id: &str,
    id: &str,
    patch: &TaskPatch,
) -> Result<Option<PersonalTask>> {
    validate_task_id(id)?;
    if let Some(title) = &patch.title {
        validate_title(title)?;
    }
    if let Some(notes) = &patch.notes {
        validate_notes(notes)?;
    }
    if let Some(status) = &patch.status {
        validate_status(status)?;
    }
    if let Some(Some(due)) = &patch.due {
        normalize_due(due)?;
    }
    let now = now_string();
    let mut sets = Vec::new();
    let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(title) = &patch.title {
        sets.push("title = ?");
        args.push(Box::new(validate_title(title)?));
    }
    if let Some(notes) = &patch.notes {
        sets.push("notes = ?");
        args.push(Box::new(validate_notes(notes)?));
    }
    if let Some(status) = &patch.status {
        sets.push("status = ?");
        args.push(Box::new(status.clone()));
    }
    if let Some(due) = &patch.due {
        sets.push("due = ?");
        args.push(Box::new(match due {
            Some(due) => Some(normalize_due(due)?),
            None => None,
        }));
    }
    if let Some(position) = patch.position {
        sets.push("position = ?");
        args.push(Box::new(position.clamp(0, 1_000_000)));
    }
    if sets.is_empty() {
        return get(store, user_id, id);
    }
    sets.push("updated_at = ?");
    args.push(Box::new(now));
    let sql = format!(
        "UPDATE user_tasks SET {} WHERE id = ? AND user_id = ?",
        sets.join(", ")
    );
    args.push(Box::new(id.to_owned()));
    args.push(Box::new(user_id.to_owned()));
    let changed = {
        let connection = store.connection()?;
        connection.execute(
            &sql,
            rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
        )?
    };
    if changed == 0 {
        return Ok(None);
    }
    get(store, user_id, id)
}

/// Mark a remote-linked row so sync can find it by its provider id.
pub fn link_remote(
    store: &Store,
    user_id: &str,
    id: &str,
    provider: &str,
    remote_id: &str,
) -> Result<()> {
    let connection = store.connection()?;
    connection.execute(
        "UPDATE user_tasks SET remote_provider = ?1, remote_id = ?2, updated_at = ?3
         WHERE id = ?4 AND user_id = ?5",
        params![provider, remote_id, now_string(), id, user_id],
    )?;
    Ok(())
}

/// Find the acting user's row linked to a remote task, if any.
pub fn find_remote(
    store: &Store,
    user_id: &str,
    provider: &str,
    remote_id: &str,
) -> Result<Option<PersonalTask>> {
    let connection = store.connection()?;
    connection
        .query_row(
            &format!(
                "SELECT {TASK_COLUMNS} FROM user_tasks
                 WHERE user_id = ?1 AND remote_provider = ?2 AND remote_id = ?3"
            ),
            params![user_id, provider, remote_id],
            map_task,
        )
        .optional()
        .map_err(SeraError::from)
}

pub fn delete(store: &Store, user_id: &str, id: &str) -> Result<bool> {
    validate_task_id(id)?;
    let connection = store.connection()?;
    Ok(connection.execute(
        "DELETE FROM user_tasks WHERE id = ?1 AND user_id = ?2",
        params![id, user_id],
    )? > 0)
}

pub fn get_prefs(store: &Store, user_id: &str) -> Result<TaskPrefs> {
    let connection = store.connection()?;
    connection
        .query_row(
            "SELECT sync_provider, updated_at FROM user_task_prefs WHERE user_id = ?1",
            params![user_id],
            |row| {
                Ok(TaskPrefs {
                    sync_provider: row.get(0)?,
                    updated_at: row.get(1)?,
                })
            },
        )
        .optional()?
        .map(Ok)
        .unwrap_or_else(|| {
            Ok(TaskPrefs {
                sync_provider: PROVIDER_NONE.to_owned(),
                updated_at: now_string(),
            })
        })
}

pub fn set_prefs(store: &Store, user_id: &str, sync_provider: &str) -> Result<TaskPrefs> {
    validate_sync_provider(sync_provider)?;
    let now = now_string();
    {
        let connection = store.connection()?;
        connection.execute(
            "INSERT INTO user_task_prefs(user_id, sync_provider, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(user_id) DO UPDATE SET sync_provider = excluded.sync_provider,
                                                 updated_at = excluded.updated_at",
            params![user_id, sync_provider, now],
        )?;
    }
    get_prefs(store, user_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn store() -> Store {
        let store = Store::open_in_memory(384).expect("in-memory store");
        let connection = store.connection().unwrap();
        for id in ["u1", "u2"] {
            connection
                .execute(
                    "INSERT INTO users(id, provider_subject, email, display_name, role, status, created_at)
                     VALUES (?1, ?2, ?3, ?4, 'member', 'active', ?5)",
                    rusqlite::params![
                        id,
                        format!("microsoft:{id}"),
                        format!("{id}@example.test"),
                        id.to_uppercase(),
                        now_string()
                    ],
                )
                .unwrap();
        }
        store
    }

    #[test]
    fn tasks_are_scoped_to_their_user() {
        let store = store();
        let task = create(
            &store,
            "u1",
            &NewTask {
                title: "Buy milk".into(),
                notes: None,
                status: None,
                due: Some("2026-11-09".into()),
            },
        )
        .unwrap();
        assert_eq!(task.status, "todo");
        assert_eq!(task.due.as_deref(), Some("2026-11-09T00:00:00Z"));
        // Another user cannot see, patch or delete it.
        assert!(get(&store, "u2", &task.id).unwrap().is_none());
        assert!(
            update(
                &store,
                "u2",
                &task.id,
                &TaskPatch {
                    status: Some("done".into()),
                    ..Default::default()
                }
            )
            .unwrap()
            .is_none()
        );
        assert!(!delete(&store, "u2", &task.id).unwrap());
        assert_eq!(list(&store, "u2", None, None, None, 100).unwrap().len(), 0);
        assert_eq!(list(&store, "u1", None, None, None, 100).unwrap().len(), 1);
    }

    #[test]
    fn patch_updates_only_given_fields_and_normalizes_due() {
        let store = store();
        let task = create(
            &store,
            "u1",
            &NewTask {
                title: "Ship it".into(),
                notes: Some("notes".into()),
                status: None,
                due: None,
            },
        )
        .unwrap();
        let patched = update(
            &store,
            "u1",
            &task.id,
            &TaskPatch {
                status: Some("doing".into()),
                due: Some(Some("2026-11-08T10:30:00+02:00".into())),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(patched.status, "doing");
        assert_eq!(patched.due.as_deref(), Some("2026-11-08T08:30:00Z"));
        assert_eq!(patched.title, "Ship it");
        assert_eq!(patched.notes, "notes");
        // Clearing the due date.
        let cleared = update(
            &store,
            "u1",
            &task.id,
            &TaskPatch {
                due: Some(None),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(cleared.due, None);
    }

    #[test]
    fn invalid_input_is_rejected() {
        let store = store();
        for bad in ["", &"x".repeat(201)] {
            assert!(
                create(
                    &store,
                    "u1",
                    &NewTask {
                        title: bad.into(),
                        notes: None,
                        status: None,
                        due: None,
                    }
                )
                .is_err()
            );
        }
        assert!(
            create(
                &store,
                "u1",
                &NewTask {
                    title: "ok".into(),
                    notes: None,
                    status: Some("archived".into()),
                    due: None,
                }
            )
            .is_err()
        );
        assert!(
            create(
                &store,
                "u1",
                &NewTask {
                    title: "ok".into(),
                    notes: None,
                    status: None,
                    due: Some("next friday".into()),
                }
            )
            .is_err()
        );
        assert!(validate_sync_provider("icloud").is_err());
        assert!(validate_sync_provider("google_tasks").is_ok());
        assert!(validate_task_id("../etc").is_err());
    }

    #[test]
    fn list_filters_by_status_and_due_range() {
        let store = store();
        let a = create(
            &store,
            "u1",
            &NewTask {
                title: "a".into(),
                notes: None,
                status: None,
                due: Some("2026-11-08".into()),
            },
        )
        .unwrap();
        create(
            &store,
            "u1",
            &NewTask {
                title: "b".into(),
                notes: None,
                status: Some("done".into()),
                due: Some("2026-12-01".into()),
            },
        )
        .unwrap();
        assert_eq!(
            list(&store, "u1", Some("done"), None, None, 100)
                .unwrap()
                .len(),
            1
        );
        let in_range = list(
            &store,
            "u1",
            None,
            Some("2026-11-01"),
            Some("2026-11-30"),
            100,
        )
        .unwrap();
        assert_eq!(in_range.len(), 1);
        assert_eq!(in_range[0].id, a.id);
    }

    #[test]
    fn remote_links_are_unique_per_provider() {
        let store = store();
        let task = create(
            &store,
            "u1",
            &NewTask {
                title: "synced".into(),
                notes: None,
                status: None,
                due: None,
            },
        )
        .unwrap();
        link_remote(&store, "u1", &task.id, "google_tasks", "remote-1").unwrap();
        let found = find_remote(&store, "u1", "google_tasks", "remote-1")
            .unwrap()
            .unwrap();
        assert_eq!(found.id, task.id);
        assert!(
            find_remote(&store, "u2", "google_tasks", "remote-1")
                .unwrap()
                .is_none()
        );
        assert!(
            find_remote(&store, "u1", "microsoft_todo", "remote-1")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn prefs_default_to_none_and_update() {
        let store = store();
        assert_eq!(get_prefs(&store, "u1").unwrap().sync_provider, "none");
        let prefs = set_prefs(&store, "u1", "google_tasks").unwrap();
        assert_eq!(prefs.sync_provider, "google_tasks");
        assert_eq!(
            get_prefs(&store, "u1").unwrap().sync_provider,
            "google_tasks"
        );
        assert_eq!(get_prefs(&store, "u2").unwrap().sync_provider, "none");
    }
}
