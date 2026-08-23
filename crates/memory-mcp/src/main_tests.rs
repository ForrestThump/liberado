//! Split from `main.rs` for module-health boundaries.

use super::*;
use liberado_memory_store::MemoryStoreConfig;
use liberado_vault::Vault;
use serde_json::{Value, json};
use tempfile::TempDir;
use turbomcp::prelude::McpTestClient;
use turbovault_vector::{Reranker, VectorError};

#[test]
fn reranker_is_opt_in_via_the_exact_string_one() {
    assert!(reranker_requested(Ok("1".into())));
    // Anything else — unset, empty, "true", "on", "2" — stays off: the model load is
    // expensive and must never happen by accident.
    for off in ["", "0", "true", "on", "2"] {
        assert!(!reranker_requested(Ok(off.into())), "{off:?} must stay off");
    }
    let _ = std::env::var("LIBERADO_MEMORY_RERANK_DEFINITELY_UNSET_XYZ");
}

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
    async fn rerank(&self, _query: &str, candidates: &[String]) -> Result<Vec<f32>, VectorError> {
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
