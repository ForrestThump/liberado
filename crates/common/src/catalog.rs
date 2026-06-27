//! Live, queryable capability catalog — the shared runtime registry of available MCPs.
//!
//! [`CapabilityCatalog`] wraps a read-optimised, thread-safe data structure behind
//! `Arc<RwLock<…>>` plus a `tokio::sync::watch` channel so consumers can react to
//! changes without polling. The catalog is populated at boot from the config's
//! `topology.mcps` and updated at runtime as MCPs come and go.
//!
//! # Concurrency model
//!
//! *Readers* (the `descriptors()` / `get()` / `is_empty()` / `len()` accessors) acquire
//! a read lock that is uncontended in practice — the only writer is the server's
//! boot/reload path.
//!
//! *Watchers* call [`subscribe()`](CapabilityCatalog::subscribe) to get a
//! `watch::Receiver` that fires `()` every time the catalog changes. This lets long-lived
//! consumers (e.g. a dispatch loop or TUI) react cheaply.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use crate::Consequence;

/// A catalog entry: describes one MCP server that the system can route to.
///
/// This is the common crate's own descriptor type (mirroring
/// `liberado_dispatcher::McpDescriptor` fields) so that the catalog lives in
/// dependency-light `liberado-common` without pulling in the dispatcher crate.
#[derive(Debug, Clone)]
pub struct McpDescriptor {
    /// Unique name of this MCP server, matching the `name` in `topology.mcps`.
    pub name: String,
    /// Short description the dispatcher and UI show.
    pub description: String,
    /// How risky this MCP's effects are (reversibility/externality).
    pub consequence: Consequence,
}

/// A live, queryable capability catalog. Multiple consumers can independently query
/// it. Updates are propagated via a watch channel.
pub struct CapabilityCatalog {
    inner: Arc<RwLock<CatalogState>>,
    updated: tokio::sync::watch::Sender<()>,
}

struct CatalogState {
    mcps: HashMap<String, McpDescriptor>,
    last_updated: Instant,
}

impl CapabilityCatalog {
    /// Create a new, empty catalog.
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::watch::channel(());
        Self {
            inner: Arc::new(RwLock::new(CatalogState {
                mcps: HashMap::new(),
                last_updated: Instant::now(),
            })),
            updated: tx,
        }
    }

    /// Register (or update) an MCP descriptor. Notifies subscribers on change.
    pub fn register(&self, mcp: McpDescriptor) {
        let mut state = self.inner.write().unwrap();
        state.mcps.insert(mcp.name.clone(), mcp);
        state.last_updated = Instant::now();
        let _ = self.updated.send(());
    }

    /// Remove an MCP descriptor by name. No-op if the name is not registered.
    /// Notifies subscribers on change.
    pub fn deregister(&self, name: &str) {
        let mut state = self.inner.write().unwrap();
        state.mcps.remove(name);
        state.last_updated = Instant::now();
        let _ = self.updated.send(());
    }

    /// Return a snapshot of all registered descriptors.
    pub fn descriptors(&self) -> Vec<McpDescriptor> {
        self.inner.read().unwrap().mcps.values().cloned().collect()
    }

    /// Look up a single descriptor by name.
    pub fn get(&self, name: &str) -> Option<McpDescriptor> {
        self.inner.read().unwrap().mcps.get(name).cloned()
    }

    /// Subscribe to catalog changes. The returned receiver yields `()` every time
    /// the catalog is modified (register/deregister).
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<()> {
        self.updated.subscribe()
    }

    /// Whether the catalog is currently empty.
    pub fn is_empty(&self) -> bool {
        self.inner.read().unwrap().mcps.is_empty()
    }

    /// Number of registered descriptors.
    pub fn len(&self) -> usize {
        self.inner.read().unwrap().mcps.len()
    }
}

impl Default for CapabilityCatalog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_descriptor(name: &str) -> McpDescriptor {
        McpDescriptor {
            name: name.into(),
            description: format!("{name} description"),
            consequence: Consequence::Reversible,
        }
    }

    #[test]
    fn empty_catalog_returns_empty() {
        let catalog = CapabilityCatalog::new();
        assert!(catalog.is_empty());
        assert_eq!(catalog.len(), 0);
        assert!(catalog.descriptors().is_empty());
    }

    #[test]
    fn register_and_retrieve() {
        let catalog = CapabilityCatalog::new();
        let desc = sample_descriptor("memory-mcp");
        catalog.register(desc.clone());

        assert_eq!(catalog.len(), 1);
        assert!(!catalog.is_empty());

        let all = catalog.descriptors();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "memory-mcp");
        assert_eq!(all[0].description, "memory-mcp description");

        let looked_up = catalog.get("memory-mcp");
        assert!(looked_up.is_some());
        assert_eq!(looked_up.unwrap().name, "memory-mcp");
    }

    #[test]
    fn deregister_removes() {
        let catalog = CapabilityCatalog::new();
        catalog.register(sample_descriptor("tasks-mcp"));
        catalog.register(sample_descriptor("email-mcp"));
        assert_eq!(catalog.len(), 2);

        catalog.deregister("tasks-mcp");
        assert_eq!(catalog.len(), 1);
        assert!(catalog.get("tasks-mcp").is_none());
        assert!(catalog.get("email-mcp").is_some());

        // Deregistering a non-existent name is a no-op.
        catalog.deregister("ghost-mcp");
        assert_eq!(catalog.len(), 1);
    }

    #[test]
    fn subscribe_receives_notification() {
        let catalog = CapabilityCatalog::new();
        let rx = catalog.subscribe();

        // Initially there should be no pending notification (the initial send was consumed
        // by the Receiver created in `subscribe()` — `has_changed` reflects new changes only).
        assert!(!rx.has_changed().unwrap());

        catalog.register(sample_descriptor("memory-mcp"));
        // After a register, the watch should fire.
        assert!(rx.has_changed().unwrap());

        catalog.deregister("memory-mcp");
        // After a deregister, another notification.
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn register_updates_existing() {
        let catalog = CapabilityCatalog::new();
        catalog.register(McpDescriptor {
            name: "mcp".into(),
            description: "v1".into(),
            consequence: Consequence::ReadOnly,
        });

        // Re-register with updated fields.
        catalog.register(McpDescriptor {
            name: "mcp".into(),
            description: "v2".into(),
            consequence: Consequence::External,
        });

        let desc = catalog.get("mcp").unwrap();
        assert_eq!(desc.description, "v2");
        assert_eq!(desc.consequence, Consequence::External);
    }
}
