//! Retrieval over the indexed document corpus.
//!
//! Citation-facing types ([`RetrievedChunk`], [`Retriever`],
//! [`NoopRetriever`], [`evidence_block`]) are kept stable. [`SqliteRetriever`]
//! adds an exact, page-aware KNN implementation backed by sqlite-vec.

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
    async fn retrieve(&self, query: &str, k: usize) -> Result<Vec<RetrievedChunk>>;
}

pub struct NoopRetriever;

#[async_trait]
impl Retriever for NoopRetriever {
    async fn retrieve(&self, _query: &str, _k: usize) -> Result<Vec<RetrievedChunk>> {
        Ok(Vec::new())
    }
}

/// Exact KNN retriever over `chunk_vectors`, joined back to chunks and
/// documents so every result carries real page provenance.
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
    async fn retrieve(&self, query: &str, k: usize) -> Result<Vec<RetrievedChunk>> {
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
             WHERE d.status = 'ready' AND c.page >= 1 AND length(trim(c.text)) > 0
             ORDER BY knn.distance ASC, c.id ASC",
        )?;
        let rows = statement.query_map(params![blob, limit as i64], |row| {
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

    async fn seed(store: &Store) {
        let embedder = DeterministicTestEmbedder::new(384);
        let texts = vec!["alpha evidence".to_owned(), "omega evidence".to_owned()];
        let vectors = embedder.embed(texts.clone()).await.expect("test vectors");
        let connection = store.connection().expect("connection");
        connection
            .execute(
                "INSERT INTO documents(id, source_path, sha256, title, mime, pages, status, error, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', NULL, ?7, ?7)",
                params!["doc-a", "/tmp/a.txt", "sha-a", "Alpha", "text/plain", 2, NOW],
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
                "INSERT INTO documents(id, source_path, sha256, title, mime, pages, status, error, created_at, updated_at)
                 VALUES ('doc-failed', '/tmp/f.txt', 'sha-f', 'Failed', 'text/plain', 1, 'failed', 'boom', ?1, ?1)",
                params![NOW],
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
        seed(&store).await;
        let retriever =
            SqliteRetriever::new(store, Arc::new(DeterministicTestEmbedder::new(384)), 8);
        let chunks = retriever.retrieve("alpha", 8).await.expect("retrieval");
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
        let retriever =
            SqliteRetriever::new(store, Arc::new(DeterministicTestEmbedder::new(384)), 8);
        assert!(
            retriever
                .retrieve("alpha", 8)
                .await
                .expect("retrieval")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rejects_empty_and_oversized_queries() {
        let store = Store::open_in_memory(384).expect("store");
        let retriever =
            SqliteRetriever::new(store, Arc::new(DeterministicTestEmbedder::new(384)), 8);
        assert!(retriever.retrieve("   ", 8).await.is_err());
        assert!(
            retriever
                .retrieve(&"x".repeat(MAX_QUERY_CHARS + 1), 8)
                .await
                .is_err()
        );
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
