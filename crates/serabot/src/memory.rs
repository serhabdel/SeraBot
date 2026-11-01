//! Reviewable agent memory.
//!
//! Memories are operator-reviewable facts with mandatory provenance, and
//! **every row belongs to one user**: each function takes the acting user's
//! id and scopes the SQL to it, so one user's memories are unreachable from
//! another user's scope (phase 2 of the multi-user reconstruction). This
//! module provides CRUD only. The runtime separately injects the 20 most
//! recently updated memories of the acting user into the orchestrator system
//! prompt as owner-approved facts, and `delegate` handoffs attach explicitly
//! selected ids. Specialists never list memory on their own.

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

/// Add a reviewable fact for the acting user. `source` is required
/// provenance (for example a conversation id, document id or operator note).
pub fn add(
    store: &Store,
    user_id: &str,
    kind: &str,
    content: &str,
    source: &str,
) -> Result<Memory> {
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
        "INSERT INTO memories(id, kind, content, source, created_at, updated_at, user_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            memory.id,
            memory.kind,
            memory.content,
            memory.source,
            now_string(memory.created_at),
            now_string(memory.updated_at),
            user_id,
        ],
    )?;
    Ok(memory)
}

/// The acting user's memory by id; another user's memory is
/// indistinguishable from a missing one.
pub fn get(store: &Store, user_id: &str, id: &str) -> Result<Memory> {
    let connection = store.connection()?;
    connection
        .query_row(
            "SELECT id, kind, content, source, created_at, updated_at
             FROM memories WHERE id = ?1 AND user_id = ?2",
            params![id, user_id],
            row_to_memory,
        )
        .optional()?
        .ok_or_else(|| SeraError::NotFound(format!("memory {id}")))
}

/// List the acting user's memories, newest first, bounded.
///
/// Rows are always filtered by `user_id`; there is no unscoped list, and
/// an unknown user yields an empty list, never another user's rows.
pub fn list(store: &Store, user_id: &str, limit: usize) -> Result<Vec<Memory>> {
    let limit = limit.clamp(1, MAX_LIST_LIMIT);
    let connection = store.connection()?;
    let mut statement = connection.prepare(
        "SELECT id, kind, content, source, created_at, updated_at
         FROM memories WHERE user_id = ?1
         ORDER BY updated_at DESC, id DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![user_id, limit as i64], row_to_memory)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Update the content of one memory of the acting user, preserving kind,
/// source and creation time.
pub fn update_content(store: &Store, user_id: &str, id: &str, content: &str) -> Result<Memory> {
    let content = normalized_content(content)?;
    let connection = store.connection()?;
    let changed = connection.execute(
        "UPDATE memories SET content = ?1, updated_at = ?2 WHERE id = ?3 AND user_id = ?4",
        params![content, now_string(Utc::now()), id, user_id],
    )?;
    if changed != 1 {
        return Err(SeraError::NotFound(format!("memory {id}")));
    }
    drop(connection);
    get(store, user_id, id)
}

/// Delete exactly one memory of the acting user. Returns `false` when the
/// id does not exist or belongs to another user.
pub fn delete(store: &Store, user_id: &str, id: &str) -> Result<bool> {
    let connection = store.connection()?;
    let changed = connection.execute(
        "DELETE FROM memories WHERE id = ?1 AND user_id = ?2",
        params![id, user_id],
    )?;
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

    /// The acting user for the single-operator CRUD flows.
    fn admin(store: &Store) -> String {
        store.admin_user_id().expect("admin")
    }

    #[test]
    fn crud_round_trip_and_isolation() {
        let store = store();
        let admin = admin(&store);
        let first = add(
            &store,
            &admin,
            "preference",
            "Prefers concise Italian summaries",
            "operator:test",
        )
        .expect("first memory");
        let second =
            add(&store, &admin, "fact", "Lives in Rome", "document:doc-a").expect("second memory");
        assert_eq!(first.kind, "preference");
        assert_eq!(second.source, "document:doc-a");

        let fetched = get(&store, &admin, &first.id).expect("get");
        assert_eq!(fetched, first);

        let listed = list(&store, &admin, 10).expect("list");
        assert_eq!(listed.len(), 2);

        let updated = update_content(&store, &admin, &first.id, "Prefers very concise summaries")
            .expect("update");
        assert_eq!(updated.id, first.id);
        assert_eq!(updated.kind, first.kind);
        assert_eq!(updated.source, first.source);
        assert_eq!(updated.created_at, first.created_at);
        assert!(updated.updated_at >= first.updated_at);

        // Updating one memory must not touch the other.
        let untouched = get(&store, &admin, &second.id).expect("second get");
        assert_eq!(untouched.content, "Lives in Rome");

        assert!(delete(&store, &admin, &first.id).expect("delete"));
        assert!(!delete(&store, &admin, &first.id).expect("idempotent delete"));
        assert!(get(&store, &admin, &first.id).is_err());
        assert_eq!(
            get(&store, &admin, &second.id)
                .expect("second stays")
                .content,
            "Lives in Rome"
        );
    }

    #[test]
    fn rejects_unknown_kind_empty_content_and_missing_provenance() {
        let store = store();
        let admin = admin(&store);
        assert!(add(&store, &admin, "guess", "content", "operator:test").is_err());
        assert!(add(&store, &admin, "fact", "   ", "operator:test").is_err());
        assert!(add(&store, &admin, "fact", "content", "  ").is_err());
        assert!(
            add(
                &store,
                &admin,
                "fact",
                &"x".repeat(MAX_CONTENT_CHARS + 1),
                "operator:test"
            )
            .is_err()
        );
        assert!(list(&store, &admin, 10).expect("list").is_empty());
    }

    #[test]
    fn update_missing_memory_is_not_found() {
        let store = store();
        let admin = admin(&store);
        assert!(update_content(&store, &admin, "missing", "content").is_err());
    }

    /// A second user never sees or touches the admin's memories: reads are
    /// empty, writes to the admin's ids fail or miss, and deletes report
    /// false.
    #[test]
    fn cross_user_access_is_impossible() {
        let store = store();
        let admin = admin(&store);
        let member = store
            .create_user("microsoft:memory-member", "m@example.example", "M")
            .expect("member");

        let admin_memory = add(
            &store,
            &admin,
            "fact",
            "admin private fact",
            "operator:test",
        )
        .expect("admin memory");
        assert!(
            list(&store, &member.id, 100)
                .expect("member list")
                .is_empty()
        );
        assert!(get(&store, &member.id, &admin_memory.id).is_err());
        assert!(update_content(&store, &member.id, &admin_memory.id, "rewritten").is_err());
        assert!(!delete(&store, &member.id, &admin_memory.id).expect("member delete"));

        // The admin's memory is intact.
        assert_eq!(
            get(&store, &admin, &admin_memory.id)
                .expect("admin get")
                .content,
            "admin private fact"
        );

        // And symmetrically, the admin cannot read the member's memories.
        let member_memory = add(
            &store,
            &member.id,
            "fact",
            "member private fact",
            "operator:test",
        )
        .expect("member memory");
        assert!(get(&store, &admin, &member_memory.id).is_err());
        assert_eq!(list(&store, &admin, 100).expect("admin list").len(), 1);
    }
}
