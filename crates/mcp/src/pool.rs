//! Exclusive connection pool for MCP tool runtimes (M1).
//!
//! Entries are checked out exclusively, rebound to the **current** execution's
//! [`WriteProvenance`] on acquire, and returned on drop. Concurrent acquisitions of the same
//! MCP while one is checked out fall back to a fresh connect (no wait queue).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use liberado_common::{CapabilityCatalog, WriteProvenance};
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};

/// A [`ToolRuntime`] that can accept a new execution's write provenance without reconnecting.
///
/// Pooling reuses the underlying MCP session/catalog; provenance (Decision 5 correlation) must
/// still be per-execution — implementors update whatever they inject into tool `_meta`.
pub trait RebindableRuntime: ToolRuntime {
    fn rebind_provenance(&mut self, provenance: WriteProvenance);

    /// `true` when the last failure was a **connection/transport** error (not an in-band tool
    /// `isError`). Pooled checkouts use this so dead peers are not checked back in.
    fn connection_is_dead(&self) -> bool {
        false
    }
}

/// Pool policy: enable/disable + idle TTL. Constructed from config at composition time.
#[derive(Debug, Clone)]
pub struct PoolPolicy {
    pub enabled: bool,
    pub idle_ttl: Duration,
}

impl Default for PoolPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_ttl: Duration::from_secs(300),
        }
    }
}

struct PoolSlot {
    runtime: Box<dyn RebindableRuntime>,
    last_used: Instant,
}

/// Shared pool state behind the registry.
///
/// Uses `std::sync::Mutex` so [`PooledCheckout`]'s `Drop` can check in synchronously (tests assert
/// reuse on the next acquisition without racing a spawned task).
pub(crate) struct ConnectionPool {
    slots: Mutex<HashMap<String, PoolSlot>>,
    policy: PoolPolicy,
    /// Test hook: override "now" for idle TTL without sleeping.
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl ConnectionPool {
    pub(crate) fn new(policy: PoolPolicy) -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            policy,
            clock: Arc::new(Instant::now),
        }
    }

    pub(crate) fn with_clock(
        policy: PoolPolicy,
        clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            policy,
            clock,
        }
    }

    pub(crate) fn policy(&self) -> &PoolPolicy {
        &self.policy
    }

    pub(crate) fn now(&self) -> Instant {
        (self.clock)()
    }

    /// Try to check out a healthy, non-expired slot. On hit, rebinds provenance.
    pub(crate) fn try_checkout(
        self: &Arc<Self>,
        name: &str,
        provenance: WriteProvenance,
    ) -> Option<PooledCheckout> {
        if !self.policy.enabled {
            return None;
        }
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let slot = slots.remove(name)?;
        let idle = self.now().saturating_duration_since(slot.last_used);
        if idle > self.policy.idle_ttl {
            // Expired — drop connection; caller will connect fresh.
            return None;
        }
        let mut runtime = slot.runtime;
        runtime.rebind_provenance(provenance);
        Some(PooledCheckout {
            name: name.to_string(),
            runtime: Some(runtime),
            pool: Arc::clone(self),
            healthy: AtomicBool::new(true),
            health: None,
        })
    }

    /// Return a runtime to the pool after use.
    pub(crate) fn checkin(&self, name: String, runtime: Box<dyn RebindableRuntime>) {
        if !self.policy.enabled {
            return;
        }
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.insert(
            name,
            PoolSlot {
                runtime,
                last_used: self.now(),
            },
        );
    }

    /// Drop a named slot without returning a runtime (e.g. after connection-level failure).
    pub(crate) fn invalidate(&self, name: &str) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.remove(name);
    }
}

/// Exclusive checkout of one pooled MCP runtime. Implements [`ToolRuntime`] and returns the
/// connection to the pool on drop (when still healthy).
pub struct PooledCheckout {
    name: String,
    runtime: Option<Box<dyn RebindableRuntime>>,
    pool: Arc<ConnectionPool>,
    /// Cleared when a connection-level invoke failure is observed (see [`RebindableRuntime::connection_is_dead`]).
    healthy: AtomicBool,
    /// Optional live catalog for M1b: transport death marks the peer degraded for routing.
    health: Option<Arc<CapabilityCatalog>>,
}

impl PooledCheckout {
    /// Wrap a freshly connected runtime as a checkout that will check in on drop.
    pub(crate) fn from_fresh(
        name: String,
        mut runtime: Box<dyn RebindableRuntime>,
        provenance: WriteProvenance,
        pool: Arc<ConnectionPool>,
    ) -> Self {
        runtime.rebind_provenance(provenance);
        Self {
            name,
            runtime: Some(runtime),
            pool,
            healthy: AtomicBool::new(true),
            health: None,
        }
    }

    /// Attach the shared capability catalog so transport death publishes M1b degraded state.
    pub(crate) fn with_health_catalog(mut self, catalog: Option<Arc<CapabilityCatalog>>) -> Self {
        self.health = catalog;
        self
    }

    fn runtime(&self) -> &dyn RebindableRuntime {
        self.runtime
            .as_deref()
            .expect("PooledCheckout used after drop")
    }

    fn mark_unhealthy(&self) {
        self.healthy.store(false, Ordering::SeqCst);
        if let Some(cat) = &self.health {
            cat.mark_degraded(&self.name);
        }
    }
}

impl Drop for PooledCheckout {
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        if !self.healthy.load(Ordering::SeqCst) || !self.pool.policy.enabled {
            return;
        }
        self.pool.checkin(self.name.clone(), runtime);
    }
}

#[async_trait]
impl ToolRuntime for PooledCheckout {
    fn catalog(&self) -> Vec<ToolDef> {
        self.runtime().catalog()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        let result = self.runtime().invoke(call).await;
        // Transport death (not tool isError) must not re-enter the pool.
        if result.is_err() && self.runtime().connection_is_dead() {
            self.mark_unhealthy();
        }
        result
    }
}

/// Erase `Box<dyn RebindableRuntime>` as `Box<dyn ToolRuntime>` when pooling is disabled.
pub(crate) struct AsToolRuntime(pub Box<dyn RebindableRuntime>);

#[async_trait]
impl ToolRuntime for AsToolRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.0.catalog()
    }
    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        self.0.invoke(call).await
    }
}
