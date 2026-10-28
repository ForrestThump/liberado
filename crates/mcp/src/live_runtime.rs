//! Chat tool surface backed by the MCP reconnect supervisor.
//!
//! `catalog` and `invoke` read the supervisor's current snapshot. They do not connect
//! and they do not sleep. A peer that is down is absent from the catalog. A call for
//! that peer fails immediately.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_common::WriteProvenance;
use liberado_provider::{ToolDef, ToolInvocation};
use liberado_tool_runtime::ToolRuntime;

use crate::McpRegistry;
use crate::factory::{PeerSupervisor, ReconnectPolicy};

/// Live chat tools for one [`McpRegistry`].
///
/// The registry value is kept so the supervisor's weak handle stays valid for the
/// life of this surface. Chat boot moves the caller's only handle in here; status
/// reads the same registry through its own clone.
pub struct LiveRegistryRuntime {
    registry: McpRegistry,
    provenance: WriteProvenance,
    policy: ReconnectPolicy,
}

impl LiveRegistryRuntime {
    /// Start the supervisor with the production backoff schedule.
    pub fn new(registry: McpRegistry, provenance: WriteProvenance) -> Self {
        Self::with_policy(registry, provenance, ReconnectPolicy::default())
    }

    /// Start the supervisor with `policy`. The first start wins for a shared registry.
    pub fn with_policy(
        registry: McpRegistry,
        provenance: WriteProvenance,
        policy: ReconnectPolicy,
    ) -> Self {
        let runtime = Self {
            registry,
            provenance,
            policy,
        };
        let _started = runtime.supervisor();
        runtime
    }

    fn supervisor(&self) -> Arc<PeerSupervisor> {
        self.registry
            .ensure_chat_supervisor(self.provenance.clone(), self.policy.clone())
    }
}

#[async_trait]
impl ToolRuntime for LiveRegistryRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.supervisor().catalog()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        self.supervisor().invoke(call).await
    }
}
