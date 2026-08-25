//! Survivor tests for `factory.rs` and `live_runtime.rs`.

use super::*;
use crate::connector::McpConnector;
use crate::live_runtime::LiveRegistryRuntime;
use crate::pool::RebindableRuntime;
use async_trait::async_trait;
use liberado_common::{CapabilityCatalog, Consequence, McpDescriptor, WriteProvenance};
use liberado_executor::{RuntimeSetupError, ToolRuntime};
use liberado_provider::{ToolDef, ToolInvocation};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A connector whose connects always succeed, counting attempts so cache
/// behaviour (refresh-once vs refresh-always) is observable.
#[derive(Clone)]
struct CountingConnector {
    connects: Arc<AtomicUsize>,
}

impl CountingConnector {
    fn new(counter: Arc<AtomicUsize>) -> Self {
        Self { connects: counter }
    }
}

#[async_trait]
impl McpConnector for CountingConnector {
    async fn connect(
        &self,
        _provenance: WriteProvenance,
    ) -> Result<Box<dyn RebindableRuntime>, RuntimeSetupError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(CountingRuntime))
    }
}

struct CountingRuntime;

#[async_trait]
impl ToolRuntime for CountingRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        vec![ToolDef::new("counted_tool", "counted", json!({}))]
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Ok("counted-ok".into())
    }
}

#[async_trait]
impl RebindableRuntime for CountingRuntime {
    fn rebind_provenance(&mut self, _provenance: WriteProvenance) {}
}

fn descriptor(name: &str) -> McpDescriptor {
    McpDescriptor {
        name: name.to_string(),
        description: "test peer".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Default::default(),
        zone_from_arg: None,
        write_tools: Default::default(),
    }
}

fn provenance(who: &str) -> WriteProvenance {
    WriteProvenance {
        source: who.to_string(),
        correlation_id: None,
        zone: None,
        note: None,
    }
}

// ── LiveRegistryRuntime: the peer cache must actually cache ─────────────────

/// Sorted names are cached: a second call with an unchanged registry must
/// reuse the connected runtime instead of reconnecting every peer. Every
/// wrong `sorted_names` body (empty / blank / sentinel) makes the cached
/// names never equal the live ones, so connects grow without bound.
#[tokio::test(flavor = "multi_thread")]
async fn an_unchanged_registry_does_not_reconnect_on_every_call() {
    let ca = Arc::new(AtomicUsize::new(0));
    let cb = Arc::new(AtomicUsize::new(0));
    let registry = McpRegistry::new()
        .register("zebra", CountingConnector::new(Arc::clone(&cb)))
        .register("alpha", CountingConnector::new(Arc::clone(&ca)));

    let live = LiveRegistryRuntime::new(registry, provenance("t"));

    let first = live.catalog();
    assert_eq!(first.len(), 2, "both peers' tools are merged: {first:?}");
    assert_eq!(ca.load(Ordering::SeqCst), 1);
    assert_eq!(cb.load(Ordering::SeqCst), 1);

    // Unchanged registry: served from cache, zero fresh connects.
    let _ = live.catalog();
    assert_eq!(ca.load(Ordering::SeqCst), 1, "alpha must not reconnect");
    assert_eq!(cb.load(Ordering::SeqCst), 1, "zebra must not reconnect");

    // Invocations ride the same cached runtime.
    let out = live
        .invoke(&ToolInvocation::new("t1", "alpha:counted_tool", json!({})))
        .await
        .expect("routed to the connected peer");
    assert_eq!(out, "counted-ok");
    assert_eq!(ca.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn catalog_and_invoke_refuse_an_empty_registry_honestly() {
    let live = LiveRegistryRuntime::new(McpRegistry::new(), provenance("t"));
    let tools = live.catalog();
    assert!(tools.is_empty(), "no peers, no tools: {tools:?}");
    let err = live
        .invoke(&ToolInvocation::new("t1", "x:y", json!({})))
        .await
        .expect_err("nothing is registered");
    assert!(!err.is_empty());
}

// ── factory: successful acquire publishes healthy ───────────────────────────

#[tokio::test]
async fn a_successful_acquire_publishes_healthy_again() {
    let catalog = Arc::new(CapabilityCatalog::new());
    catalog.register(descriptor("m"));

    let registry = McpRegistry::with_pool_settings(McpPoolSettings {
        enabled: false,
        ..McpPoolSettings::default()
    })
    .register("m", CountingConnector::new(Arc::new(AtomicUsize::new(0))))
    .with_health_catalog(Arc::clone(&catalog));

    // Pre-degrade: routing excludes the peer.
    catalog.mark_degraded("m");
    assert!(catalog.is_degraded("m"), "precondition");

    // `acquire` is the private path a public acquire wraps; as a child module
    // of factory.rs this test may call it directly.
    let rt = registry
        .acquire("m", provenance("t"))
        .await
        .expect("the connector succeeds");
    drop(rt);

    assert!(
        !catalog.is_degraded("m"),
        "a successful connect clears degraded state"
    );
}
