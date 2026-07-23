//! The [`RuntimeFactory`] over a set of MCP servers: select the servers an execution is allowed to
//! see, acquire a (possibly pooled) runtime for each, and present them as one [`ToolRuntime`]
//! whose tools are namespaced `<server>:<tool>` (Decision 4 narrowing is "which servers," routing
//! is by namespace).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use liberado_common::{CapabilityCatalog, WriteProvenance};
use liberado_executor::{RuntimeFactory, RuntimeSetupError, ToolRuntime};

use crate::MultiMcpRuntime;
use crate::connector::McpConnector;
use crate::pool::{AsToolRuntime, ConnectionPool, PoolPolicy, PooledCheckout};

/// Pooling knobs passed into [`McpRegistry`] at construction (from `tuning.mcp_pooling`).
#[derive(Debug, Clone)]
pub struct McpPoolSettings {
    /// When `true`, reuse healthy connections across acquisitions.
    pub enabled: bool,
    /// Idle TTL for checked-in connections.
    pub idle_ttl: Duration,
}

impl Default for McpPoolSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_ttl: Duration::from_secs(300),
        }
    }
}

impl From<McpPoolSettings> for PoolPolicy {
    fn from(s: McpPoolSettings) -> Self {
        PoolPolicy {
            enabled: s.enabled,
            idle_ttl: s.idle_ttl,
        }
    }
}

/// A named set of MCP servers. Implements `RuntimeFactory`: each `runtime_for` acquires a runtime
/// for the selected servers (pooled by default) and unions their tools.
pub struct McpRegistry {
    connectors: HashMap<String, Box<dyn McpConnector>>,
    pool: Arc<ConnectionPool>,
    /// Optional shared catalog for M1b peer health (degraded after connect/transport failure).
    health: Option<Arc<CapabilityCatalog>>,
}

impl Default for McpRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl McpRegistry {
    /// Registry with default pooling (**on**, 300s idle TTL).
    pub fn new() -> Self {
        Self::with_pool_settings(McpPoolSettings::default())
    }

    /// Registry with explicit pooling policy (composition root wires config here).
    pub fn with_pool_settings(settings: McpPoolSettings) -> Self {
        Self {
            connectors: HashMap::new(),
            pool: Arc::new(ConnectionPool::new(settings.into())),
            health: None,
        }
    }

    /// Custom pool clock for idle-TTL tests (inject "now" without sleeping).
    /// Not used in production wiring.
    pub fn with_pool_and_clock(
        settings: McpPoolSettings,
        clock: Arc<dyn Fn() -> std::time::Instant + Send + Sync>,
    ) -> Self {
        Self {
            connectors: HashMap::new(),
            pool: Arc::new(ConnectionPool::with_clock(settings.into(), clock)),
            health: None,
        }
    }

    /// Publish connect/transport health into the live [`CapabilityCatalog`] (M1b).
    ///
    /// Composition roots pass the same `Arc` used for dispatcher routing so
    /// `routing_descriptors()` excludes peers this registry marks degraded.
    pub fn with_health_catalog(mut self, catalog: Arc<CapabilityCatalog>) -> Self {
        self.health = Some(catalog);
        self
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

    fn publish_healthy(&self, name: &str) {
        if let Some(cat) = &self.health {
            cat.mark_healthy(name);
        }
    }

    fn publish_degraded(&self, name: &str) {
        if let Some(cat) = &self.health {
            cat.mark_degraded(name);
        }
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

    /// Whether pooling is enabled for this registry.
    pub fn pooling_enabled(&self) -> bool {
        self.pool.policy().enabled
    }

    /// Acquire one server's runtime: pool checkout (with provenance rebind) or fresh connect.
    async fn acquire(
        &self,
        name: &str,
        provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        if let Some(checkout) = self.pool.try_checkout(name, provenance.clone()) {
            // Reusing a pooled peer implies it was healthy when checked in.
            self.publish_healthy(name);
            return Ok(Box::new(checkout.with_health_catalog(self.health.clone())));
        }

        let connector = self
            .connectors
            .get(name)
            .ok_or_else(|| RuntimeSetupError(format!("MCP '{name}' is not registered")))?;

        match connector.connect(provenance.clone()).await {
            Ok(runtime) => {
                self.publish_healthy(name);
                if self.pool.policy().enabled {
                    Ok(Box::new(
                        PooledCheckout::from_fresh(
                            name.to_string(),
                            runtime,
                            provenance,
                            Arc::clone(&self.pool),
                        )
                        .with_health_catalog(self.health.clone()),
                    ))
                } else {
                    Ok(Box::new(AsToolRuntime(runtime)))
                }
            }
            Err(e) => {
                self.pool.invalidate(name);
                self.publish_degraded(name);
                Err(e)
            }
        }
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
        let names: Vec<String> = self.connectors.keys().cloned().collect();
        let attempts = names.into_iter().map(|name| {
            let provenance = provenance.clone();
            async move {
                match self.acquire(&name, provenance).await {
                    Ok(runtime) => Ok((name, runtime)),
                    Err(e) => Err((name, e)),
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
                    // Ensure a bad pool slot is not retained; health already marked degraded in acquire.
                    self.pool.invalidate(&name);
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
        let selected: Vec<String> = if allowed_mcps.is_empty() {
            self.connectors.keys().cloned().collect()
        } else {
            allowed_mcps
                .iter()
                .filter(|name| self.connectors.contains_key(name.as_str()))
                .cloned()
                .collect()
        };

        let mut servers = Vec::with_capacity(selected.len());
        for name in selected {
            let runtime = self.acquire(&name, provenance.clone()).await?;
            servers.push((name, runtime));
        }
        Ok(Box::new(MultiMcpRuntime::new(servers)))
    }
}
