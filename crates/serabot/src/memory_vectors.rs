//! Semantic memory recall (0.62).
//!
//! Keyword recall (FTS5 BM25) misses a fact that says the same thing in other
//! words: "calendar" never matches "schedule". Each memory therefore also gets
//! one local embedding from the same multilingual E5 model that document
//! retrieval uses, and recall blends the two signals in
//! [`crate::memory::recall_hybrid`]: a fact's relevance is the better of its
//! lexical and its semantic match.
//!
//! - **UID-scoped.** Every read and write filters on the acting user's id. A
//!   vector row goes with its memory (`ON DELETE CASCADE`).
//! - **Lazy.** Vectors are written best effort after a memory is stored, and
//!   missing or stale rows (content hash or model changed) are rebuilt in
//!   small batches at recall time. A failing or missing model only means
//!   keyword recall for that turn; it never fails the turn.
//! - **Bounded.** At most [`MAX_SCAN`] vectors are compared per query and
//!   [`BACKFILL_BATCH`] embedded per recall. A small circle has hundreds of
//!   facts, not millions, so a brute-force cosine in Rust is enough and keeps
//!   memory out of the sqlite-vec document index.

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::params;

use crate::embeddings::{Embedder, MAX_TEXT_CHARS};
use crate::error::{Result, SeraError};
use crate::store::Store;

/// Most vectors compared for one query.
pub const MAX_SCAN: usize = 5_000;
/// Most missing vectors embedded during one recall.
pub const BACKFILL_BATCH: usize = 32;
/// Default cosine floor below which a fact is not a semantic match. Unrelated
/// sentences sit around 0.75–0.8 with multilingual E5 small.
pub const DEFAULT_MIN_SIMILARITY: f32 = 0.82;

/// The semantic side of memory recall.
#[derive(Clone)]
pub struct SemanticIndex {
    embedder: Arc<dyn Embedder>,
    min_similarity: f32,
}

impl SemanticIndex {
    pub fn new(embedder: Arc<dyn Embedder>, min_similarity: f32) -> Self {
        Self {
            embedder,
            min_similarity: min_similarity.clamp(0.0, 0.99),
        }
    }

    pub fn min_similarity(&self) -> f32 {
        self.min_similarity
    }

    /// Embed and store the vector of one of the acting user's memories.
    pub async fn index(&self, store: &Store, user_id: &str, memory_id: &str) -> Result<()> {
        let row = {
            let connection = store.connection()?;
            connection
                .query_row(
                    "SELECT content, content_hash FROM memories WHERE id = ?1 AND user_id = ?2",
                    params![memory_id, user_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .ok()
        };
        let Some((content, hash)) = row else {
            return Ok(());
        };
        self.embed_and_store(store, user_id, vec![(memory_id.to_owned(), content, hash)])
            .await
    }

    /// Rebuild up to [`BACKFILL_BATCH`] missing or stale vectors of the acting
    /// user (active and pending facts). Returns how many were written.
    pub async fn backfill(&self, store: &Store, user_id: &str) -> Result<usize> {
        let model = self.embedder.model_id().to_owned();
        let missing: Vec<(String, String, Option<String>)> = {
            let connection = store.connection()?;
            let mut statement = connection.prepare(
                "SELECT m.id, m.content, m.content_hash FROM memories m
                 LEFT JOIN memory_vectors v ON v.memory_id = m.id AND v.user_id = m.user_id
                 WHERE m.user_id = ?1 AND m.status IN ('active', 'pending')
                   AND (v.memory_id IS NULL OR v.model != ?2
                        OR v.content_hash != COALESCE(m.content_hash, ''))
                 ORDER BY m.updated_at DESC LIMIT ?3",
            )?;
            statement
                .query_map(params![user_id, model, BACKFILL_BATCH as i64], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let count = missing.len();
        if count > 0 {
            self.embed_and_store(store, user_id, missing).await?;
        }
        Ok(count)
    }

    async fn embed_and_store(
        &self,
        store: &Store,
        user_id: &str,
        rows: Vec<(String, String, Option<String>)>,
    ) -> Result<()> {
        let texts: Vec<String> = rows
            .iter()
            .map(|(_, content, _)| content.chars().take(MAX_TEXT_CHARS).collect())
            .collect();
        let vectors = self.embedder.embed(texts).await?;
        if vectors.len() != rows.len() {
            return Err(SeraError::Internal(
                "embedder returned a different number of memory vectors".into(),
            ));
        }
        let model = self.embedder.model_id().to_owned();
        let mut connection = store.connection()?;
        let tx = connection.transaction()?;
        for ((id, _, hash), vector) in rows.iter().zip(vectors) {
            if vector.len() != self.embedder.dimension() {
                continue;
            }
            // The memory must still be this user's; a row deleted meanwhile
            // simply inserts nothing.
            tx.execute(
                "INSERT INTO memory_vectors(memory_id, user_id, model, content_hash, vector)
                 SELECT id, user_id, ?3, ?4, ?5 FROM memories WHERE id = ?1 AND user_id = ?2
                 ON CONFLICT(memory_id) DO UPDATE SET
                    model = excluded.model,
                    content_hash = excluded.content_hash,
                    vector = excluded.vector",
                params![
                    id,
                    user_id,
                    model,
                    hash.clone().unwrap_or_default(),
                    encode(&vector)
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Semantic relevance of the acting user's current facts for `query`, as
    /// `id -> 0..=1`. Facts below the similarity floor are left out; the rest
    /// are scaled so the floor maps to 0 and an identical meaning to 1.
    /// `statuses` limits the facts compared (recall uses active only).
    pub async fn scores(
        &self,
        store: &Store,
        user_id: &str,
        query: &str,
        statuses: &[&str],
    ) -> Result<HashMap<String, f64>> {
        let query: String = query.trim().chars().take(MAX_TEXT_CHARS).collect();
        if query.is_empty() {
            return Ok(HashMap::new());
        }
        let mut vectors = self.embedder.embed(vec![query]).await?;
        let Some(probe) = vectors.pop() else {
            return Ok(HashMap::new());
        };
        let rows = self.current_vectors(store, user_id, statuses)?;
        let floor = f64::from(self.min_similarity);
        let mut out = HashMap::new();
        for (id, vector) in rows {
            let Some(similarity) = cosine(&probe, &vector) else {
                continue;
            };
            let similarity = f64::from(similarity);
            if similarity >= floor {
                out.insert(id, ((similarity - floor) / (1.0 - floor)).clamp(0.0, 1.0));
            }
        }
        Ok(out)
    }

    /// Raw cosine similarity between one memory and the acting user's other
    /// current facts, best first, at or above `min`. Used by the
    /// contradiction check to find facts about the same thing.
    pub fn neighbours(
        &self,
        store: &Store,
        user_id: &str,
        memory_id: &str,
        min: f32,
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        let rows = self.current_vectors(store, user_id, &["active"])?;
        let Some(probe) = rows
            .iter()
            .find(|(id, _)| id == memory_id)
            .map(|(_, vector)| vector.clone())
        else {
            return Ok(Vec::new());
        };
        let mut hits: Vec<(String, f32)> = rows
            .into_iter()
            .filter(|(id, _)| id != memory_id)
            .filter_map(|(id, vector)| cosine(&probe, &vector).map(|score| (id, score)))
            .filter(|(_, score)| *score >= min)
            .collect();
        hits.sort_by(|a, b| b.1.total_cmp(&a.1));
        hits.truncate(limit);
        Ok(hits)
    }

    fn current_vectors(
        &self,
        store: &Store,
        user_id: &str,
        statuses: &[&str],
    ) -> Result<Vec<(String, Vec<f32>)>> {
        let model = self.embedder.model_id().to_owned();
        let connection = store.connection()?;
        let mut statement = connection.prepare(
            "SELECT m.id, m.status, v.vector FROM memory_vectors v
             JOIN memories m ON m.id = v.memory_id AND m.user_id = v.user_id
             WHERE v.user_id = ?1 AND v.model = ?2
               AND v.content_hash = COALESCE(m.content_hash, '')
             ORDER BY m.updated_at DESC LIMIT ?3",
        )?;
        let rows = statement
            .query_map(params![user_id, model, MAX_SCAN as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .filter(|(_, status, _)| statuses.contains(&status.as_str()))
            .filter_map(|(id, _, bytes)| decode(&bytes).map(|vector| (id, vector)))
            .collect())
    }
}

fn encode(vector: &[f32]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn decode(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    Some(
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect(),
    )
}

/// Cosine similarity, or `None` for mismatched or zero vectors.
pub fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x), f64::from(*y));
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    let value = (dot / (na.sqrt() * nb.sqrt())) as f32;
    value.is_finite().then_some(value)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use async_trait::async_trait;

    /// A test embedder that knows a few synonyms, so a paraphrase lands near
    /// its fact while unrelated text does not. Each concept is one axis.
    pub(crate) struct ConceptEmbedder;

    const CONCEPTS: &[&[&str]] = &[
        &["calendar", "schedule", "agenda", "meetings"],
        &["coffee", "espresso", "latte"],
        &["ferry", "boat", "harbour"],
        &["mandatory", "required", "hard", "optional", "rule"],
        &["report", "summary", "weekly"],
    ];

    #[async_trait]
    impl Embedder for ConceptEmbedder {
        async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|text| {
                    let lower = text.to_lowercase();
                    let mut vector = vec![0.05f32; CONCEPTS.len() + 1];
                    for (axis, words) in CONCEPTS.iter().enumerate() {
                        if words.iter().any(|word| lower.contains(word)) {
                            vector[axis] = 1.0;
                        }
                    }
                    vector
                })
                .collect())
        }

        fn dimension(&self) -> usize {
            CONCEPTS.len() + 1
        }

        fn model_id(&self) -> &str {
            "concept-test-v1"
        }
    }

    fn setup() -> (Store, String, SemanticIndex) {
        let store = Store::open_in_memory(384).expect("store");
        let user = store.admin_user_id().expect("admin");
        (
            store,
            user,
            SemanticIndex::new(Arc::new(ConceptEmbedder), 0.82),
        )
    }

    fn add(store: &Store, user: &str, text: &str) -> String {
        crate::memory::add(store, user, "fact", text, "operator:test")
            .expect("memory")
            .id
    }

    #[tokio::test]
    async fn a_paraphrase_is_recalled_without_shared_words() {
        let (store, user, index) = setup();
        let calendar = add(&store, &user, "Keeps the team calendar in Outlook");
        add(&store, &user, "Drinks espresso after lunch");
        // Keyword recall finds nothing for a query with no shared word.
        let lexical = crate::memory::search(
            &store,
            &user,
            "what is on my agenda",
            &crate::memory::MemoryFilter::default(),
            5,
        )
        .unwrap();
        assert!(lexical.is_empty());
        assert_eq!(index.backfill(&store, &user).await.unwrap(), 2);
        let semantic = index
            .scores(&store, &user, "what is on my agenda", &["active"])
            .await
            .unwrap();
        assert_eq!(semantic.keys().collect::<Vec<_>>(), vec![&calendar]);
        let hits = crate::memory::search_hybrid(
            &store,
            &user,
            "what is on my agenda",
            &crate::memory::MemoryFilter::default(),
            5,
            &semantic,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].memory.id, calendar);
        let recalled =
            crate::memory::recall_hybrid(&store, &user, "agenda", 1, None, &semantic).unwrap();
        assert_eq!(recalled[0].memory.id, calendar);
    }

    #[tokio::test]
    async fn vectors_are_user_scoped_and_rebuilt_when_content_changes() {
        let (store, admin, index) = setup();
        let member = store
            .create_user("microsoft:vector-member", "v@example.example", "V")
            .unwrap()
            .id;
        add(&store, &admin, "Takes the harbour ferry at 8");
        let own = add(&store, &member, "Hates meetings before 10");
        index.backfill(&store, &admin).await.unwrap();
        index.backfill(&store, &member).await.unwrap();
        let theirs = index
            .scores(&store, &member, "boat timetable", &["active"])
            .await
            .unwrap();
        assert!(theirs.is_empty(), "another user's ferry fact never matches");
        // An edit makes the stored vector stale; the next backfill rebuilds it.
        crate::memory::update_content(&store, &member, &own, "Takes the boat on Fridays").unwrap();
        assert!(
            index
                .scores(&store, &member, "boat", &["active"])
                .await
                .unwrap()
                .is_empty(),
            "a stale vector is not used"
        );
        assert_eq!(index.backfill(&store, &member).await.unwrap(), 1);
        assert!(
            index
                .scores(&store, &member, "boat", &["active"])
                .await
                .unwrap()
                .contains_key(&own)
        );
        // Deleting the memory removes its vector.
        crate::memory::delete(&store, &member, &own).unwrap();
        let count: i64 = store
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM memory_vectors WHERE user_id = ?1",
                params![member],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn neighbours_find_facts_about_the_same_thing() {
        let (store, user, index) = setup();
        let hard = add(&store, &user, "The weekly report rule is mandatory");
        let soft = add(&store, &user, "The weekly report is optional now");
        add(&store, &user, "Drinks espresso after lunch");
        index.backfill(&store, &user).await.unwrap();
        let near = index.neighbours(&store, &user, &soft, 0.9, 3).unwrap();
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].0, hard);
    }

    #[test]
    fn cosine_handles_edges() {
        assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0]), Some(1.0));
        assert_eq!(cosine(&[1.0], &[1.0, 0.0]), None);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), None);
        assert_eq!(decode(&encode(&[0.5, -2.0])), Some(vec![0.5, -2.0]));
        assert_eq!(decode(&[1, 2, 3]), None);
    }
}
