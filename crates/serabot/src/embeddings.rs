//! Local multilingual embedding service.
//!
//! Production embeddings come from `fastembed`'s multilingual E5 small model
//! (384 dimensions, onnxruntime). Model files are resolved from the configured
//! cache directory at startup; initialization is the only place that may fetch
//! the model. Unit tests never construct [`FastEmbedder`], so they never
//! download model files.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use crate::config::EmbeddingsConfig;
use crate::error::{Result, SeraError};

/// Maximum number of texts accepted in one [`Embedder::embed`] call.
pub const MAX_TEXTS_PER_CALL: usize = 256;

/// Maximum number of characters accepted for a single embedding input.
///
/// Chunking is configured independently, but document ingestion refuses a
/// chunk configuration above this bound so every chunk can be embedded.
pub const MAX_TEXT_CHARS: usize = 16_384;

/// Canonical model identifier reported by [`Embedder::model_id`].
pub const SUPPORTED_MODEL: &str = "multilingual-e5-small";

/// Dimension of `intfloat/multilingual-e5-small`.
const SUPPORTED_DIMENSION: usize = 384;

/// Pluggable local text embedding backend.
///
/// Calls are bounded by [`MAX_TEXTS_PER_CALL`] and [`MAX_TEXT_CHARS`]. The
/// implementation is responsible for rejecting empty inputs and for returning
/// exactly one vector per input, in the same order.
#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>>;
    fn dimension(&self) -> usize;
    fn model_id(&self) -> &str;
}

/// Local multilingual embedder backed by fastembed/onnxruntime.
///
/// Inference is serialized through a mutex because a single ONNX session is
/// not meant to be shared for concurrent inference on a 2 CPU host, and it is
/// offloaded with `spawn_blocking` so it does not stall the async runtime.
pub struct FastEmbedder {
    model: Arc<Mutex<TextEmbedding>>,
    dimension: usize,
    model_id: String,
    batch_size: usize,
}

impl FastEmbedder {
    /// Load the model from `cache_dir`.
    ///
    /// This is a production startup action: if the cache does not contain the
    /// model files yet, fastembed fetches them into `cache_dir`. It must never
    /// be called from unit tests.
    pub fn new(config: &EmbeddingsConfig, cache_dir: impl AsRef<Path>) -> Result<Self> {
        let configured_model = config.model.trim().to_ascii_lowercase();
        if configured_model != SUPPORTED_MODEL
            && configured_model != "intfloat/multilingual-e5-small"
        {
            return Err(SeraError::Config(format!(
                "embeddings.model must be {SUPPORTED_MODEL} (or intfloat/multilingual-e5-small); got {}",
                config.model
            )));
        }
        if config.dimension != SUPPORTED_DIMENSION {
            return Err(SeraError::Config(format!(
                "embeddings.dimension must be {SUPPORTED_DIMENSION} for {SUPPORTED_MODEL}; got {} (reindex required after changing it)",
                config.dimension
            )));
        }
        if config.batch_size == 0 || config.batch_size > MAX_TEXTS_PER_CALL {
            return Err(SeraError::Config(format!(
                "embeddings.batch_size must be between 1 and {MAX_TEXTS_PER_CALL}"
            )));
        }

        let cache_dir = cache_dir.as_ref().to_path_buf();
        tracing::info!(
            model = SUPPORTED_MODEL,
            cache = %cache_dir.display(),
            "loading local embedding model"
        );
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::MultilingualE5Small)
                .with_cache_dir(cache_dir)
                .with_show_download_progress(false),
        )
        .map_err(|error| {
            SeraError::External(format!(
                "failed to initialize embedding model {SUPPORTED_MODEL}: {error}"
            ))
        })?;

        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            dimension: config.dimension,
            model_id: SUPPORTED_MODEL.to_owned(),
            batch_size: config.batch_size,
        })
    }

    /// Load the model using the configured cache directory resolved against the
    /// configuration file's directory.
    pub fn from_config(config: &crate::config::Config) -> Result<Self> {
        let cache_dir = config.path(&config.embeddings.cache_dir);
        Self::new(&config.embeddings, cache_dir)
    }
}

/// Load the model on first use instead of at daemon startup.
///
/// The VPS has 4 GB shared with the OS and other services. Retrieval and
/// ingestion are the only consumers of the ONNX model, so delaying the load
/// keeps idle memory low and makes a missing model cache a per-request error
/// rather than a startup blocker.
pub struct LazyEmbedder {
    config: crate::config::EmbeddingsConfig,
    cache_dir: PathBuf,
    inner: tokio::sync::OnceCell<FastEmbedder>,
}

impl LazyEmbedder {
    pub fn new(config: &crate::config::EmbeddingsConfig, cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            config: config.clone(),
            cache_dir: cache_dir.into(),
            inner: tokio::sync::OnceCell::new(),
        }
    }

    async fn get(&self) -> Result<&FastEmbedder> {
        self.inner
            .get_or_try_init(|| async { FastEmbedder::new(&self.config, &self.cache_dir) })
            .await
    }
}

#[async_trait]
impl Embedder for LazyEmbedder {
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.get().await?.embed(texts).await
    }

    fn dimension(&self) -> usize {
        self.config.dimension
    }

    fn model_id(&self) -> &str {
        SUPPORTED_MODEL
    }
}

#[async_trait]
impl Embedder for FastEmbedder {
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        validate_texts(&texts)?;
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let count = texts.len();
        let dimension = self.dimension;
        let batch_size = self.batch_size;
        let model = Arc::clone(&self.model);
        let embeddings = tokio::task::spawn_blocking(move || {
            let model = model
                .lock()
                .map_err(|_| "embedding model lock poisoned".to_owned())?;
            model
                .embed(texts, Some(batch_size))
                .map_err(|error| format!("embedding inference failed: {error}"))
        })
        .await
        .map_err(|error| SeraError::Internal(format!("embedding task failed: {error}")))?
        .map_err(SeraError::Internal)?;

        if embeddings.len() != count {
            return Err(SeraError::Internal(format!(
                "embedder returned {} vectors for {count} inputs",
                embeddings.len()
            )));
        }
        for embedding in &embeddings {
            if embedding.len() != dimension {
                return Err(SeraError::Internal(format!(
                    "embedder returned dimension {} but {dimension} is configured",
                    embedding.len()
                )));
            }
            if embedding.iter().any(|value| !value.is_finite()) {
                return Err(SeraError::Internal(
                    "embedder returned non-finite values".into(),
                ));
            }
        }
        Ok(embeddings)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}

fn validate_texts(texts: &[String]) -> Result<()> {
    if texts.len() > MAX_TEXTS_PER_CALL {
        return Err(SeraError::Validation(format!(
            "embedding request contains {} texts; at most {MAX_TEXTS_PER_CALL} are allowed",
            texts.len()
        )));
    }
    for (index, text) in texts.iter().enumerate() {
        if text.trim().is_empty() {
            return Err(SeraError::Validation(format!(
                "embedding text at index {index} is empty"
            )));
        }
        let length = text.chars().count();
        if length > MAX_TEXT_CHARS {
            return Err(SeraError::Validation(format!(
                "embedding text at index {index} has {length} characters; maximum is {MAX_TEXT_CHARS}"
            )));
        }
    }
    Ok(())
}

/// Test-only lexical embedder.
///
/// This is a deterministic hashed bag-of-words vector. It exists solely to
/// exercise storage, joins and provenance plumbing in unit tests. It has no
/// semantic quality and must never be used in production.
#[cfg(test)]
pub(crate) struct DeterministicTestEmbedder {
    dimension: usize,
}

#[cfg(test)]
impl DeterministicTestEmbedder {
    pub(crate) fn new(dimension: usize) -> Self {
        Self {
            dimension: dimension.max(1),
        }
    }
}

#[cfg(test)]
#[async_trait]
impl Embedder for DeterministicTestEmbedder {
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| lexical_vector(text, self.dimension))
            .collect())
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn model_id(&self) -> &str {
        "deterministic-test-lexical-v1"
    }
}

#[cfg(test)]
fn lexical_vector(text: &str, dimension: usize) -> Vec<f32> {
    let dimension = dimension.max(1);
    let mut vector = vec![0.0f32; dimension];
    for token in text
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
    {
        if token.is_empty() {
            continue;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in token.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        vector[(hash % dimension as u64) as usize] += 1.0;
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_and_oversized_inputs_before_model_use() {
        assert!(validate_texts(&["   ".to_owned()]).is_err());
        assert!(validate_texts(&["x".repeat(MAX_TEXT_CHARS + 1)]).is_err());
        assert!(validate_texts(&vec!["ok".to_owned(); MAX_TEXTS_PER_CALL + 1]).is_err());
        assert!(validate_texts(&["ok".to_owned()]).is_ok());
        assert!(validate_texts(&[]).is_ok());
    }

    #[test]
    fn rejects_mismatched_configuration_before_loading_any_model() {
        let config = EmbeddingsConfig {
            dimension: 768,
            ..EmbeddingsConfig::default()
        };
        assert!(FastEmbedder::new(&config, std::path::Path::new("unused")).is_err());

        let config = EmbeddingsConfig {
            batch_size: 0,
            ..EmbeddingsConfig::default()
        };
        assert!(FastEmbedder::new(&config, std::path::Path::new("unused")).is_err());

        let config = EmbeddingsConfig {
            model: "some-other-model".into(),
            ..EmbeddingsConfig::default()
        };
        assert!(FastEmbedder::new(&config, std::path::Path::new("unused")).is_err());
    }

    #[tokio::test]
    async fn deterministic_test_embedder_is_dimensionally_stable() {
        let embedder = DeterministicTestEmbedder::new(32);
        let first = embedder
            .embed(vec!["alpha".to_owned(), "beta".to_owned()])
            .await
            .expect("test embedding");
        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|vector| vector.len() == 32));
        assert!(first.iter().all(|vector| {
            let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
            (norm - 1.0).abs() < 1e-5
        }));
        assert_eq!(embedder.dimension(), 32);
        assert_eq!(embedder.model_id(), "deterministic-test-lexical-v1");
    }
}
