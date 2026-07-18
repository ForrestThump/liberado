use crate::error::MemoryError;
use crate::note::MemoryNote;
use liberado_common::{GuidanceHit, ToolGuidanceSource, WriteProvenance};
use liberado_vault::Vault;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use turbovault_vector::{
    ChunkStore, EmbeddingEngine, IndexBuilder, Reranker, SearchRouter, VectorIndex,
};

/// Cosine-similarity threshold above which `add`/`add_guidance` treat a candidate as a
/// near-duplicate of an existing note and return the existing note's id instead of writing a new
/// one. A cheap, always-on mechanical guard — distinct from the fuzzier LLM-driven consolidation
/// pass that runs periodically and can merge notes this similarity check wouldn't catch.
pub const DEFAULT_DEDUP_THRESHOLD: f32 = 0.92;

#[derive(Debug, Clone)]
pub struct MemoryStoreConfig {
    pub dedup_threshold: f32,
    pub search_limit: usize,
    pub chunk_max_chars: usize,
    pub chunk_overlap_chars: usize,
    pub search_overfetch_factor: usize,
    pub min_similarity: f32,
    pub index_quantization: String,
}

impl Default for MemoryStoreConfig {
    fn default() -> Self {
        Self {
            dedup_threshold: DEFAULT_DEDUP_THRESHOLD,
            search_limit: 10,
            chunk_max_chars: 800,
            chunk_overlap_chars: 100,
            search_overfetch_factor: 5,
            min_similarity: 0.3,
            index_quantization: "f16".to_string(),
        }
    }
}

/// A single result from [`MemoryStore::search`]: the note's id and content plus its relevance
/// score (cosine similarity, or a reranked score when the store was opened with a reranker).
/// `task_type`/`tools_used` are only ever set on procedural (tool-guidance) notes.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryResult {
    pub id: String,
    pub content: String,
    pub tags: Vec<String>,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_used: Option<Vec<String>>,
}

/// A vault-backed memory store: cleartext markdown notes (source of truth, one per memory) under
/// `dir`, plus a `turbovault-vector` HNSW index + SQLite chunk sidecar scoped to just that
/// subdirectory. Used twice by `liberado-memory-mcp` — once for `memory/general` (user facts and
/// preferences) and once for `memory/procedural` (tool-selection guidance) — the two stores are
/// isolated from each other (separate directories, separate indices) even though they share this
/// same implementation.
pub struct MemoryStore {
    vault: Vault,
    dir: PathBuf,
    index: Arc<RwLock<VectorIndex>>,
    router: SearchRouter,
    builder: IndexBuilder,
    embedder: Arc<dyn EmbeddingEngine>,
    config: MemoryStoreConfig,
}

impl MemoryStore {
    /// Open (creating if necessary) the store rooted at `dir` (vault-relative, e.g.
    /// `"memory/general"`). `embedder`/`reranker` are shared across both stores by the caller —
    /// one loaded ONNX model serving general + procedural + (optionally) the whole vault's search.
    pub async fn open(
        vault: Vault,
        dir: impl Into<PathBuf>,
        embedder: Arc<dyn EmbeddingEngine>,
        reranker: Option<Arc<dyn Reranker>>,
        config: MemoryStoreConfig,
    ) -> Result<Self, MemoryError> {
        let dir = dir.into();
        let notes_dir = vault.root().join(&dir);
        let state_dir = notes_dir.join(".index");
        tokio::fs::create_dir_all(&state_dir)
            .await
            .map_err(turbovault_vector::VectorError::Io)?;

        let chunks = Arc::new(ChunkStore::open(&state_dir.join("state.db"))?);
        let dims = embedder.dimensions();
        let index = VectorIndex::open_or_create(
            &state_dir.join("hnsw.idx"),
            dims,
            &config.index_quantization,
        )?;
        let index = Arc::new(RwLock::new(index));

        let mut router = SearchRouter::new(
            index.clone(),
            embedder.clone(),
            chunks.clone(),
            60.0, // rrf_k: unused, this store only ever calls vector_only (no lexical/BM25 side)
            0.0,  // bm25_weight: unused for the same reason
            config.search_overfetch_factor,
            config.min_similarity,
        );
        if let Some(reranker) = reranker {
            router = router.with_reranker(reranker);
        }

        let builder = IndexBuilder::new(
            embedder.clone(),
            chunks,
            config.chunk_max_chars,
            config.chunk_overlap_chars,
        );

        Ok(Self {
            vault,
            dir,
            index,
            router,
            builder,
            embedder,
            config,
        })
    }

    /// Re-walk this store's notes directory and rebuild the chunk/vector index from scratch.
    /// Unlike the incremental path every `add`/`delete` call uses, this re-embeds everything —
    /// intended as a startup/recovery lever (e.g. a note was added directly in Obsidian, bypassing
    /// this store, or the index sidecar was deleted), not part of the normal write path. Memory
    /// note volumes are expected to be small enough that this is cheap to run unconditionally at
    /// process start.
    pub async fn rebuild_all(&self) -> Result<(), MemoryError> {
        let notes_root = self.vault.root().join(&self.dir);
        let mut index = self.index.write().await;
        self.builder.full_rebuild(&notes_root, &mut index).await?;
        Ok(())
    }

    /// Search for memories semantically related to `query`.
    pub async fn search(&self, query: &str) -> Result<Vec<MemoryResult>, MemoryError> {
        let hits = self
            .router
            .vector_only(query, self.config.search_limit)
            .await?;

        let mut results = Vec::with_capacity(hits.len());
        for hit in hits {
            let rel_path = self.dir.join(&hit.note_path);
            let Ok(text) = self.vault.read(&rel_path).await else {
                // Stale index entry (note deleted/moved outside this store) — skip rather than fail
                // the whole search.
                continue;
            };
            let note = MemoryNote::from_note_text(&text)?;
            results.push(MemoryResult {
                id: note.id,
                content: note.content,
                tags: note.tags,
                score: hit.score,
                task_type: note.task_type,
                tools_used: note.tools_used,
            });
        }
        Ok(results)
    }

    /// Add a general fact/preference memory. Returns the existing note's id (without writing a
    /// duplicate) if a near-identical memory already exists.
    pub async fn add(
        &self,
        content: &str,
        tags: Vec<String>,
        provenance: &WriteProvenance,
    ) -> Result<String, MemoryError> {
        if let Some(existing_id) = self.find_near_duplicate(content).await? {
            return Ok(existing_id);
        }
        let id = ulid::Ulid::new().to_string();
        let note = MemoryNote::general(&id, content, tags);
        self.write_note(&note, provenance).await?;
        Ok(id)
    }

    /// Add a tool-selection guidance directive. Returns the existing note's id (without writing a
    /// duplicate) if near-identical guidance already exists.
    pub async fn add_guidance(
        &self,
        content: &str,
        task_type: Option<String>,
        tools_used: Option<Vec<String>>,
        tags: Vec<String>,
        provenance: &WriteProvenance,
    ) -> Result<String, MemoryError> {
        if let Some(existing_id) = self.find_near_duplicate(content).await? {
            return Ok(existing_id);
        }
        let id = ulid::Ulid::new().to_string();
        let note = MemoryNote::procedural(&id, content, task_type, tools_used, tags);
        self.write_note(&note, provenance).await?;
        Ok(id)
    }

    /// Delete a memory by id. Returns `false` if no such memory exists (not an error — deleting
    /// something already gone is idempotent).
    pub async fn delete(
        &self,
        id: &str,
        provenance: &WriteProvenance,
    ) -> Result<bool, MemoryError> {
        validate_id(id)?;
        let rel_path = self.dir.join(format!("{id}.md"));
        if self.vault.read(&rel_path).await.is_err() {
            return Ok(false);
        }
        self.vault.delete(&rel_path, None, provenance).await?;
        self.reindex(&rel_path).await?;
        Ok(true)
    }

    async fn write_note(
        &self,
        note: &MemoryNote,
        provenance: &WriteProvenance,
    ) -> Result<(), MemoryError> {
        let rel_path = self.dir.join(format!("{}.md", note.id));
        let text = note.to_note_text();
        self.vault.write(&rel_path, &text, None, provenance).await?;
        self.reindex(&rel_path).await
    }

    /// Incrementally re-embed just this one note (create/update/delete) — the paragraph-level
    /// diffing `IndexBuilder::update_file` already implements, reused as-is.
    async fn reindex(&self, rel_path: &std::path::Path) -> Result<(), MemoryError> {
        let abs_path = self.vault.root().join(rel_path);
        let notes_root = self.vault.root().join(&self.dir);
        let mut index = self.index.write().await;
        self.builder
            .update_file(&abs_path, &notes_root, &mut index)
            .await?;
        Ok(())
    }

    /// Find a near-duplicate of `content` among existing notes. The vector index's own chunk
    /// text includes each note's YAML frontmatter (`turbovault_parser::to_plain_text` has no
    /// concept of frontmatter, so it's embedded as literal text) — for short memory notes that
    /// dilutes cosine similarity badly (an exact duplicate can score well under the vector
    /// index's usual similarity range). The index is only used here for cheap candidate
    /// retrieval; the actual dedup decision re-embeds each candidate's clean `content` field and
    /// compares that directly against the new content, so the threshold means what it says.
    async fn find_near_duplicate(&self, content: &str) -> Result<Option<String>, MemoryError> {
        let hits = self.router.vector_only(content, 3).await?;
        for hit in hits {
            let rel_path = self.dir.join(&hit.note_path);
            let Ok(text) = self.vault.read(&rel_path).await else {
                continue;
            };
            let note = MemoryNote::from_note_text(&text)?;
            let similarity = self.content_similarity(content, &note.content).await?;
            if similarity >= self.config.dedup_threshold {
                return Ok(Some(note.id));
            }
        }
        Ok(None)
    }

    async fn content_similarity(&self, a: &str, b: &str) -> Result<f32, MemoryError> {
        let embeddings = self.embedder.embed(&[a, b]).await?;
        Ok(cosine_similarity(&embeddings[0], &embeddings[1]))
    }
}

/// The dispatcher's procedural-memory seam (`liberado-dispatch-logic-spec.md` §2, steps 1/5) —
/// implemented directly against a `MemoryStore` opened over `memory/procedural`, called
/// in-process by `Dispatcher` (not over MCP: the dispatcher runs inside the daemon and links this
/// crate directly, the same way `liberado-server` links `liberado-chat-search` directly rather
/// than round-tripping through `chat-search-mcp` for its own use).
#[async_trait::async_trait]
impl ToolGuidanceSource for MemoryStore {
    async fn search_tool_guidance(&self, goal: &str) -> Vec<GuidanceHit> {
        match self.search(goal).await {
            Ok(results) => results
                .into_iter()
                .map(|r| GuidanceHit {
                    content: r.content,
                    tools_used: r.tools_used.unwrap_or_default(),
                    score: r.score,
                })
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "procedural memory retrieval failed; continuing without guidance");
                Vec::new()
            }
        }
    }

    async fn save_tool_guidance(
        &self,
        directive: &str,
        task_type: Option<String>,
        tools_used: Vec<String>,
    ) {
        let provenance =
            WriteProvenance::agent("liberado-dispatcher", ulid::Ulid::new().to_string());
        if let Err(e) = self
            .add_guidance(directive, task_type, Some(tools_used), vec![], &provenance)
            .await
        {
            tracing::warn!(error = %e, "failed to record tool-selection guidance");
        }
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Guards against path traversal via an agent-supplied `memory_id` (e.g. `delete_memory` taking
/// arbitrary agent input) — ids are otherwise always self-generated ULIDs, which already satisfy
/// this, but a caller could still pass anything.
fn validate_id(id: &str) -> Result<(), MemoryError> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(MemoryError::InvalidId(id.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_vault::Vault;
    use turbovault_vector::FastembedEngine;

    async fn test_store(tmp: &tempfile::TempDir) -> MemoryStore {
        let vault = Vault::open_with_ledger("test", tmp.path().to_path_buf(), tmp.path().join(".prov.jsonl"))
            .await
            .unwrap();
        let embedder: Arc<dyn EmbeddingEngine> =
            Arc::new(FastembedEngine::new("all-MiniLM-L6-v2", None).unwrap());
        MemoryStore::open(
            vault,
            "memory/general",
            embedder,
            None,
            MemoryStoreConfig::default(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    #[ignore = "downloads an ONNX model on first run; run explicitly with --ignored"]
    async fn add_then_search_finds_it() {
        let tmp = tempfile::tempdir().unwrap();
        let store = test_store(&tmp).await;
        let provenance = WriteProvenance::agent("test", "corr-1");

        let id = store
            .add("User prefers dark mode in the TUI.", vec![], &provenance)
            .await
            .unwrap();

        let results = store.search("dark mode preference").await.unwrap();
        assert!(results.iter().any(|r| r.id == id));
    }

    #[tokio::test]
    #[ignore = "downloads an ONNX model on first run; run explicitly with --ignored"]
    async fn adding_a_near_duplicate_returns_the_existing_id() {
        let tmp = tempfile::tempdir().unwrap();
        let store = test_store(&tmp).await;
        let provenance = WriteProvenance::agent("test", "corr-1");

        let first = store
            .add("User prefers dark mode in the TUI.", vec![], &provenance)
            .await
            .unwrap();
        let second = store
            .add("User prefers dark mode in the TUI.", vec![], &provenance)
            .await
            .unwrap();

        assert_eq!(first, second);
    }

    #[tokio::test]
    #[ignore = "downloads an ONNX model on first run; run explicitly with --ignored"]
    async fn delete_removes_the_note_and_its_index_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let store = test_store(&tmp).await;
        let provenance = WriteProvenance::agent("test", "corr-1");

        let id = store
            .add("Some memory.", vec![], &provenance)
            .await
            .unwrap();
        let deleted = store.delete(&id, &provenance).await.unwrap();
        assert!(deleted);

        let results = store.search("Some memory").await.unwrap();
        assert!(!results.iter().any(|r| r.id == id));

        let deleted_again = store.delete(&id, &provenance).await.unwrap();
        assert!(!deleted_again);
    }

    #[test]
    fn rejects_path_traversal_ids() {
        assert!(matches!(
            validate_id("../secrets"),
            Err(MemoryError::InvalidId(_))
        ));
        assert!(validate_id("01HZY3K9QJXG7Q").is_ok());
    }
}
