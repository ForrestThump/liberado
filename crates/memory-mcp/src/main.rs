//! Liberado memory MCP server.
//!
//! Exposes five tools over two vault-backed stores (general facts/preferences, procedural
//! tool-selection guidance) — see `liberado-memory-store` for the storage implementation. This
//! crate is deliberately thin: turbomcp wiring, env-driven model selection, and argument
//! validation only.
//!
//! Registered in `topology.toml` as a plain `stdio` MCP (workspace-local, not managed — see
//! `crates/chat-search-mcp` for the precedent: managed MCPs are `cargo install --git`'d from an
//! external repo, this crate lives in the workspace and is built locally:
//! `cargo build --bin liberado-memory-mcp`).
//!
//! Replaces the old `liberado-tool-helper-mcp`, which proxied every call over HTTP to an external
//! mem0 service (Python/Node) — this crate has no such dependency, tool names/descriptions carried
//! over for continuity.

use liberado_common::WriteProvenance;
use liberado_memory_store::{MemoryError, MemoryStore, MemoryStoreConfig};
use liberado_vault::Vault;
use std::sync::Arc;
use turbomcp::prelude::*;
use turbovault_vector::{EmbeddingEngine, FastembedEngine, FastembedReranker, Reranker};

#[derive(Clone)]
struct MemoryServer {
    general: Arc<MemoryStore>,
    procedural: Arc<MemoryStore>,
}

impl MemoryServer {
    fn new(general: Arc<MemoryStore>, procedural: Arc<MemoryStore>) -> Self {
        Self {
            general,
            procedural,
        }
    }

    fn provenance() -> WriteProvenance {
        WriteProvenance::agent("liberado-memory-mcp", ulid::Ulid::new().to_string())
    }
}

fn to_mcp_err(e: MemoryError) -> McpError {
    McpError::internal(e.to_string())
}

fn to_json(value: impl serde::Serialize) -> McpResult<String> {
    serde_json::to_string_pretty(&value).map_err(|e| McpError::internal(e.to_string()))
}

#[turbomcp::server(
    name = "liberado-memory-mcp",
    version = "0.1.0",
    description = "Search/save general memory (user facts and preferences) and procedural memory (tool-selection guidance)"
)]
impl MemoryServer {
    #[tool(
        description = "Search general memories: facts, history, preferences, past conversations. Use for personal context about the user or prior session details. Returns matching memories as JSON, most relevant first."
    )]
    async fn search_memory(&self, query: String) -> McpResult<String> {
        let results = self.general.search(&query).await.map_err(to_mcp_err)?;
        to_json(results)
    }

    #[tool(
        description = "Save a general memory: a fact, preference, or event worth remembering for future sessions. Near-duplicate content is deduplicated automatically — you don't need to check first."
    )]
    async fn add_memory(&self, content: String) -> McpResult<String> {
        let id = self
            .general
            .add(&content, vec![], &Self::provenance())
            .await
            .map_err(to_mcp_err)?;
        to_json(serde_json::json!({ "id": id }))
    }

    #[tool(
        description = "Look up prescriptive tool guidance: which tool to use for a given task type, and how to structure the work. Call this when the right tool is not immediately obvious from tool descriptions alone. Returns directives like 'Use X for Y tasks', as JSON, most relevant first."
    )]
    async fn search_tool_guidance(&self, query: String) -> McpResult<String> {
        let results = self.procedural.search(&query).await.map_err(to_mcp_err)?;
        to_json(results)
    }

    #[tool(
        description = "Save prescriptive tool guidance for future reference. Write guidance as a directive: 'Use [tool] for [task]' — not as a log of what happened. Call this after figuring out the right tool for a non-obvious task so future instances skip the discovery step. Near-duplicate guidance is deduplicated automatically."
    )]
    async fn save_tool_guidance(
        &self,
        guidance: String,
        task_type: Option<String>,
        tools_used: Option<Vec<String>>,
        tags: Option<Vec<String>>,
    ) -> McpResult<String> {
        let id = self
            .procedural
            .add_guidance(
                &guidance,
                task_type,
                tools_used,
                tags.unwrap_or_default(),
                &Self::provenance(),
            )
            .await
            .map_err(to_mcp_err)?;
        to_json(serde_json::json!({ "id": id }))
    }

    #[tool(
        description = "Delete a specific memory by its ID (general or procedural). Use only when a memory is wrong or outdated."
    )]
    async fn delete_memory(&self, memory_id: String) -> McpResult<String> {
        let provenance = Self::provenance();
        let deleted = if self
            .general
            .delete(&memory_id, &provenance)
            .await
            .map_err(to_mcp_err)?
        {
            true
        } else {
            self.procedural
                .delete(&memory_id, &provenance)
                .await
                .map_err(to_mcp_err)?
        };
        to_json(serde_json::json!({ "deleted": deleted }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // MUST write to stderr, never stdout — stdout carries the MCP JSON-RPC protocol stream.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // `load_config(None)` is all-defaults (no file read) — resolve the real config dir, or the
    // vault_path check below fires even with a valid topology.toml (this binary once shipped
    // that bug: config was never loaded, so it could never start).
    let config_dir = liberado_config::config_dir();
    let (config, _provenance) = liberado_config::load_config(config_dir.as_deref())?;
    if config.topology.vault_path.as_os_str().is_empty() {
        return Err("topology.vault_path is required (set it in topology.toml)".into());
    }

    let vault = Vault::open("memory", config.topology.vault_path.clone()).await?;

    // Deliberately its own small model, not the vault-wide default — memory notes are short
    // (facts/preferences/guidance directives), so a smaller embedding model is enough and keeps
    // this MCP's process footprint down.
    let model =
        std::env::var("LIBERADO_MEMORY_MODEL").unwrap_or_else(|_| "bge-small-en-v1.5".to_string());
    let embedder: Arc<dyn EmbeddingEngine> = Arc::new(FastembedEngine::new(&model, None)?);

    // Off by default — reranking loads a second ONNX model at startup, opt in explicitly.
    let reranker: Option<Arc<dyn Reranker>> =
        if std::env::var("LIBERADO_MEMORY_RERANK").as_deref() == Ok("1") {
            let rerank_model = std::env::var("LIBERADO_MEMORY_RERANK_MODEL")
                .unwrap_or_else(|_| "bge-reranker-base".to_string());
            Some(Arc::new(FastembedReranker::new(&rerank_model, None)?))
        } else {
            None
        };

    let general = MemoryStore::open(
        vault.clone(),
        "memory/general",
        embedder.clone(),
        reranker.clone(),
        MemoryStoreConfig::default(),
    )
    .await?;
    let procedural = MemoryStore::open(
        vault,
        "memory/procedural",
        embedder,
        reranker,
        MemoryStoreConfig::default(),
    )
    .await?;

    // Cheap at expected memory-note volumes; see MemoryStore::rebuild_all's doc comment.
    general.rebuild_all().await?;
    procedural.rebuild_all().await?;

    tracing::info!("liberado-memory-mcp starting");
    MemoryServer::new(Arc::new(general), Arc::new(procedural))
        .run_stdio()
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_memory_store::MemoryStoreConfig;
    use liberado_vault::Vault;
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use turbomcp::prelude::McpTestClient;
    use turbovault_vector::{Reranker, VectorError};

    // ── Deterministic model-free embedder (same pattern as liberado-memory-store's tests) ──

    const VOCAB: &[&str] = &[
        "alpha", "beta", "gamma", "delta", "sidebar", "tool", "guidance", "user", "prefers",
    ];

    struct StubEmbedder;

    #[async_trait::async_trait]
    impl EmbeddingEngine for StubEmbedder {
        async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, VectorError> {
            Ok(texts.iter().map(|t| embed_one(t)).collect())
        }
        fn dimensions(&self) -> usize {
            VOCAB.len()
        }
        fn model_name(&self) -> &str {
            "stub-tf-embedder"
        }
    }

    /// Term-frequency vector over VOCAB; FNV fallback so non-vocab text is never the zero vector.
    fn embed_one(text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; VOCAB.len()];
        for word in text.split(|c: char| !c.is_alphanumeric()) {
            if word.is_empty() {
                continue;
            }
            if let Some(i) = VOCAB.iter().position(|w| *w == word.to_ascii_lowercase()) {
                v[i] += 1.0;
            }
        }
        if v.iter().all(|x| *x == 0.0) {
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

    /// Wraps [`StubEmbedder`] with an injected failure — exercises the tools' `map_err(to_mcp_err)`
    /// paths, which no happy-path test can reach.
    struct FailingEmbedder;

    #[async_trait::async_trait]
    impl EmbeddingEngine for FailingEmbedder {
        async fn embed(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>, VectorError> {
            Err(VectorError::Embedding("injected test failure".into()))
        }
        fn dimensions(&self) -> usize {
            VOCAB.len()
        }
        fn model_name(&self) -> &str {
            "failing-stub"
        }
    }

    /// Cross-encoder stub: 100.0 if the candidate contains "sidebar", else 0.0.
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

    // ── Store/server helpers ─────────────────────────────────────────────────────

    async fn open_store(dir: &TempDir, subdir: &str) -> MemoryStore {
        let vault = Vault::open("test", dir.path().to_path_buf()).await.unwrap();
        let embedder: Arc<dyn EmbeddingEngine> = Arc::new(StubEmbedder);
        MemoryStore::open(vault, subdir, embedder, None, MemoryStoreConfig::default())
            .await
            .unwrap()
    }

    /// A server over scratch stores; the TempDir must outlive the server.
    async fn test_server(dir: &TempDir) -> MemoryServer {
        let general = Arc::new(open_store(dir, "memory/general").await);
        let procedural = Arc::new(open_store(dir, "memory/procedural").await);
        MemoryServer::new(general, procedural)
    }

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    // ── Tool logic (direct calls) ───────────────────────────────────────────────

    #[tokio::test]
    async fn add_memory_then_search_finds_it() {
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(&dir).await;

        let out = server
            .add_memory("User prefers alpha over beta".into())
            .await
            .unwrap();
        let id = parse(&out)["id"].as_str().unwrap().to_string();
        assert!(!id.is_empty());

        let results = parse(&server.search_memory("alpha".into()).await.unwrap());
        let arr = results.as_array().unwrap();
        assert_eq!(arr.len(), 1, "search must find the saved memory");
        assert!(arr[0].to_string().contains("prefers alpha"));
    }

    #[tokio::test]
    async fn add_memory_deduplicates_near_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(&dir).await;

        let first = parse(
            &server
                .add_memory("User prefers alpha".into())
                .await
                .unwrap(),
        );
        let second = parse(
            &server
                .add_memory("User prefers alpha".into())
                .await
                .unwrap(),
        );
        assert_eq!(
            first["id"], second["id"],
            "identical content must dedup to the same note"
        );

        let results = parse(&server.search_memory("alpha".into()).await.unwrap());
        assert_eq!(results.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_memory_empty_store_returns_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(&dir).await;

        let out = server.search_memory("nothing here".into()).await.unwrap();
        assert_eq!(parse(&out).as_array().unwrap().len(), 0);
    }

    /// A failing embedder surfaces as an MCP *error*, not a panic or a fake result — the
    /// `map_err(to_mcp_err)` arms of the tools are the only way to reach this.
    #[tokio::test]
    async fn embed_failure_is_reported_as_mcp_error() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::open("test", dir.path().to_path_buf()).await.unwrap();
        let embedder: Arc<dyn EmbeddingEngine> = Arc::new(FailingEmbedder);
        let store = MemoryStore::open(
            vault,
            "memory/general",
            embedder,
            None,
            MemoryStoreConfig::default(),
        )
        .await
        .unwrap();
        let server = MemoryServer::new(
            Arc::new(store),
            Arc::new(open_store(&dir, "memory/procedural").await),
        );

        let err = server.search_memory("anything".into()).await.unwrap_err();
        assert!(err.to_string().contains("injected test failure"), "{err:?}");
        let err = server.add_memory("anything".into()).await.unwrap_err();
        assert!(err.to_string().contains("injected test failure"), "{err:?}");
    }

    #[tokio::test]
    async fn save_tool_guidance_with_all_fields_is_searchable() {
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(&dir).await;

        let out = server
            .save_tool_guidance(
                "Use the sidebar tool for navigation".into(),
                Some("navigation".into()),
                Some(vec!["sidebar_tool".into()]),
                Some(vec!["ui".into()]),
            )
            .await
            .unwrap();
        assert!(!parse(&out)["id"].as_str().unwrap().is_empty());

        let results = parse(&server.search_tool_guidance("sidebar".into()).await.unwrap());
        assert_eq!(results.as_array().unwrap().len(), 1);
        assert!(results[0].to_string().contains("Use the sidebar tool"));
    }

    #[tokio::test]
    async fn save_tool_guidance_with_optional_fields_omitted() {
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(&dir).await;

        let out = server
            .save_tool_guidance("Use alpha for beta tasks".into(), None, None, None)
            .await
            .unwrap();
        assert!(!parse(&out)["id"].as_str().unwrap().is_empty());

        let results = parse(&server.search_tool_guidance("alpha".into()).await.unwrap());
        assert_eq!(results.as_array().unwrap().len(), 1);
    }

    /// The `Some(reranker)` branch of the store wiring: search still works, and the reranker
    /// (which scores "sidebar" content 100.0) reorders the sidebar note first.
    #[tokio::test]
    async fn store_with_reranker_ranks_sidebar_content_first() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::open("test", dir.path().to_path_buf()).await.unwrap();
        let embedder: Arc<dyn EmbeddingEngine> = Arc::new(StubEmbedder);
        let reranker: Arc<dyn Reranker> = Arc::new(StubReranker);
        let store = MemoryStore::open(
            vault,
            "memory/general",
            embedder,
            Some(reranker),
            MemoryStoreConfig::default(),
        )
        .await
        .unwrap();
        let server = MemoryServer::new(
            Arc::new(store),
            Arc::new(open_store(&dir, "memory/procedural").await),
        );

        server
            .add_memory("Use the gamma tool for alpha work".into())
            .await
            .unwrap();
        server
            .add_memory("Use the sidebar for alpha tool work".into())
            .await
            .unwrap();

        let results = parse(&server.search_memory("alpha tool".into()).await.unwrap());
        let arr = results.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(
            arr[0].to_string().contains("sidebar"),
            "reranker must promote the sidebar note, got: {arr:?}"
        );
    }

    #[tokio::test]
    async fn delete_memory_removes_from_general() {
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(&dir).await;

        let id = parse(&server.add_memory("alpha fact".into()).await.unwrap())["id"]
            .as_str()
            .unwrap()
            .to_string();
        let out = parse(&server.delete_memory(id.clone()).await.unwrap());
        assert_eq!(out["deleted"], true);

        let results = parse(&server.search_memory("alpha".into()).await.unwrap());
        assert_eq!(results.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn delete_memory_removes_from_procedural() {
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(&dir).await;

        let id = parse(
            &server
                .save_tool_guidance("Use gamma for delta work".into(), None, None, None)
                .await
                .unwrap(),
        )["id"]
            .as_str()
            .unwrap()
            .to_string();
        let out = parse(&server.delete_memory(id).await.unwrap());
        assert_eq!(out["deleted"], true);
    }

    #[tokio::test]
    async fn delete_memory_unknown_id_is_false_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let server = test_server(&dir).await;

        let out = parse(
            &server
                .delete_memory(ulid::Ulid::new().to_string())
                .await
                .unwrap(),
        );
        assert_eq!(out["deleted"], false);
    }

    // ── MCP layer (in-process client) ────────────────────────────────────────────

    #[tokio::test]
    async fn mcp_advertises_all_five_tools() {
        let dir = tempfile::tempdir().unwrap();
        let client = McpTestClient::new(test_server(&dir).await);
        assert_eq!(client.server_info().name, "liberado-memory-mcp");
        for tool in [
            "search_memory",
            "add_memory",
            "search_tool_guidance",
            "save_tool_guidance",
            "delete_memory",
        ] {
            client.assert_tool_exists(tool);
        }
    }

    #[tokio::test]
    async fn mcp_calls_add_and_search_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let client = McpTestClient::new(test_server(&dir).await);

        let added = client
            .call_tool("add_memory", json!({"content": "User prefers alpha"}))
            .await
            .unwrap();
        let added_text = added
            .first_text()
            .map(|t| t.to_string())
            .unwrap_or_default();
        let v: Value = serde_json::from_str(&added_text).unwrap();
        assert!(!v["id"].as_str().unwrap().is_empty(), "added: {added_text}");

        let searched = client
            .call_tool("search_memory", json!({"query": "alpha"}))
            .await
            .unwrap();
        let text = searched
            .first_text()
            .map(|t| t.to_string())
            .unwrap_or_default();
        assert!(text.contains("prefers alpha"), "search result: {text}");
    }
}
