//! The [`RuntimeFactory`] over a set of MCP servers: select the servers an execution is allowed to
//! see, connect to each, and present them as one [`ToolRuntime`] whose tools are namespaced
//! `<server>:<tool>` (Decision 4 narrowing is "which servers," routing is by namespace).

use std::collections::HashMap;

use async_trait::async_trait;
use liberado_common::{WriteProvenance, mcp_of};
use liberado_executor::ToolRuntime;
use liberado_orchestrator::{RuntimeFactory, RuntimeSetupError};
use liberado_provider::{ToolDef, ToolInvocation};

use crate::connector::McpConnector;

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
    pub fn register(mut self, name: impl Into<String>, connector: impl McpConnector + 'static) -> Self {
        self.connectors.insert(name.into(), Box::new(connector));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.connectors.is_empty()
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
        Ok(Box::new(CompositeToolRuntime { servers }))
    }
}

/// Presents several servers' runtimes as one. Tool names are namespaced `<server>:<tool>` in the
/// catalog; an invoke is routed to the owning server (by `mcp_of`) and the prefix stripped back off
/// before reaching it.
struct CompositeToolRuntime {
    servers: Vec<(String, Box<dyn ToolRuntime>)>,
}

#[async_trait]
impl ToolRuntime for CompositeToolRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.servers
            .iter()
            .flat_map(|(name, runtime)| {
                runtime.catalog().into_iter().map(move |mut tool| {
                    tool.name = format!("{name}:{}", tool.name);
                    tool
                })
            })
            .collect()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        let server = mcp_of(&call.name);
        let Some((name, runtime)) = self.servers.iter().find(|(n, _)| n == server) else {
            return Err(format!(
                "no MCP named '{server}' is in scope for tool '{}'",
                call.name
            ));
        };
        let bare = call
            .name
            .strip_prefix(&format!("{name}:"))
            .unwrap_or(&call.name);
        let inner = ToolInvocation::new(call.id.clone(), bare, call.arguments.clone());
        runtime.invoke(&inner).await
    }
}
