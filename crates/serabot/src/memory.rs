//! Reviewable agent memory.
//!
//! Memories are operator-reviewable facts with mandatory provenance. This
//! module provides CRUD only. The runtime separately injects the 20 most
//! recently updated memories into the orchestrator system prompt as
//! owner-approved facts, and `delegate` handoffs attach explicitly selected
//! ids. Specialists never list memory on their own.

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::domain::new_id;
use crate::error::{Result, SeraError};
use crate::store::Store;

/// Allowed memory kinds. Anything else is rejected so the review surface stays
/// predictable.
pub const MEMORY_KINDS: &[&str] = &["preference", "fact", "decision", "context", "note"];
pub const MAX_CONTENT_CHARS: usize = 4_000;
pub const MAX_SOURCE_CHARS: usize = 512;
pub const MAX_LIST_LIMIT: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Memory {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub category: String,
    pub content: String,
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Add a reviewable fact. `source` is required provenance (for example a
/// conversation id, document id or operator note).
pub fn add(store: &Store, kind: &str, content: &str, source: &str) -> Result<Memory> {
    let kind = normalize_kind(kind)?;
    let content = normalized_content(content)?;
    let source = normalized_source(source)?;
    let now = parse_datetime(&now_string(Utc::now()));
    let memory = Memory {
        id: new_id(),
        category: kind.to_owned(),
        kind: kind.to_owned(),
        content: content.to_owned(),
        source: source.to_owned(),
        created_at: now,
        updated_at: now,
    };

    let connection = store.connection()?;
    connection.execute(
        "INSERT INTO memories(id, kind, content, source, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            memory.id,
            memory.kind,
            memory.content,
            memory.source,
            now_string(memory.created_at),
            now_string(memory.updated_at),
        ],
    )?;
    Ok(memory)
}

pub fn get(store: &Store, id: &str) -> Result<Memory> {
    let connection = store.connection()?;
    connection
        .query_row(
            "SELECT id, kind, content, source, created_at, updated_at
             FROM memories WHERE id = ?1",
            params![id],
            row_to_memory,
        )
        .optional()?
        .ok_or_else(|| SeraError::NotFound(format!("memory {id}")))
}

pub fn list(store: &Store, limit: usize) -> Result<Vec<Memory>> {
    let limit = limit.clamp(1, MAX_LIST_LIMIT);
    let connection = store.connection()?;
    let mut statement = connection.prepare(
        "SELECT id, kind, content, source, created_at, updated_at
         FROM memories ORDER BY updated_at DESC, id DESC LIMIT ?1",
    )?;
    let rows = statement.query_map(params![limit as i64], row_to_memory)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Update the content of one memory, preserving kind, source and creation time.
pub fn update_content(store: &Store, id: &str, content: &str) -> Result<Memory> {
    let content = normalized_content(content)?;
    let connection = store.connection()?;
    let changed = connection.execute(
        "UPDATE memories SET content = ?1, updated_at = ?2 WHERE id = ?3",
        params![content, now_string(Utc::now()), id],
    )?;
    if changed != 1 {
        return Err(SeraError::NotFound(format!("memory {id}")));
    }
    drop(connection);
    get(store, id)
}

/// Delete exactly one memory. Returns `false` when the id does not exist.
pub fn delete(store: &Store, id: &str) -> Result<bool> {
    let connection = store.connection()?;
    let changed = connection.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
    Ok(changed == 1)
}

fn row_to_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    let kind: String = row.get(1)?;
    Ok(Memory {
        id: row.get(0)?,
        category: kind.clone(),
        kind,
        content: row.get(2)?,
        source: row.get(3)?,
        created_at: parse_datetime(&row.get::<_, String>(4)?),
        updated_at: parse_datetime(&row.get::<_, String>(5)?),
    })
}

fn normalize_kind(kind: &str) -> Result<&'static str> {
    let normalized = kind.trim().to_ascii_lowercase();
    MEMORY_KINDS
        .iter()
        .copied()
        .find(|candidate| *candidate == normalized.as_str())
        .ok_or_else(|| {
            SeraError::Validation(format!(
                "memory kind must be one of: {}",
                MEMORY_KINDS.join(", ")
            ))
        })
}

fn normalized_content(content: &str) -> Result<&str> {
    let content = content.trim();
    if content.is_empty() {
        return Err(SeraError::Validation(
            "memory content must not be empty".into(),
        ));
    }
    let length = content.chars().count();
    if length > MAX_CONTENT_CHARS {
        return Err(SeraError::Validation(format!(
            "memory content has {length} characters; maximum is {MAX_CONTENT_CHARS}"
        )));
    }
    Ok(content)
}

fn normalized_source(source: &str) -> Result<&str> {
    let source = source.trim();
    if source.is_empty() {
        return Err(SeraError::Validation(
            "memory source is required for provenance".into(),
        ));
    }
    let length = source.chars().count();
    if length > MAX_SOURCE_CHARS {
        return Err(SeraError::Validation(format!(
            "memory source has {length} characters; maximum is {MAX_SOURCE_CHARS}"
        )));
    }
    Ok(source)
}

fn now_string(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_datetime(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory(384).expect("store")
    }

    #[test]
    fn crud_round_trip_and_isolation() {
        let store = store();
        let first = add(
            &store,
            "preference",
            "Prefers concise Italian summaries",
            "operator:test",
        )
        .expect("first memory");
        let second = add(&store, "fact", "Lives in Rome", "document:doc-a").expect("second memory");
        assert_eq!(first.kind, "preference");
        assert_eq!(second.source, "document:doc-a");

        let fetched = get(&store, &first.id).expect("get");
        assert_eq!(fetched, first);

        let listed = list(&store, 10).expect("list");
        assert_eq!(listed.len(), 2);

        let updated =
            update_content(&store, &first.id, "Prefers very concise summaries").expect("update");
        assert_eq!(updated.id, first.id);
        assert_eq!(updated.kind, first.kind);
        assert_eq!(updated.source, first.source);
        assert_eq!(updated.created_at, first.created_at);
        assert!(updated.updated_at >= first.updated_at);

        // Updating one memory must not touch the other.
        let untouched = get(&store, &second.id).expect("second get");
        assert_eq!(untouched.content, "Lives in Rome");

        assert!(delete(&store, &first.id).expect("delete"));
        assert!(!delete(&store, &first.id).expect("idempotent delete"));
        assert!(get(&store, &first.id).is_err());
        assert_eq!(
            get(&store, &second.id).expect("second stays").content,
            "Lives in Rome"
        );
    }

    #[test]
    fn rejects_unknown_kind_empty_content_and_missing_provenance() {
        let store = store();
        assert!(add(&store, "guess", "content", "operator:test").is_err());
        assert!(add(&store, "fact", "   ", "operator:test").is_err());
        assert!(add(&store, "fact", "content", "  ").is_err());
        assert!(
            add(
                &store,
                "fact",
                &"x".repeat(MAX_CONTENT_CHARS + 1),
                "operator:test"
            )
            .is_err()
        );
        assert!(list(&store, 10).expect("list").is_empty());
    }

    #[test]
    fn update_missing_memory_is_not_found() {
        let store = store();
        assert!(update_content(&store, "missing", "content").is_err());
    }
}
