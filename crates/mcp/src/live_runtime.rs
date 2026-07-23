//! ToolRuntime that re-connects from a live [`McpRegistry`] when the peer set changes.
//!
//! Used by chat so empty→add (and peer toggle) via hot-reload is reflected without process restart.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use liberado_common::WriteProvenance;
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};

use crate::McpRegistry;
use crate::multi::MultiMcpRuntime;

struct Cached {
    names: Vec<String>,
    runtime: Arc<dyn ToolRuntime>,
}

/// Re-acquires tools from [`McpRegistry`] when registered names change.
pub struct LiveRegistryRuntime {
    registry: McpRegistry,
    provenance: WriteProvenance,
    cache: Mutex<Cached>,
}

impl LiveRegistryRuntime {
    pub fn new(registry: McpRegistry, provenance: WriteProvenance) -> Self {
        Self {
            registry,
            provenance,
            cache: Mutex::new(Cached {
                names: Vec::new(),
                runtime: Arc::new(MultiMcpRuntime::new(Vec::new())),
            }),
        }
    }

    fn sorted_names(registry: &McpRegistry) -> Vec<String> {
        let mut n = registry.names();
        n.sort();
        n
    }

    fn refresh_sync(&self) -> Arc<dyn ToolRuntime> {
        let current = Self::sorted_names(&self.registry);
        let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if guard.names == current {
            return guard.runtime.clone();
        }
        let registry = self.registry.clone();
        let provenance = self.provenance.clone();
        let (rt, failed) = match tokio::runtime::Handle::try_current() {
            Ok(handle) => tokio::task::block_in_place(|| {
                handle.block_on(async { registry.connect_all_best_effort(provenance).await })
            }),
            Err(_) => {
                // No runtime (sync unit test) — leave empty multi-runtime.
                (
                    Box::new(MultiMcpRuntime::new(Vec::new())) as Box<dyn ToolRuntime>,
                    Vec::new(),
                )
            }
        };
        if !failed.is_empty() {
            tracing::warn!(?failed, "LiveRegistryRuntime: some MCPs failed to connect on refresh");
        }
        let rt: Arc<dyn ToolRuntime> = Arc::from(rt);
        guard.names = current;
        guard.runtime = rt.clone();
        rt
    }
}

#[async_trait]
impl ToolRuntime for LiveRegistryRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.refresh_sync().catalog()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        // Ensure peers match live registry before invoke.
        let rt = self.refresh_sync();
        rt.invoke(call).await
    }
}
