use crate::{
    EmbeddingEngine, VectorError, VectorIndex,
    chunks::{Chunk, ChunkStore, chunk_text},
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::UNIX_EPOCH;
use tracing::{info, warn};
use turbovault_parser::to_plain_text;

fn sha256_hex(data: &[u8]) -> String {
    let hash = Sha256::digest(data);
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// Builds and updates the HNSW index from vault content.
pub struct IndexBuilder {
    embedder: Arc<dyn EmbeddingEngine>,
    chunks: Arc<ChunkStore>,
    chunk_max_chars: usize,
    chunk_overlap_chars: usize,
}

#[derive(Debug, Default)]
pub struct RebuildStats {
    pub notes_indexed: usize,
    pub chunks_created: usize,
    pub elapsed_ms: u64,
    pub model: String,
}

impl IndexBuilder {
    pub fn new(
        embedder: Arc<dyn EmbeddingEngine>,
        chunks: Arc<ChunkStore>,
        chunk_max_chars: usize,
        chunk_overlap_chars: usize,
    ) -> Self {
        Self {
            embedder,
            chunks,
            chunk_max_chars,
            chunk_overlap_chars,
        }
    }

    pub async fn full_rebuild(
        &self,
        vault_root: &Path,
        index: &mut VectorIndex,
    ) -> Result<RebuildStats, VectorError> {
        let start = std::time::Instant::now();

        self.chunks.clear_all()?;

        let md_files: Vec<std::path::PathBuf> = walkdir::WalkDir::new(vault_root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension() == Some("md".as_ref()))
            .map(|e| e.path().to_path_buf())
            .collect();

        let total = md_files.len();
        self.chunks
            .set_model_meta(self.embedder.model_name(), self.embedder.dimensions())?;

        let mut notes_indexed = 0usize;
        let mut chunks_created = 0usize;

        // (rel_path, mtime, file_hash, chunks)
        let mut pending: Vec<(String, i64, String, Vec<Chunk>)> = Vec::new();
        let mut pending_texts: Vec<String> = Vec::new();

        for file_path in &md_files {
            let raw_content = match tokio::fs::read_to_string(file_path).await {
                Ok(c) => c,
                Err(e) => {
                    warn!("Failed to read {}: {}", file_path.display(), e);
                    continue;
                }
            };

            let mtime = match std::fs::metadata(file_path)
                .and_then(|m| m.modified())
                .and_then(|t| t.duration_since(UNIX_EPOCH).map_err(std::io::Error::other))
            {
                Ok(d) => d.as_millis() as i64,
                Err(e) => {
                    warn!("Failed to get mtime for {}: {}", file_path.display(), e);
                    0
                }
            };

            let plain = to_plain_text(&raw_content);
            let file_hash = sha256_hex(plain.as_bytes());

            let rel_path = file_path
                .strip_prefix(vault_root)
                .unwrap_or(file_path)
                .to_string_lossy()
                .to_string();

            let ranges = chunk_text(&plain, self.chunk_max_chars, self.chunk_overlap_chars);
            if ranges.is_empty() {
                // Empty file: record it so incremental updates work, but no chunks.
                self.chunks.upsert_file(&rel_path, mtime, &file_hash)?;
                notes_indexed += 1;
                continue;
            }

            let first_chunk_id = self.chunks.allocate_chunk_ids(ranges.len() as u64)?;
            let total_chunks = ranges.len() as u32;
            let mut chunk_batch: Vec<Chunk> = Vec::with_capacity(ranges.len());

            for (chunk_id, (chunk_index, (start, end))) in
                (first_chunk_id..).zip(ranges.iter().enumerate())
            {
                let chunk_text_slice = &plain[*start..*end];
                pending_texts.push(chunk_text_slice.to_string());
                chunk_batch.push(Chunk {
                    id: chunk_id,
                    note_path: rel_path.clone(),
                    chunk_index: chunk_index as u32,
                    total_chunks,
                    start_byte: *start as u64,
                    end_byte: *end as u64,
                    content_hash: sha256_hex(chunk_text_slice.as_bytes()),
                    preview: chunk_text_slice.chars().take(120).collect(),
                });
            }

            pending.push((rel_path, mtime, file_hash, chunk_batch));

            if pending_texts.len() >= EMBED_BATCH_SIZE {
                flush_embed_batch(
                    self.embedder.as_ref(),
                    &self.chunks,
                    index,
                    &mut pending,
                    &mut pending_texts,
                    &mut chunks_created,
                    &mut notes_indexed,
                )
                .await?;

                if notes_indexed > 0 && notes_indexed.is_multiple_of(100) {
                    info!("Indexed {}/{} files", notes_indexed, total);
                }
            }
        }

        // Flush any remaining files.
        flush_embed_batch(
            self.embedder.as_ref(),
            &self.chunks,
            index,
            &mut pending,
            &mut pending_texts,
            &mut chunks_created,
            &mut notes_indexed,
        )
        .await?;

        index.flush()?;

        let elapsed_ms = start.elapsed().as_millis() as u64;

        Ok(RebuildStats {
            notes_indexed,
            chunks_created,
            elapsed_ms,
            model: self.embedder.model_name().to_string(),
        })
    }

    /// Incrementally index a note from its content (create/update). Only chunks
    /// whose content changed are re-embedded; unchanged chunk vectors are reused.
    /// `mtime` is stored for change-detection bookkeeping (pass `0` if unknown).
    ///
    /// This is the content-fed path for callers that already hold the note text
    /// (e.g. a plugin reading through the host API). [`update_file`] is a
    /// filesystem convenience wrapper over it.
    pub async fn update_note(
        &self,
        rel_path: &str,
        content: &str,
        mtime: i64,
        index: &mut VectorIndex,
    ) -> Result<(), VectorError> {
        // Bind to an owned String so the shared body below reads identically to
        // the filesystem path it was extracted from.
        let rel_path = rel_path.to_string();
        let plain = to_plain_text(content);
        let file_hash = sha256_hex(plain.as_bytes());

        // Fast path: content unchanged → just update mtime.
        if let Some((stored_mtime, stored_hash)) = self.chunks.get_file_info(&rel_path)?
            && stored_hash == file_hash
        {
            if stored_mtime != mtime {
                self.chunks.upsert_file(&rel_path, mtime, &file_hash)?;
            }
            return Ok(());
        }

        let ranges = chunk_text(&plain, self.chunk_max_chars, self.chunk_overlap_chars);

        // Empty file: remove all stale chunks.
        if ranges.is_empty() {
            let old_ids = self.chunks.delete_chunks_for_file(&rel_path)?;
            let had_chunks = !old_ids.is_empty();
            for id in old_ids {
                if let Err(e) = index.remove(id) {
                    warn!("Failed to remove vector for chunk {}: {}", id, e);
                }
            }
            self.chunks.upsert_file(&rel_path, mtime, &file_hash)?;
            if had_chunks {
                index.flush()?;
            }
            return Ok(());
        }

        let chunk_texts: Vec<&str> = ranges.iter().map(|(s, e)| &plain[*s..*e]).collect();
        let new_hashes: Vec<String> = chunk_texts
            .iter()
            .map(|t| sha256_hex(t.as_bytes()))
            .collect();

        let old_hashes = self.chunks.get_chunk_hashes_for_file(&rel_path)?;
        let (reuse_ids, ids_to_delete) = diff_chunks(&old_hashes, &new_hashes);

        // Remove stale vectors from HNSW before the DB transaction.
        for &id in &ids_to_delete {
            if let Err(e) = index.remove(id) {
                warn!("Failed to remove vector for chunk {}: {}", id, e);
            }
        }

        // Embed only chunks with no matching old vector.
        let embed_positions: Vec<usize> = reuse_ids
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.is_none().then_some(i))
            .collect();

        let new_embeddings = if embed_positions.is_empty() {
            vec![]
        } else {
            let texts: Vec<&str> = embed_positions.iter().map(|&i| chunk_texts[i]).collect();
            self.embedder.embed(&texts).await?
        };

        let new_count = embed_positions.len();
        let mut next_id = if new_count > 0 {
            self.chunks.allocate_chunk_ids(new_count as u64)?
        } else {
            0
        };

        // Build the full chunk list.
        let total_chunks = ranges.len() as u32;
        let mut vector_changed = !ids_to_delete.is_empty();
        let mut chunk_batch: Vec<Chunk> = Vec::with_capacity(ranges.len());

        for (i, ((start, end), (hash, reuse_id))) in ranges
            .iter()
            .zip(new_hashes.iter().zip(reuse_ids.iter()))
            .enumerate()
        {
            let chunk_id = match reuse_id {
                Some(id) => *id,
                None => {
                    let id = next_id;
                    next_id += 1;
                    vector_changed = true;
                    id
                }
            };
            chunk_batch.push(Chunk {
                id: chunk_id,
                note_path: rel_path.clone(),
                chunk_index: i as u32,
                total_chunks,
                start_byte: *start as u64,
                end_byte: *end as u64,
                content_hash: hash.clone(),
                preview: chunk_texts[i].chars().take(120).collect(),
            });
        }

        self.chunks.update_file_chunks_tx(
            &rel_path,
            mtime,
            &file_hash,
            &ids_to_delete,
            &chunk_batch,
        )?;

        // HNSW upserts for new chunks (outside the DB transaction).
        let mut embed_idx = 0;
        for (chunk, reuse_id) in chunk_batch.iter().zip(reuse_ids.iter()) {
            if reuse_id.is_none() {
                index.upsert(chunk.id, &new_embeddings[embed_idx])?;
                embed_idx += 1;
            }
        }

        if vector_changed {
            index.flush()?;
        }
        Ok(())
    }

    /// Remove a note's chunks from the index (deletion).
    pub fn remove_note(&self, rel_path: &str, index: &mut VectorIndex) -> Result<(), VectorError> {
        let old_ids = self.chunks.delete_chunks_for_file(rel_path)?;
        let had_chunks = !old_ids.is_empty();
        for id in old_ids {
            if let Err(e) = index.remove(id) {
                warn!("Failed to remove vector for chunk {}: {}", id, e);
            }
        }
        if had_chunks {
            index.flush()?;
        }
        Ok(())
    }

    /// Filesystem convenience wrapper over [`update_note`] / [`remove_note`]:
    /// reads `file_path` (deletion if it no longer exists) and indexes it under
    /// its path relative to `vault_root`.
    pub async fn update_file(
        &self,
        file_path: &Path,
        vault_root: &Path,
        index: &mut VectorIndex,
    ) -> Result<(), VectorError> {
        let rel_path = file_path
            .strip_prefix(vault_root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        let mtime = match std::fs::metadata(file_path) {
            Ok(meta) => meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return self.remove_note(&rel_path, index);
            }
            Err(e) => return Err(VectorError::Io(e)),
        };

        let raw_content = tokio::fs::read_to_string(file_path).await?;
        self.update_note(&rel_path, &raw_content, mtime, index)
            .await
    }
}

/// Number of chunk texts to accumulate across files before calling the embedder.
/// Amortises `spawn_blocking` and model-lock overhead across file boundaries so
/// that the ONNX batch dimension is well-saturated even for small per-file chunk counts.
const EMBED_BATCH_SIZE: usize = 64;

/// Embed all pending chunk texts, store them in the DB and HNSW index, then clear the buffers.
/// On embed failure, all files in the batch are skipped with a warning.
async fn flush_embed_batch(
    embedder: &dyn EmbeddingEngine,
    chunks: &ChunkStore,
    index: &mut VectorIndex,
    pending: &mut Vec<(String, i64, String, Vec<Chunk>)>,
    pending_texts: &mut Vec<String>,
    chunks_created: &mut usize,
    notes_indexed: &mut usize,
) -> Result<(), VectorError> {
    if pending.is_empty() {
        return Ok(());
    }

    let refs: Vec<&str> = pending_texts.iter().map(|s| s.as_str()).collect();
    let embeddings = match embedder.embed(&refs).await {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to embed batch of {} files: {}", pending.len(), e);
            *notes_indexed += pending.len();
            pending.clear();
            pending_texts.clear();
            return Ok(());
        }
    };

    let mut emb_idx = 0;
    for (rel_path, mtime, file_hash, chunk_batch) in pending.drain(..) {
        let n = chunk_batch.len();
        if let Err(e) = chunks.insert_chunks_tx(&rel_path, mtime, &file_hash, &chunk_batch) {
            warn!("Failed to store chunks for {}: {}", rel_path, e);
            emb_idx += n;
            *notes_indexed += 1;
            continue;
        }
        for (chunk, vector) in chunk_batch
            .iter()
            .zip(embeddings[emb_idx..emb_idx + n].iter())
        {
            index.upsert(chunk.id, vector)?;
        }
        *chunks_created += n;
        *notes_indexed += 1;
        emb_idx += n;
    }

    pending_texts.clear();
    Ok(())
}

/// Match new chunk hashes against stored chunks by content hash.
///
/// Returns `(reuse_ids, ids_to_delete)` where:
/// - `reuse_ids[i] = Some(id)` — new chunk i can reuse an existing vector; no re-embedding needed.
/// - `reuse_ids[i] = None`    — new chunk i is new or changed; must be embedded.
/// - `ids_to_delete`          — old chunk IDs not matched by any new chunk; stale, remove from index.
///
/// Duplicate content (same hash appearing multiple times) is handled via a per-hash queue:
/// each old ID is consumed at most once, preserving as many vectors as possible.
fn diff_chunks(
    old_hashes: &[(u64, String)],
    new_hashes: &[String],
) -> (Vec<Option<u64>>, Vec<u64>) {
    let mut old_by_hash: HashMap<&str, VecDeque<u64>> = HashMap::new();
    for (id, hash) in old_hashes {
        old_by_hash.entry(hash.as_str()).or_default().push_back(*id);
    }

    let reuse_ids: Vec<Option<u64>> = new_hashes
        .iter()
        .map(|hash| {
            old_by_hash
                .get_mut(hash.as_str())
                .and_then(|q| q.pop_front())
        })
        .collect();

    let ids_to_delete: Vec<u64> = old_by_hash.into_values().flatten().collect();

    (reuse_ids, ids_to_delete)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old(v: &[(u64, &str)]) -> Vec<(u64, String)> {
        v.iter().map(|(id, h)| (*id, h.to_string())).collect()
    }

    fn hashes(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // ── diff_chunks unit tests ────────────────────────────────────────────────

    #[test]
    fn diff_empty_old_and_new() {
        let (reuse, delete) = diff_chunks(&[], &[]);
        assert!(reuse.is_empty());
        assert!(delete.is_empty());
    }

    #[test]
    fn diff_all_new_no_old() {
        let (reuse, delete) = diff_chunks(&[], &hashes(&["a", "b", "c"]));
        assert_eq!(reuse, vec![None, None, None]);
        assert!(delete.is_empty());
    }

    #[test]
    fn diff_all_unchanged() {
        let (reuse, delete) = diff_chunks(
            &old(&[(1, "a"), (2, "b"), (3, "c")]),
            &hashes(&["a", "b", "c"]),
        );
        assert_eq!(reuse, vec![Some(1), Some(2), Some(3)]);
        assert!(delete.is_empty());
    }

    #[test]
    fn diff_one_chunk_changed() {
        // chunk index 1 changed from "b" to "b2"
        let (reuse, mut delete) = diff_chunks(
            &old(&[(1, "a"), (2, "b"), (3, "c")]),
            &hashes(&["a", "b2", "c"]),
        );
        assert_eq!(reuse, vec![Some(1), None, Some(3)]);
        delete.sort_unstable();
        assert_eq!(delete, vec![2]);
    }

    #[test]
    fn diff_paragraph_inserted_middle() {
        // "new" inserted between "a" and "b"; "a" and "b" content is the same
        let (reuse, delete) = diff_chunks(&old(&[(1, "a"), (2, "b")]), &hashes(&["a", "new", "b"]));
        assert_eq!(reuse, vec![Some(1), None, Some(2)]);
        assert!(delete.is_empty());
    }

    #[test]
    fn diff_paragraph_deleted() {
        // middle chunk removed
        let (reuse, mut delete) =
            diff_chunks(&old(&[(1, "a"), (2, "b"), (3, "c")]), &hashes(&["a", "c"]));
        assert_eq!(reuse, vec![Some(1), Some(3)]);
        delete.sort_unstable();
        assert_eq!(delete, vec![2]);
    }

    #[test]
    fn diff_all_deleted() {
        let (reuse, mut delete) = diff_chunks(&old(&[(1, "a"), (2, "b")]), &[]);
        assert!(reuse.is_empty());
        delete.sort_unstable();
        assert_eq!(delete, vec![1, 2]);
    }

    #[test]
    fn diff_duplicate_content_partial_match() {
        // Two identical chunks in old; new has only one → one reused, one deleted.
        let (reuse, mut delete) = diff_chunks(&old(&[(10, "dup"), (11, "dup")]), &hashes(&["dup"]));
        // One of {10, 11} is reused; the other is deleted.
        assert_eq!(reuse.len(), 1);
        assert!(reuse[0].is_some());
        delete.sort_unstable();
        assert_eq!(delete.len(), 1);
        let reused_id = reuse[0].unwrap();
        assert!(!delete.contains(&reused_id));
    }

    #[test]
    fn diff_duplicate_content_full_match() {
        // Two identical chunks in both old and new → both reused, nothing deleted.
        let (reuse, delete) =
            diff_chunks(&old(&[(10, "dup"), (11, "dup")]), &hashes(&["dup", "dup"]));
        assert_eq!(reuse.len(), 2);
        assert!(reuse.iter().all(|r| r.is_some()));
        assert!(delete.is_empty());
    }

    // ── update_file integration tests ────────────────────────────────────────

    #[cfg(feature = "local")]
    mod integration {
        use super::super::*;
        use crate::{ChunkStore, EmbeddingEngine, SearchRouter, VectorIndex};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tempfile::TempDir;
        use tokio::time::Duration;

        /// Embedder that counts how many texts were embedded and returns
        /// unit vectors of the requested dimensionality.
        struct CountingEmbedder {
            count: Arc<AtomicUsize>,
            dims: usize,
        }

        #[async_trait::async_trait]
        impl EmbeddingEngine for CountingEmbedder {
            async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, VectorError> {
                // Use the global count as the vector index so each text gets a
                // unique unit vector, even across multiple embed() calls.
                // Duplicate vectors across calls would cause usearch to silently
                // drop the second add, making index.len() disagree with chunks_created.
                Ok(texts
                    .iter()
                    .map(|_| {
                        let i = self.count.fetch_add(1, Ordering::SeqCst);
                        let mut v = vec![0.0f32; self.dims];
                        v[i % self.dims] = 1.0;
                        v
                    })
                    .collect())
            }
            fn dimensions(&self) -> usize {
                self.dims
            }
            fn model_name(&self) -> &str {
                "counting-test-embedder"
            }
        }

        const DIMS: usize = 8;

        fn setup(db_dir: &TempDir) -> (Arc<ChunkStore>, Arc<CountingEmbedder>, Arc<AtomicUsize>) {
            let chunks = Arc::new(ChunkStore::open(&db_dir.path().join("state.db")).unwrap());
            let count = Arc::new(AtomicUsize::new(0));
            let embedder = Arc::new(CountingEmbedder {
                count: count.clone(),
                dims: DIMS,
            });
            (chunks, embedder, count)
        }

        fn make_builder(embedder: Arc<CountingEmbedder>, chunks: Arc<ChunkStore>) -> IndexBuilder {
            // chunk_max_chars=30 fits each 26-27 char test sentence as exactly one chunk.
            // overlap=0 so adjacent chunks don't share content and won't be invalidated
            // by a neighbour's change.
            IndexBuilder::new(embedder, chunks, 30, 0)
        }

        fn open_index(db_dir: &TempDir) -> VectorIndex {
            VectorIndex::open_or_create(&db_dir.path().join("hnsw.idx"), DIMS, "f32").unwrap()
        }

        /// Write `content` to `path` and wait long enough for the filesystem
        /// mtime to advance (NTFS 100 ns resolution; 50 ms is more than enough).
        async fn write_and_wait(path: &std::path::Path, content: &str) {
            tokio::fs::write(path, content).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // Test content uses short sentences (≤27 chars each) separated by ". " so
        // the chunker (split-on-". ", max_chars=30, overlap=0) produces exactly one
        // chunk per sentence and edits to one sentence don't bleed into neighbours.
        //
        // to_plain_text joins blocks with "\n" (not "\n\n"), so paragraph breaks in
        // the source markdown collapse. Sentence-level splits are stable across that
        // transform and give us predictable chunk boundaries.
        const S1: &str = "Alpha unique text here."; // 23 chars
        const S2: &str = "Beta unique text here."; // 22 chars
        const S3: &str = "Gamma unique text here."; // 23 chars
        const S2_MOD: &str = "Beta changed text here."; // 23 chars — different hash from S2
        const S4: &str = "Delta unique text here."; // 23 chars — new sentence for append test

        fn three_sentences() -> String {
            format!("{S1} {S2} {S3}")
        }
        fn two_sentences() -> String {
            format!("{S1} {S2}")
        }

        #[tokio::test]
        async fn initial_index_embeds_all_chunks() {
            let vault = TempDir::new().unwrap();
            let db_dir = TempDir::new().unwrap();
            let (chunks, embedder, count) = setup(&db_dir);
            let builder = make_builder(embedder, chunks);
            let mut index = open_index(&db_dir);

            let note = vault.path().join("note.md");
            write_and_wait(&note, &three_sentences()).await;

            builder
                .update_file(&note, vault.path(), &mut index)
                .await
                .unwrap();
            assert_eq!(
                count.load(Ordering::SeqCst),
                3,
                "should embed all 3 chunks on first index"
            );
        }

        // ── content-fed API (update_note / remove_note) ──────────────────────
        // The plugin path: no filesystem, content passed directly.

        #[tokio::test]
        async fn update_note_embeds_all_then_reembeds_only_changed_chunk() {
            let db_dir = TempDir::new().unwrap();
            let (chunks, embedder, count) = setup(&db_dir);
            let builder = make_builder(embedder, chunks);
            let mut index = open_index(&db_dir);

            builder
                .update_note("note.md", &three_sentences(), 1, &mut index)
                .await
                .unwrap();
            assert_eq!(
                count.load(Ordering::SeqCst),
                3,
                "first content-fed index embeds all chunks"
            );

            // Change only the middle sentence.
            let changed = format!("{S1} {S2_MOD} {S3}");
            builder
                .update_note("note.md", &changed, 2, &mut index)
                .await
                .unwrap();
            assert_eq!(
                count.load(Ordering::SeqCst),
                4,
                "only the changed chunk is re-embedded"
            );
        }

        #[tokio::test]
        async fn update_note_unchanged_content_skips_embedding() {
            let db_dir = TempDir::new().unwrap();
            let (chunks, embedder, count) = setup(&db_dir);
            let builder = make_builder(embedder, chunks);
            let mut index = open_index(&db_dir);

            builder
                .update_note("note.md", &two_sentences(), 1, &mut index)
                .await
                .unwrap();
            let after_first = count.load(Ordering::SeqCst);

            // Same content, different mtime: the content-hash fast path skips work.
            builder
                .update_note("note.md", &two_sentences(), 2, &mut index)
                .await
                .unwrap();
            assert_eq!(
                count.load(Ordering::SeqCst),
                after_first,
                "unchanged content re-embeds nothing"
            );
        }

        #[tokio::test]
        async fn remove_note_clears_all_chunks() {
            let db_dir = TempDir::new().unwrap();
            let (chunks, embedder, _count) = setup(&db_dir);
            let builder = make_builder(embedder, chunks.clone());
            let mut index = open_index(&db_dir);

            builder
                .update_note("note.md", &three_sentences(), 1, &mut index)
                .await
                .unwrap();
            assert_eq!(
                chunks.get_chunk_hashes_for_file("note.md").unwrap().len(),
                3
            );

            builder.remove_note("note.md", &mut index).unwrap();
            assert!(
                chunks
                    .get_chunk_hashes_for_file("note.md")
                    .unwrap()
                    .is_empty(),
                "remove_note clears all chunks for the note"
            );
        }

        #[tokio::test]
        async fn unchanged_mtime_skips_all_embedding() {
            let vault = TempDir::new().unwrap();
            let db_dir = TempDir::new().unwrap();
            let (chunks, embedder, count) = setup(&db_dir);
            let builder = make_builder(embedder, chunks);
            let mut index = open_index(&db_dir);

            let note = vault.path().join("note.md");
            write_and_wait(&note, &two_sentences()).await;
            builder
                .update_file(&note, vault.path(), &mut index)
                .await
                .unwrap();
            let after_first = count.load(Ordering::SeqCst);

            // Second call with identical mtime → early exit, zero embedding.
            builder
                .update_file(&note, vault.path(), &mut index)
                .await
                .unwrap();
            assert_eq!(
                count.load(Ordering::SeqCst),
                after_first,
                "mtime unchanged: must embed nothing on second call"
            );
        }

        async fn check_incremental_update(
            initial_content: &str,
            updated_content: &str,
            expected_count: usize,
            msg: &str,
        ) {
            let vault = TempDir::new().unwrap();
            let db_dir = TempDir::new().unwrap();
            let (chunks, embedder, count) = setup(&db_dir);
            let builder = make_builder(embedder, chunks);
            let mut index = open_index(&db_dir);

            let note = vault.path().join("note.md");
            write_and_wait(&note, initial_content).await;
            builder
                .update_file(&note, vault.path(), &mut index)
                .await
                .unwrap();
            assert_eq!(count.load(Ordering::SeqCst), 3);

            write_and_wait(&note, updated_content).await;
            builder
                .update_file(&note, vault.path(), &mut index)
                .await
                .unwrap();
            assert_eq!(count.load(Ordering::SeqCst), expected_count, "{msg}");
        }

        #[tokio::test]
        async fn one_sentence_changed_reembeds_only_that_chunk() {
            check_incremental_update(
                &three_sentences(),
                &format!("{S1} {S2_MOD} {S3}"),
                4,
                "only the changed sentence-chunk should be re-embedded",
            )
            .await;
        }

        #[tokio::test]
        async fn appended_sentence_embeds_only_new_chunk() {
            check_incremental_update(
                &three_sentences(),
                &format!("{S1} {S2} {S3} {S4}"),
                4,
                "only the new appended chunk should be embedded",
            )
            .await;
        }

        #[tokio::test]
        async fn deleted_file_removes_chunks_from_index() {
            let vault = TempDir::new().unwrap();
            let db_dir = TempDir::new().unwrap();
            let (chunks, embedder, _count) = setup(&db_dir);
            let builder = make_builder(Arc::clone(&embedder), Arc::clone(&chunks));
            let mut index = open_index(&db_dir);

            let note = vault.path().join("note.md");
            write_and_wait(&note, &two_sentences()).await;
            builder
                .update_file(&note, vault.path(), &mut index)
                .await
                .unwrap();
            assert!(
                !index.is_empty(),
                "index should have entries after initial index"
            );

            tokio::fs::remove_file(&note).await.unwrap();
            builder
                .update_file(&note, vault.path(), &mut index)
                .await
                .unwrap();
            assert_eq!(
                index.len(),
                0,
                "deleted file: all vectors should be removed"
            );

            let stored = chunks.get_chunks_for_file("note.md").unwrap();
            assert!(
                stored.is_empty(),
                "deleted file: chunk records should be removed from DB"
            );
        }

        #[tokio::test]
        async fn full_rewrite_reembeds_all_chunks() {
            let vault = TempDir::new().unwrap();
            let db_dir = TempDir::new().unwrap();
            let (chunks, embedder, count) = setup(&db_dir);
            let builder = make_builder(embedder, chunks);
            let mut index = open_index(&db_dir);

            let note = vault.path().join("note.md");
            write_and_wait(&note, &two_sentences()).await;
            builder
                .update_file(&note, vault.path(), &mut index)
                .await
                .unwrap();
            let initial = count.load(Ordering::SeqCst);
            assert!(initial > 0);

            // Completely different content — no hash should match.
            write_and_wait(&note, "Rewritten first thing. Rewritten second thing.").await;
            builder
                .update_file(&note, vault.path(), &mut index)
                .await
                .unwrap();
            let after = count.load(Ordering::SeqCst);
            assert_eq!(
                after - initial,
                initial,
                "full rewrite: must re-embed every chunk (none reused)"
            );
        }

        #[tokio::test]
        async fn full_rebuild_indexes_all_files() {
            let vault = TempDir::new().unwrap();
            let db_dir = TempDir::new().unwrap();
            let (chunks, embedder, count) = setup(&db_dir);
            let builder = make_builder(embedder, chunks.clone());
            let mut index = open_index(&db_dir);

            write_and_wait(&vault.path().join("a.md"), &three_sentences()).await;
            write_and_wait(&vault.path().join("b.md"), &two_sentences()).await;

            let stats = builder
                .full_rebuild(vault.path(), &mut index)
                .await
                .unwrap();

            assert_eq!(stats.notes_indexed, 2);
            assert!(
                stats.chunks_created > 0,
                "expected chunks from at least one file"
            );
            assert_eq!(
                count.load(Ordering::SeqCst),
                stats.chunks_created,
                "embed count must equal chunks_created"
            );
            // Verify DB state: both files present with their chunks.
            let a_chunks = chunks.get_chunks_for_file("a.md").unwrap();
            let b_chunks = chunks.get_chunks_for_file("b.md").unwrap();
            assert!(!a_chunks.is_empty(), "a.md must have chunks in DB");
            assert!(!b_chunks.is_empty(), "b.md must have chunks in DB");
            assert_eq!(
                a_chunks.len() + b_chunks.len(),
                stats.chunks_created,
                "DB chunk count must equal chunks_created"
            );
        }

        #[tokio::test]
        async fn full_rebuild_twice_resets_and_reindexes() {
            let vault = TempDir::new().unwrap();
            let db_dir = TempDir::new().unwrap();
            let (chunks, embedder, count) = setup(&db_dir);
            let builder = make_builder(embedder, chunks);
            let mut index = open_index(&db_dir);

            write_and_wait(&vault.path().join("note.md"), &three_sentences()).await;

            builder
                .full_rebuild(vault.path(), &mut index)
                .await
                .unwrap();
            let after_first = count.load(Ordering::SeqCst);
            assert!(after_first > 0);

            // Second rebuild clears the DB and re-indexes from scratch.
            builder
                .full_rebuild(vault.path(), &mut index)
                .await
                .unwrap();
            assert_eq!(
                count.load(Ordering::SeqCst),
                after_first * 2,
                "second full_rebuild must re-embed all chunks"
            );
        }

        #[tokio::test]
        async fn file_emptied_removes_chunks_from_index() {
            let vault = TempDir::new().unwrap();
            let db_dir = TempDir::new().unwrap();
            let (chunks, embedder, count) = setup(&db_dir);
            let builder = make_builder(embedder, chunks.clone());
            let mut index = open_index(&db_dir);

            let note = vault.path().join("note.md");
            write_and_wait(&note, &two_sentences()).await;
            builder
                .update_file(&note, vault.path(), &mut index)
                .await
                .unwrap();
            let initial_count = count.load(Ordering::SeqCst);
            assert!(initial_count > 0);

            // Overwrite with empty content.
            write_and_wait(&note, "").await;
            builder
                .update_file(&note, vault.path(), &mut index)
                .await
                .unwrap();

            assert_eq!(
                count.load(Ordering::SeqCst),
                initial_count,
                "empty file must not trigger re-embedding"
            );
            assert_eq!(index.len(), 0, "empty file: all vectors must be removed");
            assert!(
                chunks.get_chunks_for_file("note.md").unwrap().is_empty(),
                "empty file: chunk DB records must be removed"
            );
            assert!(
                chunks.get_file_mtime("note.md").unwrap().is_some(),
                "empty file: file record must be preserved for future tracking"
            );
        }

        #[tokio::test]
        async fn mtime_changed_content_same_updates_mtime_without_reembedding() {
            let vault = TempDir::new().unwrap();
            let db_dir = TempDir::new().unwrap();
            let (chunks, embedder, count) = setup(&db_dir);
            let builder = make_builder(embedder, chunks.clone());
            let mut index = open_index(&db_dir);

            let note = vault.path().join("note.md");
            write_and_wait(&note, &two_sentences()).await;
            builder
                .update_file(&note, vault.path(), &mut index)
                .await
                .unwrap();
            let embed_count = count.load(Ordering::SeqCst);
            let old_mtime = chunks.get_file_mtime("note.md").unwrap().unwrap();

            // Rewrite identical content — mtime advances, hash stays the same.
            write_and_wait(&note, &two_sentences()).await;
            builder
                .update_file(&note, vault.path(), &mut index)
                .await
                .unwrap();

            assert_eq!(
                count.load(Ordering::SeqCst),
                embed_count,
                "same content: must not re-embed on mtime-only change"
            );
            let new_mtime = chunks.get_file_mtime("note.md").unwrap().unwrap();
            assert!(new_mtime > old_mtime, "mtime must be updated in the DB");
        }

        // ── full_rebuild batch-boundary test ─────────────────────────────────

        #[tokio::test]
        async fn full_rebuild_spans_multiple_embed_batches() {
            // 22 files × 3 chunks each = 66 chunks, which exceeds EMBED_BATCH_SIZE
            // (64). This forces the mid-loop flush path inside full_rebuild, verifying
            // that cross-batch ID allocation and chunk storage both work correctly.
            let vault = TempDir::new().unwrap();
            let db_dir = TempDir::new().unwrap();
            let (chunks, embedder, count) = setup(&db_dir);
            let builder = make_builder(Arc::clone(&embedder), Arc::clone(&chunks));
            let mut index = open_index(&db_dir);

            const N_FILES: usize = 22;
            for i in 0..N_FILES {
                tokio::fs::write(vault.path().join(format!("note{i}.md")), three_sentences())
                    .await
                    .unwrap();
            }

            let stats = builder
                .full_rebuild(vault.path(), &mut index)
                .await
                .unwrap();

            assert_eq!(stats.notes_indexed, N_FILES);
            assert_eq!(stats.chunks_created, N_FILES * 3);
            assert_eq!(
                count.load(Ordering::SeqCst),
                N_FILES * 3,
                "embed count must equal total chunks across all batches"
            );
            for i in 0..N_FILES {
                let file_chunks = chunks.get_chunks_for_file(&format!("note{i}.md")).unwrap();
                assert_eq!(file_chunks.len(), 3, "note{i}.md must have 3 chunks in DB");
            }
        }

        // ── SearchRouter: min_similarity and overfetch_factor ─────────────────

        #[tokio::test]
        async fn min_similarity_filters_below_threshold() {
            let vault = TempDir::new().unwrap();
            let db_dir = TempDir::new().unwrap();
            let (chunks, embedder, _count) = setup(&db_dir);
            let builder = make_builder(Arc::clone(&embedder), Arc::clone(&chunks));
            let index = Arc::new(tokio::sync::RwLock::new(open_index(&db_dir)));

            let note = vault.path().join("note.md");
            write_and_wait(&note, &two_sentences()).await;
            builder
                .update_file(&note, vault.path(), &mut *index.write().await)
                .await
                .unwrap();

            // CountingEmbedder emits orthogonal unit vectors for each embed() call.
            // After indexing 2 chunks (positions 0,1), the query gets position 2 —
            // orthogonal to all stored vectors → cosine similarity = 0 → score = 0.0.
            let make_router = |min_sim: f32| {
                SearchRouter::new(
                    Arc::clone(&index),
                    Arc::clone(&embedder) as Arc<dyn EmbeddingEngine>,
                    Arc::clone(&chunks),
                    60.0,
                    0.3,
                    5,
                    min_sim,
                )
            };

            // min_similarity = 0.0: score 0.0 ≥ 0.0 → results pass through.
            let results_zero = make_router(0.0).vector_only("query", 5).await.unwrap();
            assert!(
                !results_zero.is_empty(),
                "min_similarity=0.0 must not filter results with score=0.0"
            );

            // min_similarity = 0.1: score 0.0 < 0.1 → all results filtered out.
            let results_filtered = make_router(0.1).vector_only("query", 5).await.unwrap();
            assert!(
                results_filtered.is_empty(),
                "min_similarity=0.1 must filter out all results with score=0.0"
            );
        }

        #[tokio::test]
        async fn overfetch_factor_result_count_does_not_exceed_limit() {
            // Verifies that result count is bounded by `limit` regardless of
            // overfetch_factor and that the setting doesn't cause errors.
            let vault = TempDir::new().unwrap();
            let db_dir = TempDir::new().unwrap();
            let (chunks, embedder, _count) = setup(&db_dir);
            let builder = make_builder(Arc::clone(&embedder), Arc::clone(&chunks));
            let index = Arc::new(tokio::sync::RwLock::new(open_index(&db_dir)));

            for i in 0..5u32 {
                let note = vault.path().join(format!("note{i}.md"));
                tokio::fs::write(&note, format!("Unique content sentence {i}."))
                    .await
                    .unwrap();
                builder
                    .update_file(&note, vault.path(), &mut *index.write().await)
                    .await
                    .unwrap();
            }

            let router = SearchRouter::new(
                index,
                embedder as Arc<dyn EmbeddingEngine>,
                chunks,
                60.0,
                0.3,
                10, // large overfetch — should not cause a panic or extra results
                0.0,
            );

            let results = router.vector_only("query", 3).await.unwrap();
            assert!(
                results.len() <= 3,
                "result count must not exceed limit regardless of overfetch_factor"
            );
        }
    }
}
