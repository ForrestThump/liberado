//! Per-peer reconnect supervisor for the live chat tool surface.
//!
//! Each registered MCP gets one task. The task connects, publishes that peer's tools, and
//! waits until the session dies or the peer is removed. A failure drops the tools immediately
//! and waits on the capped backoff schedule before the next attempt. Removing the peer cancels
//! the task, including a wait that is already in progress.
//!
//! Chat `catalog` / `invoke` read the current snapshot. They do not connect and they do not
//! sleep. A call for a peer that is down fails fast.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use liberado_common::{WriteProvenance, mcp_of};
use liberado_provider::{ToolDef, ToolInvocation};
use liberado_tool_runtime::RebindableRuntime;
use tokio::sync::Notify;

use crate::backoff::{BASE_DELAY, CAP_DELAY, PROBE_EVERY};
use crate::connector::McpConnector;
use crate::factory::McpRegistryInner;

#[path = "supervisor_peer.rs"]
mod peer;
#[path = "supervisor_wave.rs"]
mod wave;

/// How a peer looks to `/api/status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpLinkState {
    Connected,
    Degraded,
}

/// One peer's reconnect status. `next_retry` is the remaining wait, when a retry is scheduled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPeerReport {
    pub name: String,
    pub state: McpLinkState,
    pub last_error: Option<String>,
    pub attempts: u32,
    pub next_retry: Option<Duration>,
    pub tools: usize,
}

/// Backoff and probe timing. Production uses [`ReconnectPolicy::default`].
#[derive(Clone)]
pub struct ReconnectPolicy {
    pub base: Duration,
    pub cap: Duration,
    pub probe_every: Duration,
    jitter: Arc<dyn Fn() -> f64 + Send + Sync>,
}

impl std::fmt::Debug for ReconnectPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReconnectPolicy")
            .field("base", &self.base)
            .field("cap", &self.cap)
            .field("probe_every", &self.probe_every)
            .finish()
    }
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            base: BASE_DELAY,
            cap: CAP_DELAY,
            probe_every: PROBE_EVERY,
            jitter: Arc::new(random_unit),
        }
    }
}

impl ReconnectPolicy {
    /// Tests pin the jitter draw so the wait is exact.
    pub fn with_jitter_unit(mut self, unit: f64) -> Self {
        self.jitter = Arc::new(move || unit);
        self
    }

    /// Schedule with jitter pinned at 1.0, so each wait equals the unjittered step.
    pub fn exact(base: Duration, cap: Duration, probe_every: Duration) -> Self {
        Self {
            base,
            cap,
            probe_every,
            ..Self::default()
        }
        .with_jitter_unit(1.0)
    }

    fn unit(&self) -> f64 {
        (self.jitter)()
    }
}

fn random_unit() -> f64 {
    use std::sync::atomic::AtomicU64;
    static STATE: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);
    let mixed = STATE
        .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
        .wrapping_mul(0xBF58_476D_1CE4_E5B9);
    (mixed >> 11) as f64 / ((1u64 << 53) as f64)
}

/// A one-way flag. Waiters see a trip that happened before they started waiting.
struct Latch {
    tripped: AtomicBool,
    notify: Notify,
}

impl Latch {
    fn new() -> Self {
        Self {
            tripped: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    fn trip(&self) {
        self.tripped.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn is_tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_tripped() {
                return;
            }
            notified.await;
        }
    }
}

struct PeerSlot {
    generation: u64,
    cancel: Arc<Latch>,
}

struct SurfaceSlot {
    runtime: Arc<dyn RebindableRuntime>,
    death: Arc<Latch>,
}

struct StatusRow {
    state: McpLinkState,
    last_error: Option<String>,
    attempts: u32,
    next_at: Option<tokio::time::Instant>,
    tools: usize,
}

struct SupervisorData {
    peers: HashMap<String, PeerSlot>,
    surface: HashMap<String, SurfaceSlot>,
    status: HashMap<String, StatusRow>,
}

struct Pending {
    name: String,
    connector: Arc<dyn McpConnector>,
    cancel: Arc<Latch>,
}

pub(crate) struct PeerSupervisor {
    data: Mutex<SupervisorData>,
    last_surface_count: Mutex<Option<usize>>,
    wave: Notify,
    surface_hook: Mutex<Option<wave::SurfaceHook>>,
    provenance: WriteProvenance,
    policy: ReconnectPolicy,
    registry: Weak<McpRegistryInner>,
}

struct PeerCtx {
    name: String,
    generation: u64,
    connector: Arc<dyn McpConnector>,
    provenance: WriteProvenance,
    policy: ReconnectPolicy,
    cancel: Arc<Latch>,
    sup: Arc<PeerSupervisor>,
}

enum Step {
    Again,
    Stop,
}

enum ConnectOnce {
    Cancelled,
    Failed(String),
    Ready(Box<dyn RebindableRuntime>),
}

enum DropWhy {
    Removed,
    Failed(String),
}

impl PeerSupervisor {
    fn empty(
        provenance: WriteProvenance,
        policy: ReconnectPolicy,
        registry: Weak<McpRegistryInner>,
    ) -> Self {
        Self {
            data: Mutex::new(SupervisorData {
                peers: HashMap::new(),
                surface: HashMap::new(),
                status: HashMap::new(),
            }),
            last_surface_count: Mutex::new(None),
            wave: Notify::new(),
            surface_hook: Mutex::new(None),
            provenance,
            policy,
            registry,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SupervisorData> {
        self.data.lock().unwrap_or_else(|err| err.into_inner())
    }

    pub(crate) fn reports(&self) -> Vec<McpPeerReport> {
        let now = tokio::time::Instant::now();
        let data = self.lock();
        let mut out: Vec<_> = data
            .status
            .iter()
            .map(|(name, row)| report_of(name, row, now))
            .collect();
        drop(data);
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub(crate) fn catalog(&self) -> Vec<ToolDef> {
        let data = self.lock();
        let mut tools = Vec::new();
        for (name, slot) in &data.surface {
            push_prefixed(&mut tools, name, &slot.runtime);
        }
        drop(data);
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        tools
    }

    pub(crate) async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        let server = mcp_of(&call.name);
        let Some((runtime, death)) = self.lookup(server) else {
            return Err(self.missing_message(server, &call.name));
        };
        let inner = bare_invocation(call, server);
        let result = runtime.invoke(&inner).await;
        if result.is_err() && runtime.connection_is_dead() {
            death.trip();
            return Err(unavailable(server));
        }
        result
    }

    fn lookup(&self, server: &str) -> Option<(Arc<dyn RebindableRuntime>, Arc<Latch>)> {
        let data = self.lock();
        data.surface
            .get(server)
            .map(|slot| (Arc::clone(&slot.runtime), Arc::clone(&slot.death)))
    }

    fn missing_message(&self, server: &str, tool: &str) -> String {
        let tracked = {
            let data = self.lock();
            data.peers.contains_key(server) || data.status.contains_key(server)
        };
        if tracked {
            unavailable(server)
        } else {
            format!("no MCP named '{server}' is in scope for tool '{tool}'")
        }
    }

    fn connectors(&self) -> Option<HashMap<String, Arc<dyn McpConnector>>> {
        let inner = self.registry.upgrade()?;
        let guard = inner
            .connectors
            .read()
            .unwrap_or_else(|err| err.into_inner());
        Some(guard.clone())
    }

    fn reconcile(
        self: &Arc<Self>,
        generation: u64,
        connectors: HashMap<String, Arc<dyn McpConnector>>,
    ) {
        let pending = self.plan(generation, &connectors);
        self.wake_first_wave();
        self.log_surface_if_changed();
        for item in pending {
            peer::spawn_peer(self, generation, item);
        }
    }

    fn plan(
        &self,
        generation: u64,
        connectors: &HashMap<String, Arc<dyn McpConnector>>,
    ) -> Vec<Pending> {
        let mut data = self.lock();
        let stale = stale_names(&data, generation, connectors);
        for name in stale {
            retire_locked(&mut data, &name);
        }
        new_peers(&mut data, generation, connectors)
    }

    fn stop_all(&self) {
        let mut data = self.lock();
        let names: Vec<String> = data.peers.keys().cloned().collect();
        for name in names {
            retire_locked(&mut data, &name);
        }
        drop(data);
        self.wake_first_wave();
    }

    fn install(
        &self,
        ctx: &PeerCtx,
        runtime: Arc<dyn RebindableRuntime>,
        death: Arc<Latch>,
        tools: usize,
    ) -> bool {
        let ok = {
            let mut data = self.lock();
            if owns(&data, &ctx.name, ctx.generation, &ctx.cancel) {
                data.surface
                    .insert(ctx.name.clone(), SurfaceSlot { runtime, death });
                data.status.insert(ctx.name.clone(), connected_row(tools));
                true
            } else {
                false
            }
        };
        self.wake_first_wave();
        ok
    }

    fn mark_degraded(&self, ctx: &PeerCtx, attempts: u32, err: &str, delay: Duration) {
        let mut data = self.lock();
        if owns(&data, &ctx.name, ctx.generation, &ctx.cancel) {
            data.surface.remove(&ctx.name);
            data.status.insert(
                ctx.name.clone(),
                StatusRow {
                    state: McpLinkState::Degraded,
                    last_error: Some(err.to_string()),
                    attempts,
                    next_at: Some(tokio::time::Instant::now() + delay),
                    tools: 0,
                },
            );
        }
        drop(data);
        self.wake_first_wave();
    }

    fn uninstall(&self, ctx: &PeerCtx) {
        let mut data = self.lock();
        if owns(&data, &ctx.name, ctx.generation, &ctx.cancel) {
            data.surface.remove(&ctx.name);
        }
    }

    fn drop_if_owner(&self, ctx: &PeerCtx) {
        let mut data = self.lock();
        if owns(&data, &ctx.name, ctx.generation, &ctx.cancel) {
            retire_locked(&mut data, &ctx.name);
        }
        drop(data);
        self.wake_first_wave();
    }

    fn log_surface_if_changed(&self) {
        let count = self.tool_count();
        wave::emit(self.surface_change(count));
    }

    fn tool_count(&self) -> usize {
        let data = self.lock();
        data.surface
            .values()
            .map(|slot| slot.runtime.catalog().len())
            .sum()
    }
}

fn report_of(name: &str, row: &StatusRow, now: tokio::time::Instant) -> McpPeerReport {
    McpPeerReport {
        name: name.to_string(),
        state: row.state,
        last_error: row.last_error.clone(),
        attempts: row.attempts,
        next_retry: row.next_at.map(|at| at.saturating_duration_since(now)),
        tools: row.tools,
    }
}

fn connected_row(tools: usize) -> StatusRow {
    StatusRow {
        state: McpLinkState::Connected,
        last_error: None,
        attempts: 0,
        next_at: None,
        tools,
    }
}

fn owns(data: &SupervisorData, name: &str, generation: u64, cancel: &Latch) -> bool {
    match data.peers.get(name) {
        Some(peer) => peer.generation == generation && !cancel.is_tripped(),
        None => false,
    }
}

fn retire_locked(data: &mut SupervisorData, name: &str) {
    if let Some(peer) = data.peers.remove(name) {
        peer.cancel.trip();
    }
    data.surface.remove(name);
    data.status.remove(name);
}

fn stale_names(
    data: &SupervisorData,
    generation: u64,
    connectors: &HashMap<String, Arc<dyn McpConnector>>,
) -> Vec<String> {
    data.peers
        .iter()
        .filter(|(name, peer)| peer.generation != generation || !connectors.contains_key(*name))
        .map(|(name, _)| name.clone())
        .collect()
}

fn new_peers(
    data: &mut SupervisorData,
    generation: u64,
    connectors: &HashMap<String, Arc<dyn McpConnector>>,
) -> Vec<Pending> {
    let mut pending = Vec::new();
    for (name, connector) in connectors {
        if data.peers.contains_key(name) {
            continue;
        }
        let cancel = Arc::new(Latch::new());
        data.peers.insert(
            name.clone(),
            PeerSlot {
                generation,
                cancel: Arc::clone(&cancel),
            },
        );
        pending.push(Pending {
            name: name.clone(),
            connector: Arc::clone(connector),
            cancel,
        });
    }
    pending
}

fn push_prefixed(out: &mut Vec<ToolDef>, name: &str, runtime: &Arc<dyn RebindableRuntime>) {
    for mut tool in runtime.catalog() {
        tool.name = format!("{name}:{}", tool.name);
        out.push(tool);
    }
}

fn bare_invocation(call: &ToolInvocation, server: &str) -> ToolInvocation {
    let prefix = format!("{server}:");
    let bare = call
        .name
        .strip_prefix(&prefix)
        .unwrap_or(call.name.as_str());
    ToolInvocation::new(call.id.clone(), bare, call.arguments.clone())
}

fn unavailable(name: &str) -> String {
    format!("MCP {name} unavailable, reconnecting")
}

impl crate::McpRegistry {
    pub(crate) fn bump_peers(&self) {
        self.inner.peer_set.send_modify(|generation| {
            *generation = generation.saturating_add(1);
        });
    }

    /// Start the chat reconnect supervisor when this process has a Tokio runtime.
    ///
    /// The first call wins. Later calls return the supervisor that is already running.
    /// `pub(crate)` because the returned supervisor type is crate-private.
    pub(crate) fn ensure_chat_supervisor(
        &self,
        provenance: WriteProvenance,
        policy: ReconnectPolicy,
    ) -> Arc<PeerSupervisor> {
        ensure(self, provenance, policy)
    }

    /// Per-peer reconnect status. Empty until the chat supervisor has started.
    pub fn peer_reports(&self) -> Vec<McpPeerReport> {
        let slot = self
            .inner
            .supervisor
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        slot.as_ref().map(|sup| sup.reports()).unwrap_or_default()
    }
}

pub(super) fn ensure(
    registry: &crate::McpRegistry,
    provenance: WriteProvenance,
    policy: ReconnectPolicy,
) -> Arc<PeerSupervisor> {
    let mut slot = registry
        .inner
        .supervisor
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    if let Some(existing) = slot.as_ref() {
        return Arc::clone(existing);
    }
    let sup = Arc::new(PeerSupervisor::empty(
        provenance,
        policy,
        Arc::downgrade(&registry.inner),
    ));
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let rx = registry.inner.peer_set.subscribe();
        let task = Arc::clone(&sup);
        handle.spawn(async move { peer::run_control(task, rx).await });
        *slot = Some(Arc::clone(&sup));
    }
    sup
}
