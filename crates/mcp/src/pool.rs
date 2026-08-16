//! Exclusive connection pool for MCP tool runtimes (M1).
//!
//! Entries are checked out exclusively, rebound to the **current** execution's
//! [`WriteProvenance`] on acquire, and returned on drop. Concurrent acquisitions of the same
//! MCP are limited by a per-name semaphore (`max_in_flight_per_name`); when the pool slot is
//! already out, additional holders connect fresh (still under that cap).
//!
//! Idle slots are reaped both eagerly (on pool activity) and by a background tick so infrequently
//! used stdio/HTTP children do not pin forever after check-in.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use liberado_common::{CapabilityCatalog, WriteProvenance};
use liberado_executor::{RuntimeSetupError, ToolRuntime};
use liberado_provider::{ToolDef, ToolInvocation};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Shut down each pooled runtime on a background task, then let it drop.
///
/// `Client::shutdown()` is async (it aborts the SSE task and sends the session-terminating
/// `DELETE`), so it cannot run on the pool's sync `Drop` path. Reaping by bare `Drop` leaked HTTP
/// connections: the `Client` vanished but its SSE stream stayed open server-side, piling up until
/// the proxy ran out of worker connections. Discarded pooled runtimes (reaped, dead, or
/// invalidated) are routed through here instead of being dropped bare.
fn spawn_shutdown(runtimes: Vec<Box<dyn RebindableRuntime>>) {
    // No current runtime (sync unit tests) → the only runtimes we reap there are test doubles with
    // no connection to tear down, so a bare drop is fine. In production there is always a runtime.
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        drop(runtimes);
        return;
    };
    for mut rt in runtimes {
        handle.spawn(async move {
            rt.shutdown().await;
        });
    }
}

/// A [`ToolRuntime`] that can accept a new execution's write provenance without reconnecting.
///
/// Pooling reuses the underlying MCP session/catalog; provenance (Decision 5 correlation) must
/// still be per-execution — implementors update whatever they inject into tool `_meta`.
#[async_trait]
pub trait RebindableRuntime: ToolRuntime {
    fn rebind_provenance(&mut self, provenance: WriteProvenance);

    /// `true` when the last failure was a **connection/transport** error (not an in-band tool
    /// `isError`). Pooled checkouts use this so dead peers are not checked back in.
    fn connection_is_dead(&self) -> bool {
        false
    }

    /// Gracefully shut down the underlying connection and release transport resources.
    ///
    /// Called (on a spawned task, since teardown needs `await`) before a pooled runtime is
    /// discarded — idle reap, dead checkout, or invalidate — so HTTP SSE tasks are aborted and the
    /// server-side session is terminated. Without this, a bare sync `Drop` leaks pooled HTTP
    /// connections and they pile up server-side. Default is a no-op for peers with no connection
    /// to tear down (stdio children drop their process on `Drop`).
    async fn shutdown(&mut self) {}
}

/// Pool policy: enable/disable, idle TTL, and per-name concurrency. Constructed from config at
/// composition time.
#[derive(Debug, Clone)]
pub struct PoolPolicy {
    pub enabled: bool,
    pub idle_ttl: Duration,
    /// Max simultaneous live checkouts/connects for one MCP name (including exclusive holders).
    pub max_in_flight_per_name: usize,
    /// How long an acquire waits for a concurrency permit before failing.
    pub connect_wait: Duration,
}

impl Default for PoolPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_ttl: Duration::from_secs(300),
            max_in_flight_per_name: 4,
            connect_wait: Duration::from_secs(60),
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
    /// Per-name connect/checkout concurrency limits.
    semaphores: Mutex<HashMap<String, Arc<Semaphore>>>,
    policy: PoolPolicy,
    /// Test hook: override "now" for idle TTL without sleeping.
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl ConnectionPool {
    pub(crate) fn new(policy: PoolPolicy) -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            semaphores: Mutex::new(HashMap::new()),
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
            semaphores: Mutex::new(HashMap::new()),
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

    /// Drop all slots idle longer than [`PoolPolicy::idle_ttl`]. Returns how many were removed.
    ///
    /// Called on pool activity and by the background reaper so peers that are never re-acquired
    /// still release their child processes / HTTP sessions.
    pub(crate) fn reap_idle(&self) -> usize {
        if !self.policy.enabled {
            return 0;
        }
        let reaped = self.take_expired();
        let n = reaped.len();
        if n > 0 {
            spawn_shutdown(reaped);
        }
        n
    }

    /// Remove all slots idle longer than the TTL and return their runtimes **without** dropping
    /// them. The caller owns teardown (see [`spawn_shutdown`]), so connections are shut down
    /// gracefully instead of being leaked by a bare sync `Drop`.
    fn take_expired(&self) -> Vec<Box<dyn RebindableRuntime>> {
        if !self.policy.enabled {
            return Vec::new();
        }
        let now = self.now();
        let ttl = self.policy.idle_ttl;
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let mut expired = Vec::new();
        let mut kept = HashMap::with_capacity(slots.len());
        for (name, slot) in slots.drain() {
            if now.saturating_duration_since(slot.last_used) <= ttl {
                kept.insert(name, slot);
            } else {
                expired.push(slot.runtime);
            }
        }
        *slots = kept;
        expired
    }

    /// Spawn a background task that periodically calls [`reap_idle`](Self::reap_idle).
    ///
    /// No-op when pooling is disabled or there is no current Tokio runtime (sync unit tests still
    /// reap on the next checkout via [`try_checkout`](Self::try_checkout)).
    pub(crate) fn spawn_reaper(self: &Arc<Self>) {
        if !self.policy.enabled {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let pool = Arc::clone(self);
        handle.spawn(async move {
            // Tick often enough to honor idle_ttl without spinning: half TTL, clamped.
            let period =
                (pool.policy.idle_ttl / 2).clamp(Duration::from_secs(1), Duration::from_secs(60));
            let mut interval = tokio::time::interval(period);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // First tick completes immediately; skip so we do not reap at t=0.
            interval.tick().await;
            loop {
                interval.tick().await;
                let n = pool.reap_idle();
                if n > 0 {
                    tracing::debug!(reaped = n, "MCP connection pool dropped idle connections");
                }
            }
        });
    }

    /// Wait for a per-name concurrency permit (held for the life of the checkout/runtime).
    pub(crate) async fn acquire_permit(
        self: &Arc<Self>,
        name: &str,
    ) -> Result<OwnedSemaphorePermit, RuntimeSetupError> {
        let max = self.policy.max_in_flight_per_name.max(1);
        let wait = self.policy.connect_wait;
        let sem = {
            let mut map = self.semaphores.lock().unwrap_or_else(|e| e.into_inner());
            map.entry(name.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(max)))
                .clone()
        };
        match tokio::time::timeout(wait, sem.acquire_owned()).await {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) => Err(RuntimeSetupError(format!(
                "MCP '{name}' concurrency semaphore closed"
            ))),
            Err(_) => Err(RuntimeSetupError(format!(
                "MCP '{name}' hit max concurrent connections ({max}); timed out after {wait:?}"
            ))),
        }
    }

    /// Try to check out a healthy, non-expired slot. On hit, rebinds provenance.
    ///
    /// Always reaps expired slots first so idle peers are not pinned until *their* next acquire.
    pub(crate) fn try_checkout(
        self: &Arc<Self>,
        name: &str,
        provenance: WriteProvenance,
    ) -> Option<PooledCheckout> {
        if !self.policy.enabled {
            return None;
        }
        // Eager reaping: any pool activity drops *all* expired slots (not only `name`).
        self.reap_idle();
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let slot = slots.remove(name)?;
        // Slot passed reap_idle; still guard in case of clock skew between reap and remove. If it
        // has since expired, shut it down rather than leaking it by a bare drop.
        let idle = self.now().saturating_duration_since(slot.last_used);
        if idle > self.policy.idle_ttl {
            spawn_shutdown(vec![slot.runtime]);
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
            _permit: None,
        })
    }

    /// Return a runtime to the pool after use.
    pub(crate) fn checkin(&self, name: String, runtime: Box<dyn RebindableRuntime>) {
        if !self.policy.enabled {
            return;
        }
        // Reap others while we hold the lock path; cheap and keeps idle set small.
        let _ = self.reap_idle();
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        // Re-check enabled not needed; insert may overwrite a concurrent check-in's slot (last wins).
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
        if let Some(slot) = slots.remove(name) {
            spawn_shutdown(vec![slot.runtime]);
        }
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
    /// Concurrency permit held for the lifetime of this checkout.
    _permit: Option<OwnedSemaphorePermit>,
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
            _permit: None,
        }
    }

    /// Attach the shared capability catalog so transport death publishes M1b degraded state.
    pub(crate) fn with_health_catalog(mut self, catalog: Option<Arc<CapabilityCatalog>>) -> Self {
        self.health = catalog;
        self
    }

    /// Hold a per-name concurrency permit until this checkout drops.
    pub(crate) fn with_permit(mut self, permit: OwnedSemaphorePermit) -> Self {
        self._permit = Some(permit);
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

    fn publish_healthy_after_success(&self) {
        if let Some(cat) = &self.health {
            // Only notifies watchers when the name was actually degraded.
            cat.mark_healthy(&self.name);
        }
    }
}

impl Drop for PooledCheckout {
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        // Transport death made this checkout unhealthy: discard it, but shut the connection down
        // first. A bare drop would leak the SSE task/server session exactly like an unreaped idle
        // slot, because `Client` teardown is async.
        if !self.healthy.load(Ordering::SeqCst) {
            spawn_shutdown(vec![runtime]);
            return;
        }
        if !self.pool.policy.enabled {
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
        } else if result.is_ok() {
            // Clear M1b degraded only after observed success — never on bare checkout.
            self.publish_healthy_after_success();
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

/// [`AsToolRuntime`] plus a concurrency permit (pooling disabled path).
pub(crate) struct PermittedRuntime {
    pub inner: AsToolRuntime,
    pub _permit: OwnedSemaphorePermit,
    pub name: String,
    pub health: Option<Arc<CapabilityCatalog>>,
}

#[async_trait]
impl ToolRuntime for PermittedRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.inner.catalog()
    }
    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        let result = self.inner.invoke(call).await;
        if result.is_ok() {
            if let Some(cat) = &self.health {
                cat.mark_healthy(&self.name);
            }
        } else if self.inner.0.connection_is_dead()
            && let Some(cat) = &self.health
        {
            cat.mark_degraded(&self.name);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct NoopRuntime;
    #[async_trait]
    impl ToolRuntime for NoopRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    #[async_trait]
    impl RebindableRuntime for NoopRuntime {
        fn rebind_provenance(&mut self, _provenance: WriteProvenance) {}
    }

    #[test]
    fn reap_idle_returns_zero_when_disabled() {
        let policy = PoolPolicy {
            enabled: false,
            ..PoolPolicy::default()
        };
        let pool = ConnectionPool::new(policy);
        assert_eq!(pool.reap_idle(), 0);
    }

    #[test]
    fn connection_is_dead_defaults_to_false() {
        let rt = NoopRuntime;
        assert!(!rt.connection_is_dead());
    }
}
