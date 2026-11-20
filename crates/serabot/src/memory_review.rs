//! Weekly memory review (0.70).
//!
//! A small, member-scoped clean-up queue for the Memory page. Up to ten
//! memories are offered, in this order:
//!
//! 1. **contradicted**: held for review (`pending`) because they disagree with
//!    something the member saved (see `runtime/conflicts.rs`);
//! 2. **duplicate**: two active memories that say nearly the same thing (token
//!    overlap); the older one is offered, with the other as the merge target;
//! 3. **stale**: active memories not edited or confirmed for a long time,
//!    oldest first.
//!
//! The member answers each with `keep` (confirm it; a held memory becomes
//! active), `delete`, or `merge_into:<id>` (keep the target, delete this one).
//! Everything is scoped to the acting user; another member's id is
//! indistinguishable from a missing one. Selection reads rows only: no model
//! call, no network.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use chrono::{DateTime, Duration, Utc};
use rusqlite::params;
use serde::Serialize;

use crate::error::{Result, SeraError};
use crate::memory::{self, Memory, STATUS_ACTIVE, STATUS_PENDING};
use crate::store::Store;
use crate::util::truncate_chars;

/// Most memories offered at once.
pub const MAX_CANDIDATES: usize = 10;
/// An active memory untouched for this long is "stale".
pub const STALE_DAYS: i64 = 30;
/// Token overlap that makes two memories near-duplicates (the write path
/// already supersedes at 0.8).
const DUPLICATE_JACCARD: f64 = 0.6;
/// Facts shorter than this many tokens never count as duplicates.
const MIN_DUPLICATE_TOKENS: usize = 3;
/// Memories scanned for duplicates.
const SCAN: usize = 400;
const SNIPPET_CHARS: usize = 160;

pub const REASON_CONTRADICTED: &str = "contradicted";
pub const REASON_DUPLICATE: &str = "duplicate";
pub const REASON_STALE: &str = "stale";

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ReviewCandidate {
    pub id: String,
    pub content: String,
    pub kind: String,
    pub status: String,
    pub reason: &'static str,
    /// One plain sentence for the card.
    pub detail: String,
    /// For duplicates: the memory to merge into.
    pub similar_to: Option<String>,
    pub similar_content: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewAction {
    Keep,
    Delete,
    MergeInto(String),
}

impl FromStr for ReviewAction {
    type Err = SeraError;

    fn from_str(value: &str) -> Result<Self> {
        let value = value.trim();
        match value {
            "keep" => Ok(Self::Keep),
            "delete" => Ok(Self::Delete),
            other => match other.strip_prefix("merge_into:") {
                Some(target) if valid_id(target) => Ok(Self::MergeInto(target.to_owned())),
                _ => Err(SeraError::Validation(
                    "action must be keep, delete or merge_into:<id>".into(),
                )),
            },
        }
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn tokens(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    a.intersection(b).count() as f64 / a.union(b).count() as f64
}

fn candidate(
    memory: &Memory,
    reason: &'static str,
    detail: String,
    similar: Option<&Memory>,
) -> ReviewCandidate {
    ReviewCandidate {
        id: memory.id.clone(),
        content: truncate_chars(&memory.content, SNIPPET_CHARS),
        kind: memory.kind.clone(),
        status: memory.status.clone(),
        reason,
        detail,
        similar_to: similar.map(|other| other.id.clone()),
        similar_content: similar.map(|other| truncate_chars(&other.content, SNIPPET_CHARS)),
        updated_at: memory.updated_at.to_rfc3339(),
    }
}

impl Store {
    /// The member's confirmed memories with the confirmation time.
    fn memory_confirmations(&self, user_id: &str) -> Result<HashMap<String, DateTime<Utc>>> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT memory_id, confirmed_at FROM memory_reviews WHERE user_id = ?1")?;
        let rows = statement.query_map(params![user_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let (id, at) = row?;
            if let Ok(at) = DateTime::parse_from_rfc3339(&at) {
                map.insert(id, at.with_timezone(&Utc));
            }
        }
        Ok(map)
    }

    fn confirm_memory(&self, user_id: &str, memory_id: &str, now: DateTime<Utc>) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "INSERT INTO memory_reviews(user_id, memory_id, confirmed_at)
             SELECT ?1, id, ?3 FROM memories WHERE id = ?2 AND user_id = ?1
             ON CONFLICT(user_id, memory_id) DO UPDATE SET confirmed_at = excluded.confirmed_at",
            params![user_id, memory_id, now.to_rfc3339()],
        )?;
        Ok(changed == 1)
    }
}

/// Up to `limit` (at most [`MAX_CANDIDATES`]) memories for the member to
/// review, most pressing first.
pub fn candidates(
    store: &Store,
    user_id: &str,
    now: DateTime<Utc>,
    limit: usize,
) -> Result<Vec<ReviewCandidate>> {
    let limit = limit.clamp(1, MAX_CANDIDATES);
    let confirmed = store.memory_confirmations(user_id)?;
    // A confirmation counts until the memory is edited afterwards.
    let is_confirmed = |memory: &Memory| {
        confirmed
            .get(&memory.id)
            .is_some_and(|at| *at >= memory.updated_at)
    };
    let mut out: Vec<ReviewCandidate> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let pending = memory::list_page(
        store,
        user_id,
        &memory::MemoryFilter {
            status: Some(STATUS_PENDING.into()),
            ..Default::default()
        },
        0,
        limit,
    )?;
    for item in pending.iter().filter(|item| !is_confirmed(item)) {
        if out.len() >= limit {
            return Ok(out);
        }
        seen.insert(item.id.clone());
        out.push(candidate(
            item,
            REASON_CONTRADICTED,
            "This may contradict something you saved earlier.".into(),
            None,
        ));
    }

    let active = memory::list_page(store, user_id, &memory::MemoryFilter::default(), 0, SCAN)?;
    let sets: Vec<HashSet<String>> = active.iter().map(|item| tokens(&item.content)).collect();
    for i in 0..active.len() {
        if out.len() >= limit {
            return Ok(out);
        }
        for j in (i + 1)..active.len() {
            if sets[i].len() < MIN_DUPLICATE_TOKENS || sets[j].len() < MIN_DUPLICATE_TOKENS {
                continue;
            }
            if jaccard(&sets[i], &sets[j]) < DUPLICATE_JACCARD {
                continue;
            }
            // Offer the older one; the newer one is the merge target.
            let (older, newer) = if active[i].updated_at <= active[j].updated_at {
                (&active[i], &active[j])
            } else {
                (&active[j], &active[i])
            };
            if seen.contains(&older.id) || seen.contains(&newer.id) {
                continue;
            }
            if is_confirmed(older) && is_confirmed(newer) {
                continue;
            }
            seen.insert(older.id.clone());
            out.push(candidate(
                older,
                REASON_DUPLICATE,
                "Another memory says almost the same thing.".into(),
                Some(newer),
            ));
            break;
        }
    }

    let cutoff = now - Duration::days(STALE_DAYS);
    let mut stale: Vec<&Memory> = active
        .iter()
        .filter(|item| {
            !item.pinned
                && item.updated_at < cutoff
                && !seen.contains(&item.id)
                && !is_confirmed(item)
        })
        .collect();
    stale.sort_by(|a, b| a.updated_at.cmp(&b.updated_at).then(a.id.cmp(&b.id)));
    for item in stale {
        if out.len() >= limit {
            break;
        }
        let days = (now - item.updated_at).num_days();
        out.push(candidate(
            item,
            REASON_STALE,
            format!("Not checked for {days} days."),
            None,
        ));
    }
    out.truncate(limit);
    Ok(out)
}

/// What an action did, for the API and the audit row.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ReviewOutcome {
    pub id: String,
    pub action: &'static str,
    pub merged_into: Option<String>,
}

/// Apply one review action to one of the member's memories.
pub fn apply(
    store: &Store,
    user_id: &str,
    id: &str,
    action: &ReviewAction,
    now: DateTime<Utc>,
) -> Result<ReviewOutcome> {
    if !valid_id(id) {
        return Err(SeraError::Validation("invalid memory id".into()));
    }
    // Missing or another member's memory: the same NotFound either way.
    let current = memory::get(store, user_id, id)?;
    match action {
        ReviewAction::Keep => {
            if current.status == STATUS_PENDING {
                memory::update(
                    store,
                    user_id,
                    id,
                    &memory::MemoryPatch {
                        status: Some(STATUS_ACTIVE.into()),
                        ..Default::default()
                    },
                )?;
            }
            // Stamp after the update so the confirmation is not older than it.
            store.confirm_memory(user_id, id, now.max(Utc::now()))?;
            Ok(ReviewOutcome {
                id: id.into(),
                action: "keep",
                merged_into: None,
            })
        }
        ReviewAction::Delete => {
            if !memory::delete(store, user_id, id)? {
                return Err(SeraError::NotFound(format!("memory {id}")));
            }
            Ok(ReviewOutcome {
                id: id.into(),
                action: "delete",
                merged_into: None,
            })
        }
        ReviewAction::MergeInto(target) => {
            if target == id {
                return Err(SeraError::Validation(
                    "pick a different memory to merge into".into(),
                ));
            }
            let target_row = memory::get(store, user_id, target)?;
            if target_row.status == memory::STATUS_SUPERSEDED {
                return Err(SeraError::Validation(
                    "the memory to merge into is no longer active".into(),
                ));
            }
            if !memory::delete(store, user_id, id)? {
                return Err(SeraError::NotFound(format!("memory {id}")));
            }
            store.confirm_memory(user_id, target, now.max(Utc::now()))?;
            Ok(ReviewOutcome {
                id: id.into(),
                action: "merge",
                merged_into: Some(target.clone()),
            })
        }
    }
}

/// The weekly message: short snippets, and where to act.
pub fn review_text(candidates: &[ReviewCandidate]) -> String {
    let mut lines = vec![format!(
        "🧠 SeraBot — memory review: {} to check",
        candidates.len()
    )];
    for item in candidates {
        let tag = match item.reason {
            REASON_CONTRADICTED => "may contradict",
            REASON_DUPLICATE => "duplicate",
            _ => "old",
        };
        lines.push(format!(
            "• ({tag}) {}",
            truncate_chars(&item.content, 80).replace('\n', " ")
        ));
    }
    lines.push("Open Memory → Memory review to keep, edit, merge or delete.".into());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Store, String, String) {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let other = store
            .create_user("microsoft:bob", "bob@example.example", "Bob")
            .unwrap()
            .id;
        (store, admin, other)
    }

    fn add(store: &Store, user: &str, content: &str) -> String {
        memory::add(store, user, "fact", content, "test")
            .unwrap()
            .id
    }

    fn age(store: &Store, id: &str, days: i64) {
        let at = (Utc::now() - Duration::days(days)).to_rfc3339();
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE memories SET updated_at = ?2, created_at = ?2 WHERE id = ?1",
                params![id, at],
            )
            .unwrap();
    }

    fn hold(store: &Store, user: &str, id: &str) {
        assert!(memory::hold_for_review(store, user, id).unwrap());
    }

    #[test]
    fn selection_orders_contradicted_duplicate_then_stale_and_caps_at_ten() {
        let (store, admin, _) = store();
        let held = add(&store, &admin, "The weekly report is optional now");
        hold(&store, &admin, &held);
        add(&store, &admin, "Alice prefers morning meetings on Tuesdays");
        add(
            &store,
            &admin,
            "Alice prefers morning meetings on Tuesday mornings",
        );
        let old = add(&store, &admin, "The office printer is on floor three");
        age(&store, &old, 90);
        let found = candidates(&store, &admin, Utc::now(), 10).unwrap();
        let reasons: Vec<&str> = found.iter().map(|item| item.reason).collect();
        assert_eq!(
            reasons,
            vec![REASON_CONTRADICTED, REASON_DUPLICATE, REASON_STALE]
        );
        assert_eq!(found[0].id, held);
        assert!(found[1].similar_to.is_some());
        assert_eq!(found[2].id, old);
        for index in 0..15 {
            let id = add(
                &store,
                &admin,
                &format!("unique stale note number {index} alpha{index}"),
            );
            age(&store, &id, 100 + index);
        }
        assert_eq!(
            candidates(&store, &admin, Utc::now(), 50).unwrap().len(),
            10
        );
    }

    #[test]
    fn stale_candidates_are_oldest_first_and_skip_pinned_and_recent() {
        let (store, admin, _) = store();
        let oldest = add(&store, &admin, "one two three four oldest");
        let older = add(&store, &admin, "five six seven eight older");
        let fresh = add(&store, &admin, "nine ten eleven twelve fresh");
        let pinned = add(&store, &admin, "pinned stays put forever");
        memory::update(
            &store,
            &admin,
            &pinned,
            &memory::MemoryPatch {
                pinned: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        age(&store, &oldest, 200);
        age(&store, &older, 60);
        age(&store, &pinned, 300);
        let found = candidates(&store, &admin, Utc::now(), 10).unwrap();
        let ids: Vec<&str> = found.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, vec![oldest.as_str(), older.as_str()]);
        assert!(!ids.contains(&fresh.as_str()));
    }

    #[test]
    fn keep_confirms_and_hides_until_edited_and_activates_a_held_memory() {
        let (store, admin, _) = store();
        let old = add(&store, &admin, "alpha beta gamma delta old fact");
        age(&store, &old, 120);
        assert_eq!(candidates(&store, &admin, Utc::now(), 10).unwrap().len(), 1);
        let outcome = apply(&store, &admin, &old, &ReviewAction::Keep, Utc::now()).unwrap();
        assert_eq!(outcome.action, "keep");
        assert!(
            candidates(&store, &admin, Utc::now(), 10)
                .unwrap()
                .is_empty()
        );
        // Aged again (as if edited long ago after the confirmation): it is back.
        age(&store, &old, 0);
        age(&store, &old, 120);
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE memory_reviews SET confirmed_at = ?1",
                params![(Utc::now() - Duration::days(200)).to_rfc3339()],
            )
            .unwrap();
        assert_eq!(candidates(&store, &admin, Utc::now(), 10).unwrap().len(), 1);

        let held = add(&store, &admin, "held fact about something else entirely");
        hold(&store, &admin, &held);
        apply(&store, &admin, &held, &ReviewAction::Keep, Utc::now()).unwrap();
        assert_eq!(
            memory::get(&store, &admin, &held).unwrap().status,
            STATUS_ACTIVE
        );
    }

    #[test]
    fn delete_and_merge_remove_the_source_only() {
        let (store, admin, _) = store();
        let a = add(&store, &admin, "red green blue yellow orange");
        let b = add(&store, &admin, "red green blue yellow purple");
        let c = add(&store, &admin, "something unrelated about travel plans");
        let merged = apply(
            &store,
            &admin,
            &a,
            &ReviewAction::MergeInto(b.clone()),
            Utc::now(),
        )
        .unwrap();
        assert_eq!(merged.merged_into.as_deref(), Some(b.as_str()));
        assert!(memory::get(&store, &admin, &a).is_err());
        assert!(memory::get(&store, &admin, &b).is_ok());
        let own = ReviewAction::MergeInto(c.clone());
        assert!(apply(&store, &admin, &c, &own, Utc::now()).is_err());
        let missing = ReviewAction::MergeInto("nope".into());
        assert!(apply(&store, &admin, &c, &missing, Utc::now()).is_err());
        assert!(memory::get(&store, &admin, &c).is_ok());
        apply(&store, &admin, &c, &ReviewAction::Delete, Utc::now()).unwrap();
        assert!(memory::get(&store, &admin, &c).is_err());
    }

    #[test]
    fn everything_is_scoped_to_the_member() {
        let (store, admin, bob) = store();
        let mine = add(&store, &admin, "alpha beta gamma delta mine");
        let theirs = add(&store, &bob, "alpha beta gamma delta theirs");
        age(&store, &mine, 100);
        age(&store, &theirs, 100);
        let found = candidates(&store, &admin, Utc::now(), 10).unwrap();
        assert!(found.iter().all(|item| item.id == mine));
        assert!(apply(&store, &admin, &theirs, &ReviewAction::Delete, Utc::now()).is_err());
        assert!(apply(&store, &admin, &theirs, &ReviewAction::Keep, Utc::now()).is_err());
        let cross = ReviewAction::MergeInto(theirs.clone());
        assert!(apply(&store, &admin, &mine, &cross, Utc::now()).is_err());
        assert!(memory::get(&store, &admin, &mine).is_ok());
        assert!(memory::get(&store, &bob, &theirs).is_ok());
    }

    #[test]
    fn actions_parse_strictly() {
        assert_eq!("keep".parse::<ReviewAction>().unwrap(), ReviewAction::Keep);
        assert_eq!(
            "merge_into:abc-1".parse::<ReviewAction>().unwrap(),
            ReviewAction::MergeInto("abc-1".into())
        );
        assert!("merge_into:".parse::<ReviewAction>().is_err());
        assert!("merge_into:a b".parse::<ReviewAction>().is_err());
        assert!("explode".parse::<ReviewAction>().is_err());
    }

    #[test]
    fn the_message_lists_snippets_without_ids() {
        let (store, admin, _) = store();
        let old = add(&store, &admin, "alpha beta gamma delta old fact");
        age(&store, &old, 90);
        let found = candidates(&store, &admin, Utc::now(), 10).unwrap();
        let text = review_text(&found);
        assert!(text.contains("old fact"));
        assert!(!text.contains(&old));
    }
}
