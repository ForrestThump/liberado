//! Resolved vector-search configuration.
//!
//! Standalone — no dependency on the host's config crate. Callers start from
//! [`VectorConfig::default`] and override fields from their own settings source
//! (for a plugin, that is `read_config`).

use std::path::PathBuf;

/// Resolved vector search configuration.
#[derive(Debug, Clone)]
pub struct VectorConfig {
    /// Embedding model id, e.g. `bge-base-en-v1.5`.
    pub model: String,
    pub chunk_max_chars: usize,
    pub chunk_overlap_chars: usize,
    pub rrf_k: f64,
    pub bm25_weight: f32,
    pub auto_update: bool,
    pub incremental_granularity: String,
    pub model_cache_dir: Option<PathBuf>,
    pub index_quantization: String,
    pub search_overfetch_factor: usize,
    pub min_similarity: f32,
    pub rerank_enabled: bool,
    pub rerank_model: String,
}

impl Default for VectorConfig {
    fn default() -> Self {
        Self {
            model: "bge-base-en-v1.5".to_string(),
            chunk_max_chars: 800,
            chunk_overlap_chars: 100,
            rrf_k: 60.0,
            bm25_weight: 0.3,
            auto_update: true,
            incremental_granularity: "paragraph".to_string(),
            model_cache_dir: None,
            index_quantization: "f16".to_string(),
            search_overfetch_factor: 5,
            min_similarity: 0.3,
            rerank_enabled: false,
            rerank_model: "bge-reranker-base".to_string(),
        }
    }
}
