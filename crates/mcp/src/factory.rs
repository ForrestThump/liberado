//! The [`RuntimeFactory`] over a set of MCP servers: select the servers an execution is allowed to
//! see, connect to each, and present them as one [`ToolRuntime`] whose tools are namespaced
//! `<server>:<tool>` (Decision 4 narrowing is "which servers," routing is by namespace).

use std::collections::HashMap;

use async_trait::async_trait;
use liberado_common::WriteProvenance;
use liberado_executor::{RuntimeFactory, RuntimeSetupError, ToolRuntime};

use crate::connector::McpConnector;
use crate::MultiMcpRuntime;

/// A named set of MCP servers. Implements `RuntimeFactory`: each `runtime_for`
/// connects (v1: a fresh connection per execution — no pooling) to the selected servers and unions
/// their tools.
#[derive(Default)]
pub struct McpRegistry {
    connectors: HashMap<String, Box<dyn McpConnector>>,
}

impl McpRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a server under `name` (the namespace its tools are exposed under). Chainable.
    pub fn register(
        mut self,
        name: impl Into<String>,
        connector: impl McpConnector + 'static,
    ) -> Self {
        self.connectors.insert(name.into(), Box::new(connector));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.connectors.is_empty()
    }

    /// The names of the registered MCP connectors (the routable surface). Lets a caller assert the
    /// config→registry mapping without spawning a server.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.connectors.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.connectors.len()
    }

    /// Connect to every registered MCP independently and in parallel, tolerating individual
    /// failures instead of aborting the whole batch — the "best effort" counterpart to
    /// `RuntimeFactory::runtime_for`'s strict, all-required semantics (used by dispatch-routed
    /// execution, where a missing *required* MCP should hard-fail, not silently narrow the
    /// scope). Returns the names that failed alongside the runtime, so the caller can log/report
    /// a summary; each individual failure is also logged at the point of failure.
    pub async fn connect_all_best_effort(
        &self,
        provenance: WriteProvenance,
    ) -> (Box<dyn ToolRuntime>, Vec<String>) {
        let attempts = self.connectors.iter().map(|(name, connector)| {
            let provenance = provenance.clone();
            async move {
                match connector.connect(provenance).await {
                    Ok(runtime) => Ok((name.clone(), runtime)),
                    Err(e) => Err((name.clone(), e)),
                }
            }
        });
        let results = futures::future::join_all(attempts).await;

        let mut servers = Vec::new();
        let mut failed = Vec::new();
        for result in results {
            match result {
                Ok(entry) => servers.push(entry),
                Err((name, e)) => {
                    tracing::warn!(mcp = %name, error = %e, "MCP failed to connect — continuing without it");
                    failed.push(name);
                }
            }
        }
        (Box::new(MultiMcpRuntime::new(servers)), failed)
    }
}

#[async_trait]
impl RuntimeFactory for McpRegistry {
    async fn runtime_for(
        &self,
        allowed_mcps: &[String],
        provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        // Empty allow-list = every registered server; otherwise the named (known) ones. Narrowing
        // selects *which servers* the execution may reach (a subagent's disjoint slice).
        let selected: Vec<&String> = if allowed_mcps.is_empty() {
            self.connectors.keys().collect()
        } else {
            allowed_mcps
                .iter()
                .filter(|name| self.connectors.contains_key(name.as_str()))
                .collect()
        };

        let mut servers = Vec::with_capacity(selected.len());
        for name in selected {
            let connector = self
                .connectors
                .get(name)
                .expect("name came from the registry's own keys");
            let runtime = connector.connect(provenance.clone()).await?;
            servers.push((name.clone(), runtime));
        }
        Ok(Box::new(MultiMcpRuntime::new(servers)))
    }
}
