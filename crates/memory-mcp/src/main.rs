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

    #[tool(description = "Delete a specific memory by its ID (general or procedural). Use only when a memory is wrong or outdated.")]
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

    let (config, _provenance) = liberado_config::load_config(None)?;
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
    MemoryServer {
        general: Arc::new(general),
        procedural: Arc::new(procedural),
    }
    .run_stdio()
    .await?;
    Ok(())
}
