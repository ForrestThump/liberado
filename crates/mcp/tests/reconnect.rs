//! Chat reconnect against an in-process MCP server. Time is paused.

use core::future::Future;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

use liberado_common::{CapabilityCatalog, Consequence, McpDescriptor, WriteProvenance};
use liberado_mcp::{
    LiveRegistryRuntime, McpConnector, McpLinkState, McpRegistry, RebindableRuntime,
    ReconnectPolicy,
};
use liberado_provider::ToolInvocation;
use liberado_tool_runtime::{RuntimeSetupError, ToolRuntime};
use serde_json::Value;
use turbomcp_client::Client;
use turbomcp_core::context::RequestContext;
use turbomcp_core::error::{McpError, McpResult};
use turbomcp_core::handler::McpHandler;
use turbomcp_core::marker::MaybeSend;
use turbomcp_types::{
    Prompt, PromptResult, Resource, ResourceResult, ServerInfo, Tool, ToolResult,
};

mod common;

#[derive(Clone)]
struct EchoServer;

impl McpHandler for EchoServer {
    fn server_info(&self) -> ServerInfo {
        ServerInfo::new("echo", "1.0.0")
    }

    fn list_tools(&self) -> Vec<Tool> {
        vec![Tool::new("add", "echo tool")]
    }

    mcp_handler_stubs!();

    fn call_tool<'a>(
        &'a self,
        name: &'a str,
        _args: Value,
        _ctx: &'a RequestContext,
    ) -> impl Future<Output = McpResult<ToolResult>> + MaybeSend + 'a {
        let name = name.to_string();
        async move {
            if name == "add" {
                Ok(ToolResult::text("added"))
            } else {
                Err(McpError::tool_not_found(&name))
            }
        }
    }
}

struct Gate {
    open: AtomicBool,
    down: AtomicBool,
    connects: AtomicUsize,
}

impl Gate {
    fn closed() -> Self {
        Self {
            open: AtomicBool::new(false),
            down: AtomicBool::new(false),
            connects: AtomicUsize::new(0),
        }
    }

    fn opened() -> Self {
        Self {
            open: AtomicBool::new(true),
            down: AtomicBool::new(false),
            connects: AtomicUsize::new(0),
        }
    }
}

struct GateConnector {
    gate: Arc<Gate>,
}

struct FlaggedRuntime {
    inner: Box<dyn RebindableRuntime>,
    gate: Arc<Gate>,
}

#[async_trait::async_trait]
impl McpConnector for GateConnector {
    async fn connect(
        &self,
        provenance: WriteProvenance,
    ) -> Result<Box<dyn RebindableRuntime>, RuntimeSetupError> {
        self.gate.connects.fetch_add(1, Ordering::SeqCst);
        if !self.gate.open.load(Ordering::SeqCst) || self.gate.down.load(Ordering::SeqCst) {
            return Err(RuntimeSetupError("peer down".into()));
        }
        let (transport, _server) = turbomcp_server::transport::channel::run_in_process(&EchoServer)
            .await
            .map_err(|err| RuntimeSetupError(err.to_string()))?;
        let runtime = liberado_mcp::TurbomcpRuntime::connect(Client::new(transport), provenance)
            .await
            .map_err(|err| RuntimeSetupError(err.to_string()))?;
        Ok(Box::new(FlaggedRuntime {
            inner: Box::new(runtime),
            gate: Arc::clone(&self.gate),
        }))
    }
}

#[async_trait::async_trait]
impl ToolRuntime for FlaggedRuntime {
    fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
        self.inner.catalog()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        if self.gate.down.load(Ordering::SeqCst) {
            Err("peer down".into())
        } else {
            self.inner.invoke(call).await
        }
    }
}

#[async_trait::async_trait]
impl RebindableRuntime for FlaggedRuntime {
    fn rebind_provenance(&mut self, provenance: WriteProvenance) {
        self.inner.rebind_provenance(provenance);
    }

    fn connection_is_dead(&self) -> bool {
        self.gate.down.load(Ordering::SeqCst) || self.inner.connection_is_dead()
    }

    async fn probe(&self) -> Result<(), String> {
        if self.gate.down.load(Ordering::SeqCst) {
            Err("peer down".into())
        } else {
            self.inner.probe().await
        }
    }

    async fn shutdown_shared(&self) {
        self.inner.shutdown_shared().await;
    }
}

fn policy(probe_every: Duration) -> ReconnectPolicy {
    ReconnectPolicy::exact(Duration::from_secs(1), Duration::from_secs(60), probe_every)
}

fn start(gate: Arc<Gate>, probe_every: Duration) -> (McpRegistry, LiveRegistryRuntime) {
    let registry = McpRegistry::new().register(
        "turbovault",
        GateConnector {
            gate: Arc::clone(&gate),
        },
    );
    let live = LiveRegistryRuntime::with_policy(
        registry.clone(),
        WriteProvenance::agent("test", "c"),
        policy(probe_every),
    );
    (registry, live)
}

async fn spin_until(mut ready: impl FnMut() -> bool) {
    for _ in 0..400 {
        if ready() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("condition was not met");
}

fn has_add(live: &LiveRegistryRuntime) -> bool {
    live.catalog()
        .iter()
        .any(|tool| tool.name == "turbovault:add")
}

fn state_of(registry: &McpRegistry) -> Option<McpLinkState> {
    registry
        .peer_reports()
        .into_iter()
        .find(|row| row.name == "turbovault")
        .map(|row| row.state)
}

#[tokio::test(start_paused = true)]
async fn peer_down_at_start_joins_the_catalog_when_it_comes_up() {
    let health = Arc::new(CapabilityCatalog::new());
    health.register(McpDescriptor {
        name: "turbovault".into(),
        description: "test".into(),
        consequence: Consequence::Reversible,
        ..McpDescriptor::default()
    });
    let gate = Arc::new(Gate::closed());
    let registry = McpRegistry::new()
        .with_health_catalog(Arc::clone(&health))
        .register(
            "turbovault",
            GateConnector {
                gate: Arc::clone(&gate),
            },
        );
    let live = LiveRegistryRuntime::with_policy(
        registry.clone(),
        WriteProvenance::agent("test", "c"),
        policy(Duration::from_secs(3600)),
    );

    spin_until(|| state_of(&registry) == Some(McpLinkState::Degraded)).await;
    assert!(live.catalog().is_empty());
    assert!(health.is_degraded("turbovault"));
    let row = registry.peer_reports().remove(0);
    assert_eq!(row.attempts, 1);
    assert!(row.last_error.unwrap().contains("peer down"));
    assert_eq!(row.next_retry, Some(Duration::from_secs(1)));
    assert_eq!(row.tools, 0);

    gate.open.store(true, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(1)).await;
    spin_until(|| has_add(&live)).await;

    let row = registry.peer_reports().remove(0);
    assert_eq!(row.state, McpLinkState::Connected);
    assert_eq!(row.attempts, 0);
    assert!(row.last_error.is_none());
    assert!(row.next_retry.is_none());
    assert_eq!(row.tools, 1);
    assert!(!health.is_degraded("turbovault"));
    let reply = live
        .invoke(&ToolInvocation::new(
            "c",
            "turbovault:add",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(reply, "added");
}

#[tokio::test(start_paused = true)]
async fn a_dead_session_drops_tools_and_comes_back() {
    let gate = Arc::new(Gate::opened());
    let (registry, live) = start(Arc::clone(&gate), Duration::from_secs(3600));
    spin_until(|| has_add(&live)).await;

    gate.down.store(true, Ordering::SeqCst);
    let err = live
        .invoke(&ToolInvocation::new(
            "c",
            "turbovault:add",
            serde_json::json!({}),
        ))
        .await
        .unwrap_err();
    assert_eq!(err, "MCP turbovault unavailable, reconnecting");
    spin_until(|| live.catalog().is_empty()).await;
    assert_eq!(state_of(&registry), Some(McpLinkState::Degraded));

    let unknown = live
        .invoke(&ToolInvocation::new(
            "c",
            "other:add",
            serde_json::json!({}),
        ))
        .await
        .unwrap_err();
    assert!(unknown.contains("no MCP named 'other'"), "{unknown}");

    gate.down.store(false, Ordering::SeqCst);
    gate.open.store(true, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(1)).await;
    spin_until(|| has_add(&live)).await;
    assert_eq!(state_of(&registry), Some(McpLinkState::Connected));
}

#[tokio::test(start_paused = true)]
async fn a_probe_marks_a_quiet_peer_degraded_until_it_returns() {
    let gate = Arc::new(Gate::opened());
    let (registry, live) = start(Arc::clone(&gate), Duration::from_secs(1));
    spin_until(|| has_add(&live)).await;

    gate.down.store(true, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(1)).await;
    spin_until(|| state_of(&registry) == Some(McpLinkState::Degraded)).await;
    assert!(live.catalog().is_empty());

    gate.down.store(false, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(1)).await;
    spin_until(|| has_add(&live)).await;
    assert_eq!(state_of(&registry), Some(McpLinkState::Connected));
}

#[tokio::test(start_paused = true)]
async fn a_removed_peer_stops_retrying() {
    let gate = Arc::new(Gate::closed());
    let (registry, live) = start(Arc::clone(&gate), Duration::from_secs(3600));
    spin_until(|| state_of(&registry) == Some(McpLinkState::Degraded)).await;
    let before = gate.connects.load(Ordering::SeqCst);

    registry.replace_connectors(HashMap::new());
    spin_until(|| registry.peer_reports().is_empty()).await;
    let after_remove = gate.connects.load(Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(300)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }

    assert_eq!(gate.connects.load(Ordering::SeqCst), after_remove);
    assert!(after_remove >= before);
    assert!(live.catalog().is_empty());
    assert!(registry.peer_reports().is_empty());
}

#[tokio::test(start_paused = true)]
async fn first_wave_with_no_peers_returns_immediately() {
    let live = LiveRegistryRuntime::new(McpRegistry::new(), WriteProvenance::agent("test", "c"));
    let start = tokio::time::Instant::now();
    live.wait_first_wave(Duration::from_secs(15)).await;
    assert_eq!(start.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn first_wave_returns_when_each_peer_finishes_its_first_attempt() {
    let up = Arc::new(Gate::opened());
    let down = Arc::new(Gate::closed());
    let registry = McpRegistry::new()
        .register(
            "turbovault",
            GateConnector {
                gate: Arc::clone(&up),
            },
        )
        .register(
            "search",
            GateConnector {
                gate: Arc::clone(&down),
            },
        );
    let live = LiveRegistryRuntime::with_policy(
        registry.clone(),
        WriteProvenance::agent("test", "c"),
        policy(Duration::from_secs(3600)),
    );
    let start = tokio::time::Instant::now();
    live.wait_first_wave(Duration::from_secs(15)).await;
    assert!(start.elapsed() < Duration::from_secs(15));
    assert!(has_add(&live));
    assert!(
        live.catalog()
            .iter()
            .all(|tool| !tool.name.starts_with("search:"))
    );
    assert_eq!(
        state_named(&registry, "turbovault"),
        Some(McpLinkState::Connected)
    );
    assert_eq!(
        state_named(&registry, "search"),
        Some(McpLinkState::Degraded)
    );
}

#[tokio::test(start_paused = true)]
async fn first_wave_returns_at_the_bound_while_connect_is_still_pending() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let registry = McpRegistry::new().register(
        "turbovault",
        HoldConnector {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        },
    );
    let live = LiveRegistryRuntime::with_policy(
        registry,
        WriteProvenance::agent("test", "c"),
        policy(Duration::from_secs(3600)),
    );
    let start = tokio::time::Instant::now();
    let wait = tokio::spawn(async move {
        live.wait_first_wave(Duration::from_secs(15)).await;
        live
    });
    started.notified().await;
    if !wait.is_finished() {
        tokio::time::advance(Duration::from_secs(15)).await;
    }
    let live = wait.await.unwrap();
    assert!(start.elapsed() >= Duration::from_secs(15));
    assert!(!has_add(&live));
    release.notify_one();
    spin_until(|| has_add(&live)).await;
}

#[tokio::test(start_paused = true)]
async fn surface_hook_runs_when_the_catalog_changes() {
    let gate = Arc::new(Gate::closed());
    let (registry, live) = start(Arc::clone(&gate), Duration::from_secs(3600));
    live.wait_first_wave(Duration::from_secs(15)).await;
    assert_eq!(state_of(&registry), Some(McpLinkState::Degraded));

    let seen = Arc::new(Mutex::new(Vec::new()));
    let live = Arc::new(live);
    let seen_hook = Arc::clone(&seen);
    let live_hook = Arc::clone(&live);
    live.arm_surface_hook(Arc::new(move || {
        seen_hook
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .push(live_hook.catalog().len());
    }));

    gate.open.store(true, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(1)).await;
    spin_until(|| {
        seen.lock()
            .unwrap_or_else(|err| err.into_inner())
            .contains(&1)
    })
    .await;

    gate.down.store(true, Ordering::SeqCst);
    let err = live
        .invoke(&ToolInvocation::new(
            "c",
            "turbovault:add",
            serde_json::json!({}),
        ))
        .await
        .unwrap_err();
    assert_eq!(err, "MCP turbovault unavailable, reconnecting");
    spin_until(|| {
        seen.lock()
            .unwrap_or_else(|err| err.into_inner())
            .contains(&0)
    })
    .await;

    gate.down.store(false, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(1)).await;
    spin_until(|| has_add(&live)).await;
    spin_until(|| {
        seen.lock()
            .unwrap_or_else(|err| err.into_inner())
            .last()
            .copied()
            == Some(1)
    })
    .await;
}

fn state_named(registry: &McpRegistry, name: &str) -> Option<McpLinkState> {
    registry
        .peer_reports()
        .into_iter()
        .find(|row| row.name == name)
        .map(|row| row.state)
}

struct HoldConnector {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

struct StaticTool;

#[async_trait::async_trait]
impl McpConnector for HoldConnector {
    async fn connect(
        &self,
        _: WriteProvenance,
    ) -> Result<Box<dyn RebindableRuntime>, RuntimeSetupError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(Box::new(StaticTool))
    }
}

#[async_trait::async_trait]
impl ToolRuntime for StaticTool {
    fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
        vec![liberado_provider::ToolDef {
            name: "add".into(),
            description: String::new(),
            parameters: serde_json::json!({}),
        }]
    }

    async fn invoke(&self, _: &ToolInvocation) -> Result<String, String> {
        Ok("added".into())
    }
}

#[async_trait::async_trait]
impl RebindableRuntime for StaticTool {
    fn rebind_provenance(&mut self, _: WriteProvenance) {}
}
