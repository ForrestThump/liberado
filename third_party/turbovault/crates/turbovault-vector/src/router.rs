use crate::{
    EmbeddingEngine, Reranker, VectorError, VectorIndex,
    chunks::{Chunk, ChunkStore},
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::instrument;

/// A single vector search result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VectorResult {
    pub note_path: String,
    pub chunk_preview: String,
    pub chunk_position: String,
    pub score: f32,
}

/// A hybrid BM25 + vector search result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HybridResult {
    pub note_path: String,
    pub chunk_preview: String,
    pub chunk_position: String,
    pub rrf_score: f64,
    pub bm25_rank: Option<usize>,
    pub vector_rank: Option<usize>,
}

/// Combines BM25 (Tantivy) and HNSW results via Reciprocal Rank Fusion.
pub struct SearchRouter {
    vector: Arc<RwLock<VectorIndex>>,
    embedder: Arc<dyn EmbeddingEngine>,
    chunks: Arc<ChunkStore>,
    rrf_k: f64,
    bm25_weight: f32,
    overfetch_factor: usize,
    min_similarity: f32,
    reranker: Option<Arc<dyn Reranker>>,
}

impl SearchRouter {
    pub fn new(
        vector: Arc<RwLock<VectorIndex>>,
        embedder: Arc<dyn EmbeddingEngine>,
        chunks: Arc<ChunkStore>,
        rrf_k: f64,
        bm25_weight: f32,
        overfetch_factor: usize,
        min_similarity: f32,
    ) -> Self {
        Self {
            vector,
            embedder,
            chunks,
            rrf_k,
            bm25_weight,
            overfetch_factor,
            min_similarity,
            reranker: None,
        }
    }

    /// Opt into cross-encoder reranking of the over-fetched, deduped-by-note candidate pool
    /// before the final truncation to `limit`. Reordering only — never changes which notes are
    /// eligible, only their final rank. Existing callers that don't call this see identical
    /// behavior (plain cosine / RRF sort).
    pub fn with_reranker(mut self, reranker: Arc<dyn Reranker>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    #[instrument(skip(self), fields(query_len = query.len()))]
    pub async fn vector_only(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<VectorResult>, VectorError> {
        // Embed the query
        let mut embeddings = self.embedder.embed(&[query]).await?;
        let query_vec = embeddings.remove(0);

        // Search the HNSW index (over-fetch to allow dedup by note)
        let index = self.vector.read().await;
        let raw = index.search(&query_vec, limit * self.overfetch_factor)?;
        drop(index);

        let raw: Vec<(u64, f32)> = raw
            .into_iter()
            .filter(|(_, score)| *score >= self.min_similarity)
            .collect();

        // Deduplicate by note_path keeping best score per note.
        // Results are sorted by score descending, so the first chunk seen
        // for each note is the best. Cache the Chunk to avoid a second lookup.
        let mut best_by_note: HashMap<String, (Chunk, f32)> = HashMap::new();

        for (chunk_id, score) in raw {
            let chunk = match self.chunks.get_chunk_by_id(chunk_id)? {
                Some(c) => c,
                None => continue,
            };
            let note_path = chunk.note_path.clone();
            // Only the first (highest-scoring) chunk per note is kept.
            let _ = best_by_note.entry(note_path).or_insert((chunk, score));
        }

        let mut results: Vec<VectorResult> = Vec::with_capacity(best_by_note.len());
        for (note_path, (chunk, score)) in best_by_note {
            results.push(VectorResult {
                note_path,
                chunk_preview: chunk.preview.clone(),
                chunk_position: format!(
                    "chunk {} of {}",
                    chunk.chunk_index + 1,
                    chunk.total_chunks
                ),
                score,
            });
        }

        // Rerank the full deduped candidate pool, or fall back to plain cosine-score order.
        if let Some(reranker) = &self.reranker {
            let previews: Vec<String> = results.iter().map(|r| r.chunk_preview.clone()).collect();
            let rerank_scores = reranker.rerank(query, &previews).await?;
            let mut ranked: Vec<(f32, VectorResult)> =
                rerank_scores.into_iter().zip(results).collect();
            ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            results = ranked.into_iter().map(|(_, r)| r).collect();
        } else {
            results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        results.truncate(limit);
        Ok(results)
    }

    #[instrument(skip(self, bm25_results), fields(query_len = query.len(), bm25_count = bm25_results.len()))]
    pub async fn hybrid_search(
        &self,
        query: &str,
        limit: usize,
        bm25_results: Vec<(String, f32)>,
    ) -> Result<Vec<HybridResult>, VectorError> {
        // Embed query and search HNSW (over-fetch)
        let mut embeddings = self.embedder.embed(&[query]).await?;
        let query_vec = embeddings.remove(0);

        let index = self.vector.read().await;
        let raw: Vec<(u64, f32)> = index
            .search(&query_vec, limit * self.overfetch_factor)?
            .into_iter()
            .filter(|(_, score)| *score >= self.min_similarity)
            .collect();
        drop(index);

        // Build BM25 rank map: note_path -> 0-based rank
        let bm25_rank_map: HashMap<String, usize> = bm25_results
            .iter()
            .enumerate()
            .map(|(rank, (path, _))| (path.clone(), rank))
            .collect();

        // Build vector rank map: deduplicate by note_path, keep best score per note
        // Also track the best chunk_id per note for later lookup
        let mut best_vector_by_note: HashMap<String, (usize, u64, f32)> = HashMap::new();
        // ranked_vector_notes: note_paths in rank order (0 = best)
        let mut vector_note_order: Vec<String> = Vec::new();

        for (chunk_id, score) in &raw {
            let chunk = match self.chunks.get_chunk_by_id(*chunk_id)? {
                Some(c) => c,
                None => continue,
            };
            if let std::collections::hash_map::Entry::Vacant(e) =
                best_vector_by_note.entry(chunk.note_path)
            {
                let rank = vector_note_order.len();
                vector_note_order.push(e.key().clone());
                e.insert((rank, *chunk_id, *score));
            }
        }

        let vector_rank_map: HashMap<String, usize> = best_vector_by_note
            .iter()
            .map(|(path, (rank, _, _))| (path.clone(), *rank))
            .collect();

        // Collect all unique note_paths from both result sets
        let mut all_notes: Vec<String> = bm25_rank_map.keys().cloned().collect();
        for path in vector_rank_map.keys() {
            if !bm25_rank_map.contains_key(path) {
                all_notes.push(path.clone());
            }
        }

        let vector_weight = 1.0 - self.bm25_weight as f64;
        let bm25_weight = self.bm25_weight as f64;

        // Compute RRF scores
        let mut scored: Vec<(String, f64, Option<usize>, Option<usize>)> =
            Vec::with_capacity(all_notes.len());

        for note_path in all_notes {
            let bm25_rank = bm25_rank_map.get(&note_path).copied();
            let vector_rank = vector_rank_map.get(&note_path).copied();

            let bm25_contribution = bm25_rank
                .map(|r| bm25_weight / (self.rrf_k + r as f64))
                .unwrap_or(0.0);
            let vector_contribution = vector_rank
                .map(|r| vector_weight / (self.rrf_k + r as f64))
                .unwrap_or(0.0);

            let rrf_score = bm25_contribution + vector_contribution;
            scored.push((note_path, rrf_score, bm25_rank, vector_rank));
        }

        // Sort by rrf_score descending.
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Without a reranker, only the top `limit` need their preview text looked up. With a
        // reranker, the whole RRF-ranked candidate pool must be materialized first so it can be
        // rescored against the query text, then truncated — this costs one extra chunk lookup
        // per non-selected candidate, only paid when reranking is opted into.
        if self.reranker.is_none() {
            scored.truncate(limit);
        }

        let mut results = Vec::with_capacity(scored.len());
        for (note_path, rrf_score, bm25_rank, vector_rank) in scored {
            let (preview, chunk_position) =
                if let Some((_, chunk_id, _)) = best_vector_by_note.get(&note_path) {
                    // Use the best vector chunk for this note
                    match self.chunks.get_chunk_by_id(*chunk_id)? {
                        Some(c) => (
                            c.preview.clone(),
                            format!("chunk {} of {}", c.chunk_index + 1, c.total_chunks),
                        ),
                        None => {
                            // Fallback: first chunk from file
                            get_first_chunk_info(&self.chunks, &note_path)?
                        }
                    }
                } else {
                    // BM25-only result: use first chunk from file
                    get_first_chunk_info(&self.chunks, &note_path)?
                };

            results.push(HybridResult {
                note_path,
                chunk_preview: preview,
                chunk_position,
                rrf_score,
                bm25_rank,
                vector_rank,
            });
        }

        if let Some(reranker) = &self.reranker {
            let previews: Vec<String> = results.iter().map(|r| r.chunk_preview.clone()).collect();
            let rerank_scores = reranker.rerank(query, &previews).await?;
            let mut ranked: Vec<(f32, HybridResult)> =
                rerank_scores.into_iter().zip(results).collect();
            ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            results = ranked.into_iter().map(|(_, r)| r).collect();
            results.truncate(limit);
        }

        Ok(results)
    }
}

/// Look up the first chunk for a given file path, returning (preview, position).
/// Falls back to empty strings if no chunks are indexed for the file.
fn get_first_chunk_info(
    chunks: &ChunkStore,
    note_path: &str,
) -> Result<(String, String), VectorError> {
    let file_chunks = chunks.get_chunks_for_file(note_path)?;
    if let Some(c) = file_chunks.into_iter().next() {
        Ok((
            c.preview.clone(),
            format!("chunk {} of {}", c.chunk_index + 1, c.total_chunks),
        ))
    } else {
        Ok((String::new(), String::from("chunk 1 of 1")))
    }
}
