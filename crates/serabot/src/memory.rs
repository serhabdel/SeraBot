//! Agent memory: reviewable, user-scoped facts with provenance.
//!
//! **Every row belongs to one user.** Each function takes the acting user's
//! id and every SQL statement filters on it (phase 2 of the multi-user
//! reconstruction), so one user's memories, summaries and search results are
//! unreachable from another user's scope.
//!
//! Version 2 (0.19) adds three things on top of the reviewable CRUD:
//!
//! - **Provenance.** `origin` is `explicit` (the owner asked: `memory_save`,
//!   the console, Telegram `/remember`) or `extracted` (a bounded post-turn
//!   extraction). `bot` and `conversation_id` say where a fact came from.
//! - **Hybrid recall.** [`recall`] scores FTS5 BM25 relevance, importance, a
//!   gentle recency factor and a small bot-match boost. Relevance is taken
//!   over the whole user's active memories, not the newest rows, so an old
//!   fact that matches the question is reachable.
//! - **Supersession.** An exact normalized duplicate is not stored twice. A
//!   near-duplicate (token Jaccard at least 0.8) replaces the older row: the
//!   old row stays as `superseded` and points at its successor.
//!
//! Recalled facts are injected into prompts as data (`runtime/recall.rs`).
//! [`MemoryProvider`] is the seam for the optional third-party recall source
//! in `mem0.rs`; the local SQLite store stays the source of truth.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use regex::Regex;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, params_from_iter};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::config::MemoryConfig;
use crate::domain::new_id;
use crate::error::{Result, SeraError};
use crate::gateway::Gateway;
use crate::mem0::Mem0Memory;
use crate::store::Store;
use crate::user_secrets::UserSecrets;
use crate::util::{sha256_hex, truncate_chars};

/// Allowed memory kinds. Anything else is rejected so the review surface stays
/// predictable. Extraction maps its own kinds onto these (see
/// `runtime/extract.rs`).
pub const MEMORY_KINDS: &[&str] = &["preference", "fact", "decision", "context", "note"];
pub const MAX_CONTENT_CHARS: usize = 4_000;
pub const MAX_SOURCE_CHARS: usize = 512;
pub const MAX_LIST_LIMIT: usize = 500;
/// Largest recall or search result a caller can ask for.
pub const MAX_SEARCH_LIMIT: usize = 50;

pub const ORIGIN_EXPLICIT: &str = "explicit";
pub const ORIGIN_EXTRACTED: &str = "extracted";
pub const STATUS_ACTIVE: &str = "active";
pub const STATUS_SUPERSEDED: &str = "superseded";
/// Reserved for a review queue; nothing writes it yet.
pub const STATUS_PENDING: &str = "pending";
pub const DEFAULT_IMPORTANCE: f64 = 0.5;

/// Recency multiplies a score by between `RECENCY_FLOOR` and 1.0, halving the
/// distance to the floor every `RECENCY_HALF_LIFE_DAYS`.
const RECENCY_HALF_LIFE_DAYS: f64 = 30.0;
const RECENCY_FLOOR: f64 = 0.6;
const BOT_MATCH_BOOST: f64 = 1.15;
const SUPERSEDE_JACCARD: f64 = 0.8;
/// Facts shorter than this many tokens are never superseded by similarity:
/// two-word facts overlap too easily.
const MIN_SUPERSEDE_TOKENS: usize = 3;
/// Candidate pool size: the newest rows and the best lexical hits.
const POOL_RECENT: usize = 200;
const POOL_LEXICAL: usize = 200;
const MAX_QUERY_TOKENS: usize = 16;
/// Upper bound on the rows scanned for a near-duplicate on write.
const SUPERSEDE_SCAN: usize = 500;
/// Rolling summaries are capped at this many characters.
pub const SUMMARY_MAX_CHARS: usize = 800;

const COLUMNS: &str = "id, kind, content, source, created_at, updated_at, origin, status, \
    bot, conversation_id, importance, last_recalled_at, recall_count, superseded_by";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Memory {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub category: String,
    pub content: String,
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default = "default_origin")]
    pub origin: String,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub bot: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default = "default_importance")]
    pub importance: f64,
    #[serde(default)]
    pub last_recalled_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub recall_count: i64,
    #[serde(default)]
    pub superseded_by: Option<String>,
}

fn default_origin() -> String {
    ORIGIN_EXPLICIT.into()
}

fn default_status() -> String {
    STATUS_ACTIVE.into()
}

fn default_importance() -> f64 {
    DEFAULT_IMPORTANCE
}

/// A new fact before it is stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub kind: String,
    pub content: String,
    pub source: String,
    pub origin: String,
    pub bot: Option<String>,
    pub conversation_id: Option<String>,
    pub importance: f64,
    /// `active` (recalled) or `pending` (held for the owner to approve; never
    /// recalled until approved).
    pub status: String,
}

impl Candidate {
    /// A fact the owner asked to keep.
    pub fn explicit(kind: &str, content: &str, source: &str) -> Self {
        Self {
            kind: kind.into(),
            content: content.into(),
            source: source.into(),
            origin: ORIGIN_EXPLICIT.into(),
            bot: None,
            conversation_id: None,
            importance: DEFAULT_IMPORTANCE,
            status: STATUS_ACTIVE.into(),
        }
    }
}

/// What a write did. A duplicate returns the stored row; a superseded write
/// names the older row it replaced.
#[derive(Debug, Clone, PartialEq)]
pub enum WriteOutcome {
    Inserted(Memory),
    Duplicate(Memory),
    Superseded { memory: Memory, replaced: String },
}

impl WriteOutcome {
    pub fn memory(&self) -> &Memory {
        match self {
            Self::Inserted(memory) | Self::Duplicate(memory) => memory,
            Self::Superseded { memory, .. } => memory,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Inserted(_) => "inserted",
            Self::Duplicate(_) => "duplicate",
            Self::Superseded { .. } => "superseded",
        }
    }
}

/// Listing and search filters. `None` means "no constraint" for origin and
/// bot. For status, `None` means `active` and `all` removes the constraint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryFilter {
    pub origin: Option<String>,
    pub status: Option<String>,
    pub bot: Option<String>,
}

impl MemoryFilter {
    /// Validated SQL parameters: origin, status, bot. `all` (or empty bot)
    /// disables that dimension.
    fn sql_values(&self) -> Result<(String, String, String)> {
        let origin = match self.origin.as_deref().map(str::trim) {
            None | Some("") | Some("all") => "all".to_owned(),
            Some(value) => normalize_origin(value)?.to_owned(),
        };
        let status = match self.status.as_deref().map(str::trim) {
            None | Some("") => STATUS_ACTIVE.to_owned(),
            Some("all") => "all".to_owned(),
            Some(value @ (STATUS_ACTIVE | STATUS_SUPERSEDED | STATUS_PENDING)) => value.to_owned(),
            Some(_) => {
                return Err(SeraError::Validation(
                    "status must be one of: active, superseded, pending, all".into(),
                ));
            }
        };
        let bot = match self.bot.as_deref().map(str::trim) {
            None | Some("") => String::new(),
            Some(value) if value.len() <= 64 => value.to_owned(),
            Some(_) => return Err(SeraError::Validation("bot filter is too long".into())),
        };
        Ok((origin, status, bot))
    }
}

/// Owner edits. Each present field replaces the stored value.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryPatch {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub importance: Option<f64>,
    #[serde(default)]
    pub status: Option<String>,
}

/// One scored memory from recall or search. Higher is more relevant.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Recalled {
    pub memory: Memory,
    pub score: f64,
}

/// A rolling summary of one conversation.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub conversation_id: String,
    pub bot: Option<String>,
    pub summary: String,
    pub through_message_id: Option<String>,
    pub created_at: String,
}

/// Counts for the Memory page header.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MemoryStats {
    pub active: i64,
    pub by_origin: BTreeMap<String, i64>,
    pub by_status: BTreeMap<String, i64>,
    pub last_extraction_at: Option<String>,
}

/// Reviewable write: validates the kind, content, source and importance, then
/// stores the fact, returns the existing row for an exact duplicate, or
/// supersedes a near-duplicate. `source` is required provenance (for example a
/// conversation id, document id or operator note).
pub fn add(
    store: &Store,
    user_id: &str,
    kind: &str,
    content: &str,
    source: &str,
) -> Result<Memory> {
    let outcome = write(store, user_id, &Candidate::explicit(kind, content, source))?;
    Ok(outcome.memory().clone())
}

/// Store one candidate under the acting user. See [`add`] for the rules.
pub fn write(store: &Store, user_id: &str, candidate: &Candidate) -> Result<WriteOutcome> {
    let kind = normalize_kind(&candidate.kind)?;
    let content = normalized_content(&candidate.content)?;
    let source = normalized_source(&candidate.source)?;
    let origin = normalize_origin(&candidate.origin)?;
    let importance = checked_importance(candidate.importance)?;
    let status = match candidate.status.as_str() {
        STATUS_ACTIVE => STATUS_ACTIVE,
        STATUS_PENDING => STATUS_PENDING,
        _ => {
            return Err(SeraError::Validation(
                "a new memory starts as active or pending".into(),
            ));
        }
    };
    if origin == ORIGIN_EXTRACTED && looks_sensitive(content) {
        return Err(SeraError::Validation(
            "extracted memory looks like a credential; not stored".into(),
        ));
    }
    let bot = candidate.bot.as_deref().filter(|value| !value.is_empty());
    let conversation = candidate
        .conversation_id
        .as_deref()
        .filter(|value| !value.is_empty());
    let hash = content_hash(content);

    let mut connection = store.connection()?;
    // One immediate transaction: the duplicate check, the near-duplicate scan
    // and the insert see the same rows.
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // A pending fact is a duplicate of a pending copy too; an active one only
    // of an active row, so a held copy never blocks the owner's own save.
    if let Some(existing) = find_by_hash(&tx, user_id, &hash, status == STATUS_PENDING)? {
        let memory = get_in(&tx, user_id, &existing)?;
        return Ok(WriteOutcome::Duplicate(memory));
    }

    let superseded = if status == STATUS_ACTIVE {
        find_near_duplicate(&tx, user_id, content)?
    } else {
        None
    };
    let id = new_id();
    let now = now_string(Utc::now());
    tx.execute(
        "INSERT INTO memories(id, kind, content, source, created_at, updated_at, user_id,
                              bot, conversation_id, origin, status, importance, recall_count,
                              content_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12)",
        params![
            id,
            kind,
            content,
            source,
            now,
            user_id,
            bot,
            conversation,
            origin,
            status,
            importance,
            hash,
        ],
    )?;
    let replaced = match superseded {
        Some(old_id) => {
            tx.execute(
                "UPDATE memories SET status = 'superseded', superseded_by = ?1
                 WHERE id = ?2 AND user_id = ?3",
                params![id, old_id, user_id],
            )?;
            Some(old_id)
        }
        None => None,
    };
    let memory = get_in(&tx, user_id, &id)?;
    tx.commit()?;
    Ok(match replaced {
        Some(replaced) => WriteOutcome::Superseded { memory, replaced },
        None => WriteOutcome::Inserted(memory),
    })
}

/// The acting user's memory by id; another user's memory is
/// indistinguishable from a missing one.
pub fn get(store: &Store, user_id: &str, id: &str) -> Result<Memory> {
    let connection = store.connection()?;
    get_in(&connection, user_id, id)
}

/// List the acting user's active memories, newest first, bounded.
///
/// Rows are always filtered by `user_id`; there is no unscoped list, and
/// an unknown user yields an empty list, never another user's rows.
pub fn list(store: &Store, user_id: &str, limit: usize) -> Result<Vec<Memory>> {
    list_page(store, user_id, &MemoryFilter::default(), 0, limit)
}

/// One page of the acting user's memories, newest first, with optional
/// origin, status and bot filters.
pub fn list_page(
    store: &Store,
    user_id: &str,
    filter: &MemoryFilter,
    offset: usize,
    limit: usize,
) -> Result<Vec<Memory>> {
    let limit = limit.clamp(1, MAX_LIST_LIMIT);
    let (origin, status, bot) = filter.sql_values()?;
    let connection = store.connection()?;
    let mut statement = connection.prepare(&format!(
        "SELECT {COLUMNS} FROM memories
         WHERE user_id = ?1
           AND (?2 = 'all' OR origin = ?2)
           AND (?3 = 'all' OR status = ?3)
           AND (?4 = '' OR bot = ?4)
         ORDER BY updated_at DESC, id DESC
         LIMIT ?5 OFFSET ?6"
    ))?;
    let rows = statement.query_map(
        params![user_id, origin, status, bot, limit as i64, offset as i64],
        row_to_memory,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Lexical search over the acting user's memories. Only rows that match at
/// least one query token are returned, best match first.
pub fn search(
    store: &Store,
    user_id: &str,
    query: &str,
    filter: &MemoryFilter,
    limit: usize,
) -> Result<Vec<Recalled>> {
    rank(store, user_id, query, filter, None, limit, Mode::Search)
}

/// The facts to put in front of the model for this turn. With a query, the
/// score is `(0.2 + 0.8 * lexical) * importance * recency * bot boost`, so a
/// relevant old fact can outrank a recent unrelated one. Without a query the
/// newest active facts are returned. Recalled rows get their counters bumped.
pub fn recall(
    store: &Store,
    user_id: &str,
    query: &str,
    k: usize,
    bot: Option<&str>,
) -> Result<Vec<Recalled>> {
    let filter = MemoryFilter::default();
    let ranked = if query_tokens(query).is_empty() {
        list_page(store, user_id, &filter, 0, k)?
            .into_iter()
            .map(|memory| Recalled { memory, score: 0.0 })
            .collect()
    } else {
        rank(store, user_id, query, &filter, bot, k, Mode::Recall)?
    };
    let ids: Vec<String> = ranked.iter().map(|item| item.memory.id.clone()).collect();
    touch(store, user_id, &ids)?;
    Ok(ranked)
}

/// Update one memory of the acting user. Absent fields keep their stored
/// values. Kind, source, provenance and creation time never change.
pub fn update(store: &Store, user_id: &str, id: &str, patch: &MemoryPatch) -> Result<Memory> {
    let mut connection = store.connection()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = get_in(&tx, user_id, id)?;
    let content = match &patch.content {
        Some(content) => normalized_content(content)?.to_owned(),
        None => current.content.clone(),
    };
    let importance = match patch.importance {
        Some(value) => checked_importance(value)?,
        None => current.importance,
    };
    let status = match patch.status.as_deref().map(str::trim) {
        None => current.status.clone(),
        Some(STATUS_ACTIVE) => STATUS_ACTIVE.to_owned(),
        Some(STATUS_SUPERSEDED) => STATUS_SUPERSEDED.to_owned(),
        Some(_) => {
            return Err(SeraError::Validation(
                "status can only be set to active or superseded".into(),
            ));
        }
    };
    tx.execute(
        "UPDATE memories
         SET content = ?1, importance = ?2, status = ?3, content_hash = ?4, updated_at = ?5
         WHERE id = ?6 AND user_id = ?7",
        params![
            content,
            importance,
            status,
            content_hash(&content),
            now_string(Utc::now()),
            id,
            user_id,
        ],
    )?;
    let memory = get_in(&tx, user_id, id)?;
    tx.commit()?;
    Ok(memory)
}

/// Update the content of one memory of the acting user, preserving kind,
/// source and creation time.
pub fn update_content(store: &Store, user_id: &str, id: &str, content: &str) -> Result<Memory> {
    update(
        store,
        user_id,
        id,
        &MemoryPatch {
            content: Some(content.to_owned()),
            ..MemoryPatch::default()
        },
    )
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

/// Counts by origin and status, plus the last successful extraction.
pub fn stats(store: &Store, user_id: &str) -> Result<MemoryStats> {
    let connection = store.connection()?;
    let mut by_origin = BTreeMap::new();
    let mut by_status = BTreeMap::new();
    let mut active = 0;
    {
        let mut statement = connection.prepare(
            "SELECT origin, status, COUNT(*) FROM memories WHERE user_id = ?1
             GROUP BY origin, status",
        )?;
        let rows = statement.query_map(params![user_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (origin, status, count) = row?;
            *by_origin.entry(origin).or_insert(0) += count;
            if status == STATUS_ACTIVE {
                active += count;
            }
            *by_status.entry(status).or_insert(0) += count;
        }
    }
    // The audit actor is the acting user's id (see `Store::record_audit`).
    let last_extraction_at: Option<String> = connection.query_row(
        "SELECT MAX(at) FROM audit_log WHERE actor = ?1 AND event = 'memory.extracted'",
        params![user_id],
        |row| row.get::<_, Option<String>>(0),
    )?;
    Ok(MemoryStats {
        active,
        by_origin,
        by_status,
        last_extraction_at,
    })
}

/// The latest rolling summary of one conversation of the acting user.
pub fn latest_summary(
    store: &Store,
    user_id: &str,
    conversation_id: &str,
) -> Result<Option<SessionSummary>> {
    let connection = store.connection()?;
    connection
        .query_row(
            "SELECT id, conversation_id, bot, summary, through_message_id, created_at
             FROM session_summaries
             WHERE user_id = ?1 AND conversation_id = ?2
             ORDER BY created_at DESC, id DESC LIMIT 1",
            params![user_id, conversation_id],
            |row| {
                Ok(SessionSummary {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    bot: row.get(2)?,
                    summary: row.get(3)?,
                    through_message_id: row.get(4)?,
                    created_at: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(SeraError::from)
}

/// Owner and assistant messages not yet folded into a summary, oldest first,
/// at most `limit`. Each entry is `(message id, role, content)`.
pub fn unsummarized_messages(
    store: &Store,
    user_id: &str,
    conversation_id: &str,
    limit: usize,
) -> Result<Vec<(String, String, String)>> {
    let since = match latest_summary(store, user_id, conversation_id)? {
        Some(summary) => match summary.through_message_id {
            Some(through) => {
                let connection = store.connection()?;
                connection
                    .query_row(
                        "SELECT created_at FROM messages WHERE id = ?1 AND user_id = ?2",
                        params![through, user_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .unwrap_or_default()
            }
            None => String::new(),
        },
        None => String::new(),
    };
    let connection = store.connection()?;
    let mut statement = connection.prepare(
        "SELECT id, role, content FROM messages
         WHERE user_id = ?1 AND conversation_id = ?2
           AND role IN ('user', 'assistant') AND created_at > ?3
         ORDER BY created_at, id LIMIT ?4",
    )?;
    let rows = statement.query_map(
        params![user_id, conversation_id, since, limit.max(1) as i64],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Store a rolling summary. The text is capped at `SUMMARY_MAX_CHARS`.
pub fn store_summary(
    store: &Store,
    user_id: &str,
    conversation_id: &str,
    bot: Option<&str>,
    summary: &str,
    through_message_id: Option<&str>,
) -> Result<SessionSummary> {
    let summary = truncate_chars(summary.trim(), SUMMARY_MAX_CHARS);
    if summary.is_empty() {
        return Err(SeraError::Validation("summary must not be empty".into()));
    }
    let id = new_id();
    let created_at = now_string(Utc::now());
    let connection = store.connection()?;
    connection.execute(
        "INSERT INTO session_summaries(id, user_id, conversation_id, bot, summary,
                                       through_message_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            user_id,
            conversation_id,
            bot,
            summary,
            through_message_id,
            created_at,
        ],
    )?;
    Ok(SessionSummary {
        id,
        conversation_id: conversation_id.to_owned(),
        bot: bot.map(str::to_owned),
        summary,
        through_message_id: through_message_id.map(str::to_owned),
        created_at,
    })
}

/// Fill `content_hash` for rows written before migration 0026. Runs inside the
/// store's migration, right after the batch that added the column.
pub fn backfill_content_hashes(connection: &Connection) -> Result<()> {
    let rows: Vec<(String, String)> = {
        let mut statement =
            connection.prepare("SELECT id, content FROM memories WHERE content_hash IS NULL")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, content) in rows {
        connection.execute(
            "UPDATE memories SET content_hash = ?1 WHERE id = ?2",
            params![content_hash(&content), id],
        )?;
    }
    Ok(())
}

/// Rebuild the FTS index only when it disagrees with `memories` (a restored
/// or renumbered database), comparing row counts with the FTS docsize shadow
/// table. A failure is logged and never stops startup.
pub fn refresh_fts_if_stale(connection: &Connection) {
    let stale = (|| -> rusqlite::Result<bool> {
        let rows: i64 =
            connection.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
        let indexed: i64 =
            connection.query_row("SELECT COUNT(*) FROM memories_fts_docsize", [], |row| {
                row.get(0)
            })?;
        Ok(rows != indexed)
    })();
    match stale {
        Ok(false) => {}
        Ok(true) => match rebuild_fts(connection) {
            Ok(()) => tracing::warn!("memory full-text index was out of step and was rebuilt"),
            Err(error) => tracing::warn!(%error, "memory full-text index rebuild failed"),
        },
        Err(error) => tracing::warn!(%error, "memory full-text index check failed"),
    }
}

/// Rebuild the external-content FTS index from `memories`. Cheap at this
/// scale; it keeps the index aligned after a VACUUM INTO restore.
pub fn rebuild_fts(connection: &Connection) -> Result<()> {
    connection.execute(
        "INSERT INTO memories_fts(memories_fts) VALUES ('rebuild')",
        [],
    )?;
    Ok(())
}

/// Whether text looks like a credential or bank or card detail. Extraction
/// drops such candidates before they are stored or sent anywhere.
pub fn looks_sensitive(text: &str) -> bool {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            [
                r"(?i)\b(password|passwd|passcode|mot de passe|mdp|cvv|iban|swift|bic|api[ _-]?key|secret|token)\b",
                r"\b(sk|pk|rk|ghp|gho)[-_][A-Za-z0-9_\-]{8,}",
                r"\bAKIA[0-9A-Z]{12,}\b",
                r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
                r"\b[A-Za-z]{2}\d{2}[A-Za-z0-9]{11,30}\b",
                r"\b(?:\d[ -]?){13,19}\b",
                r"\b[0-9a-fA-F]{32,}\b",
            ]
            .iter()
            .map(|pattern| Regex::new(pattern).expect("static memory sensitivity pattern"))
            .collect()
        })
        .iter()
        .any(|pattern| pattern.is_match(text))
}

/// Third-party recall and write-through, merged with the local store. The
/// local SQLite store is always written and is the source of truth; the remote
/// source only adds recall candidates.
#[async_trait]
pub trait MemoryProvider: Send + Sync {
    async fn recall(
        &self,
        user_id: &str,
        query: &str,
        k: usize,
        bot: Option<&str>,
    ) -> Result<Vec<Recalled>>;
    async fn search(
        &self,
        user_id: &str,
        query: &str,
        filter: &MemoryFilter,
        limit: usize,
    ) -> Result<Vec<Recalled>>;
    async fn write(&self, user_id: &str, candidate: &Candidate) -> Result<WriteOutcome>;
    async fn update(&self, user_id: &str, id: &str, patch: &MemoryPatch) -> Result<Memory>;
    async fn forget(&self, user_id: &str, id: &str) -> Result<bool>;
    async fn list(
        &self,
        user_id: &str,
        filter: &MemoryFilter,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<Memory>>;
}

/// The built-in provider: the local SQLite store only.
#[derive(Clone)]
pub struct SqliteMemory {
    store: Store,
}

impl SqliteMemory {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[async_trait]
impl MemoryProvider for SqliteMemory {
    async fn recall(
        &self,
        user_id: &str,
        query: &str,
        k: usize,
        bot: Option<&str>,
    ) -> Result<Vec<Recalled>> {
        recall(&self.store, user_id, query, k, bot)
    }

    async fn search(
        &self,
        user_id: &str,
        query: &str,
        filter: &MemoryFilter,
        limit: usize,
    ) -> Result<Vec<Recalled>> {
        search(&self.store, user_id, query, filter, limit)
    }

    async fn write(&self, user_id: &str, candidate: &Candidate) -> Result<WriteOutcome> {
        write(&self.store, user_id, candidate)
    }

    async fn update(&self, user_id: &str, id: &str, patch: &MemoryPatch) -> Result<Memory> {
        update(&self.store, user_id, id, patch)
    }

    async fn forget(&self, user_id: &str, id: &str) -> Result<bool> {
        delete(&self.store, user_id, id)
    }

    async fn list(
        &self,
        user_id: &str,
        filter: &MemoryFilter,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<Memory>> {
        list_page(&self.store, user_id, filter, offset, limit)
    }
}

/// The configured provider: local SQLite, plus Mem0 when `[memory]
/// provider = "mem0"` and the acting user has saved a Mem0 key.
#[derive(Clone)]
pub struct MemoryHub {
    local: SqliteMemory,
    remote: Option<Mem0Memory>,
    gateway: Gateway,
}

impl MemoryHub {
    /// Build the hub from configuration. The `mem0` provider needs the
    /// per-user secret store; without it the configuration is rejected rather
    /// than silently recalling less.
    pub fn new(config: &MemoryConfig, gateway: Gateway) -> Result<Self> {
        let remote = match config.provider.as_str() {
            "mem0" => {
                let users: std::sync::Arc<UserSecrets> =
                    gateway.user_secrets().ok_or_else(|| {
                        SeraError::Config(
                            "memory.provider = \"mem0\" needs the per-user secret store".into(),
                        )
                    })?;
                Some(Mem0Memory::new(config, users, gateway.clone())?)
            }
            _ => None,
        };
        Ok(Self {
            local: SqliteMemory::new(gateway.store().clone()),
            remote,
            gateway,
        })
    }

    /// Whether a third-party recall source is configured (it may still be
    /// disabled for a given user without a saved key).
    pub fn has_remote(&self) -> bool {
        self.remote.is_some()
    }

    pub fn local(&self) -> &SqliteMemory {
        &self.local
    }
}

#[async_trait]
impl MemoryProvider for MemoryHub {
    async fn recall(
        &self,
        user_id: &str,
        query: &str,
        k: usize,
        bot: Option<&str>,
    ) -> Result<Vec<Recalled>> {
        let local = self.local.recall(user_id, query, k, bot).await?;
        let Some(remote) = &self.remote else {
            return Ok(local);
        };
        let hits = match remote.search(user_id, query, k).await {
            Ok(hits) => hits,
            Err(error) => {
                warn!(error = %error, "third-party memory recall skipped");
                Vec::new()
            }
        };
        Ok(merge_by_score(local, hits, &self.gateway, k))
    }

    async fn search(
        &self,
        user_id: &str,
        query: &str,
        filter: &MemoryFilter,
        limit: usize,
    ) -> Result<Vec<Recalled>> {
        self.local.search(user_id, query, filter, limit).await
    }

    async fn write(&self, user_id: &str, candidate: &Candidate) -> Result<WriteOutcome> {
        let outcome = self.local.write(user_id, candidate).await?;
        // Pending facts stay local until the owner approves them.
        if let Some(remote) = &self.remote
            && candidate.status == STATUS_ACTIVE
            && !matches!(outcome, WriteOutcome::Duplicate(_))
            && let Err(error) = remote.remember(user_id, &candidate.content).await
        {
            warn!(error = %error, "third-party memory write-through skipped");
        }
        Ok(outcome)
    }

    async fn update(&self, user_id: &str, id: &str, patch: &MemoryPatch) -> Result<Memory> {
        self.local.update(user_id, id, patch).await
    }

    async fn forget(&self, user_id: &str, id: &str) -> Result<bool> {
        self.local.forget(user_id, id).await
    }

    async fn list(
        &self,
        user_id: &str,
        filter: &MemoryFilter,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<Memory>> {
        self.local.list(user_id, filter, offset, limit).await
    }
}

/// Merge local recalls with third-party hits by score, dropping hits whose
/// text duplicates a local fact or fails the blocklist. Local scores are
/// divided by their largest possible value (`score_of` with a full lexical
/// match and a bot boost) so both sources sit on a `0..=1` scale. A relative
/// normalization would make the best non-matching local fact look certain.
fn merge_by_score(
    local: Vec<Recalled>,
    hits: Vec<crate::mem0::RemoteHit>,
    gateway: &Gateway,
    k: usize,
) -> Vec<Recalled> {
    let largest = BOT_MATCH_BOOST;
    let mut seen: HashSet<String> = local
        .iter()
        .map(|item| content_hash(&item.memory.content))
        .collect();
    let mut merged: Vec<Recalled> = local
        .into_iter()
        .map(|mut item| {
            item.score = (item.score / largest).clamp(0.0, 1.0);
            item
        })
        .collect();
    let now = Utc::now();
    for hit in hits {
        if !seen.insert(content_hash(&hit.content)) || !gateway.policy().is_allowed(&hit.content) {
            continue;
        }
        merged.push(Recalled {
            memory: Memory {
                id: String::new(),
                kind: "fact".into(),
                category: "fact".into(),
                content: hit.content,
                source: "mem0".into(),
                created_at: now,
                updated_at: now,
                origin: "mem0".into(),
                status: STATUS_ACTIVE.into(),
                bot: None,
                conversation_id: None,
                importance: DEFAULT_IMPORTANCE,
                last_recalled_at: None,
                recall_count: 0,
                superseded_by: None,
            },
            score: hit.score.clamp(0.0, 1.0),
        });
    }
    merged.sort_by(|a, b| b.score.total_cmp(&a.score));
    merged.truncate(k.clamp(1, MAX_SEARCH_LIMIT));
    merged
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Relevance plus prior: every active row can appear.
    Recall,
    /// Only rows that match a query token.
    Search,
}

fn rank(
    store: &Store,
    user_id: &str,
    query: &str,
    filter: &MemoryFilter,
    bot: Option<&str>,
    limit: usize,
    mode: Mode,
) -> Result<Vec<Recalled>> {
    let limit = limit.clamp(1, MAX_SEARCH_LIMIT);
    let values = filter.sql_values()?;
    let tokens = query_tokens(query);
    if tokens.is_empty() {
        return Ok(list_page(store, user_id, filter, 0, limit)?
            .into_iter()
            .map(|memory| Recalled { memory, score: 0.0 })
            .collect());
    }
    let connection = store.connection()?;
    let lexical = lexical_hits(&connection, user_id, &tokens, &values)?;
    let best = lexical.iter().map(|(_, raw)| *raw).fold(0.0_f64, f64::max);
    let mut lexical_scores: HashMap<String, f64> = HashMap::new();
    for (id, raw) in lexical {
        let normalized = if best > 0.0 {
            (raw / best).clamp(0.0, 1.0)
        } else {
            1.0
        };
        lexical_scores.insert(id, normalized);
    }
    let mut ids: Vec<String> = lexical_scores.keys().cloned().collect();
    ids.extend(recent_ids(&connection, user_id, &values, POOL_RECENT)?);
    ids.sort();
    ids.dedup();
    let rows = load_rows(&connection, user_id, &ids)?;
    let now = Utc::now();
    let mut ranked: Vec<Recalled> = rows
        .into_iter()
        .filter_map(|memory| {
            let lexical = lexical_scores.get(&memory.id).copied();
            if mode == Mode::Search && lexical.is_none() {
                return None;
            }
            let score = score_of(&memory, lexical.unwrap_or(0.0), bot, now);
            Some(Recalled { memory, score })
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.memory.updated_at.cmp(&a.memory.updated_at))
            .then_with(|| a.memory.id.cmp(&b.memory.id))
    });
    ranked.truncate(limit);
    Ok(ranked)
}

fn score_of(memory: &Memory, lexical: f64, bot: Option<&str>, now: DateTime<Utc>) -> f64 {
    let age_days = (now - memory.updated_at).num_seconds().max(0) as f64 / 86_400.0;
    let recency =
        RECENCY_FLOOR + (1.0 - RECENCY_FLOOR) * 0.5_f64.powf(age_days / RECENCY_HALF_LIFE_DAYS);
    let boost = match (bot, memory.bot.as_deref()) {
        (Some(wanted), Some(owned)) if wanted == owned => BOT_MATCH_BOOST,
        _ => 1.0,
    };
    (0.2 + 0.8 * lexical) * memory.importance.clamp(0.05, 1.0) * recency * boost
}

/// Best-first FTS5 hits as `(id, relevance)`, where a larger relevance is a
/// better match. Only the acting user's active rows (or the filter's rows)
/// are candidates.
fn lexical_hits(
    connection: &Connection,
    user_id: &str,
    tokens: &[String],
    values: &(String, String, String),
) -> Result<Vec<(String, f64)>> {
    let matcher = tokens
        .iter()
        .map(|token| format!("\"{token}\""))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut statement = connection.prepare(
        "SELECT m.id, bm25(memories_fts) FROM memories_fts
         JOIN memories m ON m.rowid = memories_fts.rowid
         WHERE memories_fts MATCH ?1 AND m.user_id = ?5
           AND (?2 = 'all' OR m.origin = ?2)
           AND (?3 = 'all' OR m.status = ?3)
           AND (?4 = '' OR m.bot = ?4)
         ORDER BY bm25(memories_fts)
         LIMIT ?6",
    )?;
    let rows = statement.query_map(
        params![
            matcher,
            values.0,
            values.1,
            values.2,
            user_id,
            POOL_LEXICAL as i64
        ],
        |row| Ok((row.get::<_, String>(0)?, -row.get::<_, f64>(1)?)),
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn recent_ids(
    connection: &Connection,
    user_id: &str,
    values: &(String, String, String),
    limit: usize,
) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT id FROM memories
         WHERE user_id = ?1
           AND (?2 = 'all' OR origin = ?2)
           AND (?3 = 'all' OR status = ?3)
           AND (?4 = '' OR bot = ?4)
         ORDER BY updated_at DESC, id DESC LIMIT ?5",
    )?;
    let rows = statement.query_map(
        params![user_id, values.0, values.1, values.2, limit as i64],
        |row| row.get::<_, String>(0),
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn load_rows(connection: &Connection, user_id: &str, ids: &[String]) -> Result<Vec<Memory>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; ids.len()].join(", ");
    let mut statement = connection.prepare(&format!(
        "SELECT {COLUMNS} FROM memories WHERE user_id = ? AND id IN ({placeholders})"
    ))?;
    let mut values: Vec<String> = Vec::with_capacity(ids.len() + 1);
    values.push(user_id.to_owned());
    values.extend(ids.iter().cloned());
    let rows = statement.query_map(params_from_iter(values.iter()), row_to_memory)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Bump the recall counters of the rows just recalled, in one transaction.
fn touch(store: &Store, user_id: &str, ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let now = now_string(Utc::now());
    let mut connection = store.connection()?;
    let tx = connection.transaction()?;
    for id in ids {
        tx.execute(
            "UPDATE memories SET recall_count = recall_count + 1, last_recalled_at = ?1
             WHERE id = ?2 AND user_id = ?3",
            params![now, id, user_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// The row with the same normalized content (active, or also pending when
/// asked), if any.
fn find_by_hash(
    connection: &Connection,
    user_id: &str,
    hash: &str,
    include_pending: bool,
) -> Result<Option<String>> {
    let statuses = if include_pending {
        "('active', 'pending')"
    } else {
        "('active')"
    };
    connection
        .query_row(
            &format!(
                "SELECT id FROM memories
                 WHERE user_id = ?1 AND status IN {statuses} AND content_hash = ?2
                 ORDER BY updated_at DESC, id DESC LIMIT 1"
            ),
            params![user_id, hash],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(SeraError::from)
}

/// The active row whose tokens overlap the new content at least
/// `SUPERSEDE_JACCARD`, with the highest overlap. Short facts never match.
fn find_near_duplicate(
    connection: &Connection,
    user_id: &str,
    content: &str,
) -> Result<Option<String>> {
    let new_tokens = token_set(content);
    if new_tokens.len() < MIN_SUPERSEDE_TOKENS {
        return Ok(None);
    }
    let mut statement = connection.prepare(
        "SELECT id, content FROM memories
         WHERE user_id = ?1 AND status = 'active'
         ORDER BY updated_at DESC, id DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![user_id, SUPERSEDE_SCAN as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut best: Option<(String, f64)> = None;
    for row in rows {
        let (id, old) = row?;
        let score = jaccard(&new_tokens, &token_set(&old));
        if score >= SUPERSEDE_JACCARD && best.as_ref().is_none_or(|(_, top)| score > *top) {
            best = Some((id, score));
        }
    }
    Ok(best.map(|(id, _)| id))
}

fn get_in(connection: &Connection, user_id: &str, id: &str) -> Result<Memory> {
    connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM memories WHERE id = ?1 AND user_id = ?2"),
            params![id, user_id],
            row_to_memory,
        )
        .optional()?
        .ok_or_else(|| SeraError::NotFound(format!("memory {id}")))
}

fn row_to_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    let kind: String = row.get(1)?;
    let last_recalled: Option<String> = row.get(11)?;
    Ok(Memory {
        id: row.get(0)?,
        category: kind.clone(),
        kind,
        content: row.get(2)?,
        source: row.get(3)?,
        created_at: parse_datetime(&row.get::<_, String>(4)?),
        updated_at: parse_datetime(&row.get::<_, String>(5)?),
        origin: row.get(6)?,
        status: row.get(7)?,
        bot: row.get(8)?,
        conversation_id: row.get(9)?,
        importance: row.get(10)?,
        last_recalled_at: last_recalled.as_deref().map(parse_datetime),
        recall_count: row.get(12)?,
        superseded_by: row.get(13)?,
    })
}

/// Lowercase alphanumeric tokens of at least two characters, deduplicated,
/// capped. Quoted in FTS queries, so no token can carry FTS syntax.
fn query_tokens(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for token in tokens(query) {
        if token.chars().count() >= 2 && seen.insert(token.clone()) {
            out.push(token);
            if out.len() == MAX_QUERY_TOKENS {
                break;
            }
        }
    }
    out
}

fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

fn token_set(text: &str) -> HashSet<String> {
    tokens(text).into_iter().collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    shared / union
}

/// SHA-256 of the normalized content (lowercase alphanumeric tokens joined by
/// single spaces), so case and punctuation differences do not defeat the
/// exact-duplicate check.
fn content_hash(content: &str) -> String {
    sha256_hex(tokens(content).join(" "))
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

fn normalize_origin(origin: &str) -> Result<&'static str> {
    match origin.trim() {
        ORIGIN_EXPLICIT => Ok(ORIGIN_EXPLICIT),
        ORIGIN_EXTRACTED => Ok(ORIGIN_EXTRACTED),
        _ => Err(SeraError::Validation(
            "memory origin must be explicit or extracted".into(),
        )),
    }
}

fn checked_importance(value: f64) -> Result<f64> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(SeraError::Validation(
            "memory importance must be between 0 and 1".into(),
        ))
    }
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

    fn at(store: &Store, user: &str, content: &str) -> Memory {
        add(store, user, "fact", content, "operator:test").expect("memory")
    }

    /// Backdate a row so recency tests do not sleep.
    fn age(store: &Store, id: &str, days: i64) {
        let when = now_string(Utc::now() - chrono::Duration::days(days));
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE memories SET updated_at = ?1 WHERE id = ?2",
                params![when, id],
            )
            .unwrap();
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
        assert_eq!(first.origin, ORIGIN_EXPLICIT);
        assert_eq!(first.status, STATUS_ACTIVE);

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

    /// The original bug: the newest 20 rows hid an old, relevant fact. Recall
    /// and search must still find it.
    #[test]
    fn recall_reaches_an_old_relevant_fact_beyond_the_newest_rows() {
        let store = store();
        let admin = admin(&store);
        let old = at(
            &store,
            &admin,
            "The dentist appointment is on the ninth floor",
        );
        age(&store, &old.id, 400);
        for index in 0..30 {
            at(
                &store,
                &admin,
                &format!("Unrelated note number {index} about groceries"),
            );
        }
        let recalled =
            recall(&store, &admin, "when is the dentist appointment", 8, None).expect("recall");
        assert!(
            recalled.iter().any(|item| item.memory.id == old.id),
            "the old dentist fact must be recalled"
        );
        let found = search(&store, &admin, "dentist", &MemoryFilter::default(), 5).expect("search");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].memory.id, old.id);
    }

    #[test]
    fn exact_duplicates_are_not_stored_twice() {
        let store = store();
        let admin = admin(&store);
        let first = write(
            &store,
            &admin,
            &Candidate::explicit("fact", "Lives in Rome.", "operator:test"),
        )
        .expect("first");
        assert_eq!(first.label(), "inserted");
        let again = write(
            &store,
            &admin,
            &Candidate::explicit("fact", "  lives in ROME ", "operator:test"),
        )
        .expect("again");
        assert_eq!(again.label(), "duplicate");
        assert_eq!(again.memory().id, first.memory().id);
        assert_eq!(list(&store, &admin, 10).expect("list").len(), 1);
    }

    #[test]
    fn near_duplicates_supersede_the_older_row() {
        let store = store();
        let admin = admin(&store);
        let old = at(&store, &admin, "Prefers short morning meetings before noon");
        let new = write(
            &store,
            &admin,
            &Candidate::explicit(
                "preference",
                "Prefers short morning meetings before noon always",
                "operator:test",
            ),
        )
        .expect("supersede");
        assert_eq!(new.label(), "superseded");
        match &new {
            WriteOutcome::Superseded { replaced, .. } => assert_eq!(replaced, &old.id),
            other => panic!("expected supersession, got {other:?}"),
        }
        let old_now = get(&store, &admin, &old.id).expect("old row kept");
        assert_eq!(old_now.status, STATUS_SUPERSEDED);
        assert_eq!(
            old_now.superseded_by.as_deref(),
            Some(new.memory().id.as_str())
        );
        // Superseded rows never come back through recall or the active list.
        let active = list(&store, &admin, 10).expect("list");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, new.memory().id);
        let recalled = recall(&store, &admin, "morning meetings", 8, None).expect("recall");
        assert!(recalled.iter().all(|item| item.memory.id != old.id));
        // The superseded row is still visible to an explicit status filter.
        let filter = MemoryFilter {
            status: Some(STATUS_SUPERSEDED.into()),
            ..MemoryFilter::default()
        };
        assert_eq!(
            list_page(&store, &admin, &filter, 0, 10)
                .expect("page")
                .len(),
            1
        );
    }

    #[test]
    fn short_facts_and_partial_overlaps_are_kept_apart() {
        let store = store();
        let admin = admin(&store);
        at(&store, &admin, "Likes coffee");
        let tea = write(
            &store,
            &admin,
            &Candidate::explicit("preference", "Likes tea", "operator:test"),
        )
        .expect("tea");
        assert_eq!(tea.label(), "inserted");
        at(&store, &admin, "Works at the Rome office on Mondays");
        let moved = write(
            &store,
            &admin,
            &Candidate::explicit(
                "fact",
                "Works at the Milan office on Mondays",
                "operator:test",
            ),
        )
        .expect("milan");
        assert_eq!(moved.label(), "inserted", "Jaccard below the threshold");
    }

    #[test]
    fn recall_orders_by_importance_and_recency_for_equal_matches() {
        let store = store();
        let admin = admin(&store);
        let minor = write(
            &store,
            &admin,
            &Candidate {
                importance: 0.2,
                ..Candidate::explicit("fact", "Coffee shop on Via Roma", "operator:test")
            },
        )
        .expect("minor")
        .memory()
        .clone();
        let major = write(
            &store,
            &admin,
            &Candidate {
                importance: 0.9,
                ..Candidate::explicit("fact", "Coffee machine serviced yearly", "operator:test")
            },
        )
        .expect("major")
        .memory()
        .clone();
        let ranked = recall(&store, &admin, "coffee", 5, None).expect("recall");
        assert_eq!(ranked[0].memory.id, major.id);
        assert_eq!(ranked[1].memory.id, minor.id);
        assert!(ranked[0].score > ranked[1].score);

        // Equal importance: the fresher fact wins among equal lexical matches.
        let stale = at(&store, &admin, "Coffee beans from the old roaster");
        age(&store, &stale.id, 365);
        let fresh = at(&store, &admin, "Coffee grinder needs cleaning");
        let ranked = recall(&store, &admin, "coffee", 5, None).expect("recall");
        let pos_fresh = ranked
            .iter()
            .position(|item| item.memory.id == fresh.id)
            .unwrap();
        let pos_stale = ranked
            .iter()
            .position(|item| item.memory.id == stale.id)
            .unwrap();
        assert!(pos_fresh < pos_stale);
    }

    #[test]
    fn bot_match_gives_a_small_boost_only() {
        let store = store();
        let admin = admin(&store);
        let owned = write(
            &store,
            &admin,
            &Candidate {
                bot: Some("analyst".into()),
                ..Candidate::explicit("fact", "Quarterly revenue target set", "operator:test")
            },
        )
        .expect("owned")
        .memory()
        .clone();
        let other = at(&store, &admin, "Quarterly revenue review scheduled");
        let ranked = recall(&store, &admin, "quarterly revenue", 5, Some("analyst")).expect("r");
        assert_eq!(ranked[0].memory.id, owned.id);
        assert_eq!(ranked[1].memory.id, other.id);
    }

    #[test]
    fn recall_records_counters_and_empty_query_falls_back_to_newest() {
        let store = store();
        let admin = admin(&store);
        let first = at(&store, &admin, "Owns a blue bicycle");
        age(&store, &first.id, 10);
        let second = at(&store, &admin, "Owns a green scooter");
        let newest = recall(&store, &admin, "", 1, None).expect("fallback");
        assert_eq!(newest[0].memory.id, second.id);
        let hit = recall(&store, &admin, "bicycle", 3, None).expect("hit");
        assert_eq!(hit[0].memory.id, first.id);
        let stored = get(&store, &admin, &first.id).expect("get");
        assert_eq!(stored.recall_count, 1);
        assert!(stored.last_recalled_at.is_some());
    }

    #[test]
    fn filters_and_status_validation() {
        let store = store();
        let admin = admin(&store);
        at(&store, &admin, "Explicit fact about gardening");
        write(
            &store,
            &admin,
            &Candidate {
                origin: ORIGIN_EXTRACTED.into(),
                bot: Some("main".into()),
                ..Candidate::explicit("fact", "Extracted fact about gardening", "turn:1")
            },
        )
        .expect("extracted");
        let extracted = MemoryFilter {
            origin: Some(ORIGIN_EXTRACTED.into()),
            ..MemoryFilter::default()
        };
        let page = list_page(&store, &admin, &extracted, 0, 10).expect("page");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].origin, ORIGIN_EXTRACTED);
        assert_eq!(page[0].bot.as_deref(), Some("main"));
        let by_bot = MemoryFilter {
            bot: Some("main".into()),
            ..MemoryFilter::default()
        };
        assert_eq!(
            list_page(&store, &admin, &by_bot, 0, 10)
                .expect("bot")
                .len(),
            1
        );
        let bad = MemoryFilter {
            status: Some("deleted".into()),
            ..MemoryFilter::default()
        };
        assert!(list_page(&store, &admin, &bad, 0, 10).is_err());
        let stats = stats(&store, &admin).expect("stats");
        assert_eq!(stats.active, 2);
        assert_eq!(stats.by_origin.get(ORIGIN_EXTRACTED), Some(&1));
        assert!(stats.last_extraction_at.is_none());
    }

    #[test]
    fn patch_changes_only_the_named_fields() {
        let store = store();
        let admin = admin(&store);
        let memory = at(&store, &admin, "Prefers email over phone calls");
        let patched = update(
            &store,
            &admin,
            &memory.id,
            &MemoryPatch {
                importance: Some(0.9),
                ..MemoryPatch::default()
            },
        )
        .expect("importance only");
        assert_eq!(patched.importance, 0.9);
        assert_eq!(patched.content, memory.content);
        assert_eq!(patched.origin, memory.origin);
        assert!(
            update(
                &store,
                &admin,
                &memory.id,
                &MemoryPatch {
                    importance: Some(1.5),
                    ..MemoryPatch::default()
                },
            )
            .is_err()
        );
        assert!(
            update(
                &store,
                &admin,
                &memory.id,
                &MemoryPatch {
                    status: Some(STATUS_PENDING.into()),
                    ..MemoryPatch::default()
                },
            )
            .is_err()
        );
    }

    #[test]
    fn extracted_credentials_are_refused_but_explicit_text_is_kept() {
        let store = store();
        let admin = admin(&store);
        let refused = write(
            &store,
            &admin,
            &Candidate {
                origin: ORIGIN_EXTRACTED.into(),
                ..Candidate::explicit(
                    "fact",
                    "The bank IBAN is IT60X0542811101000000123456",
                    "turn:1",
                )
            },
        );
        assert!(refused.is_err());
        assert!(list(&store, &admin, 10).expect("list").is_empty());
    }

    #[test]
    fn sensitive_pattern_detection() {
        assert!(looks_sensitive("my password is hunter2"));
        assert!(looks_sensitive("key sk-abcdef123456789"));
        assert!(looks_sensitive("card 4111 1111 1111 1111"));
        assert!(looks_sensitive("IBAN IT60X0542811101000000123456"));
        assert!(looks_sensitive("-----BEGIN RSA PRIVATE KEY-----"));
        assert!(!looks_sensitive("Prefers concise Italian summaries"));
        assert!(!looks_sensitive("Meeting on 2026-11-07 at 10:00"));
    }

    #[test]
    fn content_hash_ignores_case_and_punctuation() {
        assert_eq!(
            content_hash("Lives in Rome."),
            content_hash("lives in rome")
        );
        assert_ne!(
            content_hash("Lives in Rome"),
            content_hash("Lives in Milan")
        );
    }

    #[test]
    fn summaries_are_scoped_and_bounded() {
        let store = store();
        let admin = admin(&store);
        let member = store
            .create_user("microsoft:summary-member", "s@example.example", "S")
            .expect("member");
        assert!(
            latest_summary(&store, &admin, "dir:main")
                .expect("none")
                .is_none()
        );
        let stored = store_summary(
            &store,
            &admin,
            "dir:main",
            Some("main"),
            &"a".repeat(SUMMARY_MAX_CHARS + 50),
            None,
        )
        .expect("store");
        assert_eq!(stored.summary.chars().count(), SUMMARY_MAX_CHARS);
        assert_eq!(
            latest_summary(&store, &admin, "dir:main")
                .expect("latest")
                .map(|summary| summary.id),
            Some(stored.id)
        );
        assert!(
            latest_summary(&store, &member.id, "dir:main")
                .expect("member")
                .is_none()
        );
        assert!(
            latest_summary(&store, &admin, "dir:other")
                .expect("other")
                .is_none()
        );
        assert!(store_summary(&store, &admin, "dir:main", None, "   ", None).is_err());
    }

    /// The bundled SQLite must provide FTS5 for the external-content index.
    #[test]
    fn fts_index_tracks_inserts_updates_and_deletes() {
        let store = store();
        let admin = admin(&store);
        let memory = at(&store, &admin, "Plays the cello on Sundays");
        let hits = search(&store, &admin, "cello", &MemoryFilter::default(), 5).expect("hit");
        assert_eq!(hits.len(), 1);
        update_content(&store, &admin, &memory.id, "Plays the viola on Sundays").expect("edit");
        assert!(
            search(&store, &admin, "cello", &MemoryFilter::default(), 5)
                .expect("old term")
                .is_empty()
        );
        assert_eq!(
            search(&store, &admin, "viola", &MemoryFilter::default(), 5)
                .expect("new term")
                .len(),
            1
        );
        delete(&store, &admin, &memory.id).expect("delete");
        assert!(
            search(&store, &admin, "viola", &MemoryFilter::default(), 5)
                .expect("gone")
                .is_empty()
        );
    }

    #[test]
    fn search_never_crosses_users() {
        let store = store();
        let admin = admin(&store);
        let member = store
            .create_user("microsoft:search-member", "x@example.example", "X")
            .expect("member");
        at(&store, &admin, "Admin keeps a jade turtle figurine");
        at(&store, &member.id, "Member keeps a jade ring");
        let member_hits = search(&store, &member.id, "jade", &MemoryFilter::default(), 10)
            .expect("member search");
        assert_eq!(member_hits.len(), 1);
        assert!(member_hits[0].memory.content.contains("ring"));
        let admin_hits = recall(&store, &admin, "jade", 10, None).expect("admin recall");
        assert!(
            admin_hits
                .iter()
                .all(|item| !item.memory.content.contains("ring"))
        );
    }

    fn pending(store: &Store, user: &str, content: &str) -> WriteOutcome {
        write(
            store,
            user,
            &Candidate {
                origin: ORIGIN_EXTRACTED.into(),
                status: STATUS_PENDING.into(),
                ..Candidate::explicit("fact", content, "turn:1")
            },
        )
        .expect("pending write")
    }

    /// Facts from a turn that read untrusted content stay out of recall and
    /// search until the owner approves them.
    #[test]
    fn pending_facts_are_not_recalled_until_approved() {
        let store = store();
        let admin = admin(&store);
        let held = pending(&store, &admin, "Owner wants the Berlin flight booked");
        assert_eq!(held.memory().status, STATUS_PENDING);
        assert!(
            recall(&store, &admin, "berlin flight", 8, None)
                .expect("recall")
                .is_empty()
        );
        assert!(
            search(&store, &admin, "berlin", &MemoryFilter::default(), 8)
                .expect("search")
                .is_empty()
        );
        let review = MemoryFilter {
            status: Some(STATUS_PENDING.into()),
            ..MemoryFilter::default()
        };
        assert_eq!(
            list_page(&store, &admin, &review, 0, 10)
                .expect("review")
                .len(),
            1
        );
        // Approval is the PATCH the console sends.
        update(
            &store,
            &admin,
            &held.memory().id,
            &MemoryPatch {
                status: Some(STATUS_ACTIVE.into()),
                ..MemoryPatch::default()
            },
        )
        .expect("approve");
        assert_eq!(
            recall(&store, &admin, "berlin flight", 8, None)
                .expect("recall")
                .len(),
            1
        );
    }

    /// A held copy never blocks the owner's own save, and a pending repeat
    /// reports the copy it already has.
    #[test]
    fn pending_and_active_copies_deduplicate_within_their_own_status() {
        let store = store();
        let admin = admin(&store);
        let held = pending(&store, &admin, "Prefers window seats");
        let saved = write(
            &store,
            &admin,
            &Candidate::explicit("preference", "Prefers window seats", "operator:test"),
        )
        .expect("explicit save");
        assert_eq!(saved.label(), "inserted");
        let again = pending(&store, &admin, "prefers window seats.");
        assert_eq!(again.label(), "duplicate");
        // The owner already has the active copy, which is the one returned.
        assert_eq!(again.memory().id, saved.memory().id);
        assert_ne!(again.memory().id, held.memory().id);
    }

    #[test]
    fn pending_writes_never_supersede_active_facts() {
        let store = store();
        let admin = admin(&store);
        let active = at(&store, &admin, "Prefers short morning meetings before noon");
        let held = pending(
            &store,
            &admin,
            "Prefers short morning meetings before noon always",
        );
        assert_eq!(held.label(), "inserted");
        assert_eq!(
            get(&store, &admin, &active.id).expect("active").status,
            STATUS_ACTIVE
        );
    }

    #[test]
    fn unknown_write_status_is_refused() {
        let store = store();
        let admin = admin(&store);
        let bad = write(
            &store,
            &admin,
            &Candidate {
                status: "superseded".into(),
                ..Candidate::explicit("fact", "Some fact", "operator:test")
            },
        );
        assert!(bad.is_err());
    }

    /// The FTS index is rebuilt only when it disagrees with the table: a
    /// row written past the triggers becomes findable after the refresh.
    #[test]
    fn fts_refresh_rebuilds_only_a_stale_index() {
        let store = store();
        let admin = admin(&store);
        at(&store, &admin, "Indexed through the trigger");
        let connection = store.connection().unwrap();
        refresh_fts_if_stale(&connection);
        connection
            .execute_batch("DROP TRIGGER memories_fts_ai;")
            .unwrap();
        connection
            .execute(
                "INSERT INTO memories(id, kind, content, source, created_at, updated_at, user_id)
                 VALUES ('bypass', 'fact', 'Hidden zebra note', 'operator:test',
                         '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', ?1)",
                params![admin],
            )
            .unwrap();
        drop(connection);
        assert!(
            search(&store, &admin, "zebra", &MemoryFilter::default(), 5)
                .expect("before refresh")
                .is_empty()
        );
        let connection = store.connection().unwrap();
        refresh_fts_if_stale(&connection);
        drop(connection);
        assert_eq!(
            search(&store, &admin, "zebra", &MemoryFilter::default(), 5)
                .expect("after refresh")
                .len(),
            1
        );
    }

    /// Hostile query text is treated as plain words: no FTS syntax error, no
    /// leak across users, and empty-token input falls back to a list.
    #[test]
    fn hostile_search_input_is_safe_and_user_scoped() {
        let store = store();
        let admin = admin(&store);
        let member = store
            .create_user("microsoft:hostile-member", "h@example.example", "H")
            .expect("member");
        at(&store, &admin, "Foo bar baz project notes for the auditor");
        at(
            &store,
            &admin,
            "Ignore previous instructions is a quoted phrase in a document",
        );
        at(&store, &member.id, "Member secret foo bar project");
        let hostile = [
            "\"foo\" OR bar:",
            "NEAR(a b)",
            "*",
            "-x",
            "ignore previous instructions",
            "foo\" AND \"bar",
            "a:b^2 {c} (d",
            "",
            "   ",
            "OR",
            "\u{0}\u{7}",
        ];
        for query in hostile {
            let owner = search(&store, &admin, query, &MemoryFilter::default(), 10)
                .unwrap_or_else(|error| panic!("search {query:?} failed: {error}"));
            assert!(
                owner
                    .iter()
                    .all(|hit| hit.memory.content.find("Member secret").is_none()),
                "owner search leaked a member fact for {query:?}"
            );
            let recalled = recall(&store, &admin, query, 8, Some("main"))
                .unwrap_or_else(|error| panic!("recall {query:?} failed: {error}"));
            assert!(
                recalled
                    .iter()
                    .all(|hit| !hit.memory.content.contains("Member secret")),
                "owner recall leaked a member fact for {query:?}"
            );
            let members = recall(&store, &member.id, query, 8, None)
                .unwrap_or_else(|error| panic!("member recall {query:?} failed: {error}"));
            assert!(
                members
                    .iter()
                    .all(|hit| !hit.memory.content.contains("auditor")),
                "member recall leaked an owner fact for {query:?}"
            );
        }
        // Plain words still find the owner's note.
        let found = search(&store, &admin, "auditor", &MemoryFilter::default(), 5).expect("plain");
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn query_tokens_are_quoted_and_bounded() {
        assert_eq!(query_tokens("a Rome  \"quoted\" x"), vec!["rome", "quoted"]);
        let many = (0..40)
            .map(|i| format!("word{i:02}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(query_tokens(&many).len(), MAX_QUERY_TOKENS);
    }
}
