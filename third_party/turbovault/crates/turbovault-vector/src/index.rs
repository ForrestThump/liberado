use crate::error::VectorError;
use crate::require_feature;
use std::path::{Path, PathBuf};

#[cfg(feature = "local")]
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

#[cfg(feature = "local")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum IndexMode {
    /// Fully deserialized into heap memory; supports mutations.
    Ram,
    /// Memory-mapped read-only view; OS page cache handles paging.
    View,
}

/// HNSW vector index backed by usearch.
///
/// Existing index files are opened with `view()` (mmap) by default — benchmarks show
/// identical steady-state query latency vs full `load()` while using negligible extra RAM.
/// The index automatically promotes itself to RAM mode on the first write (upsert/remove)
/// and returns to mmap mode after each `flush()`.
pub struct VectorIndex {
    #[cfg(feature = "local")]
    index: Index,
    #[cfg(feature = "local")]
    mode: IndexMode,
    #[cfg(feature = "local")]
    scalar_kind: ScalarKind,
    path: PathBuf,
    dims: usize,
}

impl VectorIndex {
    pub fn open_or_create(
        path: &Path,
        dims: usize,
        quantization: &str,
    ) -> Result<Self, VectorError> {
        require_feature!(Index, path, dims, quantization);

        #[cfg(feature = "local")]
        {
            let scalar_kind = match quantization {
                "f32" => ScalarKind::F32,
                "i8" => ScalarKind::I8,
                _ => ScalarKind::F16, // "f16" and unknown values default to F16
            };

            let index = Self::new_native_index(dims, scalar_kind)?;

            let mode = if path.exists() {
                let path_str = path.to_string_lossy();
                index
                    .view(&path_str)
                    .map_err(|e| VectorError::Index(e.to_string()))?;
                IndexMode::View
            } else {
                index
                    .reserve(1000)
                    .map_err(|e| VectorError::Index(e.to_string()))?;
                IndexMode::Ram
            };

            Ok(Self {
                index,
                mode,
                scalar_kind,
                path: path.to_path_buf(),
                dims,
            })
        }
    }

    #[cfg(feature = "local")]
    fn new_native_index(dims: usize, scalar_kind: ScalarKind) -> Result<Index, VectorError> {
        let options = IndexOptions {
            dimensions: dims,
            metric: MetricKind::Cos,
            quantization: scalar_kind,
            connectivity: 0,
            expansion_add: 0,
            expansion_search: 0,
            multi: false,
        };
        Index::new(&options).map_err(|e| VectorError::Index(e.to_string()))
    }

    /// Ensures the index is in RAM mode, promoting from mmap if necessary.
    /// Called before any mutation so we only pay the load cost once per write batch.
    #[cfg(feature = "local")]
    fn ensure_ram_mode(&mut self) -> Result<(), VectorError> {
        if self.mode == IndexMode::View {
            let path_str = self.path.to_string_lossy().to_string();
            self.index = Self::new_native_index(self.dims, self.scalar_kind)?;
            self.index
                .load(&path_str)
                .map_err(|e| VectorError::Index(e.to_string()))?;
            self.mode = IndexMode::Ram;
        }
        Ok(())
    }

    pub fn upsert(&mut self, id: u64, vector: &[f32]) -> Result<(), VectorError> {
        require_feature!(Index, id, vector);

        #[cfg(feature = "local")]
        {
            self.ensure_ram_mode()?;

            // Grow the index if needed.
            if self.index.size() + 1 > self.index.capacity() {
                let new_cap = self.index.capacity() * 2 + 100;
                self.index
                    .reserve(new_cap)
                    .map_err(|e| VectorError::Index(e.to_string()))?;
            }

            // Remove the existing entry if present so the add is an update.
            if self.index.contains(id) {
                self.index
                    .remove(id)
                    .map_err(|e| VectorError::Index(e.to_string()))?;
            }

            self.index
                .add(id, vector)
                .map_err(|e| VectorError::Index(e.to_string()))?;

            Ok(())
        }
    }

    pub fn remove(&mut self, id: u64) -> Result<(), VectorError> {
        require_feature!(Index, id);

        #[cfg(feature = "local")]
        {
            self.ensure_ram_mode()?;
            self.index
                .remove(id)
                .map_err(|e| VectorError::Index(e.to_string()))?;
            Ok(())
        }
    }

    pub fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<(u64, f32)>, VectorError> {
        require_feature!(Index, query, top_k);

        #[cfg(feature = "local")]
        {
            if self.index.size() == 0 {
                return Ok(vec![]);
            }
            let matches = self
                .index
                .search(query, top_k)
                .map_err(|e| VectorError::Index(e.to_string()))?;

            let mut results: Vec<(u64, f32)> = matches
                .keys
                .into_iter()
                .zip(matches.distances)
                .map(|(id, dist)| (id, 1.0 - dist))
                .collect();

            // Sort by score descending (highest similarity first).
            results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            Ok(results)
        }
    }

    /// Search the index and return `(chunk_id, score)` pairs sorted by score descending.
    /// Score is `1.0 - cosine_distance` so higher = more similar.
    pub fn flush(&mut self) -> Result<(), VectorError> {
        require_feature!(Index);

        #[cfg(feature = "local")]
        {
            // Write to a .tmp file, then atomically rename.
            let mut tmp_path = self.path.clone();
            let mut tmp_name = self.path.file_name().unwrap_or_default().to_os_string();
            tmp_name.push(".tmp");
            tmp_path.set_file_name(tmp_name);

            let tmp_str = tmp_path.to_string_lossy();
            self.index
                .save(&tmp_str)
                .map_err(|e| VectorError::Index(e.to_string()))?;

            // usearch::Index::save() already flushes its internal buffers to
            // the OS.  The rename itself is atomic on most platforms.  We try
            // to sync the parent directory as a best-effort durability step;
            // on Windows the temp file may still be locked by usearch, making
            // File::open fail with PermissionDenied, so we ignore that error.
            std::fs::rename(&tmp_path, &self.path)?;

            let _ = self
                .path
                .parent()
                .and_then(|p| std::fs::File::open(p).ok())
                .and_then(|f| f.sync_all().ok());

            // Drop heap memory and re-open as mmap view.
            let path_str = self.path.to_string_lossy().to_string();
            self.index = Self::new_native_index(self.dims, self.scalar_kind)?;
            self.index
                .view(&path_str)
                .map_err(|e| VectorError::Index(e.to_string()))?;
            self.mode = IndexMode::View;

            Ok(())
        }
    }

    pub fn len(&self) -> usize {
        #[cfg(feature = "local")]
        {
            self.index.size()
        }
        #[cfg(not(feature = "local"))]
        {
            0
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn dims(&self) -> usize {
        self.dims
    }
}
