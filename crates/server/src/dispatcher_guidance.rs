//! Optional in-process dispatcher procedural-memory guidance.
//!
//! Split from `lib.rs` so feature-gated vector-local helpers do not inflate the
//! crate-root module-health metrics.

use std::sync::Arc;

#[cfg(feature = "vector-local")]
use tracing::info;
use tracing::warn;

/// Build the dispatcher's optional procedural-memory guidance source (`liberado-dispatch-logic-spec.md`
/// §2 steps 1/5), opting in only when `LIBERADO_DISPATCHER_GUIDANCE=1` is set **and** this crate
/// was built with `--features vector-local`. Off by default so `liberado serve` does not link
/// turbovault-vector/fastembed; `liberado-memory-mcp` remains the subprocess that carries that
/// weight. Any failure degrades to `None`.
pub(crate) async fn dispatcher_guidance_source(
    vault_path: &str,
) -> Option<Arc<dyn liberado_common::ToolGuidanceSource>> {
    if std::env::var("LIBERADO_DISPATCHER_GUIDANCE").as_deref() != Ok("1") {
        return None;
    }

    guidance_source_when_enabled(vault_path).await
}

#[cfg(not(feature = "vector-local"))]
async fn guidance_source_when_enabled(
    vault_path: &str,
) -> Option<Arc<dyn liberado_common::ToolGuidanceSource>> {
    let _ = vault_path;
    warn!(
        "dispatcher guidance: LIBERADO_DISPATCHER_GUIDANCE=1 but this liberado-server build \
         lacks `--features vector-local` — continuing without in-process guidance \
         (use liberado-memory-mcp, or rebuild with vector-local)"
    );
    None
}

#[cfg(feature = "vector-local")]
async fn guidance_source_when_enabled(
    vault_path: &str,
) -> Option<Arc<dyn liberado_common::ToolGuidanceSource>> {
    let vault = open_guidance_vault(vault_path).await?;
    let embedder = load_guidance_embedder()?;

    match open_procedural_memory(vault, embedder).await {
        Ok(store) => {
            info!("dispatcher guidance: procedural memory enabled");
            Some(Arc::new(store))
        }
        Err(e) => {
            warn!(error = %e, "dispatcher guidance: failed to open procedural memory store — continuing without it");
            None
        }
    }
}

#[cfg(feature = "vector-local")]
async fn open_guidance_vault(vault_path: &str) -> Option<liberado_vault::Vault> {
    match liberado_vault::Vault::open("dispatcher-guidance", vault_path).await {
        Ok(v) => Some(v),
        Err(e) => {
            warn!(error = %e, "dispatcher guidance: failed to open vault — continuing without it");
            None
        }
    }
}

#[cfg(feature = "vector-local")]
fn load_guidance_embedder() -> Option<Arc<dyn turbovault_vector::EmbeddingEngine>> {
    let model =
        std::env::var("LIBERADO_MEMORY_MODEL").unwrap_or_else(|_| "bge-small-en-v1.5".to_string());
    match turbovault_vector::FastembedEngine::new(&model, None) {
        Ok(e) => Some(Arc::new(e)),
        Err(e) => {
            warn!(error = %e, "dispatcher guidance: failed to load embedding model — continuing without it");
            None
        }
    }
}

#[cfg(feature = "vector-local")]
async fn open_procedural_memory(
    vault: liberado_vault::Vault,
    embedder: Arc<dyn turbovault_vector::EmbeddingEngine>,
) -> Result<liberado_memory_store::MemoryStore, liberado_memory_store::MemoryError> {
    liberado_memory_store::MemoryStore::open(
        vault,
        "memory/procedural",
        embedder,
        None,
        liberado_memory_store::MemoryStoreConfig::default(),
    )
    .await
}
