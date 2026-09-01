//! The [`RuntimeFactory`] over a set of MCP servers: select the servers an execution is allowed to
//! see, acquire a (possibly pooled) runtime for each, and present them as one [`ToolRuntime`]
//! whose tools are namespaced `<server>:<tool>` (Decision 4 narrowing is "which servers," routing
//! is by namespace).
//!
//! The connector map is interior-mutable and the handle is cheaply [`Clone`] so composition can
//! share one live registry across pools/chat and hot-apply a new desired peer set without restart.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use liberado_common::{CapabilityCatalog, WriteProvenance};
use liberado_executor::{RuntimeFactory, RuntimeSetupError, ToolRuntime};

use crate::MultiMcpRuntime;
use crate::connector::McpConnector;
use crate::pool::{AsToolRuntime, ConnectionPool, PermittedRuntime, PoolPolicy, PooledCheckout};

#[cfg(test)]
#[path = "factory_survivor_tests.rs"]
mod survivor_tests;

/// Pooling knobs passed into [`McpRegistry`] at construction (from `tuning.mcp_pooling`).
#[derive(Debug, Clone)]
pub struct McpPoolSettings {
    /// When `true`, reuse healthy connections across acquisitions.
    pub enabled: bool,
    /// Idle TTL for checked-in connections.
    pub idle_ttl: Duration,
    /// Max simultaneous live checkouts/connects for one MCP name.
    pub max_in_flight_per_name: usize,
    /// How long an acquire waits for a concurrency permit before failing.
    pub connect_wait: Duration,
}

impl Default for McpPoolSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_ttl: Duration::from_secs(300),
            max_in_flight_per_name: 4,
            connect_wait: Duration::from_secs(60),
        }
    }
}

impl From<McpPoolSettings> for PoolPolicy {
    fn from(s: McpPoolSettings) -> Self {
        PoolPolicy {
            enabled: s.enabled,
            idle_ttl: s.idle_ttl,
            max_in_flight_per_name: s.max_in_flight_per_name,
            connect_wait: s.connect_wait,
        }
    }
}

struct McpRegistryInner {
    connectors: RwLock<HashMap<String, Arc<dyn McpConnector>>>,
    pool: Arc<ConnectionPool>,
    /// Optional shared catalog for M1b peer health (degraded after connect/transport failure).
    health: RwLock<Option<Arc<CapabilityCatalog>>>,
}

/// A named set of MCP servers. Implements `RuntimeFactory`: each `runtime_for` acquires a runtime
/// for the selected servers (pooled by default) and unions their tools.
///
/// Cheaply [`Clone`] — clones share the same live connector map and pool so hot-reload and
/// multi-pool composition update one surface.
#[derive(Clone)]
pub struct McpRegistry {
    inner: Arc<McpRegistryInner>,
}

impl Default for McpRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl McpRegistry {
    /// Registry with default pooling (**on**, 300s idle TTL, reaper + concurrency caps).
    pub fn new() -> Self {
        Self::with_pool_settings(McpPoolSettings::default())
    }

    /// Registry with explicit pooling policy (composition root wires config here).
    pub fn with_pool_settings(settings: McpPoolSettings) -> Self {
        let pool = Arc::new(ConnectionPool::new(settings.into()));
        pool.spawn_reaper();
        Self {
            inner: Arc::new(McpRegistryInner {
                connectors: RwLock::new(HashMap::new()),
                pool,
                health: RwLock::new(None),
            }),
        }
    }

    /// Custom pool clock for idle-TTL tests (inject "now" without sleeping).
    pub fn with_pool_and_clock(
        settings: McpPoolSettings,
        clock: Arc<dyn Fn() -> std::time::Instant + Send + Sync>,
    ) -> Self {
        let pool = Arc::new(ConnectionPool::with_clock(settings.into(), clock));
        pool.spawn_reaper();
        Self {
            inner: Arc::new(McpRegistryInner {
                connectors: RwLock::new(HashMap::new()),
                pool,
                health: RwLock::new(None),
            }),
        }
    }

    /// Publish connect/transport health into the live [`CapabilityCatalog`] (M1b).
    ///
    /// Composition roots pass the same `Arc` used for dispatcher routing so
    /// `routing_descriptors()` excludes peers this registry marks degraded.
    /// Shared by all clones of this registry.
    pub fn with_health_catalog(self, catalog: Arc<CapabilityCatalog>) -> Self {
        *self.inner.health.write().unwrap_or_else(|e| e.into_inner()) = Some(catalog);
        self
    }

    /// Register a server under `name` (the namespace its tools are exposed under). Chainable.
    pub fn register(self, name: impl Into<String>, connector: impl McpConnector + 'static) -> Self {
        self.inner
            .connectors
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.into(), Arc::new(connector));
        self
    }

    /// Atomically replace the connector map with `new_connectors`.
    ///
    /// Pool slots for names that disappear are invalidated so idle children for removed peers do
    /// not stick around. Names that remain may still reuse pool slots (transport identity is
    /// operator responsibility when only the command line changes).
    pub fn replace_connectors(&self, new_connectors: HashMap<String, Arc<dyn McpConnector>>) {
        let mut guard = self
            .inner
            .connectors
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let old: HashSet<String> = guard.keys().cloned().collect();
        let new_names: HashSet<String> = new_connectors.keys().cloned().collect();
        for removed in old.difference(&new_names) {
            self.inner.pool.invalidate(removed);
        }
        // Transport change for a kept name: drop any pooled session so the next acquire reconnects.
        for kept in old.intersection(&new_names) {
            self.inner.pool.invalidate(kept);
        }
        *guard = new_connectors;
    }

    fn health_catalog(&self) -> Option<Arc<CapabilityCatalog>> {
        self.inner
            .health
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn publish_healthy(&self, name: &str) {
        if let Some(cat) = self.health_catalog() {
            cat.mark_healthy(name);
        }
    }

    fn publish_degraded(&self, name: &str) {
        if let Some(cat) = self.health_catalog() {
            cat.mark_degraded(name);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inner
            .connectors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    /// Names of registered MCP connectors (the routable/connectable surface).
    pub fn names(&self) -> Vec<String> {
        self.inner
            .connectors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.inner
            .connectors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Whether pooling is enabled for this registry.
    pub fn pooling_enabled(&self) -> bool {
        self.inner.pool.policy().enabled
    }

    /// Acquire one server's runtime: pool checkout (with provenance rebind) or fresh connect.
    async fn acquire(
        &self,
        name: &str,
        provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        let permit = self.inner.pool.acquire_permit(name).await?;
        let health = self.health_catalog();

        if let Some(checkout) = self.inner.pool.try_checkout(name, provenance.clone()) {
            return Ok(Box::new(
                checkout
                    .with_health_catalog(health.clone())
                    .with_permit(permit),
            ));
        }

        let connector = {
            let guard = self
                .inner
                .connectors
                .read()
                .unwrap_or_else(|e| e.into_inner());
            guard
                .get(name)
                .cloned()
                .ok_or_else(|| RuntimeSetupError(format!("MCP '{name}' is not registered")))?
        };

        match connector.connect(provenance.clone()).await {
            Ok(runtime) => {
                self.publish_healthy(name);
                if self.inner.pool.policy().enabled {
                    Ok(Box::new(
                        PooledCheckout::from_fresh(
                            name.to_string(),
                            runtime,
                            provenance,
                            Arc::clone(&self.inner.pool),
                        )
                        .with_health_catalog(health)
                        .with_permit(permit),
                    ))
                } else {
                    Ok(Box::new(PermittedRuntime {
                        inner: AsToolRuntime(runtime),
                        _permit: permit,
                        name: name.to_string(),
                        health,
                    }))
                }
            }
            Err(e) => {
                self.inner.pool.invalidate(name);
                self.publish_degraded(name);
                Err(e)
            }
        }
    }

    /// Connect to every registered MCP independently and in parallel, tolerating individual
    /// failures instead of aborting the whole batch.
    pub async fn connect_all_best_effort(
        &self,
        provenance: WriteProvenance,
    ) -> (Box<dyn ToolRuntime>, Vec<String>) {
        self.connect_selected_best_effort(self.names(), provenance)
            .await
    }

    async fn connect_selected_best_effort(
        &self,
        names: Vec<String>,
        provenance: WriteProvenance,
    ) -> (Box<dyn ToolRuntime>, Vec<String>) {
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
                    self.inner.pool.invalidate(&name);
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
        let selected: Vec<String> = if allowed_mcps.is_empty() {
            self.names()
        } else {
            let known: HashSet<String> = self.names().into_iter().collect();
            allowed_mcps
                .iter()
                .filter(|name| known.contains(name.as_str()))
                .cloned()
                .collect()
        };

        // One explicitly selected peer failing is the task failing, so keep that error loud. With
        // a broader adaptive scope, one unrelated offline peer must not prevent healthy selected
        // peers from serving their tools. The live catalog is marked degraded by `acquire`, so the
        // next classification also stops offering the failed peer.
        if selected.len() > 1 {
            let selected_count = selected.len();
            let (runtime, failed) = self
                .connect_selected_best_effort(selected, provenance)
                .await;
            if failed.len() == selected_count {
                return Err(RuntimeSetupError(format!(
                    "all selected MCPs failed to connect: {}",
                    failed.join(", ")
                )));
            }
            return Ok(runtime);
        }

        let mut servers = Vec::with_capacity(selected.len());
        for name in selected {
            let runtime = self.acquire(&name, provenance.clone()).await?;
            servers.push((name, runtime));
        }
        Ok(Box::new(MultiMcpRuntime::new(servers)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::McpConnector;

    struct NoopConnector;
    #[async_trait]
    impl McpConnector for NoopConnector {
        async fn connect(
            &self,
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn crate::RebindableRuntime>, RuntimeSetupError> {
            Err(RuntimeSetupError("noop".into()))
        }
    }

    #[test]
    fn is_empty_true_when_no_connectors() {
        let reg = McpRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn is_empty_false_and_len_reflects_count() {
        let reg = McpRegistry::new()
            .register("a", NoopConnector)
            .register("b", NoopConnector);
        assert!(!reg.is_empty());
        assert_eq!(reg.len(), 2);
        let mut names = reg.names();
        names.sort();
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn replace_connectors_swaps_and_drops_removed_names() {
        let reg = McpRegistry::new()
            .register("a", NoopConnector)
            .register("b", NoopConnector);

        let mut new = HashMap::new();
        new.insert("b".into(), Arc::new(NoopConnector) as Arc<dyn McpConnector>);
        new.insert("c".into(), Arc::new(NoopConnector) as Arc<dyn McpConnector>);
        reg.replace_connectors(new);

        assert_eq!(reg.len(), 2);
        let mut names = reg.names();
        names.sort();
        assert_eq!(names, vec!["b".to_string(), "c".to_string()]);
    }
}
