//! `vector` — semantic search vertical for TurboVault, as a compiled-in plugin
//! module (prototype).
//!
//! Wraps the embedded `turbovault-vector` engine (fastembed + usearch + SQLite)
//! behind the plugin boundary: notes are read through [`VaultApi`], the index
//! persists in a per-vault directory under the process state dir, and the index
//! is kept current by comparing each note's version token from
//! [`VaultApi::read_note`]. Local tool names are advertised namespaced as `vector_*`.
//!
//! Prototype scope: reconcile-on-demand (before search / on reindex) rather than
//! a real-time hook-subscription task; single active vault; default config.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::{Mutex, OnceCell, RwLock};
use turbomcp_types::ToolInputSchema;
use turbovault_plugin_api::{
    Plugin, PluginContext, PluginDescriptor, PluginError, PluginProvider, PluginRequestContext,
    PluginResult, Tool, ToolResult, VaultApi,
};
use turbovault_vector::{
    ChunkStore, EmbeddingEngine, FastembedEngine, FastembedReranker, IndexBuilder, Reranker,
    SearchRouter, VectorConfig, VectorError, VectorIndex,
};

/// Compiled-in factory for the `vector` module.
pub struct VectorPlugin;

impl Plugin for VectorPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: "vector".to_string(),
            name: "Vector".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            description: "Semantic vector search over the active vault".to_string(),
        }
    }

    fn build(&self, context: PluginContext) -> PluginResult<Arc<dyn PluginProvider>> {
        Ok(Arc::new(VectorProvider {
            vault: context.vault,
            engine: OnceCell::new(),
            seen: Mutex::new(HashMap::new()),
        }))
    }
}

/// Lazily-constructed engine handles (opening the index + loading the embedder
/// is heavy and async, so it cannot happen in synchronous `Plugin::build`).
struct Engine {
    index: Arc<RwLock<VectorIndex>>,
    router: SearchRouter,
    builder: Arc<IndexBuilder>,
    config: VectorConfig,
    dims: usize,
}

pub struct VectorProvider {
    vault: VaultApi,
    engine: OnceCell<Engine>,
    /// Last-seen version token per note path — the module's own reconcile cursor.
    seen: Mutex<HashMap<String, String>>,
}

fn sanitize_path_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "vault".to_string()
    } else {
        sanitized
    }
}

/// Per-vault index directory. The host API does not expose a vault filesystem
/// path, so the index lives under `$XDG_STATE_HOME/turbovault/plugins/vector/<vault>`
/// (or `~/.local/state/...` when `XDG_STATE_HOME` is unset).
fn vector_state_dir(vault_name: &str) -> PathBuf {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => match std::env::var_os("HOME") {
            Some(home) if !home.is_empty() => PathBuf::from(home).join(".local").join("state"),
            _ => std::env::temp_dir(),
        },
    };
    base.join("turbovault")
        .join("plugins")
        .join("vector")
        .join(sanitize_path_component(vault_name))
}

fn verr(error: VectorError) -> PluginError {
    PluginError::internal(error.to_string())
}

impl VectorProvider {
    async fn state_dir(&self) -> PluginResult<PathBuf> {
        let vault = self.vault.active_vault().await?;
        let state_dir = vector_state_dir(&vault.name);
        std::fs::create_dir_all(&state_dir).map_err(|error| {
            PluginError::internal(format!(
                "failed to create vector state dir {}: {error}",
                state_dir.display()
            ))
        })?;
        Ok(state_dir)
    }

    async fn engine(&self) -> PluginResult<&Engine> {
        self.engine
            .get_or_try_init(|| async {
                let state_dir = self.state_dir().await?;
                let config = VectorConfig::default();
                let cache = config.model_cache_dir.clone();

                // prototype: fastembed model load is synchronous/blocking here.
                let embedder: Arc<dyn EmbeddingEngine> =
                    Arc::new(FastembedEngine::new(&config.model, cache.clone()).map_err(verr)?);
                let dims = embedder.dimensions();
                let chunks = Arc::new(ChunkStore::open(&state_dir.join("state.db")).map_err(verr)?);
                let index = VectorIndex::open_or_create(
                    &state_dir.join("hnsw.idx"),
                    dims,
                    &config.index_quantization,
                )
                .map_err(verr)?;
                let index = Arc::new(RwLock::new(index));

                let mut router = SearchRouter::new(
                    index.clone(),
                    embedder.clone(),
                    chunks.clone(),
                    config.rrf_k,
                    config.bm25_weight,
                    config.search_overfetch_factor,
                    config.min_similarity,
                );
                if config.rerank_enabled {
                    let reranker: Arc<dyn Reranker> = Arc::new(
                        FastembedReranker::new(&config.rerank_model, cache).map_err(verr)?,
                    );
                    router = router.with_reranker(reranker);
                }
                let builder = Arc::new(IndexBuilder::new(
                    embedder.clone(),
                    chunks.clone(),
                    config.chunk_max_chars,
                    config.chunk_overlap_chars,
                ));

                Ok::<Engine, PluginError>(Engine {
                    index,
                    router,
                    builder,
                    config,
                    dims,
                })
            })
            .await
    }

    /// Pull-based reconcile: re-embed notes whose version token moved since we
    /// last saw them, and drop notes that vanished. `update_note` skips unchanged
    /// content at chunk granularity, so this only re-embeds what actually changed.
    /// The host API has no mtime, so the stored file mtime is `0`.
    async fn reconcile(&self) -> PluginResult<usize> {
        let engine = self.engine().await?;
        let current_paths: Vec<String> = self
            .vault
            .list_notes()
            .await?
            .into_iter()
            .filter(|path| path.ends_with(".md"))
            .collect();
        let current_set: HashSet<String> = current_paths.iter().cloned().collect();

        let mut seen = self.seen.lock().await;
        let mut idx = engine.index.write().await;
        let mut changed = 0usize;

        for path in current_paths {
            let Ok(snapshot) = self.vault.read_note(&path).await else {
                continue;
            };
            if seen.get(&path) == Some(&snapshot.version) {
                continue;
            }
            engine
                .builder
                .update_note(&path, &snapshot.content, 0, &mut idx)
                .await
                .map_err(verr)?;
            seen.insert(path, snapshot.version);
            changed += 1;
        }

        let gone: Vec<String> = seen
            .keys()
            .filter(|path| !current_set.contains(*path))
            .cloned()
            .collect();
        for path in gone {
            engine.builder.remove_note(&path, &mut idx).map_err(verr)?;
            seen.remove(&path);
            changed += 1;
        }

        Ok(changed)
    }

    async fn search(&self, args: &Value) -> PluginResult<ToolResult> {
        let query = required_str(args, "query")?;
        let k = args.get("k").and_then(Value::as_u64).unwrap_or(10).max(1) as usize;
        self.reconcile().await?;
        let engine = self.engine().await?;
        let results = engine.router.vector_only(query, k).await.map_err(verr)?;
        ok_json(json!({
            "query": query,
            "count": results.len(),
            "results": results,
        }))
    }

    async fn reindex(&self) -> PluginResult<ToolResult> {
        // Force a full re-scan: clear the cursor so every note is re-read.
        self.seen.lock().await.clear();
        let changed = self.reconcile().await?;
        ok_json(json!({ "reindexed_notes": changed }))
    }

    async fn status(&self) -> PluginResult<ToolResult> {
        let engine = self.engine().await?;
        let indexed_notes = self.seen.lock().await.len();
        ok_json(json!({
            "model": engine.config.model,
            "dims": engine.dims,
            "indexed_notes": indexed_notes,
            "quantization": engine.config.index_quantization,
            "chunk_max_chars": engine.config.chunk_max_chars,
            "chunk_overlap_chars": engine.config.chunk_overlap_chars,
            "rerank_enabled": engine.config.rerank_enabled,
        }))
    }

    fn config_report(&self) -> PluginResult<ToolResult> {
        // Cheap: report resolved defaults without loading the model.
        let config = VectorConfig::default();
        ok_json(json!({
            "model": config.model,
            "chunk_max_chars": config.chunk_max_chars,
            "chunk_overlap_chars": config.chunk_overlap_chars,
            "rrf_k": config.rrf_k,
            "bm25_weight": config.bm25_weight,
            "min_similarity": config.min_similarity,
            "rerank_enabled": config.rerank_enabled,
            "rerank_model": config.rerank_model,
            "source": "default",
        }))
    }
}

#[async_trait]
impl PluginProvider for VectorProvider {
    fn tools(&self) -> Vec<Tool> {
        vec![
            tool(
                "search",
                "Semantic search over the vault; returns the most similar note chunks",
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Natural-language query" },
                        "k": { "type": "integer", "minimum": 1, "description": "Max results (default 10)" }
                    },
                    "required": ["query"]
                }),
            ),
            tool(
                "reindex",
                "Force a full re-scan of the vault into the index (normally incremental on search)",
                json!({ "type": "object", "properties": {} }),
            ),
            tool(
                "status",
                "Report index status: model, dimensions, indexed note count, chunking",
                json!({ "type": "object", "properties": {} }),
            ),
            tool(
                "config",
                "Report the resolved vector configuration and its source",
                json!({ "type": "object", "properties": {} }),
            ),
        ]
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        _context: PluginRequestContext,
    ) -> PluginResult<ToolResult> {
        match name {
            "search" => self.search(&arguments).await,
            "reindex" => self.reindex().await,
            "status" => self.status().await,
            "config" => self.config_report(),
            other => Err(PluginError::not_found(format!("unknown tool {other:?}"))),
        }
    }
}

fn tool(name: &str, description: &str, schema: Value) -> Tool {
    Tool::new(name, description).with_schema(ToolInputSchema::from_value(schema))
}

fn ok_json(value: Value) -> PluginResult<ToolResult> {
    ToolResult::json(&value).map_err(|error| PluginError::internal(error.to_string()))
}

fn required_str<'a>(args: &'a Value, key: &str) -> PluginResult<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| PluginError::invalid_input(format!("{key} is required")))
}
