//! Turning text into "meaning vectors", so that a question can find a passage that says the same
//! thing in different words ("paying late" and "late fees start after thirty days"). The model runs
//! on this server, so no document text is sent anywhere to be embedded.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

/// The model's name, stored with each vector so vectors from different models are never compared.
pub const MODEL_NAME: &str = "bge-small-en-v1.5";
/// Numbers per vector.
pub const DIMENSIONS: usize = 384;
/// The model was trained to see a question with this sentence in front of it and a passage on its
/// own, so searches are better when questions are written that way.
const QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";
/// Passages embedded in one go. Small, because the memory needed grows with the batch.
const BATCH_SIZE: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedError(pub String);

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "embedding: {}", self.0)
    }
}

impl std::error::Error for EmbedError {}

/// Something that can turn text into vectors of unit length, so the dot product of two vectors is
/// their cosine similarity (1 is the same meaning, 0 is unrelated).
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Which model made the vectors.
    fn model(&self) -> &str;
    async fn embed_passages(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedError>;
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError>;
}

/// Scales a vector to length one. A vector of all zeros stays as it is.
pub fn normalise(mut vector: Vec<f32>) -> Vec<f32> {
    let length = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    if length > 0.0 {
        vector.iter_mut().for_each(|x| *x /= length);
    }
    vector
}

/// The BGE small model, run in this process with ONNX Runtime.
pub struct LocalEmbedder {
    model: Arc<Mutex<TextEmbedding>>,
}

impl LocalEmbedder {
    /// Loads the model, downloading it (about 130 MB) the first time. `EMBEDDING_CACHE_DIR` says
    /// where it is kept; by default a folder under the home directory.
    pub fn load() -> Result<Self, EmbedError> {
        let cache = std::env::var_os("EMBEDDING_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".cache/workspace-cloud/models"))
            })
            .unwrap_or_else(|| PathBuf::from(".embedding-cache"));
        let model = TextEmbedding::try_new(
            TextInitOptions::new(EmbeddingModel::BGESmallENV15)
                .with_cache_dir(cache)
                .with_show_download_progress(false),
        )
        .map_err(|error| EmbedError(format!("could not load the model: {error}")))?;
        Ok(Self {
            model: Arc::new(Mutex::new(model)),
        })
    }

    async fn run(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedError> {
        let model = self.model.clone();
        // The model is busy for a while, so it runs off the async threads.
        tokio::task::spawn_blocking(move || {
            let mut model = model
                .lock()
                .map_err(|_| EmbedError("the model is unavailable".into()))?;
            model
                .embed(&texts, Some(BATCH_SIZE))
                .map(|vectors| vectors.into_iter().map(normalise).collect())
                .map_err(|error| EmbedError(error.to_string()))
        })
        .await
        .map_err(|error| EmbedError(format!("the model stopped: {error}")))?
    }
}

#[async_trait]
impl Embedder for LocalEmbedder {
    fn model(&self) -> &str {
        MODEL_NAME
    }

    async fn embed_passages(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedError> {
        self.run(texts).await
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let mut vectors = self.run(vec![format!("{QUERY_PREFIX}{text}")]).await?;
        vectors
            .pop()
            .ok_or_else(|| EmbedError("no vector came back".into()))
    }
}

/// A stand-in for tests that only need vectors to exist: words are hashed into a small vector, so
/// two texts that share words are close and others are not. It knows nothing about meaning.
pub struct HashingEmbedder;

impl HashingEmbedder {
    fn vector(text: &str) -> Vec<f32> {
        let mut vector = vec![0.0f32; DIMENSIONS];
        for word in text
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| word.len() > 2)
        {
            let hash = word.bytes().fold(0xcbf29ce484222325u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
            });
            vector[(hash % DIMENSIONS as u64) as usize] += 1.0;
        }
        normalise(vector)
    }
}

#[async_trait]
impl Embedder for HashingEmbedder {
    fn model(&self) -> &str {
        "hashing-test"
    }

    async fn embed_passages(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts.iter().map(|text| Self::vector(text)).collect())
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        Ok(Self::vector(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors_are_scaled_to_length_one() {
        let v = normalise(vec![3.0, 4.0]);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        assert_eq!(normalise(vec![0.0, 0.0]), vec![0.0, 0.0]);
    }

    #[tokio::test]
    async fn the_test_embedder_puts_texts_that_share_words_closer() {
        let e = HashingEmbedder;
        let query = e.embed_query("retainer payment due").await.unwrap();
        let near = e
            .embed_passages(vec!["the retainer payment is due".into()])
            .await
            .unwrap();
        let far = e
            .embed_passages(vec!["picnic at the lake".into()])
            .await
            .unwrap();
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(dot(&query, &near[0]) > dot(&query, &far[0]));
        assert_eq!(query.len(), DIMENSIONS);
    }
}
