//! SQLite/WAL persistence: users, sessions, tasks, turns, conversations,
//! approvals, events — one database behind a bounded pool.
//!
//! **User scoping is a type-level property of this module** (phase 2 of the
//! multi-user reconstruction): every method that reads or writes a
//! user-owned row takes the acting `user_id` as its first parameter and
//! filters on it in SQL. There is no unscoped variant reachable from the
//! runtime, API, or tools — cross-user access is a compile error here, not a
//! runtime check. Only a small set of maintenance helpers (task claiming,
//! lease recovery, approval expiry, routine claiming) works across users;
//! each is documented with the reason it must.
//!
//! Phase 3 adds two more user-owned tables: `bots` (keyed `(user_id, name)`,
//! with the system prompt in the row) and `user_skills` (a user's own skill
//! rows on top of the shipped catalog). Every bot and skill method below is
//! scoped exactly like the rest: the acting user's id is the first argument
//! and an unknown or foreign id is indistinguishable from a missing row.
//!
//! Phase 5 adds the per-user task quota:
//! [`Store::enqueue_task_with_quota`] counts the acting user's tasks for the
//! current UTC calendar day inside the same `BEGIN IMMEDIATE` transaction as
//! the insert. A user at the cap gets an explicit `Validation` error and no
//! row; another user's tasks never count against them. Internal continuations
//! (approval resumes, handoffs, team fan-out) use [`Store::enqueue_task`]
//! because they continue work already counted; admin-configured scheduled
//! routines use it too because they are server work, not user requests.

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rand::RngCore;
use rusqlite::{OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde_json::Value;

use crate::domain::{
    Action, Approval, AuditEntry, Event, Id, LlmMessage, Role, StoredMessage, Task, Turn, new_id,
    utc_now,
};
use crate::error::{Result, SeraError};
use crate::util::sha256_hex;

const MIGRATION_1: &str = include_str!("../migrations/0001_core.sql");
const MIGRATION_2: &str = include_str!("../migrations/0002_conversations.sql");
const MIGRATION_3: &str = include_str!("../migrations/0003_device_tokens.sql");
const MIGRATION_4: &str = include_str!("../migrations/0004_conversation_meta.sql");
const MIGRATION_5: &str = include_str!("../migrations/0005_conversation_teams.sql");
const MIGRATION_6: &str = include_str!("../migrations/0006_console_redesign.sql");
const MIGRATION_7: &str = include_str!("../migrations/0007_conversation_read_rowid.sql");
const MIGRATION_8: &str = include_str!("../migrations/0008_users_and_sessions.sql");
const MIGRATION_9: &str = include_str!("../migrations/0009_user_scoping.sql");
const MIGRATION_10: &str = include_str!("../migrations/0010_user_fleet.sql");
const MIGRATION_11: &str = include_str!("../migrations/0011_user_secrets.sql");
const MIGRATION_12: &str = include_str!("../migrations/0012_user_telegram.sql");
const MIGRATION_13: &str = include_str!("../migrations/0013_conversation_artifacts.sql");

/// Fixed admin id written by migration 0008 and mirrored in
/// `kv['admin_user_id']`. Code resolves the admin through the kv entry (the
/// database is the source of truth); this constant documents the value and
/// lets tests and migrations agree without copying the literal around.
pub const ADMIN_USER_ID: &str = "00000000-0000-7000-8000-000000000001";

/// The acting user's role. The admin sees audit records and manages users,
/// invites, and server configuration; members act only within their own
/// scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    Admin,
    Member,
}

impl UserRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }
}

impl std::str::FromStr for UserRole {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "admin" => Ok(Self::Admin),
            "member" => Ok(Self::Member),
            other => Err(format!("unknown user role: {other}")),
        }
    }
}

/// An account's availability. Disabled users fail closed: their sessions stop
/// authorizing and they cannot sign in until re-enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UserStatus {
    Active,
    Disabled,
}

impl UserStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
        }
    }
}

impl std::str::FromStr for UserStatus {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "active" => Ok(Self::Active),
            "disabled" => Ok(Self::Disabled),
            other => Err(format!("unknown user status: {other}")),
        }
    }
}

/// One account row. `provider_subject` is namespaced per provider
/// (`microsoft:<sub>`, later `google:<sub>`, `github:<sub>`) so adding a
/// provider is configuration, never a schema change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct User {
    pub id: String,
    pub provider_subject: String,
    pub email: String,
    pub display_name: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub created_at: DateTime<Utc>,
}

impl User {
    pub fn is_admin(&self) -> bool {
        self.role == UserRole::Admin
    }
}

/// Result of completing a sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginOutcome {
    pub user: User,
    /// True when this sign-in created the users row and consumed an invite;
    /// an existing subject signing in again needs no invite.
    pub created: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceToken {
    pub id: String,
    pub label: String,
    /// Owning user; the migration backfills existing rows to the admin and
    /// new operator CLI tokens default to the admin as well.
    pub user_id: Option<String>,
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

/// One per-user bot row. `config` is the JSON the runtime parses into
/// `config::BotConfig`; `prompt` is the system prompt text (the store and API
/// write paths cap it at 100 KB). The `enabled` column is authoritative over
/// the copy inside `config`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StoredBot {
    pub name: String,
    pub config: Value,
    pub prompt: String,
    pub enabled: bool,
    pub updated_at: DateTime<Utc>,
}

/// One skill row created by a user. `body` is the instruction text (the
/// markdown after the frontmatter), matching what the shipped catalog exposes
/// through `skill_load`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UserSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub type StoreConnection = PooledConnection<SqliteConnectionManager>;

/// One sealed per-user data key. The plaintext DEK never reaches this layer:
/// `user_secrets` seals it under the master key and opens it again on demand.
/// `Debug` shows lengths only.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredUserKey {
    pub wrapped_dek: Vec<u8>,
    pub nonce: Vec<u8>,
}

impl std::fmt::Debug for StoredUserKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredUserKey")
            .field("wrapped_dek_bytes", &self.wrapped_dek.len())
            .field("nonce_bytes", &self.nonce.len())
            .finish()
    }
}

/// One sealed credential blob. `Debug` shows lengths only; the bytes are
/// ciphertext and are never logged either way.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredCredential {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for StoredCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredCredential")
            .field("ciphertext_bytes", &self.ciphertext.len())
            .field("nonce_bytes", &self.nonce.len())
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

/// Whether the acting user can use a provider, without touching the secret:
/// `own` is their own credential row, `granted` an active admin grant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CredentialStatus {
    pub own: bool,
    pub granted: bool,
}

impl CredentialStatus {
    pub fn usable(self) -> bool {
        self.own || self.granted
    }
}

/// One provider grant row. Grants carry no secret: resolution reads the
/// grantor's credential at call time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderGrant {
    pub grantor: String,
    pub grantee: String,
    pub provider: String,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl ProviderGrant {
    pub fn active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// One Telegram binding row: the chat pair routes to `user_id` while
/// `revoked_at` is `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TelegramBinding {
    pub user_id: String,
    pub telegram_user_id: i64,
    pub telegram_chat_id: i64,
    pub linked_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl TelegramBinding {
    pub fn active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// Token-usage rollup over `model.used` audit rows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UsageRollup {
    pub totals: UsageTotals,
    pub per_day: Vec<UsageDay>,
    pub per_bot: Vec<UsageBot>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UsageTotals {
    pub calls: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UsageDay {
    pub day: String,
    pub calls: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UsageBot {
    pub bot: String,
    pub calls: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

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
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 8 {
            connection.execute_batch(MIGRATION_8)?;
            connection.pragma_update(None, "user_version", 8)?;
        }
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 9 {
            connection.execute_batch(MIGRATION_9)?;
            connection.pragma_update(None, "user_version", 9)?;
        }
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 10 {
            connection.execute_batch(MIGRATION_10)?;
            connection.pragma_update(None, "user_version", 10)?;
        }
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 11 {
            connection.execute_batch(MIGRATION_11)?;
            connection.pragma_update(None, "user_version", 11)?;
        }
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 12 {
            connection.execute_batch(MIGRATION_12)?;
            connection.pragma_update(None, "user_version", 12)?;
        }
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 13 {
            connection.execute_batch(MIGRATION_13)?;
            connection.pragma_update(None, "user_version", 13)?;
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

    /// Insert or replace one bot row for the acting user. Seeding and the
    /// prompt-repair path use this; the API's create path uses
    /// [`Store::insert_bot`] so a duplicate is an explicit conflict.
    pub fn upsert_bot(
        &self,
        user_id: &str,
        name: &str,
        config: &Value,
        prompt: &str,
        enabled: bool,
    ) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO bots(user_id, name, config_json, prompt, enabled, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(user_id, name) DO UPDATE SET
                 config_json=excluded.config_json,
                 prompt=excluded.prompt,
                 enabled=excluded.enabled,
                 updated_at=excluded.updated_at",
            params![
                user_id,
                name,
                serde_json::to_string(config)?,
                prompt,
                enabled as i64,
                now_string()
            ],
        )?;
        Ok(())
    }

    /// Insert a bot row for the acting user. Returns false when the user
    /// already owns a bot with that name; another user's row never conflicts.
    pub fn insert_bot(
        &self,
        user_id: &str,
        name: &str,
        config: &Value,
        prompt: &str,
        enabled: bool,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "INSERT INTO bots(user_id, name, config_json, prompt, enabled, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(user_id, name) DO NOTHING",
            params![
                user_id,
                name,
                serde_json::to_string(config)?,
                prompt,
                enabled as i64,
                now_string()
            ],
        )?;
        Ok(changed == 1)
    }

    /// Replace the behavior config, prompt, and enabled flag of one bot owned
    /// by the acting user. Returns false when the row does not exist or
    /// belongs to another user.
    pub fn update_bot(
        &self,
        user_id: &str,
        name: &str,
        config: &Value,
        prompt: &str,
        enabled: bool,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE bots SET config_json = ?3, prompt = ?4, enabled = ?5, updated_at = ?6
             WHERE user_id = ?1 AND name = ?2",
            params![
                user_id,
                name,
                serde_json::to_string(config)?,
                prompt,
                enabled as i64,
                now_string()
            ],
        )?;
        Ok(changed == 1)
    }

    /// Fill an empty prompt on one of the acting user's bot rows. Used by the
    /// startup repair for rows that migration 0010 backfilled without prompt
    /// text (SQL cannot read the prompt files).
    pub fn set_bot_prompt(&self, user_id: &str, name: &str, prompt: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE bots SET prompt = ?3, updated_at = ?4 WHERE user_id = ?1 AND name = ?2",
            params![user_id, name, prompt, now_string()],
        )?;
        Ok(changed == 1)
    }

    /// Delete one bot owned by the acting user. Returns false when the row
    /// does not exist or belongs to another user.
    pub fn delete_bot(&self, user_id: &str, name: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "DELETE FROM bots WHERE user_id = ?1 AND name = ?2",
            params![user_id, name],
        )?;
        Ok(changed == 1)
    }

    /// The acting user's fleet rows, ordered by bot name.
    pub fn list_bots(&self, user_id: &str) -> Result<Vec<StoredBot>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT name, config_json, prompt, enabled, updated_at
             FROM bots WHERE user_id = ?1 ORDER BY name",
        )?;
        let rows = statement.query_map(params![user_id], stored_bot_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// One bot of the acting user; another user's bot is indistinguishable
    /// from a missing one.
    pub fn get_bot(&self, user_id: &str, name: &str) -> Result<Option<StoredBot>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT name, config_json, prompt, enabled, updated_at
                 FROM bots WHERE user_id = ?1 AND name = ?2",
                params![user_id, name],
                stored_bot_from_row,
            )
            .optional()?)
    }

    pub fn count_bots(&self, user_id: &str) -> Result<usize> {
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM bots WHERE user_id = ?1",
            params![user_id],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as usize)
    }

    /// Insert one user-created skill. Returns false when the acting user
    /// already owns a skill with that id; another user's row never conflicts.
    pub fn insert_user_skill(
        &self,
        user_id: &str,
        id: &str,
        name: &str,
        description: &str,
        body: &str,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let stamp = now_string();
        let changed = connection.execute(
            "INSERT INTO user_skills(user_id, id, name, description, body, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(user_id, id) DO NOTHING",
            params![user_id, id, name, description, body, stamp],
        )?;
        Ok(changed == 1)
    }

    /// The acting user's own skill rows, ordered by id.
    pub fn list_user_skills(&self, user_id: &str) -> Result<Vec<UserSkill>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, name, description, body, created_at, updated_at
             FROM user_skills WHERE user_id = ?1 ORDER BY id",
        )?;
        let rows = statement.query_map(params![user_id], user_skill_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// One user-created skill of the acting user; another user's skill is
    /// indistinguishable from a missing one.
    pub fn get_user_skill(&self, user_id: &str, id: &str) -> Result<Option<UserSkill>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT id, name, description, body, created_at, updated_at
                 FROM user_skills WHERE user_id = ?1 AND id = ?2",
                params![user_id, id],
                user_skill_from_row,
            )
            .optional()?)
    }

    /// Delete one user-created skill of the acting user. Used to roll back a
    /// failed audit write after a successful insert.
    pub fn delete_user_skill(&self, user_id: &str, id: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "DELETE FROM user_skills WHERE user_id = ?1 AND id = ?2",
            params![user_id, id],
        )?;
        Ok(changed == 1)
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

    /// Insert a device token row. Only the SHA-256 hash is ever stored; the
    /// caller keeps the plaintext. `user_id: None` means the operator CLI
    /// minted the token, so it belongs to the admin account.
    pub fn insert_device_token(
        &self,
        id: &str,
        label: &str,
        token_hash: &str,
        user_id: Option<&str>,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        // Resolve before taking a pooled connection: `admin_user_id` needs
        // one itself, and the in-memory pool holds a single connection.
        let user_id = match user_id {
            Some(user_id) => user_id.to_owned(),
            None => self.admin_user_id()?,
        };
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO device_tokens(id, label, token_hash, user_id, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                label,
                token_hash,
                user_id,
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
                "SELECT id, label, user_id, created_at, last_used_at, expires_at, revoked_at
                 FROM device_tokens WHERE token_hash = ?1",
                params![token_hash],
                device_token_from_row,
            )
            .optional()?)
    }

    pub fn list_device_tokens(&self) -> Result<Vec<DeviceToken>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, label, user_id, created_at, last_used_at, expires_at, revoked_at
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

    // ---- Users, invites, and sessions (multi-user phase 1) ----

    /// The bootstrap admin id recorded by migration 0008. The kv entry is the
    /// source of truth so a restored database keeps working without config.
    pub fn admin_user_id(&self) -> Result<String> {
        self.kv_get("admin_user_id")?
            .ok_or_else(|| SeraError::Internal("admin user is missing; run migrations".into()))
    }

    /// The bootstrap admin account, or `None` before migration 0008.
    pub fn admin_user(&self) -> Result<Option<User>> {
        let id = match self.kv_get("admin_user_id")? {
            Some(id) => id,
            None => return Ok(None),
        };
        self.get_user(&id)
    }

    pub fn get_user(&self, id: &str) -> Result<Option<User>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT id, provider_subject, email, display_name, role, status, created_at
                 FROM users WHERE id = ?1",
                params![id],
                user_from_row,
            )
            .optional()?)
    }

    pub fn get_user_by_subject(&self, provider_subject: &str) -> Result<Option<User>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT id, provider_subject, email, display_name, role, status, created_at
                 FROM users WHERE provider_subject = ?1",
                params![provider_subject],
                user_from_row,
            )
            .optional()?)
    }

    /// All accounts, oldest first. Bounded: the deployment is a small trusted
    /// circle, and the cap turns runaway growth into an explicit error.
    pub fn list_users(&self) -> Result<Vec<User>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, provider_subject, email, display_name, role, status, created_at
             FROM users ORDER BY created_at ASC, id LIMIT 500",
        )?;
        let rows = statement.query_map([], user_from_row)?;
        let mut users = Vec::new();
        for row in rows {
            users.push(row?);
        }
        Ok(users)
    }

    pub fn user_count(&self) -> Result<usize> {
        let connection = self.connection()?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
        Ok(count.max(0) as usize)
    }

    /// Insert a member account. The invite must already be consumed: the only
    /// authenticated creation path is [`Store::complete_login`], which keeps
    /// invite redemption and row creation one transaction.
    pub fn create_user(
        &self,
        provider_subject: &str,
        email: &str,
        display_name: &str,
    ) -> Result<User> {
        validate_user_fields(provider_subject, email, display_name)?;
        let user = User {
            id: new_id(),
            provider_subject: provider_subject.to_owned(),
            email: email.to_owned(),
            display_name: display_name.to_owned(),
            role: UserRole::Member,
            status: UserStatus::Active,
            created_at: utc_now(),
        };
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO users(id, provider_subject, email, display_name, role, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                user.id,
                user.provider_subject,
                user.email,
                user.display_name,
                user.role.as_str(),
                user.status.as_str(),
                to_string(user.created_at),
            ],
        )?;
        Ok(user)
    }

    /// Enable or disable an account. Returns false only when the id is
    /// unknown. A disabled user's existing sessions stop authorizing
    /// immediately (`session_user` checks the status), and they cannot sign
    /// in again until re-enabled.
    pub fn set_user_status(&self, id: &str, status: UserStatus) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE users SET status = ?2 WHERE id = ?1",
            params![id, status.as_str()],
        )?;
        Ok(changed > 0)
    }

    /// Store an invitation. The raw code is passed once and only its SHA-256
    /// hash is stored; the caller shows the raw code to the invitee exactly
    /// once. `uses` is bounded so a leaked code cannot mint an unbounded
    /// number of accounts.
    pub fn mint_invite(
        &self,
        code: &str,
        uses: u32,
        expires_at: DateTime<Utc>,
        created_by: &str,
    ) -> Result<()> {
        let code = code.trim();
        if code.is_empty() || code.len() > 256 || code.chars().any(char::is_control) {
            return Err(SeraError::Validation(
                "invite code must be 1-256 characters without control characters".into(),
            ));
        }
        if uses == 0 || uses > 100 {
            return Err(SeraError::Validation(
                "invite uses must be between 1 and 100".into(),
            ));
        }
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO invites(code_hash, uses_left, expires_at, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                sha256_hex(code),
                uses as i64,
                to_string(expires_at),
                created_by,
                now_string()
            ],
        )?;
        Ok(())
    }

    /// Whether an invite can still be redeemed right now. Checked at
    /// `login_start` so a stale code never reaches the identity provider;
    /// the authoritative consumption happens in [`Store::complete_login`].
    pub fn invite_is_redeemable(&self, code_hash: &str) -> Result<bool> {
        let connection = self.connection()?;
        let found = connection
            .query_row(
                "SELECT 1 FROM invites
                 WHERE code_hash = ?1 AND uses_left > 0 AND expires_at > ?2",
                params![code_hash, now_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Consume one use of an invite, transactionally. Fails closed when the
    /// invite is unknown, exhausted, or expired; concurrent consumers race on
    /// the conditional update, so exactly one of them wins.
    pub fn consume_invite(&self, code_hash: &str) -> Result<()> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE invites SET uses_left = uses_left - 1
             WHERE code_hash = ?1 AND uses_left > 0 AND expires_at > ?2",
            params![code_hash, now_string()],
        )?;
        if changed == 0 {
            return Err(SeraError::InvalidState(
                "invitation is unknown, expired, or exhausted".into(),
            ));
        }
        Ok(())
    }

    /// Upsert the account for a verified OIDC subject, atomically.
    ///
    /// Existing subject: the profile fields the provider reported are
    /// refreshed and no invite is touched. New subject: the user cap is
    /// checked, one invite use is consumed, and the row is created inside a
    /// single transaction — a concurrent double-spend of the same invite
    /// fails closed for the loser, and the cap cannot be raced past.
    pub fn complete_login(
        &self,
        provider_subject: &str,
        email: &str,
        display_name: &str,
        invite_code_hash: Option<&str>,
        max_users: usize,
    ) -> Result<LoginOutcome> {
        validate_user_fields(provider_subject, email, display_name)?;
        if max_users == 0 {
            return Err(SeraError::InvalidState(
                "the user limit is zero; sign-in is closed".into(),
            ));
        }
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction()?;
        if let Some(existing) = transaction
            .query_row(
                "SELECT id, provider_subject, email, display_name, role, status, created_at
                 FROM users WHERE provider_subject = ?1",
                params![provider_subject],
                user_from_row,
            )
            .optional()?
        {
            transaction.execute(
                "UPDATE users SET email = ?2, display_name = ?3 WHERE id = ?1",
                params![existing.id, email, display_name],
            )?;
            transaction.commit()?;
            return Ok(LoginOutcome {
                user: User {
                    email: email.to_owned(),
                    display_name: display_name.to_owned(),
                    ..existing
                },
                created: false,
            });
        }
        let count: i64 =
            transaction.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
        if count.max(0) as usize >= max_users {
            return Err(SeraError::InvalidState(format!(
                "the user limit ({max_users}) is reached; ask the admin to raise it"
            )));
        }
        let Some(invite_code_hash) = invite_code_hash else {
            return Err(SeraError::Forbidden(
                "an invitation is required to create an account".into(),
            ));
        };
        let changed = transaction.execute(
            "UPDATE invites SET uses_left = uses_left - 1
             WHERE code_hash = ?1 AND uses_left > 0 AND expires_at > ?2",
            params![invite_code_hash, now_string()],
        )?;
        if changed == 0 {
            return Err(SeraError::InvalidState(
                "invitation is unknown, expired, or exhausted".into(),
            ));
        }
        let user = User {
            id: new_id(),
            provider_subject: provider_subject.to_owned(),
            email: email.to_owned(),
            display_name: display_name.to_owned(),
            role: UserRole::Member,
            status: UserStatus::Active,
            created_at: utc_now(),
        };
        transaction.execute(
            "INSERT INTO users(id, provider_subject, email, display_name, role, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                user.id,
                user.provider_subject,
                user.email,
                user.display_name,
                user.role.as_str(),
                user.status.as_str(),
                to_string(user.created_at),
            ],
        )?;
        transaction.commit()?;
        Ok(LoginOutcome {
            user,
            created: true,
        })
    }

    /// Session token length in bytes; the hex token the caller sees is twice
    /// this. Same shape as the operator CLI device tokens.
    pub const SESSION_TOKEN_BYTES: usize = 32;
    /// Sessions are bounded to five years so a stray long TTL cannot mint a
    /// effectively permanent credential.
    pub const MAX_SESSION_TTL: chrono::Duration = chrono::Duration::days(5 * 365);
    /// Label stored on session rows so `serabot token list` can tell them
    /// apart from operator CLI tokens.
    pub const SESSION_LABEL: &str = "session";

    /// Mint an opaque session for `user_id` and return the raw token exactly
    /// once. Only the SHA-256 hash is stored; nothing else in the process
    /// keeps the plaintext. Fails closed for unknown or disabled users.
    pub fn mint_session(&self, user_id: &str, ttl: chrono::Duration) -> Result<String> {
        let user = self
            .get_user(user_id)?
            .ok_or_else(|| SeraError::NotFound(format!("user {user_id}")))?;
        if user.status != UserStatus::Active {
            return Err(SeraError::InvalidState(
                "this account is disabled; session was not issued".into(),
            ));
        }
        if ttl <= chrono::Duration::zero() || ttl > Self::MAX_SESSION_TTL {
            return Err(SeraError::Validation(
                "session ttl must be positive and at most five years".into(),
            ));
        }
        let mut bytes = [0u8; Self::SESSION_TOKEN_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let token = hex::encode(bytes);
        let expires_at = utc_now() + ttl;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO device_tokens(id, label, token_hash, user_id, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                new_id(),
                Self::SESSION_LABEL,
                sha256_hex(&token),
                user_id,
                now_string(),
                to_string(expires_at)
            ],
        )?;
        Ok(token)
    }

    /// Resolve a session token hash to its active user. Fails closed: no
    /// row, a revoked or expired session, or a disabled account all yield
    /// `None` — never another user.
    pub fn session_user(&self, token_hash: &str) -> Result<Option<User>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT u.id, u.provider_subject, u.email, u.display_name, u.role, u.status, u.created_at
                 FROM device_tokens d JOIN users u ON u.id = d.user_id
                 WHERE d.token_hash = ?1
                   AND d.revoked_at IS NULL
                   AND (d.expires_at IS NULL OR d.expires_at > ?2)
                   AND u.status = 'active'",
                params![token_hash, now_string()],
                user_from_row,
            )
            .optional()?)
    }

    /// Revoke a session by token hash. Returns false only when the hash is
    /// unknown; revoking twice stays idempotent.
    pub fn revoke_session(&self, token_hash: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE device_tokens SET revoked_at = COALESCE(revoked_at, ?2) WHERE token_hash = ?1",
            params![token_hash, now_string()],
        )?;
        Ok(changed > 0)
    }

    // ---- Per-user credentials and provider grants (multi-user phase 4) ----
    //
    // These methods store and return *sealed* bytes only. Encryption and
    // decryption live in `user_secrets`; nothing here ever sees a plaintext
    // credential, and every method takes the acting user's id.

    /// The acting user's sealed data key, or `None` before the first write.
    pub fn user_key(&self, user_id: &str) -> Result<Option<StoredUserKey>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT wrapped_dek, nonce FROM user_keys WHERE user_id = ?1",
                params![user_id],
                |row| {
                    Ok(StoredUserKey {
                        wrapped_dek: row.get(0)?,
                        nonce: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// Insert the acting user's sealed data key. A concurrent insert wins
    /// once: the caller re-reads and uses the stored key (never its own
    /// losing copy), so two racing writes cannot produce two DEKs.
    pub fn put_user_key(&self, user_id: &str, wrapped_dek: &[u8], nonce: &[u8]) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO user_keys(user_id, wrapped_dek, nonce, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id) DO NOTHING",
            params![user_id, wrapped_dek, nonce, now_string()],
        )?;
        Ok(())
    }

    /// The acting user's sealed credential for `provider`.
    pub fn user_credential(
        &self,
        user_id: &str,
        provider: &str,
    ) -> Result<Option<StoredCredential>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT ciphertext, nonce, updated_at FROM user_credentials
                 WHERE user_id = ?1 AND provider = ?2",
                params![user_id, provider],
                |row| {
                    Ok(StoredCredential {
                        ciphertext: row.get(0)?,
                        nonce: row.get(1)?,
                        updated_at: parse_datetime(&row.get::<_, String>(2)?),
                    })
                },
            )
            .optional()?)
    }

    /// Insert or replace the acting user's sealed credential for `provider`.
    pub fn put_user_credential(
        &self,
        user_id: &str,
        provider: &str,
        ciphertext: &[u8],
        nonce: &[u8],
    ) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO user_credentials(user_id, provider, ciphertext, nonce, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(user_id, provider) DO UPDATE SET
                 ciphertext = excluded.ciphertext,
                 nonce = excluded.nonce,
                 updated_at = excluded.updated_at",
            params![user_id, provider, ciphertext, nonce, now_string()],
        )?;
        Ok(())
    }

    /// Delete the acting user's credential for `provider`. Returns false when
    /// the acting user has no such row; another user's row is never touched.
    pub fn delete_user_credential(&self, user_id: &str, provider: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "DELETE FROM user_credentials WHERE user_id = ?1 AND provider = ?2",
            params![user_id, provider],
        )?;
        Ok(changed == 1)
    }

    /// The acting user's own credential provider names, sorted. Names only:
    /// no ciphertext, no value.
    pub fn list_user_credential_providers(&self, user_id: &str) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT provider FROM user_credentials WHERE user_id = ?1 ORDER BY provider LIMIT 256",
        )?;
        let rows = statement.query_map(params![user_id], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Whether the acting user has their own credential and/or an active
    /// admin grant for `provider`. Row existence only; nothing is decrypted.
    pub fn credential_status(&self, user_id: &str, provider: &str) -> Result<CredentialStatus> {
        let connection = self.connection()?;
        let own: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM user_credentials WHERE user_id = ?1 AND provider = ?2)",
            params![user_id, provider],
            |row| row.get(0),
        )?;
        let granted: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM provider_grants
                           WHERE grantee = ?1 AND provider = ?2 AND revoked_at IS NULL)",
            params![user_id, provider],
            |row| row.get(0),
        )?;
        Ok(CredentialStatus { own, granted })
    }

    /// Record (or re-activate) an admin grant of `provider` to `grantee`.
    ///
    /// The grantor must be the bootstrap admin: the product rule is that only
    /// the admin shares an admin-owned provider, and enforcing it here keeps
    /// the rule true for every caller. A self-grant is rejected.
    pub fn upsert_provider_grant(
        &self,
        grantor: &str,
        grantee: &str,
        provider: &str,
    ) -> Result<ProviderGrant> {
        if grantor == grantee {
            return Err(SeraError::Validation(
                "a provider cannot be granted to its own owner".into(),
            ));
        }
        if grantor != self.admin_user_id()? {
            return Err(SeraError::Forbidden(
                "only the admin can grant a provider".into(),
            ));
        }
        let target = self
            .get_user(grantee)?
            .ok_or_else(|| SeraError::NotFound(format!("user {grantee}")))?;
        if target.status != UserStatus::Active {
            return Err(SeraError::InvalidState(
                "the grantee account is disabled".into(),
            ));
        }
        let created_at = now_string();
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO provider_grants(grantor, grantee, provider, created_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT(grantor, grantee, provider) DO UPDATE SET
                 created_at = excluded.created_at,
                 revoked_at = NULL",
            params![grantor, grantee, provider, created_at],
        )?;
        Ok(ProviderGrant {
            grantor: grantor.to_owned(),
            grantee: grantee.to_owned(),
            provider: provider.to_owned(),
            created_at: parse_datetime(&created_at),
            revoked_at: None,
        })
    }

    /// Revoke one grant. Returns false when it was already revoked or never
    /// existed; the row is kept for the audit trail.
    pub fn revoke_provider_grant(
        &self,
        grantor: &str,
        grantee: &str,
        provider: &str,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE provider_grants SET revoked_at = ?4
             WHERE grantor = ?1 AND grantee = ?2 AND provider = ?3 AND revoked_at IS NULL",
            params![grantor, grantee, provider, now_string()],
        )?;
        Ok(changed == 1)
    }

    /// The active grantor of `provider` for `grantee`, if the admin granted
    /// it. Used only to resolve a credential and to route rotation back to
    /// the owner; the id never reaches an API response.
    pub fn active_grant_grantor(
        &self,
        grantor: &str,
        grantee: &str,
        provider: &str,
    ) -> Result<Option<String>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT grantor FROM provider_grants
                 WHERE grantor = ?1 AND grantee = ?2 AND provider = ?3 AND revoked_at IS NULL",
                params![grantor, grantee, provider],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Every grant the grantor has ever made, revoked ones included, newest
    /// first. Admin-facing: carries no secret.
    pub fn list_provider_grants(&self, grantor: &str) -> Result<Vec<ProviderGrant>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT grantor, grantee, provider, created_at, revoked_at
             FROM provider_grants WHERE grantor = ?1
             ORDER BY created_at DESC, grantee, provider LIMIT 500",
        )?;
        let rows = statement.query_map(params![grantor], provider_grant_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The active grants a user holds, sorted by provider. Names only.
    pub fn list_active_grants_for(&self, grantee: &str) -> Result<Vec<ProviderGrant>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT grantor, grantee, provider, created_at, revoked_at
             FROM provider_grants WHERE grantee = ?1 AND revoked_at IS NULL
             ORDER BY provider LIMIT 256",
        )?;
        let rows = statement.query_map(params![grantee], provider_grant_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ---- Telegram bindings (multi-user phase 4) ----

    /// The active binding for a Telegram (user, chat) pair, if any. An
    /// unbound pair is indistinguishable from a revoked one: both are `None`.
    pub fn telegram_binding(
        &self,
        telegram_user_id: i64,
        telegram_chat_id: i64,
    ) -> Result<Option<TelegramBinding>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT user_id, telegram_user_id, telegram_chat_id, linked_at, revoked_at
                 FROM user_telegram
                 WHERE telegram_user_id = ?1 AND telegram_chat_id = ?2 AND revoked_at IS NULL",
                params![telegram_user_id, telegram_chat_id],
                telegram_binding_from_row,
            )
            .optional()?)
    }

    /// The acting user's active binding, if any.
    pub fn telegram_binding_for_user(&self, user_id: &str) -> Result<Option<TelegramBinding>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT user_id, telegram_user_id, telegram_chat_id, linked_at, revoked_at
                 FROM user_telegram WHERE user_id = ?1 AND revoked_at IS NULL",
                params![user_id],
                telegram_binding_from_row,
            )
            .optional()?)
    }

    /// Bind a Telegram pair to the acting user, replacing their own revoked
    /// row. A pair already bound to another account fails closed.
    pub fn link_telegram(
        &self,
        user_id: &str,
        telegram_user_id: i64,
        telegram_chat_id: i64,
    ) -> Result<TelegramBinding> {
        if telegram_user_id <= 0 || telegram_chat_id == 0 {
            return Err(SeraError::Validation(
                "invalid Telegram user or chat id".into(),
            ));
        }
        let user = self
            .get_user(user_id)?
            .ok_or_else(|| SeraError::NotFound(format!("user {user_id}")))?;
        if user.status != UserStatus::Active {
            return Err(SeraError::InvalidState(
                "this account is disabled; Telegram was not linked".into(),
            ));
        }
        if let Some(existing) = self.telegram_binding(telegram_user_id, telegram_chat_id)?
            && existing.user_id != user_id
        {
            return Err(SeraError::Validation(
                "this Telegram account is already linked to another account".into(),
            ));
        }
        let linked_at = now_string();
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO user_telegram(user_id, telegram_user_id, telegram_chat_id, linked_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT(user_id) DO UPDATE SET
                 telegram_user_id = excluded.telegram_user_id,
                 telegram_chat_id = excluded.telegram_chat_id,
                 linked_at = excluded.linked_at,
                 revoked_at = NULL",
            params![user_id, telegram_user_id, telegram_chat_id, linked_at],
        )?;
        Ok(TelegramBinding {
            user_id: user_id.to_owned(),
            telegram_user_id,
            telegram_chat_id,
            linked_at: parse_datetime(&linked_at),
            revoked_at: None,
        })
    }

    /// Revoke the acting user's binding. Returns false when none is active.
    pub fn unlink_telegram(&self, user_id: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE user_telegram SET revoked_at = ?2
             WHERE user_id = ?1 AND revoked_at IS NULL",
            params![user_id, now_string()],
        )?;
        Ok(changed == 1)
    }

    /// The chat id of the acting user's active binding, if any.
    pub fn telegram_chat_for(&self, user_id: &str) -> Result<Option<i64>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT telegram_chat_id FROM user_telegram
                 WHERE user_id = ?1 AND revoked_at IS NULL",
                params![user_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Store a hashed, single-use Telegram linking code for `user_id`.
    ///
    /// Codes live in `kv` (server-level state, not user data) under
    /// `telegram_link:<sha256>`, so the polling loop and the API share them
    /// without a second in-memory service. Expired codes are pruned here; the
    /// raw code is never stored.
    pub fn put_telegram_link_code(
        &self,
        code_hash: &str,
        user_id: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<()> {
        self.prune_telegram_link_codes()?;
        let value = serde_json::to_string(&TelegramLinkCode {
            user_id: user_id.to_owned(),
            expires_at,
        })?;
        self.kv_set(&telegram_link_key(code_hash), &value)
    }

    /// Consume a Telegram linking code exactly once. Returns the user id, or
    /// `None` for an unknown, expired, or already-used code.
    pub fn take_telegram_link_code(&self, code_hash: &str) -> Result<Option<String>> {
        let key = telegram_link_key(code_hash);
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let value: Option<String> = transaction
            .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(value) = value else {
            return Ok(None);
        };
        transaction.execute("DELETE FROM kv WHERE key = ?1", params![key])?;
        transaction.commit()?;
        let code: TelegramLinkCode = serde_json::from_str(&value)
            .map_err(|_| SeraError::InvalidState("stored Telegram link code is invalid".into()))?;
        if code.expires_at <= Utc::now() {
            return Ok(None);
        }
        Ok(Some(code.user_id))
    }

    /// Drop expired linking codes. Bounded: a deployment has at most a
    /// handful of pending codes, and the scan never touches user data.
    fn prune_telegram_link_codes(&self) -> Result<()> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT key, value FROM kv WHERE key LIKE 'telegram_link:%' LIMIT 200")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut expired = Vec::new();
        for row in rows {
            let (key, value) = row?;
            match serde_json::from_str::<TelegramLinkCode>(&value) {
                Ok(code) if code.expires_at <= Utc::now() => expired.push(key),
                Ok(_) => {}
                // A malformed row can never be redeemed; drop it.
                Err(_) => expired.push(key),
            }
        }
        for key in expired {
            connection.execute("DELETE FROM kv WHERE key = ?1", params![key])?;
        }
        Ok(())
    }

    /// Append one stored message to the acting user's conversation.
    ///
    /// Fails closed when the conversation id is owned by another user: the
    /// message is never written, and the error is indistinguishable from an
    /// unknown conversation. A conversation id with no row yet is allowed
    /// (lazy creation — `ensure_conversation_accepts_turns` creates it under
    /// the acting user on first use).
    pub fn insert_message(&self, user_id: &str, message: &StoredMessage) -> Result<()> {
        self.guard_conversation_writable(user_id, &message.conversation_id)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO messages(id, conversation_id, turn_id, role, content, name, tool_call_id, created_at, user_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                message.id,
                message.conversation_id,
                message.turn_id,
                message.role.as_str(),
                message.content,
                message.name,
                message.tool_call_id,
                to_string(message.created_at),
                user_id,
            ],
        )?;
        connection.execute(
            "UPDATE conversations SET updated_at = ?3 WHERE id = ?1 AND user_id = ?2",
            params![
                message.conversation_id,
                user_id,
                to_string(message.created_at)
            ],
        )?;
        Ok(())
    }

    pub fn append_message(
        &self,
        user_id: &str,
        conversation_id: &str,
        turn_id: Option<&str>,
        role: Role,
        content: &str,
    ) -> Result<StoredMessage> {
        self.append_message_named(user_id, conversation_id, turn_id, role, content, None)
    }

    pub fn append_message_named(
        &self,
        user_id: &str,
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
        self.insert_message(user_id, &message)?;
        Ok(message)
    }

    pub fn create_conversation(
        &self,
        user_id: &str,
        id: &str,
        kind: &str,
        parent_id: Option<&str>,
    ) -> Result<()> {
        self.create_conversation_titled(user_id, id, kind, parent_id, None)
    }

    pub fn create_conversation_titled(
        &self,
        user_id: &str,
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
            let parent_row = self.get_conversation(user_id, parent)?;
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
                "INSERT INTO conversations(id, kind, parent_id, title, user_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![id, kind, parent_id, title, user_id, now_string()],
            )
            .map_err(map_conversation_write)?;
        Ok(())
    }

    pub fn get_conversation(&self, user_id: &str, id: &str) -> Result<Option<StoredConversation>> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT c.id, c.kind, c.parent_id, c.title, c.created_at, c.updated_at,
                        c.archived_at,
                        (SELECT MAX(m.created_at) FROM messages m
                          WHERE m.conversation_id = c.id AND m.user_id = ?2),
                        (SELECT COUNT(*) FROM messages m
                          WHERE m.conversation_id = c.id AND m.user_id = ?2
                           AND m.role = 'assistant'
                           AND (c.last_read_rowid IS NULL OR m.rowid > c.last_read_rowid))
                 FROM conversations c WHERE c.id = ?1 AND c.user_id = ?2",
                params![id, user_id],
                conversation_from_row,
            )
            .optional()
            .map_err(SeraError::from)
    }

    pub fn list_conversations(
        &self,
        user_id: &str,
        limit: usize,
        before: Option<(DateTime<Utc>, &str)>,
        include_archived: bool,
    ) -> Result<Vec<StoredConversation>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT c.id, c.kind, c.parent_id, c.title, c.created_at, c.updated_at,
                    c.archived_at,
                    (SELECT MAX(m.created_at) FROM messages m
                      WHERE m.conversation_id = c.id AND m.user_id = ?2),
                    (SELECT COUNT(*) FROM messages m
                      WHERE m.conversation_id = c.id AND m.user_id = ?2
                       AND m.role = 'assistant'
                       AND (c.last_read_rowid IS NULL OR m.rowid > c.last_read_rowid))
             FROM conversations c
             WHERE c.user_id = ?2
               AND (?4 = 1 OR c.archived_at IS NULL)
               AND (?5 IS NULL OR c.updated_at < ?5 OR (c.updated_at = ?5 AND c.id < ?6))
             ORDER BY c.updated_at DESC, c.id DESC
             LIMIT ?1",
        )?;
        let cursor = before.as_ref().map(|(at, _)| to_string(*at));
        let cursor_id = before.as_ref().map(|(_, id)| *id);
        let rows = statement.query_map(
            params![
                limit.clamp(1, 200) as i64,
                user_id,
                user_id,
                include_archived as i64,
                cursor,
                cursor_id
            ],
            conversation_from_row,
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Returns false when the conversation does not exist or belongs to
    /// another user; the two cases are indistinguishable to the caller.
    pub fn rename_conversation(&self, user_id: &str, id: &str, title: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE conversations SET title = ?3, updated_at = ?4 WHERE id = ?1 AND user_id = ?2",
            params![id, user_id, title, now_string()],
        )?;
        Ok(changed == 1)
    }

    /// The acting user opened the conversation: assistant messages up to now
    /// count as read. Returns false when the id is unknown or not theirs.
    pub fn mark_conversation_read(&self, user_id: &str, id: &str) -> Result<bool> {
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
                     WHERE m.conversation_id = ?1 AND m.user_id = ?2
                )
              WHERE id = ?1 AND user_id = ?2",
            params![id, user_id],
        )?;
        Ok(changed == 1)
    }

    /// Soft archive: messages are kept. Returns false when the id is unknown
    /// or not the acting user's.
    pub fn archive_conversation(&self, user_id: &str, id: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE conversations
                SET archived_at = COALESCE(archived_at, ?3), updated_at = ?3
              WHERE id = ?1 AND user_id = ?2",
            params![id, user_id, now_string()],
        )?;
        Ok(changed == 1)
    }

    pub fn unarchive_conversation(&self, user_id: &str, id: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE conversations SET archived_at = NULL, updated_at = ?3
             WHERE id = ?1 AND user_id = ?2",
            params![id, user_id, now_string()],
        )?;
        Ok(changed == 1)
    }

    /// Reject container conversations; lazily record turn-capable scopes on
    /// first use, under the acting user. A conversation id that already
    /// exists under another user fails closed with an explicit error — the
    /// ids form a global namespace (the primary key), so this is the
    /// cross-user collision path.
    pub fn ensure_conversation_accepts_turns(&self, user_id: &str, id: &str) -> Result<()> {
        use crate::orchestration::{parse_conversation_scope, validate_conversation_id};
        validate_conversation_id(id)?;
        let scope = parse_conversation_scope(id)?;
        if let Some(record) = self.get_conversation(user_id, id)? {
            let kind = parse_stored_conversation_kind(&record.kind)?;
            if !kind.accepts_turns() {
                return Err(SeraError::Validation(
                    "conversation cannot accept turns".into(),
                ));
            }
            return Ok(());
        }
        if self.conversation_exists(id)? {
            return Err(SeraError::InvalidState(
                "conversation id is not available".into(),
            ));
        }
        if !scope.kind.accepts_turns() {
            return Err(SeraError::Validation(
                "conversation cannot accept turns".into(),
            ));
        }
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO conversations(id, kind, parent_id, user_id, created_at, updated_at)
             VALUES (?1, ?2, NULL, ?3, ?4, ?4)
             ON CONFLICT(id) DO NOTHING",
            params![id, conversation_kind_str(scope.kind), user_id, now_string()],
        )?;
        Ok(())
    }

    /// Whether any user owns this conversation id. A namespace guard, never a
    /// data access path: it answers only "is this id taken", and exists so
    /// lazy creation and cross-user writes can fail closed instead of
    /// silently attributing rows to the wrong account.
    pub fn conversation_exists(&self, id: &str) -> Result<bool> {
        let connection = self.connection()?;
        let found = connection
            .query_row(
                "SELECT 1 FROM conversations WHERE id = ?1",
                params![id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Fail closed when the conversation id belongs to another user. Used by
    /// the message/turn writers; answers nothing about other users' content.
    fn guard_conversation_writable(&self, user_id: &str, conversation_id: &str) -> Result<()> {
        let connection = self.connection()?;
        let owner: Option<String> = connection
            .query_row(
                "SELECT user_id FROM conversations WHERE id = ?1",
                params![conversation_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(owner) = owner
            && owner != user_id
        {
            return Err(SeraError::NotFound(format!(
                "conversation {conversation_id}"
            )));
        }
        Ok(())
    }

    /// Team attached to a room conversation, if the acting user's room has
    /// one. Reached only through the user-scoped conversation row.
    pub fn get_conversation_team(
        &self,
        user_id: &str,
        conversation_id: &str,
    ) -> Result<Option<ConversationTeam>> {
        let connection = self.connection()?;
        let row: Option<(String, String, String, String, String)> = connection
            .query_row(
                "SELECT ct.conversation_id, ct.members, ct.mode, ct.rules, ct.updated_at
                 FROM conversation_teams ct
                 JOIN conversations c ON c.id = ct.conversation_id
                 WHERE ct.conversation_id = ?1 AND c.user_id = ?2",
                params![conversation_id, user_id],
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

    /// Attach a team to the acting user's room conversation. The conversation
    /// must exist and belong to the acting user (no implicit creation).
    pub fn set_conversation_team(
        &self,
        user_id: &str,
        conversation_id: &str,
        members: &[String],
        mode: &str,
        rules: &str,
    ) -> Result<()> {
        if self.get_conversation(user_id, conversation_id)?.is_none() {
            return Err(SeraError::NotFound(format!(
                "conversation {conversation_id}"
            )));
        }
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
    /// `team.replied` decision. Not user-filtered by design: the audit ledger
    /// is global (the admin's visibility surface) and the root task id is an
    /// unguessable uuidv7 held only by the turn that owns it.
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

    /// Peer replies one bot already handed to peers in one room turn. Audit
    /// derived, like [`Store::count_team_events`]: the ledger is global and
    /// the root task id identifies the turn, not a user.
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

    /// Team turns of one room turn that are still queued or running, within
    /// the acting user's rows.
    pub fn count_pending_team_tasks(&self, user_id: &str, root_task_id: &str) -> Result<i64> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT COUNT(*) FROM tasks
             WHERE user_id = ?1
               AND status IN ('queued','running','awaiting_approval')
               AND json_extract(payload_json, '$.team.root_task_id') = ?2",
            params![user_id, root_task_id],
            |row| row.get(0),
        )?)
    }

    pub fn list_messages(
        &self,
        user_id: &str,
        conversation_id: &str,
        limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, conversation_id, turn_id, role, content, name, tool_call_id, created_at
             FROM messages
             WHERE conversation_id = ?1 AND user_id = ?2
             ORDER BY created_at DESC, rowid DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(params![conversation_id, user_id, limit as i64], |row| {
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

    /// Record an artifact produced by a tool inside the acting user's
    /// conversation.
    ///
    /// The gateway `actions` table only holds tools that asked for approval, so
    /// a document built by a non-external tool would otherwise never appear in
    /// the owner's library. This row is written for every tool that returns an
    /// artifact hash, and the listing below unions both sources. The row is
    /// scoped to the acting user, so one member's library never sees another's.
    #[allow(clippy::too_many_arguments)]
    pub fn record_conversation_artifact(
        &self,
        user_id: &str,
        conversation_id: &str,
        turn_id: Option<&str>,
        tool: &str,
        hash: &str,
        filename: &str,
        title: Option<&str>,
    ) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO conversation_artifacts
                 (hash, conversation_id, user_id, turn_id, tool, filename, title, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(hash, conversation_id, user_id) DO UPDATE SET
                 turn_id = excluded.turn_id,
                 tool = excluded.tool,
                 filename = excluded.filename,
                 title = excluded.title,
                 created_at = excluded.created_at",
            params![
                hash,
                conversation_id,
                user_id,
                turn_id,
                tool,
                filename,
                title,
                now_string(),
            ],
        )?;
        Ok(())
    }

    /// Artifacts produced inside the acting user's conversation, newest
    /// first. Joins gateway actions to their turn; the hash may be recorded
    /// at approval time or in the execution outcome, so both are checked.
    /// Artifacts recorded by non-approval tools are merged in from
    /// `conversation_artifacts`.
    pub fn list_conversation_artifacts(
        &self,
        user_id: &str,
        conversation_id: &str,
    ) -> Result<Vec<StoredConversationArtifact>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT a.id, a.tool, a.created_at,
                    COALESCE(a.artifact_hash, json_extract(a.outcome_json, '$.artifact_hash'))
             FROM actions a
             JOIN turns t ON t.id = a.turn_id
             WHERE t.conversation_id = ?1 AND t.user_id = ?2
               AND COALESCE(a.artifact_hash, json_extract(a.outcome_json, '$.artifact_hash')) IS NOT NULL
             ORDER BY a.created_at DESC, a.rowid DESC
             LIMIT 100",
        )?;
        let rows = statement.query_map(params![conversation_id, user_id], |row| {
            Ok(StoredConversationArtifact {
                action_id: row.get(0)?,
                tool: row.get(1)?,
                created_at: parse_datetime(&row.get::<_, String>(2)?),
                hash: row.get(3)?,
            })
        })?;
        let mut artifacts = rows.collect::<std::result::Result<Vec<_>, _>>()?;

        let mut statement = connection.prepare(
            "SELECT hash, tool, created_at
             FROM conversation_artifacts
             WHERE conversation_id = ?1 AND user_id = ?2
             ORDER BY created_at DESC
             LIMIT 100",
        )?;
        let rows = statement.query_map(params![conversation_id, user_id], |row| {
            Ok(StoredConversationArtifact {
                action_id: row.get(0)?,
                tool: row.get(1)?,
                created_at: parse_datetime(&row.get::<_, String>(2)?),
                hash: row.get(0)?,
            })
        })?;
        let recorded = rows.collect::<std::result::Result<Vec<_>, _>>()?;

        // An approval row is the better record when both exist: it carries the
        // real action id the console uses. Keep it and add the rest.
        let mut seen = artifacts
            .iter()
            .map(|entry| entry.hash.clone())
            .collect::<std::collections::HashSet<_>>();
        for entry in recorded {
            if seen.insert(entry.hash.clone()) {
                artifacts.push(entry);
            }
        }
        artifacts.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.action_id.cmp(&left.action_id))
        });
        artifacts.truncate(100);
        Ok(artifacts)
    }

    pub fn list_visible_messages(
        &self,
        user_id: &str,
        conversation_id: &str,
        limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT m.id, m.conversation_id, m.turn_id, m.role, m.content, m.name,
                    m.created_at, t.bot
             FROM messages m LEFT JOIN turns t ON t.id = m.turn_id
             WHERE m.conversation_id = ?1 AND m.user_id = ?2 AND m.role IN ('user', 'assistant')
             ORDER BY m.created_at DESC, m.rowid DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![conversation_id, user_id, limit.clamp(1, 200) as i64],
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

    pub fn history_for_llm(
        &self,
        user_id: &str,
        conversation_id: &str,
        limit: usize,
    ) -> Result<Vec<LlmMessage>> {
        Ok(self
            .list_messages(user_id, conversation_id, limit)?
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

    /// Record the turn of the acting user's task in the acting user's
    /// conversation. The task and the conversation must both belong to the
    /// acting user; a mismatch fails closed with NotFound.
    pub fn create_turn(
        &self,
        user_id: &str,
        task_id: &str,
        bot: &str,
        conversation_id: &str,
        state: &Value,
    ) -> Result<Turn> {
        {
            let connection = self.connection()?;
            let task_owned = connection
                .query_row(
                    "SELECT 1 FROM tasks WHERE id = ?1 AND user_id = ?2",
                    params![task_id, user_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if task_owned.is_none() {
                return Err(SeraError::NotFound(format!("task {task_id}")));
            }
        }
        self.guard_conversation_writable(user_id, conversation_id)?;
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
            "INSERT INTO turns(id, task_id, bot, conversation_id, status, state_json, created_at, updated_at, user_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                turn.id,
                turn.task_id,
                turn.bot,
                turn.conversation_id,
                turn.status,
                serde_json::to_string(&turn.state)?,
                to_string(turn.created_at),
                to_string(turn.updated_at),
                user_id,
            ],
        )?;
        Ok(turn)
    }

    pub fn get_turn(&self, user_id: &str, id: &str) -> Result<Turn> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, task_id, bot, conversation_id, status, state_json, created_at, updated_at
                 FROM turns WHERE id = ?1 AND user_id = ?2",
                params![id, user_id],
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

    pub fn get_turn_by_task(&self, user_id: &str, task_id: &str) -> Result<Option<Turn>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT id, task_id, bot, conversation_id, status, state_json, created_at, updated_at
                 FROM turns WHERE task_id = ?1 AND user_id = ?2
                 ORDER BY created_at DESC LIMIT 1",
                params![task_id, user_id],
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

    pub fn update_turn(&self, user_id: &str, id: &str, status: &str, state: &Value) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE turns SET status=?1, state_json=?2, updated_at=?3 WHERE id=?4 AND user_id=?5",
            params![
                status,
                serde_json::to_string(state)?,
                now_string(),
                id,
                user_id
            ],
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

    /// Audit rows are global and append-only; `actor` carries the acting
    /// user's id ("system" for maintenance). Unscoped by design — the audit
    /// ledger is the admin's cross-user visibility surface (spec: "the admin
    /// sees everything in audit, but is not in the approval path").
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

    /// The acting user's own audit rows, newest first. Members see only rows
    /// recorded under their id; the admin's cross-user view is
    /// [`Store::list_audit`] (audit-only visibility, never data rows).
    pub fn list_audit_for_user(&self, user_id: &str, limit: usize) -> Result<Vec<AuditEntry>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, at, actor, event, tool, task_id, approval_id, action_id, policy_version, details_json
             FROM audit_log WHERE actor = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![user_id, limit as i64], |row| {
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

    /// Token-usage rollup over the acting user's `model.used` audit rows
    /// since `since`.
    ///
    /// A row counts for the acting user when its task belongs to them, or —
    /// when the row carries no task — when its actor is the acting user.
    /// `bot` attributes rows through the joined task's bot (falling back to a
    /// `bot` field in the audit details, which the current writer does not
    /// set); `conversation_id` filters through the task payload. Rows without
    /// a `usage` object still count as calls with zero tokens. All lists are
    /// capped.
    pub fn usage_rollup(
        &self,
        user_id: &str,
        since: DateTime<Utc>,
        bot: Option<&str>,
        conversation_id: Option<&str>,
    ) -> Result<UsageRollup> {
        use rusqlite::types::Value as SqlValue;

        let mut where_clause = String::from(
            " FROM audit_log a LEFT JOIN tasks t ON t.id = a.task_id
              WHERE a.event = 'model.used' AND a.at >= ?2
                AND COALESCE(t.user_id, a.actor) = ?1",
        );
        let mut args: Vec<SqlValue> = vec![
            SqlValue::Text(user_id.to_owned()),
            SqlValue::Text(to_string(since)),
        ];
        if let Some(bot) = bot {
            where_clause
                .push_str(" AND COALESCE(t.bot, json_extract(a.details_json, '$.bot')) = ?");
            args.push(SqlValue::Text(bot.to_owned()));
        }
        if let Some(conversation_id) = conversation_id {
            where_clause.push_str(
                " AND COALESCE(json_extract(t.payload_json, '$.conversation_id'), json_extract(a.details_json, '$.conversation_id')) = ?",
            );
            args.push(SqlValue::Text(conversation_id.to_owned()));
        }
        let token_sums = "COUNT(*),
            COALESCE(SUM(CAST(json_extract(a.details_json, '$.usage.prompt_tokens') AS INTEGER)), 0),
            COALESCE(SUM(CAST(json_extract(a.details_json, '$.usage.completion_tokens') AS INTEGER)), 0),
            COALESCE(SUM(CAST(json_extract(a.details_json, '$.usage.total_tokens') AS INTEGER)), 0)";

        let connection = self.connection()?;
        let totals = {
            let sql = format!("SELECT {token_sums}{where_clause}");
            connection.query_row(&sql, rusqlite::params_from_iter(args.iter()), |row| {
                Ok(UsageTotals {
                    calls: row.get(0)?,
                    prompt_tokens: row.get(1)?,
                    completion_tokens: row.get(2)?,
                    total_tokens: row.get(3)?,
                })
            })?
        };
        let per_day = {
            let sql = format!(
                "SELECT substr(a.at, 1, 10), {token_sums}{where_clause}
                 GROUP BY substr(a.at, 1, 10) ORDER BY substr(a.at, 1, 10) DESC LIMIT 100"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(args.iter()), |row| {
                Ok(UsageDay {
                    day: row.get(0)?,
                    calls: row.get(1)?,
                    prompt_tokens: row.get(2)?,
                    completion_tokens: row.get(3)?,
                    total_tokens: row.get(4)?,
                })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let per_bot = {
            let sql = format!(
                "SELECT COALESCE(t.bot, json_extract(a.details_json, '$.bot'), 'unknown'), {token_sums}{where_clause}
                 GROUP BY COALESCE(t.bot, json_extract(a.details_json, '$.bot'), 'unknown')
                 ORDER BY COALESCE(t.bot, json_extract(a.details_json, '$.bot'), 'unknown') ASC LIMIT 100"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(args.iter()), |row| {
                Ok(UsageBot {
                    bot: row.get(0)?,
                    calls: row.get(1)?,
                    prompt_tokens: row.get(2)?,
                    completion_tokens: row.get(3)?,
                    total_tokens: row.get(4)?,
                })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        Ok(UsageRollup {
            totals,
            per_day,
            per_bot,
        })
    }

    /// Total number of reviewable memories owned by the acting user.
    pub fn count_memories(&self, user_id: &str) -> Result<i64> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT COUNT(*) FROM memories WHERE user_id = ?1",
            params![user_id],
            |row| row.get(0),
        )?)
    }

    /// Artifacts produced for the acting user by one bot: distinct artifact
    /// hashes on the user's actions whose turn belonged to the bot, plus
    /// `artifact.created` audit rows recorded with the bot as actor (none
    /// today; the gateway actor is a user id or "system").
    pub fn count_bot_artifacts(&self, user_id: &str, bot: &str) -> Result<i64> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT COUNT(*) FROM (
                SELECT a.artifact_hash AS hash
                  FROM actions a JOIN turns t ON t.id = a.turn_id
                 WHERE t.bot = ?2 AND t.user_id = ?1 AND a.artifact_hash IS NOT NULL
                UNION
                SELECT ca.hash
                  FROM conversation_artifacts ca JOIN turns t ON t.id = ca.turn_id
                 WHERE t.bot = ?2 AND t.user_id = ?1
             )",
            params![user_id, bot],
            |row| row.get(0),
        )?)
    }

    /// Append one event to the acting user's event stream.
    pub fn append_event(&self, user_id: &str, kind: &str, payload: &Value) -> Result<i64> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO events(at, kind, payload_json, user_id) VALUES (?1, ?2, ?3, ?4)",
            params![now_string(), kind, serde_json::to_string(payload)?, user_id],
        )?;
        Ok(connection.last_insert_rowid())
    }

    /// Events of the acting user after `after_id`, oldest first. SSE streams
    /// resolve the cursor per user, so one user's stream never carries
    /// another user's events.
    pub fn events_after(&self, user_id: &str, after_id: i64, limit: usize) -> Result<Vec<Event>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, at, kind, payload_json FROM events
             WHERE id > ?2 AND user_id = ?1 ORDER BY id ASC LIMIT ?3",
        )?;
        let rows = statement.query_map(params![user_id, after_id, limit as i64], |row| {
            Ok(Event {
                id: row.get(0)?,
                at: parse_datetime(&row.get::<_, String>(1)?),
                kind: row.get(2)?,
                payload: serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or(Value::Null),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Retention sweep over the global event ledger. Cross-user by design:
    /// the retention window (`server.event_retention_days`) is a server
    /// property, and pruning per user would keep expired rows alive forever
    /// for users who never sign in.
    pub fn prune_events(&self, before: DateTime<Utc>) -> Result<usize> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "DELETE FROM events WHERE at < ?1",
            params![to_string(before)],
        )?)
    }

    /// Record one gateway action under the acting user. When a turn id is
    /// given, it must belong to the acting user — a mismatch fails closed
    /// with NotFound, before any row is written.
    #[allow(clippy::too_many_arguments)]
    pub fn create_action(
        &self,
        user_id: &str,
        tool: &str,
        task_id: Option<&str>,
        turn_id: Option<&str>,
        canonical_args: &str,
        canonical_hash: &str,
        destination: Option<&str>,
        artifact_hash: Option<&str>,
        policy_version: &str,
    ) -> Result<Action> {
        if let Some(turn_id) = turn_id {
            let connection = self.connection()?;
            let owned = connection
                .query_row(
                    "SELECT 1 FROM turns WHERE id = ?1 AND user_id = ?2",
                    params![turn_id, user_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if owned.is_none() {
                return Err(SeraError::NotFound(format!("turn {turn_id}")));
            }
        }
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
            "INSERT INTO actions(id, tool, task_id, turn_id, canonical_args, canonical_hash, destination, artifact_hash, policy_version, status, outcome_json, created_at, updated_at, user_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11, ?12, ?13)",
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
                user_id,
            ],
        )?;
        Ok(action)
    }

    /// The acting user's action by id; another user's action is
    /// indistinguishable from a missing one.
    pub fn get_action(&self, user_id: &str, id: &str) -> Result<Action> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, tool, task_id, turn_id, canonical_args, canonical_hash, destination, artifact_hash, policy_version, status, outcome_json, created_at, updated_at
                 FROM actions WHERE id = ?1 AND user_id = ?2",
                params![id, user_id],
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

    pub fn update_action(
        &self,
        user_id: &str,
        id: &str,
        status: &str,
        outcome: Option<&Value>,
    ) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE actions SET status=?1, outcome_json=?2, updated_at=?3 WHERE id=?4 AND user_id=?5",
            params![
                status,
                outcome.map(serde_json::to_string).transpose()?,
                now_string(),
                id,
                user_id
            ],
        )?;
        Ok(())
    }

    pub fn transition_action(&self, user_id: &str, id: &str, from: &str, to: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE actions SET status=?1, updated_at=?2 WHERE id=?3 AND status=?4 AND user_id=?5",
            params![to, now_string(), id, from, user_id],
        )?;
        Ok(changed == 1)
    }

    /// Create the approval for one of the acting user's actions. The action
    /// must exist and belong to the acting user; otherwise nothing is
    /// written and the error is indistinguishable from unknown.
    pub fn create_approval(
        &self,
        user_id: &str,
        action_id: &str,
        tool: &str,
        summary: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<Approval> {
        {
            let connection = self.connection()?;
            let owned = connection
                .query_row(
                    "SELECT 1 FROM actions WHERE id = ?1 AND user_id = ?2",
                    params![action_id, user_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if owned.is_none() {
                return Err(SeraError::NotFound(format!("action {action_id}")));
            }
        }
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
            "INSERT INTO approvals(id, action_id, tool, summary, state, scope, expires_at, created_at, decided_at, note, user_id)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, NULL, NULL, ?8)",
            params![
                approval.id,
                approval.action_id,
                approval.tool,
                approval.summary,
                approval.state,
                to_string(approval.expires_at),
                to_string(approval.created_at),
                user_id,
            ],
        )?;
        Ok(approval)
    }

    /// The acting user's approval by id; another user's approval is
    /// indistinguishable from a missing one.
    pub fn get_approval(&self, user_id: &str, id: &str) -> Result<Approval> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, action_id, tool, summary, state, scope, expires_at, created_at, decided_at, note
                 FROM approvals WHERE id = ?1 AND user_id = ?2",
                params![id, user_id],
                map_approval,
            )
            .optional()?
            .ok_or_else(|| SeraError::NotFound(format!("approval {id}")))
    }

    /// The acting user's approval for one of their actions, newest first.
    ///
    /// Used by the gateway to re-verify the approval chain before an approved
    /// action is dispatched: an approval recorded under another user is
    /// indistinguishable from a missing one, so the caller fails closed.
    pub fn get_approval_for_action(&self, user_id: &str, action_id: &str) -> Result<Approval> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, action_id, tool, summary, state, scope, expires_at, created_at, decided_at, note
                 FROM approvals WHERE action_id = ?1 AND user_id = ?2
                 ORDER BY created_at DESC LIMIT 1",
                params![action_id, user_id],
                map_approval,
            )
            .optional()?
            .ok_or_else(|| SeraError::NotFound(format!("approval for action {action_id}")))
    }

    /// The acting user's pending approvals, oldest first.
    pub fn list_pending_approvals(&self, user_id: &str, limit: usize) -> Result<Vec<Approval>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, action_id, tool, summary, state, scope, expires_at, created_at, decided_at, note
             FROM approvals WHERE state='pending' AND user_id = ?1
             ORDER BY created_at ASC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![user_id, limit as i64], map_approval)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn transition_approval(
        &self,
        user_id: &str,
        id: &str,
        to: &str,
        scope: Option<&str>,
        note: Option<&str>,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let now = now_string();
        let changed = connection.execute(
            "UPDATE approvals SET state=?1, scope=?2, note=?3, decided_at=?4
             WHERE id=?5 AND user_id=?6 AND state='pending' AND expires_at > ?4",
            params![to, scope, note, now, id, user_id],
        )?;
        Ok(changed == 1)
    }

    /// Expiry cascade over every user's pending approvals. Cross-user by
    /// design: expiry is a time-driven maintenance sweep (an approval past
    /// its expiry is dead regardless of whose it is), and the cascade also
    /// fails the suspended task and turn of the owning user.
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

    /// Invalidate pending approvals bound to an outdated policy version.
    /// Cross-user by design: the policy blocklist is a global server policy
    /// with one version hash for everyone, so its invalidation semantics
    /// cannot be per-user.
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

    /// Record one standing grant for the acting user. Grants match only
    /// within the granting user's rows.
    pub fn insert_grant(
        &self,
        user_id: &str,
        tool: &str,
        canonical_hash: &str,
        destination: Option<&str>,
        policy_version: &str,
        approval_id: &str,
    ) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO standing_grants(id, tool, canonical_hash, destination, policy_version, approval_id, revoked_at, created_at, user_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8)",
            params![
                new_id(),
                tool,
                canonical_hash,
                destination,
                policy_version,
                approval_id,
                now_string(),
                user_id
            ],
        )?;
        Ok(())
    }

    /// Whether the acting user holds an unrevoked standing grant for this
    /// exact tool, canonical hash and destination under the current policy
    /// version. Another user's grant never matches.
    pub fn matching_grant(
        &self,
        user_id: &str,
        tool: &str,
        canonical_hash: &str,
        destination: Option<&str>,
        policy_version: &str,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM standing_grants
             WHERE user_id=?5 AND tool=?1 AND canonical_hash=?2 AND policy_version=?3 AND revoked_at IS NULL
               AND ((destination IS NULL AND ?4 IS NULL) OR destination = ?4)",
            params![tool, canonical_hash, policy_version, destination, user_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Revoke the acting user's standing grants. Never touches another
    /// user's grants: revocation is a per-owner control.
    pub fn revoke_grants(&self, user_id: &str) -> Result<usize> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "UPDATE standing_grants SET revoked_at=?2 WHERE revoked_at IS NULL AND user_id = ?1",
            params![user_id, now_string()],
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

    /// Claim the next runnable task for a worker. Cross-user by design: the
    /// worker pool is shared — a task runs under its owning user (returned
    /// on the task), regardless of which worker claims it.
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
                    created_at, updated_at, user_id",
                params![worker_id, to_string(lease_until), to_string(now)],
                map_task,
            )
            .optional()?;
        transaction.commit()?;
        Ok(task)
    }

    /// Complete the acting user's task. The worker passes the task's owning
    /// user, resolved from [`Store::claim_task`].
    pub fn complete_task(&self, user_id: &str, id: &str, fencing_token: i64) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE tasks SET status='done', lease_until=NULL, worker_id=NULL, updated_at=?1
             WHERE id=?2 AND fencing_token=?3 AND status='running' AND user_id=?4",
            params![now_string(), id, fencing_token, user_id],
        )?;
        Ok(changed == 1)
    }

    pub fn fail_task(
        &self,
        user_id: &str,
        id: &str,
        fencing_token: i64,
        error: &str,
        retry_backoff_secs: u64,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let now = utc_now();
        let task = connection
            .query_row(
                "SELECT attempts, max_attempts, uncertain FROM tasks
                 WHERE id=?1 AND fencing_token=?2 AND user_id=?3",
                params![id, fencing_token, user_id],
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
             WHERE id=?5 AND fencing_token=?6 AND status='running' AND user_id=?7",
            params![
                status,
                truncate(error, 2000),
                to_string(run_after),
                to_string(now),
                id,
                fencing_token,
                user_id
            ],
        )?;
        Ok(changed == 1)
    }

    pub fn fail_task_final(
        &self,
        user_id: &str,
        id: &str,
        fencing_token: i64,
        error: &str,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE tasks SET status='failed', last_error=?1, lease_until=NULL, worker_id=NULL, updated_at=?2
             WHERE id=?3 AND fencing_token=?4 AND status='running' AND user_id=?5",
            params![truncate(error, 2000), now_string(), id, fencing_token, user_id],
        )?;
        Ok(changed == 1)
    }

    pub fn await_approval_task(&self, user_id: &str, id: &str, fencing_token: i64) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE tasks SET status='awaiting_approval', lease_until=NULL, worker_id=NULL, updated_at=?1
             WHERE id=?2 AND fencing_token=?3 AND status='running' AND user_id=?4",
            params![now_string(), id, fencing_token, user_id],
        )?;
        Ok(changed == 1)
    }

    pub fn requeue_task(&self, user_id: &str, id: &str, run_after: DateTime<Utc>) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE tasks SET status='queued', run_after=?1, lease_until=NULL, worker_id=NULL, updated_at=?2
             WHERE id=?3 AND user_id=?4 AND status IN ('awaiting_approval','running')",
            params![to_string(run_after), now_string(), id, user_id],
        )?;
        Ok(changed == 1)
    }

    /// Recover tasks whose worker lease expired. Cross-user by design: a
    /// crashed worker strand tasks of every user; lease recovery must return
    /// them to the shared queue regardless of owner.
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

    /// The acting user's task by id; another user's task is
    /// indistinguishable from a missing one.
    pub fn get_task(&self, user_id: &str, id: &str) -> Result<Task> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, kind, bot, payload_json, status, priority, attempts, max_attempts,
                        run_after, lease_until, worker_id, fencing_token, uncertain, last_error,
                        created_at, updated_at, user_id
                 FROM tasks WHERE id=?1 AND user_id=?2",
                params![id, user_id],
                map_task,
            )
            .optional()?
            .ok_or_else(|| SeraError::NotFound(format!("task {id}")))
    }

    /// Queue one task for the acting user without a quota check. Every task
    /// carries its owner; the worker later executes it under this id.
    ///
    /// This is the low-level insert for work that continues an already
    /// counted request (approval resumes, handoffs, team fan-out) and for
    /// admin-configured scheduled routines. User-initiated enqueues go
    /// through [`Store::enqueue_task_with_quota`].
    pub fn enqueue_task(
        &self,
        user_id: &str,
        kind: &str,
        bot: &str,
        payload: &Value,
        priority: i64,
        run_after: DateTime<Utc>,
    ) -> Result<Task> {
        self.enqueue_task_inner(user_id, kind, bot, payload, priority, run_after, None)
    }

    /// Queue one task for the acting user, enforcing the user's daily task
    /// quota inside the same immediate transaction as the insert.
    ///
    /// The quota window is the **UTC calendar day** (`created_at >= today
    /// 00:00:00Z`), the same bucketing `usage_rollup` uses for its `per_day`
    /// rows. A user at the limit gets an explicit `Validation` error naming
    /// the limit and no row is written. `BEGIN IMMEDIATE` takes the write lock
    /// before the count, so two concurrent enqueues for the same user cannot
    /// both claim the last slot.
    #[allow(clippy::too_many_arguments)]
    pub fn enqueue_task_with_quota(
        &self,
        user_id: &str,
        kind: &str,
        bot: &str,
        payload: &Value,
        priority: i64,
        run_after: DateTime<Utc>,
        daily_limit: u64,
    ) -> Result<Task> {
        self.enqueue_task_inner(
            user_id,
            kind,
            bot,
            payload,
            priority,
            run_after,
            Some(daily_limit),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn enqueue_task_inner(
        &self,
        user_id: &str,
        kind: &str,
        bot: &str,
        payload: &Value,
        priority: i64,
        run_after: DateTime<Utc>,
        daily_limit: Option<u64>,
    ) -> Result<Task> {
        let now = utc_now();
        let id = new_id();
        let connection = self.connection()?;
        match daily_limit {
            Some(daily_limit) => {
                let limit = i64::try_from(daily_limit).unwrap_or(i64::MAX);
                let day_start = now
                    .date_naive()
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight is a valid time")
                    .and_utc();
                let transaction =
                    Transaction::new_unchecked(&connection, TransactionBehavior::Immediate)?;
                let used: i64 = transaction.query_row(
                    "SELECT COUNT(*) FROM tasks WHERE user_id = ?1 AND created_at >= ?2",
                    params![user_id, to_string(day_start)],
                    |row| row.get(0),
                )?;
                if used >= limit {
                    return Err(SeraError::Validation(format!(
                        "daily task quota exceeded: {daily_limit} tasks per user per UTC day"
                    )));
                }
                insert_task_row(
                    &transaction,
                    &id,
                    user_id,
                    kind,
                    bot,
                    payload,
                    priority,
                    run_after,
                    now,
                )?;
                transaction.commit()?;
            }
            None => {
                insert_task_row(
                    &connection,
                    &id,
                    user_id,
                    kind,
                    bot,
                    payload,
                    priority,
                    run_after,
                    now,
                )?;
            }
        }
        drop(connection);
        self.get_task(user_id, &id)
    }

    /// Tasks of the acting user still in flight.
    pub fn pending_task_count(&self, user_id: &str) -> Result<i64> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT COUNT(*) FROM tasks
             WHERE user_id = ?1 AND status IN ('queued','running','awaiting_approval')",
            params![user_id],
            |row| row.get(0),
        )?)
    }

    /// Tasks of every user still in flight. Cross-user by design: the
    /// restart gate must know about the whole queue — a restart strands any
    /// user's running task, not just the requesting admin's.
    pub fn pending_task_count_all(&self) -> Result<i64> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT COUNT(*) FROM tasks WHERE status IN ('queued','running','awaiting_approval')",
            [],
            |row| row.get(0),
        )?)
    }

    /// Cancel the acting user's task.
    pub fn cancel_task(&self, user_id: &str, id: &str, reason: &str) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE tasks SET status='cancelled', last_error=?1, lease_until=NULL, worker_id=NULL, updated_at=?2
             WHERE id=?3 AND user_id=?4 AND status IN ('queued','running','awaiting_approval')",
            params![truncate(reason, 2000), now_string(), id, user_id],
        )?;
        Ok(changed == 1)
    }

    /// Reconcile actions left in `dispatching` by a crash. Cross-user by
    /// design: crash recovery must mark every interrupted dispatch
    /// regardless of which user's task was running when the process died.
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
        user_id: row.get(16)?,
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

/// Insert one queued task row. Shared by the plain and quota-checked
/// enqueue paths so the row shape (and its owner) has one definition.
#[allow(clippy::too_many_arguments)]
fn insert_task_row(
    connection: &rusqlite::Connection,
    id: &str,
    user_id: &str,
    kind: &str,
    bot: &str,
    payload: &Value,
    priority: i64,
    run_after: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<()> {
    connection.execute(
        "INSERT INTO tasks(id, kind, bot, payload_json, status, priority, attempts, max_attempts,
            run_after, lease_until, worker_id, fencing_token, uncertain, last_error, created_at, updated_at, user_id)
         VALUES (?1, ?2, ?3, ?4, 'queued', ?5, 0, 5, ?6, NULL, NULL, 0, 0, NULL, ?7, ?7, ?8)",
        params![
            id,
            kind,
            bot,
            serde_json::to_string(payload)?,
            priority,
            to_string(run_after),
            to_string(now),
            user_id
        ],
    )?;
    Ok(())
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
        user_id: row.get(2)?,
        created_at: parse_datetime(&row.get::<_, String>(3)?),
        last_used_at: row
            .get::<_, Option<String>>(4)?
            .as_deref()
            .map(parse_datetime),
        expires_at: row
            .get::<_, Option<String>>(5)?
            .as_deref()
            .map(parse_datetime),
        revoked_at: row
            .get::<_, Option<String>>(6)?
            .as_deref()
            .map(parse_datetime),
    })
}

fn user_from_row(row: &Row<'_>) -> rusqlite::Result<User> {
    let role: &str = &row.get::<_, String>(4)?;
    let status: &str = &row.get::<_, String>(5)?;
    Ok(User {
        id: row.get(0)?,
        provider_subject: row.get(1)?,
        email: row.get(2)?,
        display_name: row.get(3)?,
        role: parse_user_role(role)?,
        status: parse_user_status(status)?,
        created_at: parse_datetime(&row.get::<_, String>(6)?),
    })
}

fn provider_grant_from_row(row: &Row<'_>) -> rusqlite::Result<ProviderGrant> {
    Ok(ProviderGrant {
        grantor: row.get(0)?,
        grantee: row.get(1)?,
        provider: row.get(2)?,
        created_at: parse_datetime(&row.get::<_, String>(3)?),
        revoked_at: row
            .get::<_, Option<String>>(4)?
            .as_deref()
            .map(parse_datetime),
    })
}

fn telegram_binding_from_row(row: &Row<'_>) -> rusqlite::Result<TelegramBinding> {
    Ok(TelegramBinding {
        user_id: row.get(0)?,
        telegram_user_id: row.get(1)?,
        telegram_chat_id: row.get(2)?,
        linked_at: parse_datetime(&row.get::<_, String>(3)?),
        revoked_at: row
            .get::<_, Option<String>>(4)?
            .as_deref()
            .map(parse_datetime),
    })
}

/// One pending Telegram linking code, stored hashed in `kv` until the chat
/// redeems it or it expires.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct TelegramLinkCode {
    user_id: String,
    expires_at: DateTime<Utc>,
}

fn telegram_link_key(code_hash: &str) -> String {
    format!("telegram_link:{code_hash}")
}

fn stored_bot_from_row(row: &Row<'_>) -> rusqlite::Result<StoredBot> {
    let config_json: String = row.get(1)?;
    let config = serde_json::from_str(&config_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            config_json.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    Ok(StoredBot {
        name: row.get(0)?,
        config,
        prompt: row.get(2)?,
        enabled: row.get::<_, i64>(3)? != 0,
        updated_at: parse_datetime(&row.get::<_, String>(4)?),
    })
}

fn user_skill_from_row(row: &Row<'_>) -> rusqlite::Result<UserSkill> {
    Ok(UserSkill {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        body: row.get(3)?,
        created_at: parse_datetime(&row.get::<_, String>(4)?),
        updated_at: parse_datetime(&row.get::<_, String>(5)?),
    })
}

/// Roles and statuses are written only through the checked enums above and
/// the schema CHECK constraints, so an unparseable value is a corrupt row:
/// fail the read loudly instead of guessing a default (a wrong guess here
/// either escalates privilege or locks the owner out).
fn enum_field<T: Copy>(label: &str, value: &str, known: &[(&str, T)]) -> rusqlite::Result<T> {
    known
        .iter()
        .find(|(name, _)| *name == value)
        .map(|(_, value)| *value)
        .ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                value.len(),
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::other(format!(
                    "corrupt users.{label} value: '{value}'"
                ))),
            )
        })
}

fn parse_user_role(value: &str) -> rusqlite::Result<UserRole> {
    enum_field(
        "role",
        value,
        &[("admin", UserRole::Admin), ("member", UserRole::Member)],
    )
}

fn parse_user_status(value: &str) -> rusqlite::Result<UserStatus> {
    enum_field(
        "status",
        value,
        &[
            ("active", UserStatus::Active),
            ("disabled", UserStatus::Disabled),
        ],
    )
}

/// Hard bounds on identity fields. Provider-reported values are untrusted
/// input: callers sanitize before calling, and the store still refuses
/// anything oversized rather than storing it silently truncated.
fn validate_user_fields(provider_subject: &str, email: &str, display_name: &str) -> Result<()> {
    if provider_subject.is_empty()
        || provider_subject.len() > 200
        || provider_subject.chars().any(char::is_control)
    {
        return Err(SeraError::Validation(
            "provider subject must be 1-200 characters without control characters".into(),
        ));
    }
    if email.len() > 320 || email.chars().any(char::is_control) {
        return Err(SeraError::Validation(
            "email must be at most 320 characters without control characters".into(),
        ));
    }
    if display_name.len() > 200 || display_name.chars().any(char::is_control) {
        return Err(SeraError::Validation(
            "display name must be at most 200 characters without control characters".into(),
        ));
    }
    Ok(())
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// Queue one outbox delivery for the acting user.
pub fn insert_outbox(
    store: &Store,
    user_id: &str,
    kind: &str,
    destination: &str,
    payload: &Value,
) -> Result<Id> {
    let id = new_id();
    let connection = store.connection()?;
    connection.execute(
        "INSERT INTO outbox(id, kind, destination, payload_json, status, attempts, last_error, created_at, updated_at, user_id)
         VALUES (?1, ?2, ?3, ?4, 'pending', 0, NULL, ?5, ?5, ?6)",
        params![
            id,
            kind,
            destination,
            serde_json::to_string(payload)?,
            now_string(),
            user_id
        ],
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

    /// The bootstrap admin's id — the acting user for the single-operator
    /// flows the existing tests exercise.
    fn admin(store: &Store) -> String {
        store.admin_user_id().unwrap()
    }

    /// A second, synthetic member account for the isolation suite.
    fn member(store: &Store) -> User {
        store
            .create_user(
                "microsoft:store-member",
                "member@example.example",
                "Store Member",
            )
            .unwrap()
    }

    #[test]
    fn visible_history_filters_before_limiting_and_orders_timestamp_ties() {
        let store = store();
        let admin = admin(&store);
        let first = store
            .append_message(&admin, "owner", None, Role::User, "first")
            .unwrap();
        let second = store
            .append_message(&admin, "owner", None, Role::Assistant, "second")
            .unwrap();
        store
            .append_message(&admin, "owner", None, Role::System, "hidden")
            .unwrap();
        store
            .append_message(&admin, "owner", None, Role::Tool, "internal")
            .unwrap();
        store
            .append_message(&admin, "other", None, Role::User, "other")
            .unwrap();
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE messages SET created_at = '2026-01-01T00:00:00.000Z'",
                [],
            )
            .unwrap();
        let messages = store.list_visible_messages(&admin, "owner", 2).unwrap();
        assert_eq!(
            messages.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec![first.id.as_str(), second.id.as_str()]
        );
        assert_eq!(
            store.list_visible_messages(&admin, "owner", 1).unwrap()[0].id,
            second.id
        );
        assert!(
            store
                .list_visible_messages(&admin, "absent", 100)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn unread_counts_assistant_messages_until_marked_read() {
        let store = store();
        let admin = admin(&store);
        store
            .ensure_conversation_accepts_turns(&admin, "dir:main")
            .unwrap();
        store
            .append_message(&admin, "dir:main", None, Role::Assistant, "first")
            .unwrap();
        store
            .append_message(&admin, "dir:main", None, Role::Assistant, "second")
            .unwrap();
        store
            .append_message(&admin, "dir:main", None, Role::User, "owner reply")
            .unwrap();
        let record = store.get_conversation(&admin, "dir:main").unwrap().unwrap();
        assert_eq!(record.unread_count, 2);

        assert!(store.mark_conversation_read(&admin, "dir:main").unwrap());
        let record = store.get_conversation(&admin, "dir:main").unwrap().unwrap();
        assert_eq!(record.unread_count, 0);

        store
            .append_message(&admin, "dir:main", None, Role::Assistant, "third")
            .unwrap();
        let listed = store.list_conversations(&admin, 10, None, false).unwrap();
        let record = listed.iter().find(|entry| entry.id == "dir:main").unwrap();
        assert_eq!(record.unread_count, 1);
        assert!(!store.mark_conversation_read(&admin, "absent").unwrap());
    }

    #[test]
    fn team_rules_round_trip_and_default_empty() {
        let store = store();
        let admin = admin(&store);
        store
            .create_conversation(&admin, "room:review", "room", None)
            .unwrap();
        assert!(
            store
                .get_conversation_team(&admin, "room:review")
                .unwrap()
                .is_none()
        );
        store
            .set_conversation_team(
                &admin,
                "room:review",
                &["analyst".into()],
                "selected",
                "Always cite sources.",
            )
            .unwrap();
        let team = store
            .get_conversation_team(&admin, "room:review")
            .unwrap()
            .unwrap();
        assert_eq!(team.rules, "Always cite sources.");
        assert_eq!(team.members, vec!["analyst".to_owned()]);
    }

    fn model_used(
        store: &Store,
        actor: &str,
        task_id: Option<&str>,
        prompt: i64,
        completion: i64,
    ) -> i64 {
        store
            .record_audit(
                actor,
                "model.used",
                None,
                task_id,
                None,
                None,
                Some("policy-synthetic"),
                Some(&serde_json::json!({
                    "provider": "default",
                    "model": "synthetic-model",
                    "usage": {
                        "prompt_tokens": prompt,
                        "completion_tokens": completion,
                        "total_tokens": prompt + completion,
                    },
                    "history_messages": 3,
                    "prompt_chars_est": 120,
                })),
            )
            .unwrap()
    }

    #[test]
    fn usage_rollup_aggregates_by_day_bot_and_conversation_with_filters() {
        let store = store();
        let admin = admin(&store);
        let task_main = store
            .enqueue_task(
                &admin,
                "turn",
                "main",
                &serde_json::json!({"conversation_id": "room:review"}),
                0,
                utc_now(),
            )
            .unwrap();
        let task_analyst = store
            .enqueue_task(
                &admin,
                "turn",
                "analyst",
                &serde_json::json!({"conversation_id": "dir:analyst"}),
                0,
                utc_now(),
            )
            .unwrap();
        model_used(&store, &admin, Some(&task_main.id), 10, 5);
        model_used(&store, &admin, Some(&task_main.id), 20, 10);
        model_used(&store, &admin, Some(&task_analyst.id), 1, 1);
        // A model.used row without a usage object still counts as a call.
        model_used(&store, &admin, None, 0, 0);
        // Other events are ignored.
        store
            .record_audit(
                "owner",
                "model.selected",
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();

        // Backdate the analyst row to three days ago for the per_day split.
        let three_days_ago = to_string(Utc::now() - chrono::Duration::days(3));
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE audit_log SET at = ?1 WHERE task_id = ?2",
                params![three_days_ago, task_analyst.id],
            )
            .unwrap();

        let week = Utc::now() - chrono::Duration::days(7);
        let rollup = store.usage_rollup(&admin, week, None, None).unwrap();
        assert_eq!(rollup.totals.calls, 4);
        assert_eq!(rollup.totals.prompt_tokens, 31);
        assert_eq!(rollup.totals.completion_tokens, 16);
        assert_eq!(rollup.totals.total_tokens, 47);
        assert_eq!(rollup.per_day.len(), 2);
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let old = (Utc::now() - chrono::Duration::days(3))
            .format("%Y-%m-%d")
            .to_string();
        let today_row = rollup.per_day.iter().find(|row| row.day == today).unwrap();
        assert_eq!(today_row.calls, 3);
        assert_eq!(today_row.total_tokens, 45);
        let old_row = rollup.per_day.iter().find(|row| row.day == old).unwrap();
        assert_eq!(old_row.calls, 1);
        assert_eq!(old_row.total_tokens, 2);

        let per_bot: std::collections::BTreeMap<&str, &UsageBot> = rollup
            .per_bot
            .iter()
            .map(|row| (row.bot.as_str(), row))
            .collect();
        assert_eq!(per_bot.len(), 3);
        assert_eq!(per_bot["main"].calls, 2);
        assert_eq!(per_bot["main"].total_tokens, 45);
        assert_eq!(per_bot["analyst"].calls, 1);
        assert_eq!(per_bot["unknown"].calls, 1);
        assert_eq!(per_bot["unknown"].total_tokens, 0);

        // Bot filter.
        let rollup = store
            .usage_rollup(&admin, week, Some("analyst"), None)
            .unwrap();
        assert_eq!(rollup.totals.calls, 1);
        assert_eq!(rollup.totals.total_tokens, 2);
        assert_eq!(rollup.per_bot.len(), 1);
        assert_eq!(rollup.per_bot[0].bot, "analyst");

        // Conversation filter attributes through the task payload.
        let rollup = store
            .usage_rollup(&admin, week, None, Some("room:review"))
            .unwrap();
        assert_eq!(rollup.totals.calls, 2);
        assert_eq!(rollup.totals.total_tokens, 45);
        assert!(rollup.per_bot.iter().all(|row| row.bot == "main"));

        // The `since` cutoff excludes the backdated row.
        let rollup = store
            .usage_rollup(&admin, Utc::now() - chrono::Duration::days(1), None, None)
            .unwrap();
        assert_eq!(rollup.totals.calls, 3);
        assert_eq!(rollup.per_day.len(), 1);
    }

    #[test]
    fn usage_rollup_empty_result_shape_and_row_caps() {
        let store = store();
        let admin = admin(&store);
        let rollup = store
            .usage_rollup(&admin, Utc::now() - chrono::Duration::days(7), None, None)
            .unwrap();
        assert_eq!(rollup.totals, UsageTotals::default());
        assert!(rollup.per_day.is_empty());
        assert!(rollup.per_bot.is_empty());
        // per_bot is capped at 100 rows even with more bots.
        for index in 0..120 {
            let task = store
                .enqueue_task(
                    &admin,
                    "turn",
                    &format!("bot-{index:03}"),
                    &serde_json::json!({}),
                    0,
                    utc_now(),
                )
                .unwrap();
            model_used(&store, &admin, Some(&task.id), 1, 1);
        }
        let rollup = store
            .usage_rollup(&admin, Utc::now() - chrono::Duration::days(90), None, None)
            .unwrap();
        assert_eq!(rollup.totals.calls, 120);
        assert_eq!(rollup.per_bot.len(), 100);
    }

    #[test]
    fn memory_and_bot_artifact_counts() {
        let store = store();
        let admin = admin(&store);
        assert_eq!(store.count_memories(&admin).unwrap(), 0);
        assert_eq!(store.count_bot_artifacts(&admin, "main").unwrap(), 0);
        crate::memory::add(&store, &admin, "fact", "synthetic fact", "test").unwrap();
        assert_eq!(store.count_memories(&admin).unwrap(), 1);

        let task = store
            .enqueue_task(&admin, "turn", "main", &serde_json::json!({}), 0, utc_now())
            .unwrap();
        store
            .ensure_conversation_accepts_turns(&admin, "owner")
            .unwrap();
        let turn = store
            .create_turn(&admin, &task.id, "main", "owner", &serde_json::json!({}))
            .unwrap();
        store
            .create_action(
                &admin,
                "deck_build",
                Some(&task.id),
                Some(&turn.id),
                "{}",
                "hash-a",
                None,
                Some("artifact-a"),
                "policy-synthetic",
            )
            .unwrap();
        store
            .create_action(
                &admin,
                "deck_build",
                Some(&task.id),
                Some(&turn.id),
                "{}",
                "hash-b",
                None,
                Some("artifact-a"),
                "policy-synthetic",
            )
            .unwrap();
        // Another bot's action must not count for main.
        let other_task = store
            .enqueue_task(
                &admin,
                "turn",
                "analyst",
                &serde_json::json!({}),
                0,
                utc_now(),
            )
            .unwrap();
        let other_turn = store
            .create_turn(
                &admin,
                &other_task.id,
                "analyst",
                "owner",
                &serde_json::json!({}),
            )
            .unwrap();
        store
            .create_action(
                &admin,
                "deck_build",
                Some(&other_task.id),
                Some(&other_turn.id),
                "{}",
                "hash-c",
                None,
                Some("artifact-c"),
                "policy-synthetic",
            )
            .unwrap();
        // Distinct hashes for main: artifact-a counted once.
        assert_eq!(store.count_bot_artifacts(&admin, "main").unwrap(), 1);
        assert_eq!(store.count_bot_artifacts(&admin, "analyst").unwrap(), 1);
        assert_eq!(store.count_bot_artifacts(&admin, "nobody").unwrap(), 0);
    }

    #[test]
    fn conversation_library_joins_actions_to_turns() {
        let store = store();
        let admin = admin(&store);
        store
            .ensure_conversation_accepts_turns(&admin, "dir:main")
            .unwrap();
        let task = store
            .enqueue_task(&admin, "turn", "main", &serde_json::json!({}), 0, utc_now())
            .unwrap();
        let turn = store
            .create_turn(&admin, &task.id, "main", "dir:main", &serde_json::json!({}))
            .unwrap();
        store
            .create_action(
                &admin,
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
                &admin,
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
                &admin,
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
                &admin,
                &action.id,
                "executed",
                Some(&serde_json::json!({"artifact_hash": "def456"})),
            )
            .unwrap();
        let artifacts = store
            .list_conversation_artifacts(&admin, "dir:main")
            .unwrap();
        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].hash, "def456");
        assert_eq!(artifacts[0].tool, "deck_render");
        assert_eq!(artifacts[1].hash, "abc123");
        assert!(
            store
                .list_conversation_artifacts(&admin, "dir:other")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn conversation_library_includes_non_approval_artifacts() {
        let store = store();
        let admin = admin(&store);
        store
            .ensure_conversation_accepts_turns(&admin, "dir:main")
            .unwrap();
        let task = store
            .enqueue_task(&admin, "turn", "main", &serde_json::json!({}), 0, utc_now())
            .unwrap();
        let turn = store
            .create_turn(&admin, &task.id, "main", "dir:main", &serde_json::json!({}))
            .unwrap();
        store
            .record_conversation_artifact(
                &admin,
                "dir:main",
                Some(&turn.id),
                "docx_build",
                "hash-doc",
                "report.docx",
                None,
            )
            .unwrap();
        // A second write for the same hash updates the row instead of
        // duplicating it.
        store
            .record_conversation_artifact(
                &admin,
                "dir:main",
                Some(&turn.id),
                "docx_build",
                "hash-doc",
                "report.docx",
                None,
            )
            .unwrap();
        let artifacts = store
            .list_conversation_artifacts(&admin, "dir:main")
            .unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].hash, "hash-doc");
        assert_eq!(artifacts[0].tool, "docx_build");
        assert_eq!(store.count_bot_artifacts(&admin, "main").unwrap(), 1);
    }

    #[test]
    fn visible_history_keeps_specialist_names() {
        let store = store();
        let admin = admin(&store);
        store
            .append_message_named(
                &admin,
                "room:review",
                None,
                Role::Assistant,
                "from analyst",
                Some("analyst"),
            )
            .unwrap();
        store
            .append_message(&admin, "room:review", None, Role::Tool, "hidden tool")
            .unwrap();
        let messages = store
            .list_visible_messages(&admin, "room:review", 10)
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].name.as_deref(), Some("analyst"));
        assert_eq!(messages[0].content, "from analyst");
    }

    #[test]
    fn visible_history_attributes_assistant_messages_to_the_turn_bot() {
        let store = store();
        let admin = admin(&store);
        let task = store
            .enqueue_task(
                &admin,
                "turn",
                "analyst",
                &serde_json::json!({}),
                0,
                utc_now(),
            )
            .unwrap();
        let turn = store
            .create_turn(
                &admin,
                &task.id,
                "analyst",
                "dir:analyst",
                &serde_json::json!({}),
            )
            .unwrap();
        store
            .append_message(
                &admin,
                "dir:analyst",
                Some(&turn.id),
                Role::Assistant,
                "from analyst",
            )
            .unwrap();
        let messages = store
            .list_visible_messages(&admin, "dir:analyst", 10)
            .unwrap();
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
        assert_eq!(version, 13);
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
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='users'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='invites'",
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
        assert_eq!(version, 13);
        assert!(store.list_device_tokens().unwrap().is_empty());
        // The team table arrives with the same migration batch.
        assert!(
            store
                .get_conversation_team(&admin(&store), "room:review")
                .unwrap()
                .is_none()
        );
        // The identity batch arrived too: the bootstrap admin exists.
        let admin = store.admin_user().unwrap().unwrap();
        assert_eq!(admin.id, ADMIN_USER_ID);
        assert_eq!(admin.role, UserRole::Admin);
    }

    /// A database at `user_version = 7` (the last single-operator schema)
    /// carries over: the admin user appears, `kv['admin_user_id']` is set,
    /// and pre-existing device tokens and conversations resolve to the admin.
    #[test]
    fn migrates_a_version_seven_database_with_admin_backfill() {
        let path = std::env::temp_dir().join(format!("serabot-store-v7-{}.db", new_id()));
        {
            let connection = rusqlite::Connection::open(&path).unwrap();
            let migrations = [
                MIGRATION_1,
                MIGRATION_2,
                MIGRATION_3,
                MIGRATION_4,
                MIGRATION_5,
                MIGRATION_6,
                MIGRATION_7,
            ];
            for (index, migration) in migrations.iter().enumerate() {
                connection.execute_batch(migration).unwrap();
                connection
                    .pragma_update(None, "user_version", index as i64 + 1)
                    .unwrap();
            }
            // Pre-migration operator state: a CLI device token, a
            // conversation, and the file-era bot row, with no user anywhere.
            connection
                .execute(
                    "INSERT INTO device_tokens(id, label, token_hash, created_at)
                     VALUES ('tok-legacy', 'desktop', 'legacy-hash', '2026-01-01T00:00:00.000Z')",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO conversations(id, kind, created_at, updated_at)
                     VALUES ('dir:main', 'direct', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO bots(name, config_json, enabled, updated_at)
                     VALUES ('main', '{\"name\":\"main\"}', 1, '2026-01-01T00:00:00.000Z')",
                    [],
                )
                .unwrap();
        }
        let store = Store::open(&path, 384, 5000).unwrap();
        let admin_id = admin(&store);
        let version: i64 = store
            .connection()
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 13);

        let admin = store.admin_user().unwrap().unwrap();
        assert_eq!(admin.id, ADMIN_USER_ID);
        assert_eq!(admin.provider_subject, "local:admin");
        assert_eq!(admin.role, UserRole::Admin);
        assert_eq!(admin.status, UserStatus::Active);
        assert_eq!(admin.email, "");
        assert_eq!(admin.display_name, "Admin");
        // created_at is a real RFC3339 millisecond timestamp, not utc_now()
        // fallback drift.
        assert!(admin.created_at > Utc::now() - chrono::Duration::hours(1));
        assert_eq!(
            store.kv_get("admin_user_id").unwrap().as_deref(),
            Some(ADMIN_USER_ID)
        );

        // Existing data survives untouched and now belongs to the admin.
        let token = store.device_token_by_hash("legacy-hash").unwrap().unwrap();
        assert_eq!(token.id, "tok-legacy");
        assert_eq!(token.user_id.as_deref(), Some(ADMIN_USER_ID));
        let legacy = store.get_conversation(&admin_id, "dir:main").unwrap();
        assert!(legacy.is_some());

        // The admin can no longer be created again by sign-in: the subject is
        // taken, so an attacker cannot claim `local:admin` through OIDC.
        let attempt = store
            .complete_login(
                "local:admin",
                "attacker@example.example",
                "Attacker",
                None,
                10,
            )
            .unwrap();
        assert!(!attempt.created);
        assert_eq!(attempt.user.id, ADMIN_USER_ID);
        assert_eq!(attempt.user.role, UserRole::Admin, "role is never upserted");

        // The file-era bot row now belongs to the admin; its prompt is empty
        // until the startup fleet seeding repairs it from the template.
        let bots = store.list_bots(&admin_id).unwrap();
        assert_eq!(bots.len(), 1);
        assert_eq!(bots[0].name, "main");
        assert!(bots[0].enabled);
        assert_eq!(bots[0].prompt, "");
        assert_eq!(bots[0].config["name"], "main");
        assert!(
            store
                .get_bot(&admin_id, "main")
                .unwrap()
                .is_some_and(|bot| bot.prompt.is_empty())
        );
        // The per-user skill table arrived with the same migration.
        let connection = store.connection().unwrap();
        let user_skills: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='user_skills'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(user_skills, 1);
    }

    /// A database at `user_version = 8` (phase 1, users exist but data rows
    /// are still unscoped) migrates to 9: every user-owned table carries
    /// `user_id` (NOT NULL, referencing a user) and every pre-existing row
    /// is backfilled to the admin.
    #[test]
    fn migrates_a_version_eight_database_with_user_scoping() {
        let path = std::env::temp_dir().join(format!("serabot-store-v8-{}.db", new_id()));
        {
            let connection = rusqlite::Connection::open(&path).unwrap();
            let migrations = [
                MIGRATION_1,
                MIGRATION_2,
                MIGRATION_3,
                MIGRATION_4,
                MIGRATION_5,
                MIGRATION_6,
                MIGRATION_7,
                MIGRATION_8,
            ];
            for (index, migration) in migrations.iter().enumerate() {
                connection.execute_batch(migration).unwrap();
                connection
                    .pragma_update(None, "user_version", index as i64 + 1)
                    .unwrap();
            }
            // Pre-migration single-operator state: one row in every table
            // that migration 0009 must scope.
            connection.execute_batch(
                "INSERT INTO conversations(id, kind, created_at, updated_at)
                 VALUES ('owner', 'legacy', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');
                 INSERT INTO tasks(id, kind, bot, payload_json, status, run_after, created_at, updated_at)
                 VALUES ('task-1', 'turn', 'main', '{}', 'done', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');
                 INSERT INTO turns(id, task_id, bot, conversation_id, status, state_json, created_at, updated_at)
                 VALUES ('turn-1', 'task-1', 'main', 'owner', 'completed', '{}', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');
                 INSERT INTO messages(id, conversation_id, turn_id, role, content, created_at)
                 VALUES ('msg-1', 'owner', 'turn-1', 'user', 'hello', '2026-01-01T00:00:00.000Z');
                 INSERT INTO actions(id, tool, task_id, turn_id, canonical_args, canonical_hash, policy_version, status, created_at, updated_at)
                 VALUES ('act-1', 'upload', 'task-1', 'turn-1', '{}', 'hash-1', 'v1', 'executed', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');
                 INSERT INTO approvals(id, action_id, tool, summary, state, expires_at, created_at)
                 VALUES ('apr-1', 'act-1', 'upload', 'upload', 'approved_once', '2026-01-02T00:00:00.000Z', '2026-01-01T00:00:00.000Z');
                 INSERT INTO standing_grants(id, tool, canonical_hash, policy_version, approval_id, created_at)
                 VALUES ('grt-1', 'upload', 'hash-1', 'v1', 'apr-1', '2026-01-01T00:00:00.000Z');
                 INSERT INTO memories(id, kind, content, source, created_at, updated_at)
                 VALUES ('mem-1', 'fact', 'synthetic fact', 'test', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');
                 INSERT INTO documents(id, source_path, sha256, title, mime, status, created_at, updated_at)
                 VALUES ('doc-1', '/tmp/a.txt', 'sha-1', 'A', 'text/plain', 'ready', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');
                 INSERT INTO routines(id, name, kind, enabled, next_run_at, config_json, created_at, updated_at)
                 VALUES ('rtn-1', 'morning_brief', 'daily', 1, '2026-01-01T00:00:00.000Z', '{}', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');
                 INSERT INTO outbox(id, kind, destination, payload_json, status, created_at, updated_at)
                 VALUES ('obx-1', 'telegram', 'owner', '{}', 'pending', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');
                 INSERT INTO events(at, kind, payload_json)
                 VALUES ('2026-01-01T00:00:00.000Z', 'message', '{}');
                 INSERT INTO opportunities(id, source, external_id, title, first_seen, last_seen)
                 VALUES ('opp-1', 'portal', 'ext-1', 'Tender', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');
                 INSERT INTO deadlines(id, opportunity_id, due_date, precision, source_text, status, observed_at)
                 VALUES ('dl-1', 'opp-1', '2026-01-05', 'date', '5 January', 'upcoming', '2026-01-01T00:00:00.000Z');
                 INSERT INTO source_runs(id, source, started_at, ok, items_seen)
                 VALUES ('run-1', 'portal', '2026-01-01T00:00:00.000Z', 1, 1);",
            )
            .unwrap();
        }
        let store = Store::open(&path, 384, 5000).unwrap();
        let connection = store.connection().unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 13);

        // Every scoped table carries user_id = the admin, and the column is
        // NOT NULL with a real foreign key.
        for table in [
            "conversations",
            "tasks",
            "turns",
            "messages",
            "actions",
            "approvals",
            "standing_grants",
            "memories",
            "documents",
            "routines",
            "outbox",
            "events",
            "opportunities",
            "deadlines",
            "source_runs",
        ] {
            let (count, distinct): (i64, i64) = connection
                .query_row(
                    &format!(
                        "SELECT COUNT(*), COUNT(DISTINCT user_id) FROM {table} WHERE user_id IS NULL OR user_id <> ?1"
                    ),
                    params![ADMIN_USER_ID],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(count, 0, "{table} has rows not owned by the admin");
            assert_eq!(distinct, 0);
            // The migration rebuilt the table, so user_id is NOT NULL and
            // references users(id); an insert with an unknown owner must
            // fail the foreign-key check.
            let not_null: i64 = connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = 'user_id' AND \"notnull\" = 1"),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(not_null, 1, "{table}.user_id must be NOT NULL");
            let violation: Option<i64> = connection
                .query_row(
                    &format!(
                        "SELECT 1 FROM pragma_foreign_key_list('{table}') WHERE \"table\" = 'users' AND \"from\" = 'user_id'"
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(None);
            assert_eq!(
                violation,
                Some(1),
                "{table}.user_id must reference users(id)"
            );
        }
        // The rebuilt schema is referentially clean as a whole.
        let fk_errors: i64 = connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(fk_errors, 0);
        // Indexes survived the rebuild and the new user indexes exist.
        for index in [
            "idx_tasks_claim",
            "idx_tasks_lease",
            "idx_tasks_user",
            "idx_turns_task",
            "idx_messages_conversation",
            "idx_actions_hash",
            "idx_actions_user",
            "idx_approvals_pending",
            "idx_grants_match",
            "idx_memories_user",
            "idx_outbox_status",
            "idx_events_user",
            "idx_deadlines_date",
            "idx_source_runs_source",
            "idx_conversations_parent",
            "idx_conversations_archived",
            "idx_conversations_user",
        ] {
            let found: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    params![index],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(found, 1, "index {index} must exist");
        }
        // And the events sequence did not reset: new events keep counting up.
        let before: i64 = connection
            .query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = 'events'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        store
            .append_event(ADMIN_USER_ID, "test", &serde_json::json!({}))
            .unwrap();
        let after: i64 = connection
            .query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = 'events'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(after > before, "events sequence must keep advancing");
    }

    /// A database at `user_version = 9` (phase 2, every user-owned table is
    /// scoped but `bots` is still keyed by name) migrates to 10: the bot rows
    /// land under the admin with an empty prompt, and `user_skills` exists
    /// with a per-user primary key.
    #[test]
    fn migrates_a_version_nine_database_to_per_user_fleets() {
        let path = std::env::temp_dir().join(format!("serabot-store-v9-{}.db", new_id()));
        {
            let connection = rusqlite::Connection::open(&path).unwrap();
            let migrations = [
                MIGRATION_1,
                MIGRATION_2,
                MIGRATION_3,
                MIGRATION_4,
                MIGRATION_5,
                MIGRATION_6,
                MIGRATION_7,
                MIGRATION_8,
                MIGRATION_9,
            ];
            for (index, migration) in migrations.iter().enumerate() {
                connection.execute_batch(migration).unwrap();
                connection
                    .pragma_update(None, "user_version", index as i64 + 1)
                    .unwrap();
            }
            connection
                .execute(
                    "INSERT INTO bots(name, config_json, enabled, updated_at)
                     VALUES ('main', '{\"name\":\"main\",\"role\":\"orchestrator\"}', 1,
                             '2026-01-01T00:00:00.000Z')",
                    [],
                )
                .unwrap();
        }
        let store = Store::open(&path, 384, 5000).unwrap();
        let connection = store.connection().unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 13);

        // The old row is now the admin's, with the empty prompt the migration
        // header documents; startup seeding fills it from the template.
        let bot = store.get_bot(ADMIN_USER_ID, "main").unwrap().unwrap();
        assert_eq!(bot.name, "main");
        assert_eq!(bot.prompt, "");
        assert!(bot.enabled);
        assert_eq!(bot.config["role"], "orchestrator");

        // The bots primary key is (user_id, name): the same bot name can
        // exist for two users, but not twice for one user.
        let member = store
            .create_user("microsoft:fleet-member", "m@example.example", "M")
            .unwrap();
        store
            .upsert_bot(
                &member.id,
                "main",
                &serde_json::json!({"name": "main"}),
                "member prompt",
                true,
            )
            .unwrap();
        assert_eq!(store.count_bots(ADMIN_USER_ID).unwrap(), 1);
        assert_eq!(store.count_bots(&member.id).unwrap(), 1);
        let duplicate = connection.execute(
            "INSERT INTO bots(user_id, name, config_json, prompt, enabled, updated_at)
             VALUES (?1, 'main', '{}', '', 1, '2026-01-01T00:00:00.000Z')",
            params![ADMIN_USER_ID],
        );
        assert!(duplicate.is_err(), "bots PK must be (user_id, name)");

        // user_skills is per user: the same id can exist for both users.
        assert!(
            store
                .insert_user_skill(ADMIN_USER_ID, "notes", "Notes", "Admin notes", "# Notes\n")
                .unwrap()
        );
        assert!(
            store
                .insert_user_skill(&member.id, "notes", "Notes", "Member notes", "# Notes\n")
                .unwrap()
        );
        assert!(
            !store
                .insert_user_skill(ADMIN_USER_ID, "notes", "Notes", "Again", "# Notes\n")
                .unwrap()
        );
        assert_eq!(store.list_user_skills(ADMIN_USER_ID).unwrap().len(), 1);
        assert_eq!(store.list_user_skills(&member.id).unwrap().len(), 1);
        assert_eq!(
            store
                .get_user_skill(&member.id, "notes")
                .unwrap()
                .unwrap()
                .description,
            "Member notes"
        );
        assert!(
            store
                .get_user_skill(ADMIN_USER_ID, "missing")
                .unwrap()
                .is_none()
        );

        // Both new tables reference users(id) and are referentially clean.
        for table in ["bots", "user_skills"] {
            let fk: Option<i64> = connection
                .query_row(
                    &format!(
                        "SELECT 1 FROM pragma_foreign_key_list('{table}') WHERE \"table\" = 'users' AND \"from\" = 'user_id'"
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(None);
            assert_eq!(fk, Some(1), "{table}.user_id must reference users(id)");
        }
        let fk_errors: i64 = connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(fk_errors, 0);
        let index: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_bots_user'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index, 1);
    }

    /// Bot rows are strictly per user: the same name is two independent rows,
    /// and every read/update/delete is filtered by the acting user.
    #[test]
    fn bots_are_scoped_per_user() {
        let store = store();
        let admin = admin(&store);
        let member = store
            .create_user("microsoft:bot-member", "b@example.example", "B")
            .unwrap();
        let admin_config = serde_json::json!({"name": "notes", "enabled": true});
        let member_config = serde_json::json!({"name": "notes", "enabled": false});
        assert!(
            store
                .insert_bot(&admin, "notes", &admin_config, "admin prompt", true)
                .unwrap()
        );
        assert!(
            store
                .insert_bot(&member.id, "notes", &member_config, "member prompt", false)
                .unwrap()
        );
        assert!(
            !store
                .insert_bot(&admin, "notes", &admin_config, "again", true)
                .unwrap()
        );

        let admin_bot = store.get_bot(&admin, "notes").unwrap().unwrap();
        assert_eq!(admin_bot.prompt, "admin prompt");
        assert!(admin_bot.enabled);
        let member_bot = store.get_bot(&member.id, "notes").unwrap().unwrap();
        assert_eq!(member_bot.prompt, "member prompt");
        assert!(!member_bot.enabled);

        // Cross-user writes and deletes miss; the owner's row is untouched.
        assert!(
            store
                .insert_bot(&admin, "admin-only", &admin_config, "admin only", true)
                .unwrap()
        );
        assert!(
            !store
                .update_bot(&member.id, "admin-only", &member_config, "hijacked", true)
                .unwrap()
        );
        assert_eq!(
            store.get_bot(&admin, "admin-only").unwrap().unwrap().prompt,
            "admin only"
        );
        assert!(!store.delete_bot(&member.id, "admin-only").unwrap());
        assert!(store.get_bot(&admin, "admin-only").unwrap().is_some());
        assert!(store.get_bot(&admin, "notes").unwrap().is_some());

        // The owner can update and delete their own row.
        assert!(
            store
                .update_bot(&admin, "notes", &admin_config, "new prompt", false)
                .unwrap()
        );
        assert_eq!(
            store.get_bot(&admin, "notes").unwrap().unwrap().prompt,
            "new prompt"
        );
        assert!(store.delete_bot(&admin, "notes").unwrap());
        assert!(store.get_bot(&admin, "notes").unwrap().is_none());
        assert!(store.get_bot(&member.id, "notes").unwrap().is_some());
    }

    /// User-created skills are strictly per user: the same id can exist for
    /// two users, and one user's row is invisible to the other.
    #[test]
    fn user_skills_are_scoped_per_user() {
        let store = store();
        let admin = admin(&store);
        let member = store
            .create_user("microsoft:skill-member", "s@example.example", "S")
            .unwrap();
        assert!(
            store
                .insert_user_skill(&admin, "notes", "Notes", "Admin notes", "# Admin\n")
                .unwrap()
        );
        assert!(
            store
                .insert_user_skill(&member.id, "notes", "Notes", "Member notes", "# Member\n")
                .unwrap()
        );
        assert!(
            !store
                .insert_user_skill(&admin, "notes", "Notes", "Again", "# Again\n")
                .unwrap()
        );
        assert_eq!(
            store
                .get_user_skill(&admin, "notes")
                .unwrap()
                .unwrap()
                .description,
            "Admin notes"
        );
        assert_eq!(
            store
                .get_user_skill(&member.id, "notes")
                .unwrap()
                .unwrap()
                .description,
            "Member notes"
        );
        assert!(
            store
                .get_user_skill(&member.id, "missing")
                .unwrap()
                .is_none()
        );
        assert!(store.delete_user_skill(&member.id, "notes").unwrap());
        assert!(store.get_user_skill(&admin, "notes").unwrap().is_some());
        assert!(!store.delete_user_skill(&member.id, "notes").unwrap());
    }

    #[test]
    fn users_round_trip_and_admin_is_unique() {
        let store = store();
        let admin = store.admin_user().unwrap().unwrap();
        assert_eq!(admin.role, UserRole::Admin);
        assert_eq!(store.user_count().unwrap(), 1);

        let created = store
            .create_user("microsoft:abc123", "member@example.example", "Member One")
            .unwrap();
        assert_eq!(created.role, UserRole::Member);
        assert_eq!(created.status, UserStatus::Active);
        assert_ne!(created.id, admin.id);

        assert_eq!(
            store
                .get_user_by_subject("microsoft:abc123")
                .unwrap()
                .unwrap()
                .id,
            created.id
        );
        assert!(store.get_user("missing-user").unwrap().is_none());
        assert!(
            store
                .get_user_by_subject("microsoft:nobody")
                .unwrap()
                .is_none()
        );

        let listed = store.list_users().unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(
            listed[0].id, admin.id,
            "oldest first: admin bootstrapped first"
        );

        // Duplicate subjects are rejected by the UNIQUE constraint.
        assert!(
            store
                .create_user("microsoft:abc123", "clone@example.example", "Clone")
                .is_err()
        );

        // Oversized identity fields fail closed.
        assert!(
            store
                .create_user(
                    &format!("microsoft:{}", "x".repeat(400)),
                    "big@example.example",
                    "Big"
                )
                .is_err()
        );
        assert!(
            store
                .create_user("microsoft:ok", &"e".repeat(400), "Email Too Long")
                .is_err()
        );
    }

    #[test]
    fn disabled_users_cannot_authenticate_and_can_be_reenabled() {
        let store = store();
        let user = store
            .create_user("microsoft:disable-me", "off@example.example", "Off")
            .unwrap();
        let token = store
            .mint_session(&user.id, chrono::Duration::hours(1))
            .unwrap();
        let hash = sha256_hex(&token);
        assert!(store.session_user(&hash).unwrap().is_some());

        assert!(
            store
                .set_user_status(&user.id, UserStatus::Disabled)
                .unwrap()
        );
        assert_eq!(
            store.get_user(&user.id).unwrap().unwrap().status,
            UserStatus::Disabled
        );
        // The minted session immediately stops authorizing.
        assert!(store.session_user(&hash).unwrap().is_none());
        // And no new session can be minted while disabled.
        assert!(
            store
                .mint_session(&user.id, chrono::Duration::hours(1))
                .is_err()
        );
        // `complete_login` still resolves the disabled account (so the OIDC
        // callback audits the attempt); session issuance — the assertion
        // above — is the gate that fails closed.
        assert!(
            store
                .complete_login(
                    "microsoft:disable-me",
                    "off@example.example",
                    "Off",
                    None,
                    10,
                )
                .is_ok()
        );

        assert!(store.set_user_status(&user.id, UserStatus::Active).unwrap());
        assert!(store.session_user(&hash).unwrap().is_some());
        assert!(
            !store
                .set_user_status("missing-user", UserStatus::Active)
                .unwrap()
        );
    }

    #[test]
    fn invites_enforce_ttl_uses_and_transactional_consumption() {
        let store = store();
        let admin_id = store.admin_user_id().unwrap();

        let code = "invite-code-with-entropy-0123456789";
        store
            .mint_invite(code, 2, utc_now() + chrono::Duration::hours(48), &admin_id)
            .unwrap();
        let hash = sha256_hex(code);
        assert!(store.invite_is_redeemable(&hash).unwrap());
        assert!(
            !store
                .invite_is_redeemable(&sha256_hex("unknown-code"))
                .unwrap()
        );

        store.consume_invite(&hash).unwrap();
        assert!(store.invite_is_redeemable(&hash).unwrap(), "one use left");
        store.consume_invite(&hash).unwrap();
        assert!(
            !store.invite_is_redeemable(&hash).unwrap(),
            "uses exhausted"
        );
        assert!(store.consume_invite(&hash).is_err());

        // Expired invites are not redeemable and fail consumption.
        let expired = "expired-invite-code-0123456789abcdef";
        store
            .mint_invite(
                expired,
                5,
                utc_now() - chrono::Duration::seconds(1),
                &admin_id,
            )
            .unwrap();
        let expired_hash = sha256_hex(expired);
        assert!(!store.invite_is_redeemable(&expired_hash).unwrap());
        assert!(store.consume_invite(&expired_hash).is_err());

        // Bounds: zero uses and absurd codes are rejected.
        assert!(
            store
                .mint_invite(
                    "zero-uses",
                    0,
                    utc_now() + chrono::Duration::hours(1),
                    &admin_id
                )
                .is_err()
        );
        assert!(
            store
                .mint_invite("", 1, utc_now() + chrono::Duration::hours(1), &admin_id)
                .is_err()
        );
        assert!(
            store
                .mint_invite(
                    &"x".repeat(300),
                    1,
                    utc_now() + chrono::Duration::hours(1),
                    &admin_id
                )
                .is_err()
        );
    }

    #[test]
    fn complete_login_creates_with_invite_and_reuses_existing_subject() {
        let store = store();
        let admin_id = store.admin_user_id().unwrap();
        let code = "first-login-invite-0123456789abcdef";
        store
            .mint_invite(code, 1, utc_now() + chrono::Duration::hours(48), &admin_id)
            .unwrap();
        let invite_hash = sha256_hex(code);

        // No invite: creating a new subject fails closed.
        assert!(
            store
                .complete_login("microsoft:new-user", "new@example.example", "New", None, 10)
                .is_err()
        );

        // With the invite: the user is created and the invite is consumed.
        let outcome = store
            .complete_login(
                "microsoft:new-user",
                "new@example.example",
                "New",
                Some(&invite_hash),
                10,
            )
            .unwrap();
        assert!(outcome.created);
        assert_eq!(outcome.user.role, UserRole::Member);
        assert!(!store.invite_is_redeemable(&invite_hash).unwrap());

        // The same subject signing in again needs no new invite and refreshes
        // the profile fields.
        let again = store
            .complete_login(
                "microsoft:new-user",
                "renamed@example.example",
                "Renamed",
                None,
                10,
            )
            .unwrap();
        assert!(!again.created);
        assert_eq!(again.user.id, outcome.user.id);
        assert_eq!(again.user.email, "renamed@example.example");
        assert_eq!(again.user.display_name, "Renamed");

        // Exhausted invite cannot create a second account.
        let other = store
            .complete_login(
                "microsoft:second-user",
                "second@example.example",
                "Second",
                Some(&invite_hash),
                10,
            )
            .unwrap_err();
        assert!(other.to_string().contains("invitation"));

        // The user cap is enforced, invite or not.
        let cap_code = "cap-invite-0123456789abcdef";
        store
            .mint_invite(
                cap_code,
                10,
                utc_now() + chrono::Duration::hours(48),
                &admin_id,
            )
            .unwrap();
        let error = store
            .complete_login(
                "microsoft:third-user",
                "third@example.example",
                "Third",
                Some(&sha256_hex(cap_code)),
                2, // admin + new-user already exist
            )
            .unwrap_err();
        assert!(error.to_string().contains("limit"));
    }

    #[test]
    fn sessions_store_hashes_only_and_revoke_expires() {
        let store = store();
        let user = store
            .create_user("microsoft:session-user", "s@example.example", "S")
            .unwrap();

        let token = store
            .mint_session(&user.id, chrono::Duration::hours(2))
            .unwrap();
        assert_eq!(token.len(), Store::SESSION_TOKEN_BYTES * 2);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        // The stored hash is not the token, and the token appears nowhere.
        let hash = sha256_hex(&token);
        let record = store.device_token_by_hash(&hash).unwrap().unwrap();
        assert_eq!(record.user_id.as_deref(), Some(user.id.as_str()));
        assert_eq!(record.label, Store::SESSION_LABEL);
        assert!(record.expires_at.is_some());
        assert!(record.active_at(utc_now()));

        let resolved = store.session_user(&hash).unwrap().unwrap();
        assert_eq!(resolved.id, user.id);

        // Unknown users and bogus ttls fail closed.
        assert!(
            store
                .mint_session("missing-user", chrono::Duration::hours(1))
                .is_err()
        );
        assert!(
            store
                .mint_session(&user.id, chrono::Duration::zero())
                .is_err()
        );
        assert!(
            store
                .mint_session(&user.id, Store::MAX_SESSION_TTL * 2)
                .is_err()
        );

        // Revocation kills the session and is idempotent.
        assert!(store.revoke_session(&hash).unwrap());
        assert!(store.session_user(&hash).unwrap().is_none());
        assert!(
            store.revoke_session(&hash).unwrap(),
            "revoking twice stays true"
        );
        assert!(!store.revoke_session(&sha256_hex("never-minted")).unwrap());

        // Expiry: a session with a past TTL never resolves.
        let expired_token = store
            .mint_session(&user.id, chrono::Duration::hours(1))
            .unwrap();
        let expired_hash = sha256_hex(&expired_token);
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE device_tokens SET expires_at = ?1 WHERE token_hash = ?2",
                params![
                    to_string(utc_now() - chrono::Duration::seconds(1)),
                    expired_hash
                ],
            )
            .unwrap();
        assert!(store.session_user(&expired_hash).unwrap().is_none());
    }

    #[test]
    fn device_tokens_round_trip_without_storing_plaintext() {
        let store = store();
        let expires = utc_now() + chrono::Duration::days(30);
        store
            .insert_device_token("tok-1", "desktop-win", "hash-only", None, Some(expires))
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
                None,
                Some(utc_now() - chrono::Duration::seconds(1)),
            )
            .unwrap();
        let token = store.device_token_by_hash("expired-hash").unwrap().unwrap();
        assert!(!token.active_at(utc_now()));
    }

    #[test]
    fn conversations_keep_metadata_and_paginate() {
        let store = store();
        let admin = admin(&store);
        store
            .create_conversation(&admin, "sec:finance", "section", None)
            .unwrap();
        store
            .create_conversation(&admin, "grp:ops", "group", Some("sec:finance"))
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
            .append_message(&admin, "grp:ops", None, Role::User, "hello")
            .unwrap();

        let all = store.list_conversations(&admin, 10, None, false).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "grp:ops", "message activity sorts first");
        assert!(all[0].last_message_at.is_some());
        assert!(all[1].title.is_none());

        assert!(
            store
                .rename_conversation(&admin, "grp:ops", "Operations")
                .unwrap()
        );
        assert!(!store.rename_conversation(&admin, "missing", "x").unwrap());
        let renamed = store.get_conversation(&admin, "grp:ops").unwrap().unwrap();
        assert_eq!(renamed.title.as_deref(), Some("Operations"));

        let page = store.list_conversations(&admin, 1, None, false).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, "grp:ops");
        let rest = store
            .list_conversations(&admin, 10, Some((page[0].updated_at, &page[0].id)), false)
            .unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].id, "sec:finance");

        assert!(store.archive_conversation(&admin, "grp:ops").unwrap());
        let visible = store.list_conversations(&admin, 10, None, false).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "sec:finance");
        let with_archived = store.list_conversations(&admin, 10, None, true).unwrap();
        assert_eq!(with_archived.len(), 2);
        let archived = store.get_conversation(&admin, "grp:ops").unwrap().unwrap();
        assert!(archived.archived_at.is_some());
        assert_eq!(
            store
                .list_visible_messages(&admin, "grp:ops", 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn claims_competing_tasks_once() {
        let store = store();
        let admin = admin(&store);
        store
            .enqueue_task(
                &admin,
                "turn",
                "main",
                &serde_json::json!({"x": 1}),
                0,
                utc_now(),
            )
            .unwrap();
        let first = store.claim_task("a", 30).unwrap();
        let second = store.claim_task("b", 30).unwrap();
        assert!(first.is_some());
        assert!(second.is_none());
    }

    #[test]
    fn fencing_prevents_stale_completion() {
        let store = store();
        let admin = admin(&store);
        let task = store
            .enqueue_task(&admin, "turn", "main", &serde_json::json!({}), 0, utc_now())
            .unwrap();
        let claimed = store.claim_task("worker", 30).unwrap().unwrap();
        assert_eq!(claimed.user_id, admin);
        assert!(
            !store
                .complete_task(&admin, &task.id, claimed.fencing_token + 1)
                .unwrap()
        );
        assert!(
            store
                .complete_task(&admin, &task.id, claimed.fencing_token)
                .unwrap()
        );
    }

    /// The daily quota counts only the acting user's tasks for the current
    /// UTC day. The (limit + 1)th enqueue fails with an explicit validation
    /// error and writes nothing; another user still has their own budget.
    #[test]
    fn task_quota_is_per_user_and_per_utc_day() {
        let store = store();
        let admin = admin(&store);
        let member = store
            .create_user("microsoft:quota-member", "q@example.example", "Q")
            .unwrap();
        for _ in 0..2 {
            store
                .enqueue_task_with_quota(
                    &admin,
                    "turn",
                    "main",
                    &serde_json::json!({}),
                    0,
                    utc_now(),
                    2,
                )
                .unwrap();
        }
        let error = store
            .enqueue_task_with_quota(
                &admin,
                "turn",
                "main",
                &serde_json::json!({}),
                0,
                utc_now(),
                2,
            )
            .unwrap_err();
        assert!(
            matches!(&error, SeraError::Validation(message) if message.contains("2 tasks per user per UTC day")),
            "unexpected error: {error}"
        );
        assert_eq!(store.pending_task_count(&admin).unwrap(), 2);
        // The member's budget is untouched by the admin's usage.
        store
            .enqueue_task_with_quota(
                &member.id,
                "turn",
                "main",
                &serde_json::json!({}),
                0,
                utc_now(),
                2,
            )
            .unwrap();
        assert_eq!(store.pending_task_count(&member.id).unwrap(), 1);

        // Backdating the admin's rows past the UTC day boundary frees the
        // budget: the window is the calendar day, not a rolling counter.
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE tasks SET created_at = ?1 WHERE user_id = ?2",
                params![to_string(utc_now() - chrono::Duration::days(1)), admin],
            )
            .unwrap();
        store
            .enqueue_task_with_quota(
                &admin,
                "turn",
                "main",
                &serde_json::json!({}),
                0,
                utc_now(),
                2,
            )
            .unwrap();
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
        let admin = admin(&store);
        let action = store
            .create_action(
                &admin,
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
                &admin,
                &action.id,
                "onedrive_upload",
                "upload",
                utc_now() + chrono::Duration::hours(1),
            )
            .unwrap();
        assert!(
            store
                .transition_approval(&admin, &approval.id, "approved_once", None, None)
                .unwrap()
        );
        assert!(
            !store
                .transition_approval(&admin, &approval.id, "denied", None, None)
                .unwrap()
        );
    }

    #[test]
    fn expired_approvals_cancel_suspended_tasks() {
        let store = store();
        let admin = admin(&store);
        let task = store
            .enqueue_task(&admin, "turn", "main", &serde_json::json!({}), 0, utc_now())
            .unwrap();
        let claimed = store.claim_task("worker", 30).unwrap().unwrap();
        store
            .await_approval_task(&admin, &task.id, claimed.fencing_token)
            .unwrap();
        let action = store
            .create_action(
                &admin,
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
                &admin,
                &action.id,
                "upload",
                "upload",
                utc_now() - chrono::Duration::minutes(1),
            )
            .unwrap();
        assert_eq!(store.expire_approvals().unwrap(), 1);
        assert_eq!(
            store.get_approval(&admin, &approval.id).unwrap().state,
            "expired"
        );
        assert_eq!(
            store.get_action(&admin, &action.id).unwrap().status,
            "expired"
        );
        assert_eq!(store.get_task(&admin, &task.id).unwrap().status, "failed");
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
        let admin = admin(&store);
        let task = store
            .enqueue_task(&admin, "turn", "main", &serde_json::json!({}), 0, utc_now())
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
        let recovered = store.get_task(&admin, &task.id).unwrap();
        assert_eq!(recovered.status, "failed");
        assert_eq!(recovered.fencing_token, claimed.fencing_token);
    }

    #[test]
    fn conversation_metadata_rejects_container_turns() {
        let store = store();
        let admin = admin(&store);
        store
            .create_conversation(&admin, "sec:finance", "section", None)
            .unwrap();
        assert!(
            store
                .ensure_conversation_accepts_turns(&admin, "sec:finance")
                .is_err()
        );
        assert!(
            store
                .ensure_conversation_accepts_turns(&admin, "grp:ops")
                .is_err()
        );
        assert!(store.get_conversation(&admin, "grp:ops").unwrap().is_none());
        store
            .ensure_conversation_accepts_turns(&admin, "owner")
            .unwrap();
        store
            .ensure_conversation_accepts_turns(&admin, "dir:main")
            .unwrap();
        assert_eq!(
            store
                .get_conversation(&admin, "owner")
                .unwrap()
                .unwrap()
                .kind,
            "legacy"
        );
        assert_eq!(
            store
                .get_conversation(&admin, "dir:main")
                .unwrap()
                .unwrap()
                .kind,
            "direct"
        );
        store
            .ensure_conversation_accepts_turns(&admin, "owner")
            .unwrap();
    }

    #[test]
    fn conversation_tree_links_parents_and_rejects_bad_edges() {
        let store = store();
        let admin = admin(&store);
        store
            .create_conversation(&admin, "sec:finance", "section", None)
            .unwrap();
        store
            .create_conversation(&admin, "grp:ops", "group", Some("sec:finance"))
            .unwrap();
        store
            .create_conversation(&admin, "dir:analyst", "direct", Some("grp:ops"))
            .unwrap();
        store
            .create_conversation(&admin, "room:review", "room", Some("sec:finance"))
            .unwrap();
        assert_eq!(
            store
                .get_conversation(&admin, "dir:analyst")
                .unwrap()
                .unwrap()
                .parent_id
                .as_deref(),
            Some("grp:ops")
        );
        assert!(
            store
                .ensure_conversation_accepts_turns(&admin, "sec:finance")
                .is_err()
        );
        assert!(
            store
                .ensure_conversation_accepts_turns(&admin, "grp:ops")
                .is_err()
        );
        store
            .ensure_conversation_accepts_turns(&admin, "dir:analyst")
            .unwrap();
        store
            .ensure_conversation_accepts_turns(&admin, "room:review")
            .unwrap();
        assert!(
            store
                .create_conversation(&admin, "grp:other", "group", None)
                .is_err()
        );
        assert!(
            store
                .create_conversation(&admin, "sec:nested", "section", Some("sec:finance"))
                .is_err()
        );
        assert!(
            store
                .create_conversation(&admin, "dir:bad", "direct", Some("dir:analyst"))
                .is_err()
        );
        assert!(
            store
                .create_conversation(&admin, "sec:finance", "section", None)
                .unwrap_err()
                .to_string()
                .contains("already exists")
        );
    }

    // ---- Isolation suite (phase 2: cross-user access is impossible) ----

    /// User B never sees user A's conversations, messages, memories,
    /// documents, approvals, tasks or events: every scoped read returns
    /// empty for the rows of another user.
    #[test]
    fn isolation_cross_user_reads_return_nothing() {
        let store = store();
        let admin_id = admin(&store);
        let member = member(&store);

        // Admin owns a conversation with messages.
        store
            .ensure_conversation_accepts_turns(&admin_id, "dir:main")
            .unwrap();
        store
            .append_message(&admin_id, "dir:main", None, Role::User, "admin secret")
            .unwrap();
        let task = store
            .enqueue_task(
                &admin_id,
                "turn",
                "main",
                &serde_json::json!({}),
                0,
                utc_now(),
            )
            .unwrap();
        let turn = store
            .create_turn(
                &admin_id,
                &task.id,
                "main",
                "dir:main",
                &serde_json::json!({}),
            )
            .unwrap();
        let action = store
            .create_action(
                &admin_id,
                "upload",
                Some(&task.id),
                Some(&turn.id),
                "{}",
                "hash-x",
                Some("folder"),
                None,
                "p1",
            )
            .unwrap();
        let _approval = store
            .create_approval(
                &admin_id,
                &action.id,
                "upload",
                "admin upload",
                utc_now() + chrono::Duration::hours(1),
            )
            .unwrap();
        crate::memory::add(&store, &admin_id, "fact", "admin private fact", "test").unwrap();
        store
            .append_event(
                &admin_id,
                "message",
                &serde_json::json!({"text": "admin event"}),
            )
            .unwrap();

        // The member sees none of it.
        assert!(
            store
                .list_conversations(&member.id, 100, None, true)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .get_conversation(&member.id, "dir:main")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .list_messages(&member.id, "dir:main", 100)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .list_visible_messages(&member.id, "dir:main", 100)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .history_for_llm(&member.id, "dir:main", 100)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .get_conversation_team(&member.id, "dir:main")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .list_conversation_artifacts(&member.id, "dir:main")
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .list_pending_approvals(&member.id, 100)
                .unwrap()
                .is_empty()
        );
        assert!(store.get_task(&member.id, &task.id).is_err());
        assert_eq!(store.count_memories(&member.id).unwrap(), 0);
        assert!(store.events_after(&member.id, 0, 100).unwrap().is_empty());
        assert_eq!(store.pending_task_count(&member.id).unwrap(), 0);
        // The member's audit view holds only their own rows; the admin's
        // event/audit rows are not theirs.
        assert!(
            store
                .list_audit_for_user(&member.id, 100)
                .unwrap()
                .iter()
                .all(|entry| entry.actor == member.id)
        );
    }

    /// Cross-user writes fail closed: another user's conversation id cannot
    /// receive messages or turns, lazy creation refuses a taken id, and
    /// renames/archives on foreign rows report "not found".
    #[test]
    fn isolation_cross_user_writes_fail_closed() {
        let store = store();
        let admin_id = admin(&store);
        let member = member(&store);

        store
            .ensure_conversation_accepts_turns(&admin_id, "dir:main")
            .unwrap();

        // The member cannot append to the admin's conversation.
        assert!(
            store
                .append_message(&member.id, "dir:main", None, Role::User, "intrusion")
                .is_err()
        );
        // The member cannot create a turn under the admin's task.
        let admin_task = store
            .enqueue_task(
                &admin_id,
                "turn",
                "main",
                &serde_json::json!({}),
                0,
                utc_now(),
            )
            .unwrap();
        assert!(
            store
                .create_turn(
                    &member.id,
                    &admin_task.id,
                    "main",
                    "dir:main",
                    &serde_json::json!({})
                )
                .is_err()
        );
        // The member's own task cannot create a turn in the admin's
        // conversation either.
        let member_task = store
            .enqueue_task(
                &member.id,
                "turn",
                "main",
                &serde_json::json!({}),
                0,
                utc_now(),
            )
            .unwrap();
        assert!(
            store
                .create_turn(
                    &member.id,
                    &member_task.id,
                    "main",
                    "dir:main",
                    &serde_json::json!({})
                )
                .is_err()
        );
        // Lazy creation refuses an id another user already owns.
        assert!(
            store
                .ensure_conversation_accepts_turns(&member.id, "dir:main")
                .is_err()
        );
        // Foreign conversation mutations report the same as missing ids.
        assert!(
            !store
                .rename_conversation(&member.id, "dir:main", "Mine")
                .unwrap()
        );
        assert!(!store.archive_conversation(&member.id, "dir:main").unwrap());
        assert!(
            !store
                .unarchive_conversation(&member.id, "dir:main")
                .unwrap()
        );
        assert!(
            !store
                .mark_conversation_read(&member.id, "dir:main")
                .unwrap()
        );
        assert!(
            store
                .set_conversation_team(&member.id, "dir:main", &["analyst".into()], "selected", "")
                .is_err()
        );
        // The admin's task cannot be cancelled or completed by the member.
        assert!(
            !store
                .cancel_task(&member.id, &admin_task.id, "intrusion")
                .unwrap()
        );
        assert!(
            !store
                .requeue_task(&member.id, &admin_task.id, utc_now())
                .unwrap()
        );
        // Grants never match across users.
        let grant_action = store
            .create_action(
                &admin_id,
                "upload",
                None,
                None,
                "{}",
                "grant-hash",
                None,
                None,
                "p1",
            )
            .unwrap();
        let grant_approval = store
            .create_approval(
                &admin_id,
                &grant_action.id,
                "upload",
                "grant seed",
                utc_now() + chrono::Duration::hours(1),
            )
            .unwrap();
        store
            .insert_grant(
                &admin_id,
                "upload",
                "grant-hash",
                None,
                "p1",
                &grant_approval.id,
            )
            .unwrap();
        assert!(
            !store
                .matching_grant(&member.id, "upload", "grant-hash", None, "p1")
                .unwrap()
        );
        assert!(
            store
                .matching_grant(&admin_id, "upload", "grant-hash", None, "p1")
                .unwrap()
        );
        // Revoking grants touches only the acting user's rows.
        assert_eq!(store.revoke_grants(&member.id).unwrap(), 0);
        assert!(
            store
                .matching_grant(&admin_id, "upload", "grant-hash", None, "p1")
                .unwrap()
        );
        assert_eq!(store.revoke_grants(&admin_id).unwrap(), 1);
    }

    /// The pre-existing single-operator flow keeps working end to end under
    /// the admin's id, and the worker executes a task under its owning user
    /// only: a member's task resolves member-scoped state everywhere.
    #[test]
    fn isolation_worker_processes_tasks_under_the_owning_user() {
        let store = store();
        let admin_id = admin(&store);
        let member = member(&store);

        // Admin flow: enqueue → claim → turn rows all carry the admin id.
        let admin_task = store
            .enqueue_task(
                &admin_id,
                "turn",
                "main",
                &serde_json::json!({"conversation_id": "owner"}),
                0,
                utc_now(),
            )
            .unwrap();
        assert_eq!(admin_task.user_id, admin_id);
        store
            .ensure_conversation_accepts_turns(&admin_id, "owner")
            .unwrap();
        let claimed = store.claim_task("worker-1", 30).unwrap().unwrap();
        assert_eq!(claimed.id, admin_task.id);
        assert_eq!(claimed.user_id, admin_id);
        let turn = store
            .create_turn(
                &admin_id,
                &claimed.id,
                "main",
                "owner",
                &serde_json::json!({}),
            )
            .unwrap();
        store
            .append_message(&admin_id, "owner", Some(&turn.id), Role::User, "hello")
            .unwrap();
        store
            .update_turn(&admin_id, &turn.id, "completed", &serde_json::json!({}))
            .unwrap();
        assert!(
            store
                .complete_task(&admin_id, &claimed.id, claimed.fencing_token)
                .unwrap()
        );
        let owner: String = store
            .connection()
            .unwrap()
            .query_row(
                "SELECT user_id FROM turns WHERE id = ?1",
                params![turn.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owner, admin_id);

        // Member flow: the same pipeline under the member's id, invisible to
        // the admin and vice versa.
        store
            .ensure_conversation_accepts_turns(&member.id, "dir:main")
            .unwrap();
        store
            .append_message(&member.id, "dir:main", None, Role::User, "member hello")
            .unwrap();
        let member_task = store
            .enqueue_task(
                &member.id,
                "turn",
                "main",
                &serde_json::json!({"conversation_id": "dir:main"}),
                0,
                utc_now(),
            )
            .unwrap();
        assert_eq!(member_task.user_id, member.id);
        let claimed = store.claim_task("worker-2", 30).unwrap().unwrap();
        assert_eq!(claimed.id, member_task.id);
        assert_eq!(claimed.user_id, member.id);
        let member_turn = store
            .create_turn(
                &member.id,
                &claimed.id,
                "main",
                "dir:main",
                &serde_json::json!({}),
            )
            .unwrap();
        store
            .update_turn(
                &member.id,
                &member_turn.id,
                "completed",
                &serde_json::json!({}),
            )
            .unwrap();
        assert!(
            store
                .complete_task(&member.id, &claimed.id, claimed.fencing_token)
                .unwrap()
        );
        // The admin still cannot read the member's turn or conversation
        // state through any scoped path.
        assert!(store.get_turn(&admin_id, &member_turn.id).is_err());
        assert!(
            store
                .get_turn_by_task(&admin_id, &claimed.id)
                .unwrap()
                .is_none()
        );
        assert!(store.get_task(&admin_id, &claimed.id).is_err());
        assert!(
            store
                .list_messages(&admin_id, "dir:main", 10)
                .unwrap()
                .is_empty()
        );
    }

    /// `pending_task_count` is per user; `pending_task_count_all` is the
    /// documented cross-user maintenance view.
    #[test]
    fn isolation_pending_counts_are_per_user() {
        let store = store();
        let admin_id = admin(&store);
        let member = member(&store);
        store
            .enqueue_task(
                &admin_id,
                "turn",
                "main",
                &serde_json::json!({}),
                0,
                utc_now(),
            )
            .unwrap();
        store
            .enqueue_task(
                &member.id,
                "turn",
                "main",
                &serde_json::json!({}),
                0,
                utc_now(),
            )
            .unwrap();
        store
            .enqueue_task(
                &member.id,
                "brief",
                "watcher",
                &serde_json::json!({}),
                0,
                utc_now(),
            )
            .unwrap();
        assert_eq!(store.pending_task_count(&admin_id).unwrap(), 1);
        assert_eq!(store.pending_task_count(&member.id).unwrap(), 2);
        assert_eq!(store.pending_task_count_all().unwrap(), 3);
    }
}
