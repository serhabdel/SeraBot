//! Retrieval over the indexed document corpus.
//!
//! Citation-facing types ([`RetrievedChunk`], [`Retriever`],
//! [`NoopRetriever`], [`evidence_block`]) are kept stable. [`SqliteRetriever`]
//! adds an exact, page-aware KNN implementation backed by sqlite-vec.
//!
//! Every retrieval is scoped to the acting user (phase 2): results join back
//! to that user's documents only, so one user's corpus never surfaces in
//! another user's search.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::embeddings::Embedder;
use crate::error::{Result, SeraError};
use crate::store::Store;

/// Bound for incoming search queries, matching the RAG tool's own limit.
pub const MAX_QUERY_CHARS: usize = 2_000;
const MAX_K: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetrievedChunk {
    pub chunk_id: String,
    pub document_id: String,
    pub document_title: String,
    pub page: usize,
    pub text: String,
    pub score: f32,
}

#[async_trait]
pub trait Retriever: Send + Sync {
    /// Search the acting user's corpus.
    async fn retrieve(&self, user_id: &str, query: &str, k: usize) -> Result<Vec<RetrievedChunk>>;
}

pub struct NoopRetriever;

#[async_trait]
impl Retriever for NoopRetriever {
    async fn retrieve(
        &self,
        _user_id: &str,
        _query: &str,
        _k: usize,
    ) -> Result<Vec<RetrievedChunk>> {
        Ok(Vec::new())
    }
}

/// Exact KNN retriever over `chunk_vectors`, joined back to chunks and
/// documents so every result carries real page provenance — and filtered to
/// the acting user's documents.
#[derive(Clone)]
pub struct SqliteRetriever {
    store: Store,
    embedder: Arc<dyn Embedder>,
    k: usize,
}

impl SqliteRetriever {
    /// `k` is the maximum number of results this retriever will return.
    pub fn new(store: Store, embedder: Arc<dyn Embedder>, k: usize) -> Self {
        Self {
            store,
            embedder,
            k: k.clamp(1, MAX_K),
        }
    }

    pub fn max_k(&self) -> usize {
        self.k
    }
}

#[async_trait]
impl Retriever for SqliteRetriever {
    async fn retrieve(&self, user_id: &str, query: &str, k: usize) -> Result<Vec<RetrievedChunk>> {
        let query = query.trim();
        if query.is_empty() {
            return Err(SeraError::Validation(
                "search query must not be empty".into(),
            ));
        }
        if query.chars().count() > MAX_QUERY_CHARS {
            return Err(SeraError::Validation(format!(
                "search query exceeds {MAX_QUERY_CHARS} characters"
            )));
        }

        let dimension = self.store.dimension();
        if self.embedder.dimension() != dimension {
            return Err(SeraError::Config(format!(
                "embedder dimension {} does not match store dimension {dimension}",
                self.embedder.dimension()
            )));
        }

        let limit = if k == 0 { self.k } else { k.min(self.k) }.clamp(1, MAX_K);
        // Oversample the global sqlite-vec KNN subquery before filtering by
        // `d.user_id` and `d.status`, so a user with many indexed chunks cannot
        // starve another user's vector recall.
        let knn_k = (limit.max(1) * 10).max(100) as i64;
        let mut embeddings = self.embedder.embed(vec![query.to_owned()]).await?;
        if embeddings.len() != 1 {
            return Err(SeraError::Internal(format!(
                "embedder returned {} vectors for one query",
                embeddings.len()
            )));
        }
        let vector = embeddings.pop().ok_or_else(|| {
            SeraError::Internal("embedder returned no vector for the query".into())
        })?;
        if vector.len() != dimension {
            return Err(SeraError::Internal(format!(
                "query embedding has dimension {} but the store is configured for {dimension}",
                vector.len()
            )));
        }
        if vector.iter().any(|value| !value.is_finite()) {
            return Err(SeraError::Internal(
                "query embedding contains non-finite values".into(),
            ));
        }

        let blob = encode_vector(&vector);
        let connection = self.store.connection()?;
        let mut statement = connection.prepare(
            "SELECT c.id, c.document_id, d.title, c.page, c.text, knn.distance
             FROM (
                 SELECT chunk_id, distance
                 FROM chunk_vectors
                 WHERE embedding MATCH ?1 AND k = ?2
             ) AS knn
             JOIN chunks c ON c.id = knn.chunk_id
             JOIN documents d ON d.id = c.document_id
             WHERE d.status = 'ready' AND d.user_id = ?3
               AND c.page >= 1 AND length(trim(c.text)) > 0
             ORDER BY knn.distance ASC, c.id ASC
             LIMIT ?4",
        )?;
        let rows = statement.query_map(params![blob, knn_k, user_id, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, f64>(5)?,
            ))
        })?;

        let mut chunks = Vec::new();
        for row in rows {
            let (chunk_id, document_id, document_title, page, text, distance) = row?;
            if page < 1 || text.trim().is_empty() || !distance.is_finite() {
                continue;
            }
            // Monotonic in distance: closer vectors always score higher.
            let score = 1.0 / (1.0 + distance.max(0.0));
            chunks.push(RetrievedChunk {
                chunk_id,
                document_id,
                document_title,
                page: page as usize,
                text,
                score: score as f32,
            });
        }
        Ok(chunks)
    }
}

fn encode_vector(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

pub fn evidence_block(chunks: &[RetrievedChunk]) -> String {
    let mut output = String::new();
    for chunk in chunks {
        output.push_str(&format!(
            "[doc:{} p.{}] {}\n{}\n\n",
            chunk.document_id, chunk.page, chunk.document_title, chunk.text
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::DeterministicTestEmbedder;

    const NOW: &str = "2026-01-01T00:00:00.000Z";

    async fn seed(store: &Store, user_id: &str) {
        let embedder = DeterministicTestEmbedder::new(384);
        let texts = vec!["alpha evidence".to_owned(), "omega evidence".to_owned()];
        let vectors = embedder.embed(texts.clone()).await.expect("test vectors");
        let connection = store.connection().expect("connection");
        connection
            .execute(
                "INSERT INTO documents(id, source_path, sha256, title, mime, pages, status, error, created_at, updated_at, user_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', NULL, ?7, ?7, ?8)",
                params!["doc-a", "/tmp/a.txt", "sha-a", "Alpha", "text/plain", 2, NOW, user_id],
            )
            .expect("document");
        for (index, (text, vector)) in texts.iter().zip(vectors.iter()).enumerate() {
            let chunk_id = format!("chunk-{index}");
            connection
                .execute(
                    "INSERT INTO chunks(id, document_id, page, ordinal, text, created_at)
                     VALUES (?1, 'doc-a', ?2, ?2, ?3, ?4)",
                    params![chunk_id, (index + 1) as i64, text, NOW],
                )
                .expect("chunk");
            connection
                .execute(
                    "INSERT INTO chunk_vectors(chunk_id, embedding) VALUES (?1, ?2)",
                    params![chunk_id, encode_vector(vector)],
                )
                .expect("vector");
        }
        // A failed document must never surface, even if its vector is nearest.
        connection
            .execute(
                "INSERT INTO documents(id, source_path, sha256, title, mime, pages, status, error, created_at, updated_at, user_id)
                 VALUES ('doc-failed', '/tmp/f.txt', 'sha-f', 'Failed', 'text/plain', 1, 'failed', 'boom', ?1, ?1, ?2)",
                params![NOW, user_id],
            )
            .expect("failed document");
        connection
            .execute(
                "INSERT INTO chunks(id, document_id, page, ordinal, text, created_at)
                 VALUES ('chunk-failed', 'doc-failed', 1, 0, 'alpha evidence', ?1)",
                params![NOW],
            )
            .expect("failed chunk");
        connection
            .execute(
                "INSERT INTO chunk_vectors(chunk_id, embedding) VALUES ('chunk-failed', ?1)",
                params![encode_vector(&vectors[0])],
            )
            .expect("failed vector");
    }

    #[tokio::test]
    async fn returns_exact_provenance_with_monotonic_scores_and_filters_failed_documents() {
        let store = Store::open_in_memory(384).expect("store");
        let admin = store.admin_user_id().unwrap();
        seed(&store, &admin).await;
        let retriever =
            SqliteRetriever::new(store, Arc::new(DeterministicTestEmbedder::new(384)), 8);
        let chunks = retriever
            .retrieve(&admin, "alpha", 8)
            .await
            .expect("retrieval");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chunk_id, "chunk-0");
        assert_eq!(chunks[0].document_id, "doc-a");
        assert_eq!(chunks[0].document_title, "Alpha");
        assert_eq!(chunks[0].page, 1);
        assert!(chunks[0].text.contains("alpha"));
        assert!(chunks[0].score >= chunks[1].score);
        assert!(chunks.iter().all(|chunk| chunk.document_id != "doc-failed"));
    }

    #[tokio::test]
    async fn empty_index_returns_no_evidence() {
        let store = Store::open_in_memory(384).expect("store");
        let admin = store.admin_user_id().unwrap();
        let retriever =
            SqliteRetriever::new(store, Arc::new(DeterministicTestEmbedder::new(384)), 8);
        assert!(
            retriever
                .retrieve(&admin, "alpha", 8)
                .await
                .expect("retrieval")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rejects_empty_and_oversized_queries() {
        let store = Store::open_in_memory(384).expect("store");
        let admin = store.admin_user_id().unwrap();
        let retriever =
            SqliteRetriever::new(store, Arc::new(DeterministicTestEmbedder::new(384)), 8);
        assert!(retriever.retrieve(&admin, "   ", 8).await.is_err());
        assert!(
            retriever
                .retrieve(&admin, &"x".repeat(MAX_QUERY_CHARS + 1), 8)
                .await
                .is_err()
        );
    }

    /// A second user never retrieves another user's documents: the join is
    /// on the acting user's document rows, so a foreign corpus is invisible
    /// even when its vectors are the nearest neighbours.
    #[tokio::test]
    async fn retrieval_never_crosses_users() {
        let store = Store::open_in_memory(384).expect("store");
        let admin = store.admin_user_id().unwrap();
        let member = store
            .create_user("microsoft:retrieval-member", "r@example.example", "R")
            .unwrap();
        seed(&store, &admin).await;
        let retriever =
            SqliteRetriever::new(store, Arc::new(DeterministicTestEmbedder::new(384)), 8);
        // The member's search returns nothing, even though the admin's
        // vectors are the exact nearest matches for the query.
        assert!(
            retriever
                .retrieve(&member.id, "alpha", 8)
                .await
                .expect("member retrieval")
                .is_empty()
        );
        // And the admin still sees their own corpus.
        assert_eq!(
            retriever
                .retrieve(&admin, "alpha", 8)
                .await
                .expect("admin retrieval")
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn large_corpus_user_does_not_starve_other_users_recall() {
        let store = Store::open_in_memory(384).expect("store");
        let user_a = store
            .create_user("microsoft:user-a", "a@example.example", "User A")
            .unwrap();
        let user_b = store
            .create_user("microsoft:user-b", "b@example.example", "User B")
            .unwrap();

        let embedder = DeterministicTestEmbedder::new(384);
        let a_texts: Vec<String> = (0..50).map(|_| "alpha".to_owned()).collect();
        let a_vectors = embedder.embed(a_texts.clone()).await.expect("a vectors");
        let b_texts = vec!["alpha".to_owned(), "omega".to_owned()];
        let b_vectors = embedder.embed(b_texts.clone()).await.expect("b vectors");

        {
            let connection = store.connection().expect("connection");
            connection
                .execute(
                    "INSERT INTO documents(id, source_path, sha256, title, mime, pages, status, error, created_at, updated_at, user_id)
                     VALUES ('doc-user-a', '/tmp/ua.txt', 'sha-ua', 'User A Doc', 'text/plain', 50, 'ready', NULL, ?1, ?1, ?2)",
                    params![NOW, user_a.id],
                )
                .expect("doc-user-a");
            for (index, (text, vector)) in a_texts.iter().zip(a_vectors.iter()).enumerate() {
                let chunk_id = format!("chunk-a-{index}");
                connection
                    .execute(
                        "INSERT INTO chunks(id, document_id, page, ordinal, text, created_at)
                         VALUES (?1, 'doc-user-a', ?2, ?2, ?3, ?4)",
                        params![chunk_id, (index + 1) as i64, text, NOW],
                    )
                    .expect("chunk-a");
                connection
                    .execute(
                        "INSERT INTO chunk_vectors(chunk_id, embedding) VALUES (?1, ?2)",
                        params![chunk_id, encode_vector(vector)],
                    )
                    .expect("vec-a");
            }

            connection
                .execute(
                    "INSERT INTO documents(id, source_path, sha256, title, mime, pages, status, error, created_at, updated_at, user_id)
                     VALUES ('doc-user-b', '/tmp/ub.txt', 'sha-ub', 'User B Doc', 'text/plain', 2, 'ready', NULL, ?1, ?1, ?2)",
                    params![NOW, user_b.id],
                )
                .expect("doc-user-b");
            for (index, (text, vector)) in b_texts.iter().zip(b_vectors.iter()).enumerate() {
                let chunk_id = format!("chunk-b-{index}");
                connection
                    .execute(
                        "INSERT INTO chunks(id, document_id, page, ordinal, text, created_at)
                         VALUES (?1, 'doc-user-b', ?2, ?2, ?3, ?4)",
                        params![chunk_id, (index + 1) as i64, text, NOW],
                    )
                    .expect("chunk-b");
                connection
                    .execute(
                        "INSERT INTO chunk_vectors(chunk_id, embedding) VALUES (?1, ?2)",
                        params![chunk_id, encode_vector(vector)],
                    )
                    .expect("vec-b");
            }
        }

        let retriever =
            SqliteRetriever::new(store, Arc::new(DeterministicTestEmbedder::new(384)), 8);
        let b_chunks = retriever
            .retrieve(&user_b.id, "alpha", 2)
            .await
            .expect("user b retrieval");
        assert_eq!(b_chunks.len(), 2);
        assert!(b_chunks.iter().all(|c| c.document_id == "doc-user-b"));
        assert_eq!(b_chunks[0].chunk_id, "chunk-b-0");
        assert_eq!(b_chunks[1].chunk_id, "chunk-b-1");

        let a_chunks = retriever
            .retrieve(&user_a.id, "alpha", 2)
            .await
            .expect("user a retrieval");
        assert_eq!(a_chunks.len(), 2);
        assert!(a_chunks.iter().all(|c| c.document_id == "doc-user-a"));
    }

    #[test]
    fn evidence_block_marks_page_provenance() {
        let chunk = RetrievedChunk {
            chunk_id: "c1".into(),
            document_id: "doc-a".into(),
            document_title: "Alpha".into(),
            page: 3,
            text: "evidence".into(),
            score: 0.9,
        };
        let block = evidence_block(&[chunk]);
        assert!(block.contains("[doc:doc-a p.3]"));
    }
}
