//! The [`RuntimeFactory`] over a set of MCP servers: select the servers an execution is allowed to
//! see, connect to each, and present them as one [`ToolRuntime`] whose tools are namespaced
//! `<server>:<tool>` (Decision 4 narrowing is "which servers," routing is by namespace).

use std::collections::HashMap;

use async_trait::async_trait;
use liberado_common::WriteProvenance;
use liberado_executor::ToolRuntime;
use liberado_orchestrator::{RuntimeFactory, RuntimeSetupError};

use crate::connector::McpConnector;
use crate::MultiMcpRuntime;

/// A named set of MCP servers. Implements the orchestrator's `RuntimeFactory`: each `runtime_for`
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
