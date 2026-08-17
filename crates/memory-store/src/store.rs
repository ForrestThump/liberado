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
    use std::sync::atomic::{AtomicBool, Ordering};
    use turbovault_vector::VectorError;

    // ── Test doubles ──────────────────────────────────────────────────────────────
    //
    // The production `FastembedEngine` downloads an ONNX model on first use, which is why the
    // tests this module replaces were `#[ignore]`d. These stubs replace exactly that one moving
    // part; everything else (Vault, SQLite `ChunkStore`, HNSW `VectorIndex`, chunking) is the
    // real code, so the tests exercise the real index round trip.

    /// Vocabulary the stub embedder counts. Chosen to cover the words used in this module's test
    /// content; tokens outside it (YAML frontmatter keys, ULIDs, stopwords) are ignored, so a
    /// note's frontmatter does not dilute cosine similarity the way it does with a real embedder.
    const VOCAB: &[&str] = &[
        "user",
        "prefers",
        "dark",
        "mode",
        "tui",
        "theme",
        "sidebar",
        "weather",
        "forecast",
        "lookup",
        "search",
        "always",
        "check",
        "planning",
        "build",
        "use",
        "tool",
        "guidance",
        "directive",
        "task",
        "alpha",
        "beta",
        "gamma",
        "delta",
        "epsilon",
        "zeta",
        "eta",
        "theta",
        "memory",
        "note",
        "fact",
        "store",
        "manual",
        "test",
    ];

    const N1: &str = "User prefers dark mode in the TUI.";
    const N1_QUERY: &str = "dark mode preference";
    const N2: &str = "Always check the weather forecast before planning a build.";
    const N3: &str = "The dark mode theme is in the sidebar settings.";
    const N3_QUERY: &str = "dark mode";
    const G1: &str = "Always use the forecast tool for weather lookup.";
    const G1_QUERY: &str = "weather forecast lookup";
    const MANUAL: &str = "Manual memory note written directly into the store directory.";

    /// Deterministic, model-free embedder: a term-frequency vector over [`VOCAB`]. Identical
    /// text embeds identically (cosine 1.0, so exact-duplicate dedup at the 0.92 threshold
    /// fires); text sharing vocabulary terms lands at a moderate cosine (search finds it);
    /// disjoint text scores 0 and is filtered by `min_similarity`. A `fail` flag injects embed
    /// errors for the degrade-gracefully paths of [`ToolGuidanceSource`].
    struct StubEmbedder {
        fail: AtomicBool,
    }

    impl StubEmbedder {
        fn new() -> Self {
            Self {
                fail: AtomicBool::new(false),
            }
        }

        /// Make the next `embed` call fail (error-path injection).
        fn fail_next_embed(&self) {
            self.fail.store(true, Ordering::SeqCst);
        }

        fn embed_one(text: &str) -> Vec<f32> {
            let mut v = vec![0.0f32; VOCAB.len()];
            for word in text.split(|c: char| !c.is_alphanumeric()) {
                if word.is_empty() {
                    continue;
                }
                let lower = word.to_ascii_lowercase();
                if let Some(i) = VOCAB.iter().position(|w| *w == lower.as_str()) {
                    v[i] += 1.0;
                }
            }
            if v.iter().all(|x| *x == 0.0) {
                // No vocabulary terms (e.g. an all-digits text): fall back to a deterministic
                // pseudo-random vector so the HNSW index never sees a zero vector — cosine is
                // undefined on it and usearch silently drops duplicate vectors.
                let mut h = 0xcbf29ce484222325u64;
                for b in text.bytes() {
                    h ^= b as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
                for slot in v.iter_mut() {
                    h = h.wrapping_mul(0x9e3779b97f4a7c15).rotate_left(13) ^ (h >> 7);
                    *slot = ((h >> 32) as u32 % 997) as f32 / 997.0;
                }
            }
            v
        }
    }

    #[async_trait::async_trait]
    impl EmbeddingEngine for StubEmbedder {
        async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, VectorError> {
            if self.fail.swap(false, Ordering::SeqCst) {
                return Err(VectorError::Embedding("injected test failure".into()));
            }
            Ok(texts.iter().map(|t| Self::embed_one(t)).collect())
        }

        fn dimensions(&self) -> usize {
            VOCAB.len()
        }

        fn model_name(&self) -> &str {
            "stub-tf-embedder"
        }
    }

    /// Cross-encoder stub: scores candidates by content — 100.0 if the chunk preview contains
    /// "sidebar", else 0.0. The reorder is therefore driven by content, not by the cosine order
    /// `vector_only` already produced (which HNSW approximate search does not guarantee to be
    /// stable). Deterministic and cheap, so the `with_reranker` branch of [`MemoryStore::open`]
    /// and the reorder path of `search` are exercised without loading a reranker model.
    struct StubReranker;

    #[async_trait::async_trait]
    impl Reranker for StubReranker {
        async fn rerank(
            &self,
            _query: &str,
            candidates: &[String],
        ) -> Result<Vec<f32>, VectorError> {
            Ok(candidates
                .iter()
                .map(|c| if c.contains("sidebar") { 100.0 } else { 0.0 })
                .collect())
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────────────

    fn provenance() -> WriteProvenance {
        WriteProvenance::agent("test", "corr-1")
    }

    async fn open_store(
        tmp: &tempfile::TempDir,
        subdir: &str,
        embedder: Arc<StubEmbedder>,
        reranker: Option<Arc<dyn Reranker>>,
        config: MemoryStoreConfig,
    ) -> MemoryStore {
        let embedder: Arc<dyn EmbeddingEngine> = embedder;
        let vault = Vault::open("test", tmp.path().to_path_buf()).await.unwrap();
        MemoryStore::open(vault, subdir, embedder, reranker, config)
            .await
            .unwrap()
    }

    async fn default_store(tmp: &tempfile::TempDir) -> (MemoryStore, Arc<StubEmbedder>) {
        let embedder = Arc::new(StubEmbedder::new());
        let store = open_store(
            tmp,
            "memory/general",
            embedder.clone(),
            None,
            MemoryStoreConfig::default(),
        )
        .await;
        (store, embedder)
    }

    fn note_file_count(store: &MemoryStore) -> usize {
        std::fs::read_dir(store.vault.root().join(&store.dir))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension() == Some("md".as_ref()))
            .count()
    }

    // ── open ─────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn open_creates_state_directory_and_chunk_db() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;

        let state = store.vault.root().join("memory/general/.index");
        assert!(
            state.is_dir(),
            "open must create the per-store .index state dir"
        );
        assert!(
            state.join("state.db").exists(),
            "SQLite chunk sidecar must exist"
        );
        // The HNSW file itself only materializes on the first flush (first write), not at open.
    }

    // ── add / add_guidance ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn add_writes_note_file_and_search_finds_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;

        let id = store
            .add(N1, vec!["ui".into(), "preferences".into()], &provenance())
            .await
            .unwrap();

        // The note is a real, human-readable markdown file with frontmatter + body.
        let text = store
            .vault
            .read(store.dir.join(format!("{id}.md")))
            .await
            .unwrap();
        let note = MemoryNote::from_note_text(&text).unwrap();
        assert_eq!(note.content, N1);
        assert_eq!(note.tags, vec!["ui".to_string(), "preferences".to_string()]);
        assert_eq!(note.task_type, None);

        let results = store.search(N1_QUERY).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, id);
        assert_eq!(results[0].content, N1);
        assert_eq!(
            results[0].tags,
            vec!["ui".to_string(), "preferences".to_string()]
        );
        assert!(results[0].score > 0.0);
    }

    #[tokio::test]
    async fn adding_exact_duplicate_returns_existing_id_without_new_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;

        let first = store.add(N1, vec![], &provenance()).await.unwrap();
        let second = store.add(N1, vec![], &provenance()).await.unwrap();

        assert_eq!(
            first, second,
            "exact duplicate must return the existing note's id"
        );
        assert_eq!(
            note_file_count(&store),
            1,
            "dedup must not write a second file"
        );
    }

    #[tokio::test]
    async fn adding_distinct_content_creates_a_new_note() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;

        let a = store.add(N1, vec![], &provenance()).await.unwrap();
        let b = store.add(N2, vec![], &provenance()).await.unwrap();

        assert_ne!(a, b, "semantically different content must not dedup");
        assert_eq!(note_file_count(&store), 2);
    }

    #[tokio::test]
    async fn guidance_round_trips_through_search() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;

        let id = store
            .add_guidance(
                G1,
                Some("lookup".into()),
                Some(vec!["weather-mcp".into()]),
                vec!["dispatch".into()],
                &provenance(),
            )
            .await
            .unwrap();

        let results = store.search(G1_QUERY).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, id);
        assert_eq!(results[0].task_type.as_deref(), Some("lookup"));
        assert_eq!(results[0].tools_used, Some(vec!["weather-mcp".to_string()]));
        assert_eq!(results[0].tags, vec!["dispatch".to_string()]);
    }

    #[tokio::test]
    async fn concurrent_adds_all_persist_without_lost_updates() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;
        let store = Arc::new(store);

        let contents = [
            "alpha fact one",
            "beta fact two",
            "gamma fact three",
            "delta fact four",
            "epsilon fact five",
            "zeta fact six",
            "eta fact seven",
            "theta fact eight",
        ];
        let mut handles = Vec::new();
        for content in contents {
            let store = store.clone();
            let prov = provenance();
            handles.push(tokio::spawn(async move {
                store.add(content, vec![], &prov).await.unwrap()
            }));
        }

        let mut ids: Vec<String> = Vec::new();
        for h in handles {
            ids.push(h.await.unwrap());
        }
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), contents.len(), "each add must get its own id");
        assert_eq!(note_file_count(&store), contents.len());
    }

    #[tokio::test]
    async fn adding_exact_duplicate_guidance_returns_existing_id() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;

        let first = store
            .add_guidance(G1, Some("lookup".into()), None, vec![], &provenance())
            .await
            .unwrap();
        let second = store
            .add_guidance(G1, Some("lookup".into()), None, vec![], &provenance())
            .await
            .unwrap();

        assert_eq!(first, second, "exact-duplicate guidance must dedup too");
        assert_eq!(note_file_count(&store), 1);
    }

    #[tokio::test]
    async fn dedup_skips_candidates_whose_files_were_deleted_outside_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;
        let first = store.add(N1, vec![], &provenance()).await.unwrap();

        // Delete the file behind the store's back — the chunk stays in the index as a dedup
        // candidate, but the read fails and the candidate must be skipped, not fatal.
        store
            .vault
            .delete(store.dir.join(format!("{first}.md")), None, &provenance())
            .await
            .unwrap();

        let second = store.add(N1, vec![], &provenance()).await.unwrap();
        assert_ne!(
            first, second,
            "a stale candidate is not a duplicate — a new note is written"
        );
        assert_eq!(note_file_count(&store), 1);
    }

    #[tokio::test]
    async fn open_fails_when_state_dir_cannot_be_created() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = Vault::open("test", tmp.path().to_path_buf()).await.unwrap();
        // Occupy the `.index` path with a plain FILE so `create_dir_all` fails.
        let state = tmp.path().join("memory/general/.index");
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        std::fs::write(&state, "in the way").unwrap();

        let embedder: Arc<dyn EmbeddingEngine> = Arc::new(StubEmbedder::new());
        let result = MemoryStore::open(
            vault,
            "memory/general",
            embedder,
            None,
            MemoryStoreConfig::default(),
        )
        .await;
        assert!(
            matches!(result, Err(MemoryError::Vector(_))),
            "open must surface the state-dir creation failure"
        );
    }

    // ── search ───────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn search_on_empty_store_returns_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;

        assert!(store.search(N1_QUERY).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_skips_stale_index_entries_for_notes_deleted_outside_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;
        let id = store.add(N1, vec![], &provenance()).await.unwrap();

        // Delete the file behind the store's back — the chunk/vector entries survive.
        store
            .vault
            .delete(store.dir.join(format!("{id}.md")), None, &provenance())
            .await
            .unwrap();

        let results = store.search(N1_QUERY).await.unwrap();
        assert!(
            results.is_empty(),
            "stale index entry must be skipped, not surfaced as an error"
        );
    }

    #[tokio::test]
    async fn search_limit_caps_the_number_of_results() {
        let tmp = tempfile::tempdir().unwrap();
        let embedder = Arc::new(StubEmbedder::new());
        let config = MemoryStoreConfig {
            search_limit: 1,
            ..MemoryStoreConfig::default()
        };
        let store = open_store(&tmp, "memory/general", embedder, None, config).await;
        store.add(N1, vec![], &provenance()).await.unwrap();
        store.add(N3, vec![], &provenance()).await.unwrap();

        // Both notes match "dark mode"; the limit must cap the response.
        assert_eq!(store.search(N3_QUERY).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_with_reranker_reorders_results_by_rerank_score() {
        let tmp = tempfile::tempdir().unwrap();
        let embedder = Arc::new(StubEmbedder::new());
        let store = open_store(
            &tmp,
            "memory/general",
            embedder,
            Some(Arc::new(StubReranker)),
            MemoryStoreConfig::default(),
        )
        .await;
        let n1 = store.add(N1, vec![], &provenance()).await.unwrap();
        let n3 = store.add(N3, vec![], &provenance()).await.unwrap();

        let results = store.search(N3_QUERY).await.unwrap();
        assert_eq!(results.len(), 2);
        // The stub reranker gives the "sidebar" note 100.0 and everything else 0.0, so the final
        // order is [n3, n1] no matter what the cosine-only order was.
        assert_eq!(results[0].id, n3);
        assert_eq!(results[1].id, n1);
        // Scores in the results stay the cosine scores — the reranker only reorders.
        assert!(results[0].score > results[1].score);
    }

    // ── delete ───────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn delete_removes_note_and_index_entry_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;
        let id = store.add(N1, vec![], &provenance()).await.unwrap();

        assert!(store.delete(&id, &provenance()).await.unwrap());
        assert!(store.search(N1_QUERY).await.unwrap().is_empty());
        assert_eq!(note_file_count(&store), 0);
        assert!(
            !store.delete(&id, &provenance()).await.unwrap(),
            "deleting an already-deleted note is idempotent (false, not an error)"
        );
    }

    #[tokio::test]
    async fn delete_of_unknown_but_well_formed_id_is_false_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;

        assert!(!store.delete("01HZY3K9QJXG7Q", &provenance()).await.unwrap());
    }

    #[tokio::test]
    async fn delete_rejects_path_traversal_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;

        assert!(matches!(
            store.delete("../secrets.md", &provenance()).await,
            Err(MemoryError::InvalidId(_))
        ));
    }

    // ── rebuild_all ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn rebuild_all_indexes_notes_written_outside_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;
        let id = store.add(N1, vec![], &provenance()).await.unwrap();

        // A note dropped straight into the directory, bypassing the store (e.g. written in
        // Obsidian). It needs valid frontmatter to parse back out of search.
        let manual = MemoryNote::general("manual-note", MANUAL, vec![]);
        store
            .vault
            .write(
                store.dir.join("manual-note.md"),
                &manual.to_note_text(),
                None,
                &provenance(),
            )
            .await
            .unwrap();

        store.rebuild_all().await.unwrap();

        let results = store.search("manual memory note").await.unwrap();
        assert!(
            results.iter().any(|r| r.id == "manual-note"),
            "rebuild must pick up notes written outside the store"
        );
        assert!(
            store
                .search(N1_QUERY)
                .await
                .unwrap()
                .iter()
                .any(|r| r.id == id),
            "store-created notes must survive the rebuild"
        );
    }

    // ── reopening an existing store ──────────────────────────────────────────────

    #[tokio::test]
    async fn reopening_an_existing_store_reads_back_indexed_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let id = {
            let (store, _) = default_store(&tmp).await;
            store.add(N1, vec![], &provenance()).await.unwrap()
        };

        // Reopen over the same directory — the HNSW file is on disk now, so the index opens in
        // mmap view mode.
        let (store, _) = default_store(&tmp).await;
        assert!(
            store
                .search(N1_QUERY)
                .await
                .unwrap()
                .iter()
                .any(|r| r.id == id),
            "notes indexed before the reopen must still be found"
        );

        // Writes after reopen promote the index back to RAM and keep working.
        let second = store.add(N2, vec![], &provenance()).await.unwrap();
        assert!(
            store
                .search("weather forecast")
                .await
                .unwrap()
                .iter()
                .any(|r| r.id == second)
        );
    }

    // ── ToolGuidanceSource (dispatcher seam) ─────────────────────────────────────

    #[tokio::test]
    async fn tool_guidance_search_returns_hits() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;
        store
            .add_guidance(
                G1,
                Some("lookup".into()),
                Some(vec!["weather-mcp".into()]),
                vec![],
                &provenance(),
            )
            .await
            .unwrap();

        let hits = store.search_tool_guidance(G1_QUERY).await;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content, G1);
        assert_eq!(hits[0].tools_used, vec!["weather-mcp".to_string()]);
        assert!(hits[0].score > 0.0);
    }

    #[tokio::test]
    async fn tool_guidance_search_failure_degrades_to_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, embedder) = default_store(&tmp).await;
        store
            .add_guidance(G1, None, None, vec![], &provenance())
            .await
            .unwrap();

        embedder.fail_next_embed();
        let hits = store.search_tool_guidance(G1_QUERY).await;
        assert!(
            hits.is_empty(),
            "a backend failure must degrade to 'no guidance', not propagate"
        );
    }

    #[tokio::test]
    async fn tool_guidance_save_records_a_searchable_directive() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = default_store(&tmp).await;

        store
            .save_tool_guidance(G1, Some("lookup".into()), vec!["weather-mcp".into()])
            .await;

        let hits = store.search_tool_guidance(G1_QUERY).await;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content, G1);
        assert_eq!(hits[0].tools_used, vec!["weather-mcp".to_string()]);
    }

    #[tokio::test]
    async fn tool_guidance_save_failure_is_swallowed() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, embedder) = default_store(&tmp).await;

        embedder.fail_next_embed();
        // Best-effort by contract: a failure here must not panic or propagate.
        store.save_tool_guidance(G1, None, vec![]).await;

        assert!(store.search_tool_guidance(G1_QUERY).await.is_empty());
    }

    // ── pure helpers ─────────────────────────────────────────────────────────────

    #[test]
    fn cosine_similarity_semantics() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]) - 0.0).abs() < 1e-6);
        // A zero vector has no direction — similarity is 0, not NaN.
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
        assert_eq!(cosine_similarity(&[1.0, 1.0], &[0.0, 0.0]), 0.0);
    }

    #[test]
    fn validate_id_rejects_traversal_and_odd_characters() {
        for bad in [
            "",
            ".",
            "..",
            "../secrets",
            "a/b",
            "a b",
            "a.b",
            "a_b",
            "日本語",
            "id\n",
        ] {
            assert!(
                matches!(validate_id(bad), Err(MemoryError::InvalidId(_))),
                "validate_id must reject {bad:?}"
            );
        }
        for good in ["01HZY3K9QJXG7Q", "a", "a-b-c"] {
            assert!(
                validate_id(good).is_ok(),
                "validate_id must accept {good:?}"
            );
        }
    }

    #[test]
    fn default_config_values() {
        let c = MemoryStoreConfig::default();
        assert_eq!(c.dedup_threshold, DEFAULT_DEDUP_THRESHOLD);
        assert_eq!(c.search_limit, 10);
        assert_eq!(c.chunk_max_chars, 800);
        assert_eq!(c.chunk_overlap_chars, 100);
        assert_eq!(c.search_overfetch_factor, 5);
        assert_eq!(c.min_similarity, 0.3);
        assert_eq!(c.index_quantization, "f16");
    }
}
