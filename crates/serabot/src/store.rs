use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{OptionalExtension, Row, params};
use serde_json::Value;

use crate::domain::{
    Action, Approval, AuditEntry, Event, Id, LlmMessage, Role, StoredMessage, Task, Turn, new_id,
    utc_now,
};
use crate::error::{Result, SeraError};

const MIGRATION_1: &str = include_str!("../migrations/0001_core.sql");
const MIGRATION_2: &str = include_str!("../migrations/0002_conversations.sql");
const MIGRATION_3: &str = include_str!("../migrations/0003_device_tokens.sql");
const MIGRATION_4: &str = include_str!("../migrations/0004_conversation_meta.sql");
const MIGRATION_5: &str = include_str!("../migrations/0005_conversation_teams.sql");
const MIGRATION_6: &str = include_str!("../migrations/0006_console_redesign.sql");
const MIGRATION_7: &str = include_str!("../migrations/0007_conversation_read_rowid.sql");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceToken {
    pub id: String,
    pub label: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl DeviceToken {
    pub fn active_at(&self, now: DateTime<Utc>) -> bool {
        self.revoked_at.is_none() && self.expires_at.is_none_or(|expires| expires > now)
    }
}

pub type StoreConnection = PooledConnection<SqliteConnectionManager>;

/// The team attached to a `room:` conversation: an ordered list of bot ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationTeam {
    pub conversation_id: String,
    pub members: Vec<String>,
    pub mode: String,
    pub rules: String,
    pub updated_at: DateTime<Utc>,
}

static INIT_VEC: Once = Once::new();

fn init_sqlite_vec() {
    INIT_VEC.call_once(|| unsafe {
        use rusqlite::ffi::sqlite3_auto_extension;
        // sqlite-vec exposes the standard SQLite extension entry point; the
        // pointer cast is required by the C API and cannot be typed more
        // narrowly through rustc's signature inference.
        #[allow(clippy::missing_transmute_annotations)]
        sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

#[derive(Clone)]
pub struct Store {
    pool: Pool<SqliteConnectionManager>,
    path: PathBuf,
    dimension: usize,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("path", &self.path)
            .field("dimension", &self.dimension)
            .finish()
    }
}

impl Store {
    pub fn open(path: impl AsRef<Path>, dimension: usize, busy_timeout_ms: u32) -> Result<Self> {
        init_sqlite_vec();
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let manager = SqliteConnectionManager::file(&path).with_init(move |connection| {
            connection.busy_timeout(Duration::from_millis(busy_timeout_ms as u64))?;
            connection.execute_batch(
                "PRAGMA journal_mode=WAL;\
                 PRAGMA synchronous=FULL;\
                 PRAGMA foreign_keys=ON;\
                 PRAGMA temp_store=MEMORY;",
            )?;
            Ok(())
        });
        let pool = Pool::builder().max_size(8).build(manager)?;
        let store = Self {
            pool,
            path,
            dimension,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory(dimension: usize) -> Result<Self> {
        init_sqlite_vec();
        let manager = SqliteConnectionManager::memory().with_init(|connection| {
            connection.busy_timeout(Duration::from_millis(5000))?;
            connection.execute_batch(
                "PRAGMA foreign_keys=ON;\
                 PRAGMA temp_store=MEMORY;",
            )?;
            Ok(())
        });
        let pool = Pool::builder().max_size(1).build(manager)?;
        let store = Self {
            pool,
            path: PathBuf::from(":memory:"),
            dimension,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn dimension(&self) -> usize {
        self.dimension
    }

    pub fn connection(&self) -> Result<StoreConnection> {
        self.pool.get().map_err(SeraError::from)
    }

    pub fn health(&self) -> Result<()> {
        let connection = self.connection()?;
        connection.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?;
        Ok(())
    }

    fn migrate(&self) -> Result<()> {
        let connection = self.connection()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 1 {
            connection.execute_batch(MIGRATION_1)?;
            connection.pragma_update(None, "user_version", 1)?;
        }
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 2 {
            connection.execute_batch(MIGRATION_2)?;
            connection.pragma_update(None, "user_version", 2)?;
        }
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 3 {
            connection.execute_batch(MIGRATION_3)?;
            connection.pragma_update(None, "user_version", 3)?;
        }
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 4 {
            connection.execute_batch(MIGRATION_4)?;
            connection.pragma_update(None, "user_version", 4)?;
        }
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 5 {
            connection.execute_batch(MIGRATION_5)?;
            connection.pragma_update(None, "user_version", 5)?;
        }
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 6 {
            connection.execute_batch(MIGRATION_6)?;
            connection.pragma_update(None, "user_version", 6)?;
        }
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 7 {
            connection.execute_batch(MIGRATION_7)?;
            connection.pragma_update(None, "user_version", 7)?;
        }
        let stored_dimension: Option<String> = connection
            .query_row(
                "SELECT value FROM kv WHERE key = 'embedding_dimension'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(stored) = stored_dimension {
            let stored: usize = stored
                .parse()
                .map_err(|_| SeraError::Internal("invalid stored embedding dimension".into()))?;
            if stored != self.dimension {
                return Err(SeraError::Config(format!(
                    "embedding dimension changed from {stored} to {}; reindex required",
                    self.dimension
                )));
            }
        } else {
            connection.execute(
                "INSERT INTO kv(key, value, updated_at) VALUES ('embedding_dimension', ?1, ?2)",
                params![self.dimension.to_string(), now_string()],
            )?;
        }
        connection.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS chunk_vectors USING vec0(chunk_id TEXT PRIMARY KEY, embedding float[{}]);",
            self.dimension
        ))?;
        Ok(())
    }

    pub fn upsert_bot(&self, name: &str, config: &Value, enabled: bool) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO bots(name, config_json, enabled, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET config_json=excluded.config_json, enabled=excluded.enabled, updated_at=excluded.updated_at",
            params![name, serde_json::to_string(config)?, enabled as i64, now_string()],
        )?;
        Ok(())
    }

    pub fn kv_get(&self, key: &str) -> Result<Option<String>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO kv(key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![key, value, now_string()],
        )?;
        Ok(())
    }

    pub fn insert_device_token(
        &self,
        id: &str,
        label: &str,
        token_hash: &str,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO device_tokens(id, label, token_hash, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                label,
                token_hash,
                now_string(),
                expires_at.map(to_string)
            ],
        )?;
        Ok(())
    }

    pub fn device_token_by_hash(&self, token_hash: &str) -> Result<Option<DeviceToken>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT id, label, created_at, last_used_at, expires_at, revoked_at
                 FROM device_tokens WHERE token_hash = ?1",
                params![token_hash],
                device_token_from_row,
            )
            .optional()?)
    }

    pub fn list_device_tokens(&self) -> Result<Vec<DeviceToken>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, label, created_at, last_used_at, expires_at, revoked_at
             FROM device_tokens ORDER BY created_at DESC, id",
        )?;
        let rows = statement.query_map([], device_token_from_row)?;
        let mut tokens = Vec::new();
        for row in rows {
            tokens.push(row?);
        }
        Ok(tokens)
    }

    /// Marks a token revoked. Returns false only when the id does not exist.
    pub fn revoke_device_token(&self, id: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE device_tokens SET revoked_at = COALESCE(revoked_at, ?2) WHERE id = ?1",
            params![id, now_string()],
        )?;
        Ok(changed > 0)
    }

    /// Records use at most once per hour to keep token checks read-mostly.
    pub fn touch_device_token(&self, id: &str, at: DateTime<Utc>) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE device_tokens SET last_used_at = ?2
             WHERE id = ?1 AND (last_used_at IS NULL OR last_used_at < ?3)",
            params![
                id,
                to_string(at),
                to_string(at - chrono::Duration::hours(1))
            ],
        )?;
        Ok(())
    }

    pub fn insert_message(&self, message: &StoredMessage) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO messages(id, conversation_id, turn_id, role, content, name, tool_call_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                message.id,
                message.conversation_id,
                message.turn_id,
                message.role.as_str(),
                message.content,
                message.name,
                message.tool_call_id,
                to_string(message.created_at),
            ],
        )?;
        connection.execute(
            "UPDATE conversations SET updated_at = ?2 WHERE id = ?1",
            params![message.conversation_id, to_string(message.created_at)],
        )?;
        Ok(())
    }

    pub fn append_message(
        &self,
        conversation_id: &str,
        turn_id: Option<&str>,
        role: Role,
        content: &str,
    ) -> Result<StoredMessage> {
        self.append_message_named(conversation_id, turn_id, role, content, None)
    }

    pub fn append_message_named(
        &self,
        conversation_id: &str,
        turn_id: Option<&str>,
        role: Role,
        content: &str,
        name: Option<&str>,
    ) -> Result<StoredMessage> {
        let message = StoredMessage {
            id: new_id(),
            conversation_id: conversation_id.to_owned(),
            turn_id: turn_id.map(str::to_owned),
            role,
            content: content.to_owned(),
            name: name.map(str::to_owned),
            bot: None,
            tool_call_id: None,
            created_at: utc_now(),
        };
        self.insert_message(&message)?;
        Ok(message)
    }

    pub fn create_conversation(&self, id: &str, kind: &str, parent_id: Option<&str>) -> Result<()> {
        self.create_conversation_titled(id, kind, parent_id, None)
    }

    pub fn create_conversation_titled(
        &self,
        id: &str,
        kind: &str,
        parent_id: Option<&str>,
        title: Option<&str>,
    ) -> Result<()> {
        use crate::orchestration::{
            ConversationKind, parse_conversation_scope, validate_conversation_id,
        };
        validate_conversation_id(id)?;
        let scope = parse_conversation_scope(id)?;
        let expected = conversation_kind_str(scope.kind);
        if kind != expected {
            return Err(SeraError::Validation(
                "conversation kind does not match id".into(),
            ));
        }
        if let Some(parent) = parent_id {
            validate_conversation_id(parent)?;
            let parent_row = self.get_conversation(parent)?;
            let Some(parent_row) = parent_row else {
                return Err(SeraError::NotFound(format!("conversation {parent}")));
            };
            let parent_kind = parse_stored_conversation_kind(&parent_row.kind)?;
            if !matches!(
                (parent_kind, scope.kind),
                (ConversationKind::Section, ConversationKind::Group)
                    | (ConversationKind::Section, ConversationKind::Direct)
                    | (ConversationKind::Section, ConversationKind::Room)
                    | (ConversationKind::Group, ConversationKind::Direct)
                    | (ConversationKind::Group, ConversationKind::Room)
            ) {
                return Err(SeraError::Validation(
                    "invalid conversation parent for kind".into(),
                ));
            }
        } else if matches!(scope.kind, ConversationKind::Group) {
            return Err(SeraError::Validation(
                "group conversations require a section parent".into(),
            ));
        }
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO conversations(id, kind, parent_id, title, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![id, kind, parent_id, title, now_string()],
            )
            .map_err(map_conversation_write)?;
        Ok(())
    }

    pub fn get_conversation(&self, id: &str) -> Result<Option<StoredConversation>> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT c.id, c.kind, c.parent_id, c.title, c.created_at, c.updated_at,
                        c.archived_at,
                        (SELECT MAX(m.created_at) FROM messages m WHERE m.conversation_id = c.id),
                        (SELECT COUNT(*) FROM messages m WHERE m.conversation_id = c.id
                           AND m.role = 'assistant'
                           AND (c.last_read_rowid IS NULL OR m.rowid > c.last_read_rowid))
                 FROM conversations c WHERE c.id = ?1",
                params![id],
                conversation_from_row,
            )
            .optional()
            .map_err(SeraError::from)
    }

    pub fn list_conversations(
        &self,
        limit: usize,
        before: Option<(DateTime<Utc>, &str)>,
        include_archived: bool,
    ) -> Result<Vec<StoredConversation>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT c.id, c.kind, c.parent_id, c.title, c.created_at, c.updated_at,
                    c.archived_at,
                    (SELECT MAX(m.created_at) FROM messages m WHERE m.conversation_id = c.id),
                    (SELECT COUNT(*) FROM messages m WHERE m.conversation_id = c.id
                       AND m.role = 'assistant'
                       AND (c.last_read_rowid IS NULL OR m.rowid > c.last_read_rowid))
             FROM conversations c
             WHERE (?2 = 1 OR c.archived_at IS NULL)
               AND (?3 IS NULL OR c.updated_at < ?3 OR (c.updated_at = ?3 AND c.id < ?4))
             ORDER BY c.updated_at DESC, c.id DESC
             LIMIT ?1",
        )?;
        let cursor = before.as_ref().map(|(at, _)| to_string(*at));
        let cursor_id = before.as_ref().map(|(_, id)| *id);
        let rows = statement.query_map(
            params![
                limit.clamp(1, 200) as i64,
                include_archived as i64,
                cursor,
                cursor_id
            ],
            conversation_from_row,
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Returns false when the conversation does not exist.
    pub fn rename_conversation(&self, id: &str, title: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE conversations SET title = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, title, now_string()],
        )?;
        Ok(changed == 1)
    }

    /// The owner opened the conversation: assistant messages up to now count as read.
    /// Returns false when the id is unknown.
    pub fn mark_conversation_read(&self, id: &str) -> Result<bool> {
        let connection = self.connection()?;
        // Record the newest message's rowid, not the wall clock: rowids are
        // monotonic, so a message appended immediately after this call is
        // always counted as unread. Timestamp comparison could not promise
        // that (see migrations/0007_conversation_read_rowid.sql).
        let changed = connection.execute(
            "UPDATE conversations
                SET last_read_rowid = (
                    SELECT COALESCE(MAX(m.rowid), 0)
                      FROM messages m
                     WHERE m.conversation_id = ?1
                )
              WHERE id = ?1",
            params![id],
        )?;
        Ok(changed == 1)
    }

    /// Soft archive: messages are kept. Returns false when the id is unknown.
    pub fn archive_conversation(&self, id: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE conversations SET archived_at = COALESCE(archived_at, ?2), updated_at = ?2
             WHERE id = ?1",
            params![id, now_string()],
        )?;
        Ok(changed == 1)
    }

    pub fn unarchive_conversation(&self, id: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE conversations SET archived_at = NULL, updated_at = ?2 WHERE id = ?1",
            params![id, now_string()],
        )?;
        Ok(changed == 1)
    }

    /// Reject container conversations; lazily record turn-capable scopes on first use.
    pub fn ensure_conversation_accepts_turns(&self, id: &str) -> Result<()> {
        use crate::orchestration::{parse_conversation_scope, validate_conversation_id};
        validate_conversation_id(id)?;
        let scope = parse_conversation_scope(id)?;
        if let Some(record) = self.get_conversation(id)? {
            let kind = parse_stored_conversation_kind(&record.kind)?;
            if !kind.accepts_turns() {
                return Err(SeraError::Validation(
                    "conversation cannot accept turns".into(),
                ));
            }
            return Ok(());
        }
        if !scope.kind.accepts_turns() {
            return Err(SeraError::Validation(
                "conversation cannot accept turns".into(),
            ));
        }
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO conversations(id, kind, parent_id, created_at, updated_at)
             VALUES (?1, ?2, NULL, ?3, ?3)
             ON CONFLICT(id) DO NOTHING",
            params![id, conversation_kind_str(scope.kind), now_string()],
        )?;
        Ok(())
    }

    /// Team attached to a room conversation, if the owner set one.
    pub fn get_conversation_team(&self, conversation_id: &str) -> Result<Option<ConversationTeam>> {
        let connection = self.connection()?;
        let row: Option<(String, String, String, String, String)> = connection
            .query_row(
                "SELECT conversation_id, members, mode, rules, updated_at
                 FROM conversation_teams WHERE conversation_id = ?1",
                params![conversation_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(SeraError::from)?;
        let Some((conversation_id, members, mode, rules, updated_at)) = row else {
            return Ok(None);
        };
        let members: Vec<String> = serde_json::from_str(&members)
            .map_err(|_| SeraError::InvalidState("stored team members are invalid".into()))?;
        Ok(Some(ConversationTeam {
            conversation_id,
            members,
            mode,
            rules,
            updated_at: parse_datetime(&updated_at),
        }))
    }

    pub fn set_conversation_team(
        &self,
        conversation_id: &str,
        members: &[String],
        mode: &str,
        rules: &str,
    ) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO conversation_teams(conversation_id, members, mode, rules, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(conversation_id) DO UPDATE SET
                 members=excluded.members, mode=excluded.mode, rules=excluded.rules,
                 updated_at=excluded.updated_at",
            params![
                conversation_id,
                serde_json::to_string(members)?,
                mode,
                rules,
                now_string()
            ],
        )?;
        Ok(())
    }

    /// Team messages already queued or posted in one room turn. Counted from the
    /// audit ledger, which is the append-only record of every `team.asked` and
    /// `team.replied` decision.
    pub fn count_team_events(&self, root_task_id: &str) -> Result<i64> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT COUNT(*) FROM audit_log
             WHERE event IN ('team.asked','team.replied')
               AND json_extract(details_json, '$.root_task_id') = ?1",
            params![root_task_id],
            |row| row.get(0),
        )?)
    }

    /// Peer replies one bot already handed to peers in one room turn.
    pub fn count_peer_replies(&self, root_task_id: &str, from_bot: &str) -> Result<i64> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT COUNT(*) FROM audit_log
             WHERE event = 'team.replied'
               AND json_extract(details_json, '$.root_task_id') = ?1
               AND json_extract(details_json, '$.from_bot') = ?2",
            params![root_task_id, from_bot],
            |row| row.get(0),
        )?)
    }

    /// Team turns of one room turn that are still queued or running.
    pub fn count_pending_team_tasks(&self, root_task_id: &str) -> Result<i64> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT COUNT(*) FROM tasks
             WHERE status IN ('queued','running','awaiting_approval')
               AND json_extract(payload_json, '$.team.root_task_id') = ?1",
            params![root_task_id],
            |row| row.get(0),
        )?)
    }

    pub fn list_messages(&self, conversation_id: &str, limit: usize) -> Result<Vec<StoredMessage>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, conversation_id, turn_id, role, content, name, tool_call_id, created_at
             FROM messages WHERE conversation_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![conversation_id, limit as i64], |row| {
            Ok(StoredMessage {
                id: row.get(0)?,
                conversation_id: row.get(1)?,
                turn_id: row.get(2)?,
                role: parse_role(&row.get::<_, String>(3)?),
                content: row.get(4)?,
                name: row.get(5)?,
                bot: None,
                tool_call_id: row.get(6)?,
                created_at: parse_datetime(&row.get::<_, String>(7)?),
            })
        })?;
        let mut messages = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        messages.reverse();
        Ok(messages)
    }

    /// Artifacts produced inside a conversation, newest first. Joins gateway
    /// actions to their turn; the hash may be recorded at approval time or in
    /// the execution outcome, so both are checked.
    pub fn list_conversation_artifacts(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<StoredConversationArtifact>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT a.id, a.tool, a.created_at,
                    COALESCE(a.artifact_hash, json_extract(a.outcome_json, '$.artifact_hash'))
             FROM actions a
             JOIN turns t ON t.id = a.turn_id
             WHERE t.conversation_id = ?1
               AND COALESCE(a.artifact_hash, json_extract(a.outcome_json, '$.artifact_hash')) IS NOT NULL
             ORDER BY a.created_at DESC, a.rowid DESC
             LIMIT 100",
        )?;
        let rows = statement.query_map(params![conversation_id], |row| {
            Ok(StoredConversationArtifact {
                action_id: row.get(0)?,
                tool: row.get(1)?,
                created_at: parse_datetime(&row.get::<_, String>(2)?),
                hash: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn list_visible_messages(
        &self,
        conversation_id: &str,
        limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT m.id, m.conversation_id, m.turn_id, m.role, m.content, m.name,
                    m.created_at, t.bot
             FROM messages m LEFT JOIN turns t ON t.id = m.turn_id
             WHERE m.conversation_id = ?1 AND m.role IN ('user', 'assistant')
             ORDER BY m.created_at DESC, m.rowid DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![conversation_id, limit.clamp(1, 200) as i64],
            |row| {
                Ok(StoredMessage {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    turn_id: row.get(2)?,
                    role: parse_role(&row.get::<_, String>(3)?),
                    content: row.get(4)?,
                    name: row.get(5)?,
                    bot: row.get(7)?,
                    tool_call_id: None,
                    created_at: parse_datetime(&row.get::<_, String>(6)?),
                })
            },
        )?;
        let mut messages = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        messages.reverse();
        Ok(messages)
    }

    pub fn history_for_llm(&self, conversation_id: &str, limit: usize) -> Result<Vec<LlmMessage>> {
        Ok(self
            .list_messages(conversation_id, limit)?
            .into_iter()
            .filter(|message| message.role != Role::System)
            .map(|message| LlmMessage {
                role: message.role,
                content: Some(message.content),
                tool_calls: Vec::new(),
                tool_call_id: message.tool_call_id,
                name: message.name,
            })
            .collect())
    }

    pub fn create_turn(
        &self,
        task_id: &str,
        bot: &str,
        conversation_id: &str,
        state: &Value,
    ) -> Result<Turn> {
        let turn = Turn {
            id: new_id(),
            task_id: task_id.to_owned(),
            bot: bot.to_owned(),
            conversation_id: conversation_id.to_owned(),
            status: "active".into(),
            state: state.clone(),
            created_at: utc_now(),
            updated_at: utc_now(),
        };
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO turns(id, task_id, bot, conversation_id, status, state_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                turn.id,
                turn.task_id,
                turn.bot,
                turn.conversation_id,
                turn.status,
                serde_json::to_string(&turn.state)?,
                to_string(turn.created_at),
                to_string(turn.updated_at),
            ],
        )?;
        Ok(turn)
    }

    pub fn get_turn(&self, id: &str) -> Result<Turn> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, task_id, bot, conversation_id, status, state_json, created_at, updated_at
                 FROM turns WHERE id = ?1",
                params![id],
                |row| {
                    Ok(Turn {
                        id: row.get(0)?,
                        task_id: row.get(1)?,
                        bot: row.get(2)?,
                        conversation_id: row.get(3)?,
                        status: row.get(4)?,
                        state: serde_json::from_str(&row.get::<_, String>(5)?)
                            .unwrap_or(Value::Null),
                        created_at: parse_datetime(&row.get::<_, String>(6)?),
                        updated_at: parse_datetime(&row.get::<_, String>(7)?),
                    })
                },
            )
            .optional()?
            .ok_or_else(|| SeraError::NotFound(format!("turn {id}")))
    }

    pub fn get_turn_by_task(&self, task_id: &str) -> Result<Option<Turn>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT id, task_id, bot, conversation_id, status, state_json, created_at, updated_at
                 FROM turns WHERE task_id = ?1 ORDER BY created_at DESC LIMIT 1",
                params![task_id],
                |row| {
                    Ok(Turn {
                        id: row.get(0)?,
                        task_id: row.get(1)?,
                        bot: row.get(2)?,
                        conversation_id: row.get(3)?,
                        status: row.get(4)?,
                        state: serde_json::from_str(&row.get::<_, String>(5)?)
                            .unwrap_or(Value::Null),
                        created_at: parse_datetime(&row.get::<_, String>(6)?),
                        updated_at: parse_datetime(&row.get::<_, String>(7)?),
                    })
                },
            )
            .optional()?)
    }

    pub fn update_turn(&self, id: &str, status: &str, state: &Value) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE turns SET status=?1, state_json=?2, updated_at=?3 WHERE id=?4",
            params![status, serde_json::to_string(state)?, now_string(), id],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_audit(
        &self,
        actor: &str,
        event: &str,
        tool: Option<&str>,
        task_id: Option<&str>,
        approval_id: Option<&str>,
        action_id: Option<&str>,
        policy_version: Option<&str>,
        details: Option<&Value>,
    ) -> Result<i64> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO audit_log(at, actor, event, tool, task_id, approval_id, action_id, policy_version, details_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                now_string(),
                actor,
                event,
                tool,
                task_id,
                approval_id,
                action_id,
                policy_version,
                details.map(serde_json::to_string).transpose()?,
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    pub fn list_audit(&self, limit: usize) -> Result<Vec<AuditEntry>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, at, actor, event, tool, task_id, approval_id, action_id, policy_version, details_json
             FROM audit_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as i64], |row| {
            Ok(AuditEntry {
                id: row.get(0)?,
                at: parse_datetime(&row.get::<_, String>(1)?),
                actor: row.get(2)?,
                event: row.get(3)?,
                tool: row.get(4)?,
                task_id: row.get(5)?,
                approval_id: row.get(6)?,
                action_id: row.get(7)?,
                policy_version: row.get(8)?,
                details: row
                    .get::<_, Option<String>>(9)?
                    .and_then(|value| serde_json::from_str(&value).ok()),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn append_event(&self, kind: &str, payload: &Value) -> Result<i64> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO events(at, kind, payload_json) VALUES (?1, ?2, ?3)",
            params![now_string(), kind, serde_json::to_string(payload)?],
        )?;
        Ok(connection.last_insert_rowid())
    }

    pub fn events_after(&self, after_id: i64, limit: usize) -> Result<Vec<Event>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, at, kind, payload_json FROM events WHERE id > ?1 ORDER BY id ASC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![after_id, limit as i64], |row| {
            Ok(Event {
                id: row.get(0)?,
                at: parse_datetime(&row.get::<_, String>(1)?),
                kind: row.get(2)?,
                payload: serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or(Value::Null),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn prune_events(&self, before: DateTime<Utc>) -> Result<usize> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "DELETE FROM events WHERE at < ?1",
            params![to_string(before)],
        )?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_action(
        &self,
        tool: &str,
        task_id: Option<&str>,
        turn_id: Option<&str>,
        canonical_args: &str,
        canonical_hash: &str,
        destination: Option<&str>,
        artifact_hash: Option<&str>,
        policy_version: &str,
    ) -> Result<Action> {
        let action = Action {
            id: new_id(),
            tool: tool.to_owned(),
            task_id: task_id.map(str::to_owned),
            turn_id: turn_id.map(str::to_owned),
            canonical_args: canonical_args.to_owned(),
            canonical_hash: canonical_hash.to_owned(),
            destination: destination.map(str::to_owned),
            artifact_hash: artifact_hash.map(str::to_owned),
            policy_version: policy_version.to_owned(),
            status: "pending_approval".into(),
            outcome: None,
            created_at: utc_now(),
            updated_at: utc_now(),
        };
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO actions(id, tool, task_id, turn_id, canonical_args, canonical_hash, destination, artifact_hash, policy_version, status, outcome_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11, ?12)",
            params![
                action.id,
                action.tool,
                action.task_id,
                action.turn_id,
                action.canonical_args,
                action.canonical_hash,
                action.destination,
                action.artifact_hash,
                action.policy_version,
                action.status,
                to_string(action.created_at),
                to_string(action.updated_at),
            ],
        )?;
        Ok(action)
    }

    pub fn get_action(&self, id: &str) -> Result<Action> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, tool, task_id, turn_id, canonical_args, canonical_hash, destination, artifact_hash, policy_version, status, outcome_json, created_at, updated_at
                 FROM actions WHERE id = ?1",
                params![id],
                |row| {
                    Ok(Action {
                        id: row.get(0)?,
                        tool: row.get(1)?,
                        task_id: row.get(2)?,
                        turn_id: row.get(3)?,
                        canonical_args: row.get(4)?,
                        canonical_hash: row.get(5)?,
                        destination: row.get(6)?,
                        artifact_hash: row.get(7)?,
                        policy_version: row.get(8)?,
                        status: row.get(9)?,
                        outcome: row
                            .get::<_, Option<String>>(10)?
                            .and_then(|value| serde_json::from_str(&value).ok()),
                        created_at: parse_datetime(&row.get::<_, String>(11)?),
                        updated_at: parse_datetime(&row.get::<_, String>(12)?),
                    })
                },
            )
            .optional()?
            .ok_or_else(|| SeraError::NotFound(format!("action {id}")))
    }

    pub fn update_action(&self, id: &str, status: &str, outcome: Option<&Value>) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE actions SET status=?1, outcome_json=?2, updated_at=?3 WHERE id=?4",
            params![
                status,
                outcome.map(serde_json::to_string).transpose()?,
                now_string(),
                id
            ],
        )?;
        Ok(())
    }

    pub fn transition_action(&self, id: &str, from: &str, to: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE actions SET status=?1, updated_at=?2 WHERE id=?3 AND status=?4",
            params![to, now_string(), id, from],
        )?;
        Ok(changed == 1)
    }

    pub fn create_approval(
        &self,
        action_id: &str,
        tool: &str,
        summary: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<Approval> {
        let approval = Approval {
            id: new_id(),
            action_id: action_id.to_owned(),
            tool: tool.to_owned(),
            summary: summary.to_owned(),
            state: "pending".into(),
            scope: None,
            expires_at,
            created_at: utc_now(),
            decided_at: None,
            note: None,
        };
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO approvals(id, action_id, tool, summary, state, scope, expires_at, created_at, decided_at, note)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, NULL, NULL)",
            params![
                approval.id,
                approval.action_id,
                approval.tool,
                approval.summary,
                approval.state,
                to_string(approval.expires_at),
                to_string(approval.created_at),
            ],
        )?;
        Ok(approval)
    }

    pub fn get_approval(&self, id: &str) -> Result<Approval> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, action_id, tool, summary, state, scope, expires_at, created_at, decided_at, note
                 FROM approvals WHERE id = ?1",
                params![id],
                map_approval,
            )
            .optional()?
            .ok_or_else(|| SeraError::NotFound(format!("approval {id}")))
    }

    pub fn list_pending_approvals(&self, limit: usize) -> Result<Vec<Approval>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, action_id, tool, summary, state, scope, expires_at, created_at, decided_at, note
             FROM approvals WHERE state='pending' ORDER BY created_at ASC LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as i64], map_approval)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn transition_approval(
        &self,
        id: &str,
        to: &str,
        scope: Option<&str>,
        note: Option<&str>,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let now = now_string();
        let changed = connection.execute(
            "UPDATE approvals SET state=?1, scope=?2, note=?3, decided_at=?4
             WHERE id=?5 AND state='pending' AND expires_at > ?4",
            params![to, scope, note, now, id],
        )?;
        Ok(changed == 1)
    }

    pub fn expire_approvals(&self) -> Result<usize> {
        let connection = self.connection()?;
        let now = now_string();
        let transaction = connection.unchecked_transaction()?;
        let expiring: Vec<(String, String, Option<String>)> = {
            let mut statement = transaction.prepare(
                "SELECT ap.id, ap.action_id, ac.task_id
                 FROM approvals ap JOIN actions ac ON ac.id = ap.action_id
                 WHERE ap.state='pending' AND ap.expires_at <= ?1",
            )?;
            let rows = statement.query_map(params![now], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (approval_id, action_id, task_id) in &expiring {
            transaction.execute(
                "UPDATE approvals SET state='expired', decided_at=?1 WHERE id=?2 AND state='pending'",
                params![now, approval_id],
            )?;
            transaction.execute(
                "UPDATE actions SET status='expired', updated_at=?1 WHERE id=?2 AND status='pending_approval'",
                params![now, action_id],
            )?;
            if let Some(task_id) = task_id {
                transaction.execute(
                    "UPDATE tasks SET status='failed', last_error='approval expired', lease_until=NULL, worker_id=NULL, updated_at=?1
                     WHERE id=?2 AND status='awaiting_approval'",
                    params![now, task_id],
                )?;
                transaction.execute(
                    "UPDATE turns SET status='approval_expired', updated_at=?1
                     WHERE task_id=?2 AND status='awaiting_approval'",
                    params![now, task_id],
                )?;
            }
        }
        transaction.commit()?;
        Ok(expiring.len())
    }

    pub fn invalidate_approvals_for_old_policy(&self, current_policy: &str) -> Result<usize> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "UPDATE approvals SET state='invalidated', decided_at=?1
             WHERE state='pending' AND action_id IN (
               SELECT id FROM actions WHERE policy_version <> ?2
             )",
            params![now_string(), current_policy],
        )?)
    }

    pub fn insert_grant(
        &self,
        tool: &str,
        canonical_hash: &str,
        destination: Option<&str>,
        policy_version: &str,
        approval_id: &str,
    ) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO standing_grants(id, tool, canonical_hash, destination, policy_version, approval_id, revoked_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7)",
            params![
                new_id(),
                tool,
                canonical_hash,
                destination,
                policy_version,
                approval_id,
                now_string()
            ],
        )?;
        Ok(())
    }

    pub fn matching_grant(
        &self,
        tool: &str,
        canonical_hash: &str,
        destination: Option<&str>,
        policy_version: &str,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM standing_grants
             WHERE tool=?1 AND canonical_hash=?2 AND policy_version=?3 AND revoked_at IS NULL
               AND ((destination IS NULL AND ?4 IS NULL) OR destination = ?4)",
            params![tool, canonical_hash, policy_version, destination],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn revoke_grants(&self) -> Result<usize> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "UPDATE standing_grants SET revoked_at=?1 WHERE revoked_at IS NULL",
            params![now_string()],
        )?)
    }

    pub fn record_telegram_update(&self, update_id: i64) -> Result<bool> {
        let connection = self.connection()?;
        let inserted = connection.execute(
            "INSERT OR IGNORE INTO telegram_updates(update_id, received_at) VALUES (?1, ?2)",
            params![update_id, now_string()],
        )?;
        Ok(inserted == 1)
    }

    pub fn claim_task(&self, worker_id: &str, lease_secs: u64) -> Result<Option<Task>> {
        let connection = self.connection()?;
        let now = utc_now();
        let lease_until = now + chrono::Duration::seconds(lease_secs as i64);
        let transaction = connection.unchecked_transaction()?;
        let task = transaction
            .query_row(
                "UPDATE tasks SET status='running', worker_id=?1, lease_until=?2,
                    fencing_token=fencing_token+1, attempts=attempts+1, updated_at=?3
                 WHERE id = (
                    SELECT id FROM tasks
                    WHERE status='queued' AND run_after <= ?3
                    ORDER BY priority DESC, created_at ASC LIMIT 1
                 )
                 RETURNING id, kind, bot, payload_json, status, priority, attempts, max_attempts,
                    run_after, lease_until, worker_id, fencing_token, uncertain, last_error,
                    created_at, updated_at",
                params![worker_id, to_string(lease_until), to_string(now)],
                map_task,
            )
            .optional()?;
        transaction.commit()?;
        Ok(task)
    }

    pub fn complete_task(&self, id: &str, fencing_token: i64) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE tasks SET status='done', lease_until=NULL, worker_id=NULL, updated_at=?1
             WHERE id=?2 AND fencing_token=?3 AND status='running'",
            params![now_string(), id, fencing_token],
        )?;
        Ok(changed == 1)
    }

    pub fn fail_task(
        &self,
        id: &str,
        fencing_token: i64,
        error: &str,
        retry_backoff_secs: u64,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let now = utc_now();
        let task = connection
            .query_row(
                "SELECT attempts, max_attempts, uncertain FROM tasks WHERE id=?1 AND fencing_token=?2",
                params![id, fencing_token],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)? != 0,
                    ))
                },
            )
            .optional()?;
        let Some((attempts, max_attempts, uncertain)) = task else {
            return Ok(false);
        };
        let (status, run_after) = if uncertain || attempts >= max_attempts {
            ("failed", now)
        } else {
            let delay = retry_backoff_secs.saturating_mul(attempts.max(1) as u64);
            (
                "queued",
                now + chrono::Duration::seconds(delay.min(3600) as i64),
            )
        };
        let changed = connection.execute(
            "UPDATE tasks SET status=?1, last_error=?2, run_after=?3, lease_until=NULL, worker_id=NULL, updated_at=?4
             WHERE id=?5 AND fencing_token=?6 AND status='running'",
            params![status, truncate(error, 2000), to_string(run_after), to_string(now), id, fencing_token],
        )?;
        Ok(changed == 1)
    }

    pub fn fail_task_final(&self, id: &str, fencing_token: i64, error: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE tasks SET status='failed', last_error=?1, lease_until=NULL, worker_id=NULL, updated_at=?2
             WHERE id=?3 AND fencing_token=?4 AND status='running'",
            params![truncate(error, 2000), now_string(), id, fencing_token],
        )?;
        Ok(changed == 1)
    }

    pub fn await_approval_task(&self, id: &str, fencing_token: i64) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE tasks SET status='awaiting_approval', lease_until=NULL, worker_id=NULL, updated_at=?1
             WHERE id=?2 AND fencing_token=?3 AND status='running'",
            params![now_string(), id, fencing_token],
        )?;
        Ok(changed == 1)
    }

    pub fn requeue_task(&self, id: &str, run_after: DateTime<Utc>) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE tasks SET status='queued', run_after=?1, lease_until=NULL, worker_id=NULL, updated_at=?2
             WHERE id=?3 AND status IN ('awaiting_approval','running')",
            params![to_string(run_after), now_string(), id],
        )?;
        Ok(changed == 1)
    }

    pub fn recover_expired_tasks(&self) -> Result<usize> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "UPDATE tasks SET
                status=CASE WHEN uncertain=1 THEN 'failed' ELSE 'queued' END,
                last_error=CASE WHEN uncertain=1 THEN 'outcome unknown after worker lease expiry' ELSE last_error END,
                lease_until=NULL, worker_id=NULL, updated_at=?1
             WHERE status='running' AND lease_until IS NOT NULL AND lease_until < ?1",
            params![now_string()],
        )?)
    }

    pub fn get_task(&self, id: &str) -> Result<Task> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, kind, bot, payload_json, status, priority, attempts, max_attempts,
                        run_after, lease_until, worker_id, fencing_token, uncertain, last_error,
                        created_at, updated_at
                 FROM tasks WHERE id=?1",
                params![id],
                map_task,
            )
            .optional()?
            .ok_or_else(|| SeraError::NotFound(format!("task {id}")))
    }

    pub fn enqueue_task(
        &self,
        kind: &str,
        bot: &str,
        payload: &Value,
        priority: i64,
        run_after: DateTime<Utc>,
    ) -> Result<Task> {
        let now = utc_now();
        let id = new_id();
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO tasks(id, kind, bot, payload_json, status, priority, attempts, max_attempts,
                run_after, lease_until, worker_id, fencing_token, uncertain, last_error, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'queued', ?5, 0, 5, ?6, NULL, NULL, 0, 0, NULL, ?7, ?7)",
            params![id, kind, bot, serde_json::to_string(payload)?, priority, to_string(run_after), to_string(now)],
        )?;
        drop(connection);
        self.get_task(&id)
    }

    pub fn pending_task_count(&self) -> Result<i64> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT COUNT(*) FROM tasks WHERE status IN ('queued','running','awaiting_approval')",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn cancel_task(&self, id: &str, reason: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE tasks SET status='cancelled', last_error=?1, lease_until=NULL, worker_id=NULL, updated_at=?2
             WHERE id=?3 AND status IN ('queued','running','awaiting_approval')",
            params![truncate(reason, 2000), now_string(), id],
        )?;
        Ok(changed == 1)
    }

    /// Actions that were dispatched before a crash must never be replayed blindly.
    pub fn reconcile_dispatching_actions(&self) -> Result<usize> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "UPDATE actions SET status='outcome_unknown', outcome_json=?1, updated_at=?2
             WHERE status='dispatching'",
            params![
                serde_json::to_string(&serde_json::json!({
                    "error": "process stopped after dispatch; external outcome must be reconciled by the owner"
                }))?,
                now_string()
            ],
        )?)
    }

    /// Create a consistent SQLite backup without overwriting an existing file.
    pub fn backup_to(&self, destination: &Path) -> Result<()> {
        if destination.exists() {
            return Err(SeraError::Validation(
                "backup destination already exists".into(),
            ));
        }
        if let Some(parent) = destination.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let connection = self.connection()?;
        connection.execute("VACUUM INTO ?1", params![destination.to_string_lossy()])?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredConversation {
    pub id: String,
    pub kind: String,
    pub parent_id: Option<String>,
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    pub last_message_at: Option<DateTime<Utc>>,
    pub unread_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredConversationArtifact {
    pub action_id: String,
    pub tool: String,
    pub created_at: DateTime<Utc>,
    pub hash: String,
}

fn map_conversation_write(error: rusqlite::Error) -> SeraError {
    if let rusqlite::Error::SqliteFailure(err, _) = &error {
        if err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
            || err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
        {
            return SeraError::InvalidState("conversation already exists".into());
        }
        if err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY {
            return SeraError::Validation("invalid conversation parent".into());
        }
    }
    SeraError::Database(error)
}

fn conversation_kind_str(kind: crate::orchestration::ConversationKind) -> &'static str {
    use crate::orchestration::ConversationKind;
    match kind {
        ConversationKind::Section => "section",
        ConversationKind::Group => "group",
        ConversationKind::Direct => "direct",
        ConversationKind::Room => "room",
        ConversationKind::Legacy => "legacy",
    }
}

fn conversation_from_row(row: &Row<'_>) -> rusqlite::Result<StoredConversation> {
    Ok(StoredConversation {
        id: row.get(0)?,
        kind: row.get(1)?,
        parent_id: row.get(2)?,
        title: row.get(3)?,
        created_at: parse_datetime(&row.get::<_, String>(4)?),
        updated_at: parse_datetime(&row.get::<_, String>(5)?),
        archived_at: row
            .get::<_, Option<String>>(6)?
            .as_deref()
            .map(parse_datetime),
        last_message_at: row
            .get::<_, Option<String>>(7)?
            .as_deref()
            .map(parse_datetime),
        unread_count: row.get(8)?,
    })
}

pub(crate) fn parse_stored_conversation_kind(
    kind: &str,
) -> Result<crate::orchestration::ConversationKind> {
    use crate::orchestration::ConversationKind;
    match kind {
        "section" => Ok(ConversationKind::Section),
        "group" => Ok(ConversationKind::Group),
        "direct" => Ok(ConversationKind::Direct),
        "room" => Ok(ConversationKind::Room),
        "legacy" => Ok(ConversationKind::Legacy),
        _ => Err(SeraError::Internal(
            "invalid stored conversation kind".into(),
        )),
    }
}

fn map_task(row: &Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get(0)?,
        kind: row.get(1)?,
        bot: row.get(2)?,
        payload: serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or(Value::Null),
        status: row.get(4)?,
        priority: row.get(5)?,
        attempts: row.get(6)?,
        max_attempts: row.get(7)?,
        run_after: parse_datetime(&row.get::<_, String>(8)?),
        lease_until: row
            .get::<_, Option<String>>(9)?
            .map(|value| parse_datetime(&value)),
        worker_id: row.get(10)?,
        fencing_token: row.get(11)?,
        uncertain: row.get::<_, i64>(12)? != 0,
        last_error: row.get(13)?,
        created_at: parse_datetime(&row.get::<_, String>(14)?),
        updated_at: parse_datetime(&row.get::<_, String>(15)?),
    })
}

fn map_approval(row: &Row<'_>) -> rusqlite::Result<Approval> {
    Ok(Approval {
        id: row.get(0)?,
        action_id: row.get(1)?,
        tool: row.get(2)?,
        summary: row.get(3)?,
        state: row.get(4)?,
        scope: row.get(5)?,
        expires_at: parse_datetime(&row.get::<_, String>(6)?),
        created_at: parse_datetime(&row.get::<_, String>(7)?),
        decided_at: row
            .get::<_, Option<String>>(8)?
            .map(|value| parse_datetime(&value)),
        note: row.get(9)?,
    })
}

fn parse_role(value: &str) -> Role {
    value.parse().unwrap_or(Role::User)
}

fn to_string(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn now_string() -> String {
    to_string(utc_now())
}

fn parse_datetime(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|_| utc_now())
}

fn device_token_from_row(row: &Row<'_>) -> rusqlite::Result<DeviceToken> {
    Ok(DeviceToken {
        id: row.get(0)?,
        label: row.get(1)?,
        created_at: parse_datetime(&row.get::<_, String>(2)?),
        last_used_at: row
            .get::<_, Option<String>>(3)?
            .as_deref()
            .map(parse_datetime),
        expires_at: row
            .get::<_, Option<String>>(4)?
            .as_deref()
            .map(parse_datetime),
        revoked_at: row
            .get::<_, Option<String>>(5)?
            .as_deref()
            .map(parse_datetime),
    })
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

pub fn insert_outbox(store: &Store, kind: &str, destination: &str, payload: &Value) -> Result<Id> {
    let id = new_id();
    let connection = store.connection()?;
    connection.execute(
        "INSERT INTO outbox(id, kind, destination, payload_json, status, attempts, last_error, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'pending', 0, NULL, ?5, ?5)",
        params![id, kind, destination, serde_json::to_string(payload)?, now_string()],
    )?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        // File-backed tests exercise the same multi-connection pool as production;
        // `:memory:` with a one-connection pool can deadlock on nested lookups.
        Store::open(
            std::env::temp_dir().join(format!("serabot-store-test-{}.db", new_id())),
            384,
            5000,
        )
        .unwrap()
    }

    #[test]
    fn visible_history_filters_before_limiting_and_orders_timestamp_ties() {
        let store = store();
        let first = store
            .append_message("owner", None, Role::User, "first")
            .unwrap();
        let second = store
            .append_message("owner", None, Role::Assistant, "second")
            .unwrap();
        store
            .append_message("owner", None, Role::System, "hidden")
            .unwrap();
        store
            .append_message("owner", None, Role::Tool, "internal")
            .unwrap();
        store
            .append_message("other", None, Role::User, "other")
            .unwrap();
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE messages SET created_at = '2026-01-01T00:00:00.000Z'",
                [],
            )
            .unwrap();
        let messages = store.list_visible_messages("owner", 2).unwrap();
        assert_eq!(
            messages.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec![first.id.as_str(), second.id.as_str()]
        );
        assert_eq!(
            store.list_visible_messages("owner", 1).unwrap()[0].id,
            second.id
        );
        assert!(
            store
                .list_visible_messages("absent", 100)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn unread_counts_assistant_messages_until_marked_read() {
        let store = store();
        store.ensure_conversation_accepts_turns("dir:main").unwrap();
        store
            .append_message("dir:main", None, Role::Assistant, "first")
            .unwrap();
        store
            .append_message("dir:main", None, Role::Assistant, "second")
            .unwrap();
        store
            .append_message("dir:main", None, Role::User, "owner reply")
            .unwrap();
        let record = store.get_conversation("dir:main").unwrap().unwrap();
        assert_eq!(record.unread_count, 2);

        assert!(store.mark_conversation_read("dir:main").unwrap());
        let record = store.get_conversation("dir:main").unwrap().unwrap();
        assert_eq!(record.unread_count, 0);

        store
            .append_message("dir:main", None, Role::Assistant, "third")
            .unwrap();
        let listed = store.list_conversations(10, None, false).unwrap();
        let record = listed.iter().find(|entry| entry.id == "dir:main").unwrap();
        assert_eq!(record.unread_count, 1);
        assert!(!store.mark_conversation_read("absent").unwrap());
    }

    #[test]
    fn team_rules_round_trip_and_default_empty() {
        let store = store();
        store
            .create_conversation("room:review", "room", None)
            .unwrap();
        assert!(
            store
                .get_conversation_team("room:review")
                .unwrap()
                .is_none()
        );
        store
            .set_conversation_team(
                "room:review",
                &["analyst".into()],
                "selected",
                "Always cite sources.",
            )
            .unwrap();
        let team = store.get_conversation_team("room:review").unwrap().unwrap();
        assert_eq!(team.rules, "Always cite sources.");
        assert_eq!(team.members, vec!["analyst".to_owned()]);
    }

    #[test]
    fn conversation_library_joins_actions_to_turns() {
        let store = store();
        store.ensure_conversation_accepts_turns("dir:main").unwrap();
        let task = store
            .enqueue_task("turn", "main", &serde_json::json!({}), 0, utc_now())
            .unwrap();
        let turn = store
            .create_turn(&task.id, "main", "dir:main", &serde_json::json!({}))
            .unwrap();
        store
            .create_action(
                "deck_generate",
                Some(&task.id),
                Some(&turn.id),
                "{}",
                "hash-args",
                None,
                Some("abc123"),
                "v1",
            )
            .unwrap();
        store
            .create_action(
                "tavily_search",
                Some(&task.id),
                Some(&turn.id),
                "{}",
                "hash-search",
                None,
                None,
                "v1",
            )
            .unwrap();
        let action = store
            .create_action(
                "deck_render",
                Some(&task.id),
                Some(&turn.id),
                "{}",
                "hash-render",
                None,
                None,
                "v1",
            )
            .unwrap();
        store
            .update_action(
                &action.id,
                "executed",
                Some(&serde_json::json!({"artifact_hash": "def456"})),
            )
            .unwrap();
        let artifacts = store.list_conversation_artifacts("dir:main").unwrap();
        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].hash, "def456");
        assert_eq!(artifacts[0].tool, "deck_render");
        assert_eq!(artifacts[1].hash, "abc123");
        assert!(
            store
                .list_conversation_artifacts("dir:other")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn visible_history_keeps_specialist_names() {
        let store = store();
        store
            .append_message_named(
                "room:review",
                None,
                Role::Assistant,
                "from analyst",
                Some("analyst"),
            )
            .unwrap();
        store
            .append_message("room:review", None, Role::Tool, "hidden tool")
            .unwrap();
        let messages = store.list_visible_messages("room:review", 10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].name.as_deref(), Some("analyst"));
        assert_eq!(messages[0].content, "from analyst");
    }

    #[test]
    fn visible_history_attributes_assistant_messages_to_the_turn_bot() {
        let store = store();
        let task = store
            .enqueue_task("turn", "analyst", &serde_json::json!({}), 0, utc_now())
            .unwrap();
        let turn = store
            .create_turn(&task.id, "analyst", "dir:analyst", &serde_json::json!({}))
            .unwrap();
        store
            .append_message(
                "dir:analyst",
                Some(&turn.id),
                Role::Assistant,
                "from analyst",
            )
            .unwrap();
        let messages = store.list_visible_messages("dir:analyst", 10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].bot.as_deref(), Some("analyst"));
    }

    #[test]
    fn applies_migrations_and_vector_table() {
        let store = store();
        store.health().unwrap();
        let connection = store.connection().unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 7);
        connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='conversations'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='conversation_teams'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='device_tokens'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO chunk_vectors(chunk_id, embedding) VALUES ('x', ?1)",
                params![
                    vec![0.0f32; 384]
                        .iter()
                        .flat_map(|v| v.to_le_bytes())
                        .collect::<Vec<u8>>()
                ],
            )
            .unwrap();
    }

    #[test]
    fn upgrades_a_version_two_database_in_place() {
        let path = std::env::temp_dir().join(format!("serabot-store-v2-{}.db", new_id()));
        {
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection.execute_batch(MIGRATION_1).unwrap();
            connection.execute_batch(MIGRATION_2).unwrap();
            connection.pragma_update(None, "user_version", 2).unwrap();
        }
        let store = Store::open(&path, 384, 5000).unwrap();
        store.health().unwrap();
        let version: i64 = store
            .connection()
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 7);
        assert!(store.list_device_tokens().unwrap().is_empty());
        // The team table arrives with the same migration batch.
        assert!(
            store
                .get_conversation_team("room:review")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn device_tokens_round_trip_without_storing_plaintext() {
        let store = store();
        let expires = utc_now() + chrono::Duration::days(30);
        store
            .insert_device_token("tok-1", "desktop-win", "hash-only", Some(expires))
            .unwrap();
        let found = store.device_token_by_hash("hash-only").unwrap().unwrap();
        assert_eq!(found.id, "tok-1");
        assert_eq!(found.label, "desktop-win");
        assert!(found.expires_at.is_some());
        assert!(found.last_used_at.is_none());
        assert!(found.active_at(utc_now()));
        assert!(store.device_token_by_hash("absent").unwrap().is_none());

        store.touch_device_token("tok-1", utc_now()).unwrap();
        assert!(
            store.list_device_tokens().unwrap()[0]
                .last_used_at
                .is_some()
        );

        assert!(store.revoke_device_token("tok-1").unwrap());
        let revoked = store.device_token_by_hash("hash-only").unwrap().unwrap();
        assert!(revoked.revoked_at.is_some());
        assert!(!revoked.active_at(utc_now()));
        assert!(!store.revoke_device_token("absent").unwrap());
    }

    #[test]
    fn expired_tokens_are_inactive() {
        let store = store();
        store
            .insert_device_token(
                "tok-2",
                "old-laptop",
                "expired-hash",
                Some(utc_now() - chrono::Duration::seconds(1)),
            )
            .unwrap();
        let token = store.device_token_by_hash("expired-hash").unwrap().unwrap();
        assert!(!token.active_at(utc_now()));
    }

    #[test]
    fn conversations_keep_metadata_and_paginate() {
        let store = store();
        store
            .create_conversation("sec:finance", "section", None)
            .unwrap();
        store
            .create_conversation("grp:ops", "group", Some("sec:finance"))
            .unwrap();
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE conversations SET updated_at = '2026-01-01T00:00:00.000Z'",
                [],
            )
            .unwrap();
        store
            .append_message("grp:ops", None, Role::User, "hello")
            .unwrap();

        let all = store.list_conversations(10, None, false).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "grp:ops", "message activity sorts first");
        assert!(all[0].last_message_at.is_some());
        assert!(all[1].title.is_none());

        assert!(store.rename_conversation("grp:ops", "Operations").unwrap());
        assert!(!store.rename_conversation("missing", "x").unwrap());
        let renamed = store.get_conversation("grp:ops").unwrap().unwrap();
        assert_eq!(renamed.title.as_deref(), Some("Operations"));

        let page = store.list_conversations(1, None, false).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, "grp:ops");
        let rest = store
            .list_conversations(10, Some((page[0].updated_at, &page[0].id)), false)
            .unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].id, "sec:finance");

        assert!(store.archive_conversation("grp:ops").unwrap());
        let visible = store.list_conversations(10, None, false).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "sec:finance");
        let with_archived = store.list_conversations(10, None, true).unwrap();
        assert_eq!(with_archived.len(), 2);
        let archived = store.get_conversation("grp:ops").unwrap().unwrap();
        assert!(archived.archived_at.is_some());
        assert_eq!(store.list_visible_messages("grp:ops", 10).unwrap().len(), 1);
    }

    #[test]
    fn claims_competing_tasks_once() {
        let store = store();
        store
            .enqueue_task("turn", "main", &serde_json::json!({"x": 1}), 0, utc_now())
            .unwrap();
        let first = store.claim_task("a", 30).unwrap();
        let second = store.claim_task("b", 30).unwrap();
        assert!(first.is_some());
        assert!(second.is_none());
    }

    #[test]
    fn fencing_prevents_stale_completion() {
        let store = store();
        let task = store
            .enqueue_task("turn", "main", &serde_json::json!({}), 0, utc_now())
            .unwrap();
        let claimed = store.claim_task("worker", 30).unwrap().unwrap();
        assert!(
            !store
                .complete_task(&task.id, claimed.fencing_token + 1)
                .unwrap()
        );
        assert!(
            store
                .complete_task(&task.id, claimed.fencing_token)
                .unwrap()
        );
    }

    #[test]
    fn duplicate_telegram_updates_are_ignored() {
        let store = store();
        assert!(store.record_telegram_update(42).unwrap());
        assert!(!store.record_telegram_update(42).unwrap());
    }

    #[test]
    fn approval_is_single_use_and_expires() {
        let store = store();
        let action = store
            .create_action(
                "onedrive_upload",
                None,
                None,
                "{}",
                "hash",
                Some("folder"),
                None,
                "p1",
            )
            .unwrap();
        let approval = store
            .create_approval(
                &action.id,
                "onedrive_upload",
                "upload",
                utc_now() + chrono::Duration::hours(1),
            )
            .unwrap();
        assert!(
            store
                .transition_approval(&approval.id, "approved_once", None, None)
                .unwrap()
        );
        assert!(
            !store
                .transition_approval(&approval.id, "denied", None, None)
                .unwrap()
        );
    }

    #[test]
    fn expired_approvals_cancel_suspended_tasks() {
        let store = store();
        let task = store
            .enqueue_task("turn", "main", &serde_json::json!({}), 0, utc_now())
            .unwrap();
        let claimed = store.claim_task("worker", 30).unwrap().unwrap();
        store
            .await_approval_task(&task.id, claimed.fencing_token)
            .unwrap();
        let action = store
            .create_action(
                "upload",
                Some(&task.id),
                None,
                "{}",
                "hash",
                Some("folder"),
                None,
                "p1",
            )
            .unwrap();
        let approval = store
            .create_approval(
                &action.id,
                "upload",
                "upload",
                utc_now() - chrono::Duration::minutes(1),
            )
            .unwrap();
        assert_eq!(store.expire_approvals().unwrap(), 1);
        assert_eq!(store.get_approval(&approval.id).unwrap().state, "expired");
        assert_eq!(store.get_action(&action.id).unwrap().status, "expired");
        assert_eq!(store.get_task(&task.id).unwrap().status, "failed");
    }

    #[test]
    fn backup_refuses_to_overwrite() {
        let store = store();
        let destination = std::env::temp_dir().join(format!("serabot-backup-test-{}.db", new_id()));
        store.backup_to(&destination).unwrap();
        assert!(destination.exists());
        assert!(store.backup_to(&destination).is_err());
        let _ = std::fs::remove_file(destination);
    }

    #[test]
    fn uncertain_expired_tasks_are_not_replayed() {
        let store = store();
        let task = store
            .enqueue_task("turn", "main", &serde_json::json!({}), 0, utc_now())
            .unwrap();
        let claimed = store.claim_task("worker", 1).unwrap().unwrap();
        let connection = store.connection().unwrap();
        connection
            .execute(
                "UPDATE tasks SET lease_until=?1, uncertain=1 WHERE id=?2",
                params![to_string(utc_now() - chrono::Duration::seconds(5)), task.id],
            )
            .unwrap();
        drop(connection);
        store.recover_expired_tasks().unwrap();
        let recovered = store.get_task(&task.id).unwrap();
        assert_eq!(recovered.status, "failed");
        assert_eq!(recovered.fencing_token, claimed.fencing_token);
    }

    #[test]
    fn conversation_metadata_rejects_container_turns() {
        let store = store();
        store
            .create_conversation("sec:finance", "section", None)
            .unwrap();
        assert!(
            store
                .ensure_conversation_accepts_turns("sec:finance")
                .is_err()
        );
        assert!(store.ensure_conversation_accepts_turns("grp:ops").is_err());
        assert!(store.get_conversation("grp:ops").unwrap().is_none());
        store.ensure_conversation_accepts_turns("owner").unwrap();
        store.ensure_conversation_accepts_turns("dir:main").unwrap();
        assert_eq!(
            store.get_conversation("owner").unwrap().unwrap().kind,
            "legacy"
        );
        assert_eq!(
            store.get_conversation("dir:main").unwrap().unwrap().kind,
            "direct"
        );
        store.ensure_conversation_accepts_turns("owner").unwrap();
    }

    #[test]
    fn conversation_tree_links_parents_and_rejects_bad_edges() {
        let store = store();
        store
            .create_conversation("sec:finance", "section", None)
            .unwrap();
        store
            .create_conversation("grp:ops", "group", Some("sec:finance"))
            .unwrap();
        store
            .create_conversation("dir:analyst", "direct", Some("grp:ops"))
            .unwrap();
        store
            .create_conversation("room:review", "room", Some("sec:finance"))
            .unwrap();
        assert_eq!(
            store
                .get_conversation("dir:analyst")
                .unwrap()
                .unwrap()
                .parent_id
                .as_deref(),
            Some("grp:ops")
        );
        assert!(
            store
                .ensure_conversation_accepts_turns("sec:finance")
                .is_err()
        );
        assert!(store.ensure_conversation_accepts_turns("grp:ops").is_err());
        store
            .ensure_conversation_accepts_turns("dir:analyst")
            .unwrap();
        store
            .ensure_conversation_accepts_turns("room:review")
            .unwrap();
        assert!(
            store
                .create_conversation("grp:other", "group", None)
                .is_err()
        );
        assert!(
            store
                .create_conversation("sec:nested", "section", Some("sec:finance"))
                .is_err()
        );
        assert!(
            store
                .create_conversation("dir:bad", "direct", Some("dir:analyst"))
                .is_err()
        );
        assert!(
            store
                .create_conversation("sec:finance", "section", None)
                .unwrap_err()
                .to_string()
                .contains("already exists")
        );
    }
}
