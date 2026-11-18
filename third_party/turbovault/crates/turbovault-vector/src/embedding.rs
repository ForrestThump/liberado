use crate::error::VectorError;
use crate::require_feature;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "local")]
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

#[cfg(feature = "local")]
use fastembed::{RerankInitOptions, RerankerModel, TextRerank};

/// Common interface for embedding models.
#[async_trait::async_trait]
pub trait EmbeddingEngine: Send + Sync {
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, VectorError>;
    fn dimensions(&self) -> usize;
    fn model_name(&self) -> &str;
}

/// Local fastembed-backed embedding engine.
pub struct FastembedEngine {
    #[cfg(feature = "local")]
    model: Arc<std::sync::Mutex<TextEmbedding>>,
    dims: usize,
    model_name_str: String,
}

impl FastembedEngine {
    pub fn new(model_name: &str, cache_dir: Option<PathBuf>) -> Result<Self, VectorError> {
        require_feature!(Embedding, model_name, cache_dir);

        #[cfg(feature = "local")]
        {
            let (embedding_model, dims) = match model_name {
                "bge-base-en-v1.5" => (EmbeddingModel::BGEBaseENV15, 768usize),
                "bge-small-en-v1.5" => (EmbeddingModel::BGESmallENV15, 384usize),
                "all-MiniLM-L6-v2" => (EmbeddingModel::AllMiniLML6V2, 384usize),
                other => {
                    return Err(VectorError::Embedding(format!(
                        "unknown model name: {other}"
                    )));
                }
            };

            let mut opts = TextInitOptions::new(embedding_model).with_show_download_progress(true);
            if let Some(dir) = cache_dir {
                opts = opts.with_cache_dir(dir);
            }

            let text_embedding =
                TextEmbedding::try_new(opts).map_err(|e| VectorError::Embedding(e.to_string()))?;

            Ok(Self {
                model: Arc::new(std::sync::Mutex::new(text_embedding)),
                dims,
                model_name_str: model_name.to_string(),
            })
        }
    }
}

#[async_trait::async_trait]
impl EmbeddingEngine for FastembedEngine {
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, VectorError> {
        require_feature!(Embedding, texts);

        #[cfg(feature = "local")]
        {
            let model = self.model.clone();
            let texts_owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
            let result = tokio::task::spawn_blocking(move || {
                let mut guard = model.lock().expect("FastembedEngine mutex poisoned");
                guard.embed(texts_owned, Some(32))
            })
            .await
            .map_err(|e| VectorError::Embedding(e.to_string()))?
            .map_err(|e| VectorError::Embedding(e.to_string()))?;
            Ok(result)
        }
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn model_name(&self) -> &str {
        &self.model_name_str
    }
}

/// Cross-encoder reranking of over-fetched candidates before the final truncation to `limit`.
/// Reordering only — a reranker never adds or removes candidates from the recall set.
#[async_trait::async_trait]
pub trait Reranker: Send + Sync {
    /// Score each of `candidates` against `query`, returning scores in the SAME order as
    /// `candidates` (index-aligned) so callers can zip them back onto their own result structs
    /// without needing to track document text round-trips.
    async fn rerank(&self, query: &str, candidates: &[String]) -> Result<Vec<f32>, VectorError>;
}

/// Local fastembed-backed cross-encoder reranker (`fastembed::TextRerank`).
pub struct FastembedReranker {
    #[cfg(feature = "local")]
    model: Arc<std::sync::Mutex<TextRerank>>,
}

impl FastembedReranker {
    pub fn new(model_name: &str, cache_dir: Option<PathBuf>) -> Result<Self, VectorError> {
        require_feature!(Rerank, model_name, cache_dir);

        #[cfg(feature = "local")]
        {
            let reranker_model = match model_name {
                "bge-reranker-base" => RerankerModel::BGERerankerBase,
                "bge-reranker-v2-m3" => RerankerModel::BGERerankerV2M3,
                "jina-reranker-v1-turbo-en" => RerankerModel::JINARerankerV1TurboEn,
                "jina-reranker-v2-base-multilingual" => {
                    RerankerModel::JINARerankerV2BaseMultiligual
                }
                other => {
                    return Err(VectorError::Rerank(format!("unknown model name: {other}")));
                }
            };

            let mut opts = RerankInitOptions::new(reranker_model).with_show_download_progress(true);
            if let Some(dir) = cache_dir {
                opts = opts.with_cache_dir(dir);
            }

            let text_rerank =
                TextRerank::try_new(opts).map_err(|e| VectorError::Rerank(e.to_string()))?;

            Ok(Self {
                model: Arc::new(std::sync::Mutex::new(text_rerank)),
            })
        }
    }
}

#[async_trait::async_trait]
impl Reranker for FastembedReranker {
    async fn rerank(&self, query: &str, candidates: &[String]) -> Result<Vec<f32>, VectorError> {
        require_feature!(Rerank, query, candidates);

        #[cfg(feature = "local")]
        {
            let model = self.model.clone();
            let query_owned = query.to_string();
            let candidates_owned = candidates.to_vec();
            let results = tokio::task::spawn_blocking(move || {
                let mut guard = model.lock().expect("FastembedReranker mutex poisoned");
                guard.rerank(query_owned, candidates_owned, false, None)
            })
            .await
            .map_err(|e| VectorError::Rerank(e.to_string()))?
            .map_err(|e| VectorError::Rerank(e.to_string()))?;

            // `rerank` returns results sorted by score descending with `index` pointing back into
            // the input slice — restore input order so callers can zip scores against candidates.
            let mut scores = vec![0.0f32; candidates.len()];
            for result in results {
                scores[result.index] = result.score;
            }
            Ok(scores)
        }
    }
}
