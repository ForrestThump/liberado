//! Live, queryable capability catalog — the shared runtime registry of available MCPs.
//!
//! [`CapabilityCatalog`] wraps a read-optimised, thread-safe data structure behind
//! `Arc<RwLock<…>>` plus a `tokio::sync::watch` channel so consumers can react to
//! changes without polling. The catalog is populated at boot from the config's
//! `topology.mcps` and updated at runtime as MCPs come and go.
//!
//! # M1b — degraded peers
//!
//! Connect/transport failures (the same class pooling already discards) can
//! [`mark_degraded`](CapabilityCatalog::mark_degraded). **Routing** consumers must use
//! [`routing_descriptors`](CapabilityCatalog::routing_descriptors) so the dispatcher never
//! steers the model at a peer already known dead. Full [`descriptors`](CapabilityCatalog::descriptors)
//! still lists every registered MCP for zone/authority checks. Success
//! [`mark_healthy`](CapabilityCatalog::mark_healthy) clears the flag (recovery).
//!
//! Degraded entries carry a timestamp and **half-open** after
//! [`DEFAULT_DEGRADED_HALF_OPEN`](DEFAULT_DEGRADED_HALF_OPEN) (configurable via
//! [`set_degraded_half_open_ttl`](CapabilityCatalog::set_degraded_half_open_ttl)): the peer is
//! re-offered to classifiers so a transient outage can recover without an out-of-band acquire.
//!
//! # Concurrency model
//!
//! *Readers* (the `descriptors()` / `get()` / `is_empty()` / `len()` accessors) acquire
//! a read lock that is uncontended in practice — writers are the server's boot/reload path
//! and the MCP registry's health updates.
//!
//! *Watchers* call [`subscribe()`](CapabilityCatalog::subscribe) to get a
//! `watch::Receiver` that fires `()` every time the catalog changes. This lets long-lived
//! consumers (e.g. a dispatch loop or TUI) react cheaply.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::{Consequence, WriteClass};

/// Default time a peer stays out of [`CapabilityCatalog::routing_descriptors`] after
/// [`mark_degraded`](CapabilityCatalog::mark_degraded) before half-open re-inclusion.
pub const DEFAULT_DEGRADED_HALF_OPEN: Duration = Duration::from_secs(60);

/// A catalog entry: describes one MCP server that the system can route to.
///
/// This is the common crate's own descriptor type (mirroring
/// `liberado_dispatcher::McpDescriptor` fields) so that the catalog lives in
/// dependency-light `liberado-common` without pulling in the dispatcher crate.
#[derive(Debug, Clone, Default)]
pub struct McpDescriptor {
    /// Unique name of this MCP server, matching the `name` in `topology.mcps`.
    pub name: String,
    /// Short description the dispatcher and UI show.
    pub description: String,
    /// How risky this MCP's effects are (reversibility/externality).
    pub consequence: Consequence,
    /// The correlation_id of the session that created this MCP via self-extension (riggers).
    /// `None` for human-configured static MCPs declared in `topology.toml`.
    pub provenance: Option<String>,
    /// Default target zone for this MCP's write tools — a tool not named in `tool_zones` below
    /// inherits this. `None` if this MCP hasn't opted into zone tracking (most MCPs aren't vault
    /// writers at all). Mirrors `liberado_config_loader::McpConfig::default_zone`, kept as a plain
    /// field here rather than depending on that crate's `ToolImpact` type, since this crate is
    /// deliberately dependency-light.
    pub default_zone: Option<String>,
    /// Per-tool zone overrides: `(bare tool name, target zone)`. A tool named here with `None`
    /// explicitly overrides to "not a zone write" even when `default_zone` is set. Mirrors
    /// `liberado_config_loader::McpConfig::tools`.
    pub tool_zones: Vec<(String, Option<String>)>,
    /// For a **path-addressed** MCP: the argument whose leading path segment names the target zone.
    ///
    /// A tool→zone map cannot describe an MCP like TurboVault, where one `write_note` lands in
    /// `tasks/`, `decisions/` or `finance/` depending entirely on its `path` argument. Declaring
    /// `default_zone = "tasks"` for such an MCP would be a *lie*: a grant holding `Write(tasks)`
    /// would be waved through a write to `decisions/`. So the zone is resolved from the call's
    /// arguments instead — see [`write_target`].
    pub zone_from_arg: Option<String>,
    /// The tools of a path-addressed MCP that actually **write**. Everything else is a read.
    ///
    /// Needed because `zone_from_arg` alone cannot distinguish `read_note` from `write_note` —
    /// both carry a path. Without this list, reads would be made to require a `Write` capability.
    pub write_tools: Vec<String>,
}

/// What a tool call writes to, resolved against its MCP's declaration *and its arguments*.
///
/// Deliberately a three-state answer, not `Option<String>`: "this is a write whose zone I cannot
/// determine" is a real, distinct outcome (a path-addressed write tool called with no path), and it
/// must **not** collapse into "not a write". That collapse is exactly the class of bug F1 was — a
/// guard that says nothing when it does not know, and is therefore silently absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteTarget {
    /// Not a zone write: a declared read, or an MCP that writes nothing.
    NotAWrite,
    /// Writes to this zone. The caller must hold `Capability::Write(Zone::vault(zone))`.
    Zone(String),
    /// It *is* a write, but the target zone could not be determined. **Fail closed** — refuse.
    Undeterminable(String),
}

/// Resolve what `bare_tool_name` on `descriptor` writes to, given the call's `arguments`.
///
/// Two declaration styles, because two kinds of MCP exist:
///
/// * **Fixed-zone** (a tasks MCP whose every tool touches `tasks/`): `default_zone` + per-tool
///   `tool_zones` overrides. Zone depends only on the tool name.
/// * **Path-addressed** (TurboVault): `zone_from_arg` + `write_tools`. Zone depends on the call's
///   arguments, so it can only be known here, at the boundary, with the call in hand.
pub fn write_target(
    descriptor: &McpDescriptor,
    bare_tool_name: &str,
    arguments: &serde_json::Value,
) -> WriteTarget {
    if let Some(arg) = &descriptor.zone_from_arg {
        if !descriptor.write_tools.iter().any(|t| t == bare_tool_name) {
            return WriteTarget::NotAWrite;
        }
        let Some(path) = arguments.get(arg).and_then(|v| v.as_str()) else {
            return WriteTarget::Undeterminable(format!(
                "'{bare_tool_name}' writes, and its zone comes from the '{arg}' argument, which is \
                 missing or not a string"
            ));
        };
        // The zone is the leading path segment: `tasks/foo.md` -> `tasks`. A bare filename has no
        // zone, and a write with no zone is a write we cannot authorize.
        let segment = path.split(['/', '\\']).find(|s| !s.is_empty() && *s != ".");
        return match segment {
            Some(zone) if path.contains('/') || path.contains('\\') => {
                WriteTarget::Zone(zone.to_string())
            }
            _ => WriteTarget::Undeterminable(format!(
                "'{bare_tool_name}' writes to '{path}', which names no zone (expected \
                 '<zone>/...')"
            )),
        };
    }

    match resolve_zone(descriptor, bare_tool_name) {
        Some(zone) => WriteTarget::Zone(zone),
        None => WriteTarget::NotAWrite,
    }
}

/// Resolve the target zone for `bare_tool_name` given `descriptor`'s zone declarations. `None`
/// means "not a zone-write concern" — a declared read, or an MCP that hasn't opted into zone
/// tracking at all — distinct from "a write whose zone is unknown," which callers (the zone-
/// write-class guard) should treat conservatively rather than silently skip. Mirrors
/// `liberado_config_loader::resolve_declared_zone` exactly; kept separate because it operates on
/// this crate's lighter `McpDescriptor` (what the dispatcher/runtime actually see), not the config
/// crate's richer `McpConfig`.
pub fn resolve_zone(descriptor: &McpDescriptor, bare_tool_name: &str) -> Option<String> {
    match descriptor
        .tool_zones
        .iter()
        .find(|(name, _)| name == bare_tool_name)
    {
        Some((_, zone)) => zone.clone(),
        None => descriptor.default_zone.clone(),
    }
}

/// The zone-write-class guard (dispatch-logic-spec §6 #2): whether a call to `bare_tool_name` on
/// `mcp_name` targets a vault zone whose declared write class doesn't allow a direct agent write
/// (`ProposalOnly`/`HumanOnly`). Returns the restricted zone's name if so, `None` if the call is
/// unrestricted.
///
/// Shared between the dispatcher's pre-flight guard (`liberado-dispatcher/src/guards.rs`, checked
/// against a decision's *seed calls* before anything runs) and the runtime guard
/// (`liberado-executor`'s `RiskGatedToolRuntime`, checked against every adaptive call as it
/// happens) — this is the actual authority boundary; the pre-flight check is a best-effort early
/// warning. Both call sites used to implement this lookup independently; unifying it here is what
/// keeps them from silently drifting apart if the resolution rules ever change.
///
/// An MCP not in `zone_catalog` returns `None` (not a zone-write concern here — the capability
/// guard, checked separately by each caller, already rejects an MCP that isn't granted at all). A
/// tool that hasn't opted into zone tracking (`resolve_zone` returns `None`) is not restricted. A
/// resolved zone absent from `zone_write_classes` fails safe to `WriteClass::default()`
/// (`ProposalOnly`) rather than silently passing.
pub fn zone_write_restriction(
    mcp_name: &str,
    bare_tool_name: &str,
    zone_catalog: &[McpDescriptor],
    zone_write_classes: &[(String, WriteClass)],
) -> Option<String> {
    let descriptor = zone_catalog.iter().find(|d| d.name == mcp_name)?;
    let zone = resolve_zone(descriptor, bare_tool_name)?;
    let write_class = zone_write_classes
        .iter()
        .find(|(z, _)| *z == zone)
        .map(|(_, wc)| *wc)
        .unwrap_or_default();
    (!write_class.allows_direct_agent_write()).then_some(zone)
}

/// A live, queryable capability catalog. Multiple consumers can independently query
/// it. Updates are propagated via a watch channel.
pub struct CapabilityCatalog {
    inner: Arc<RwLock<CatalogState>>,
    updated: tokio::sync::watch::Sender<()>,
}

struct CatalogState {
    mcps: HashMap<String, McpDescriptor>,
    /// MCP names currently known-unhealthy for **routing** (connect/transport failure class),
    /// mapped to the instant they were marked. Cleared by
    /// [`CapabilityCatalog::mark_healthy`] or by half-open expiry
    /// ([`CatalogState::degraded_ttl`]). Authority/zone catalogs still list them.
    degraded: HashMap<String, Instant>,
    /// After this duration past the mark Instant, the peer is half-open again (routing eligible).
    degraded_ttl: Duration,
    last_updated: Instant,
}

impl CapabilityCatalog {
    /// Create a new, empty catalog.
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::watch::channel(());
        Self {
            inner: Arc::new(RwLock::new(CatalogState {
                mcps: HashMap::new(),
                degraded: HashMap::new(),
                degraded_ttl: DEFAULT_DEGRADED_HALF_OPEN,
                last_updated: crate::clock::now(),
            })),
            updated: tx,
        }
    }

    /// Override the M1b half-open window (default [`DEFAULT_DEGRADED_HALF_OPEN`]).
    ///
    /// Tests may set `Duration::ZERO` so the next [`is_degraded`](Self::is_degraded) /
    /// [`routing_descriptors`](Self::routing_descriptors) call expires every mark immediately.
    pub fn set_degraded_half_open_ttl(&self, ttl: Duration) {
        self.inner
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .degraded_ttl = ttl;
    }

    /// Register (or update) an MCP descriptor. Notifies subscribers on change.
    pub fn register(&self, mcp: McpDescriptor) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        state.mcps.insert(mcp.name.clone(), mcp);
        state.last_updated = crate::clock::now();
        let _ = self.updated.send(());
    }

    /// Remove an MCP descriptor by name. No-op if the name is not registered.
    /// Notifies subscribers on change.
    pub fn deregister(&self, name: &str) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        state.mcps.remove(name);
        state.degraded.remove(name);
        state.last_updated = crate::clock::now();
        let _ = self.updated.send(());
    }

    /// Mark `name` degraded for routing after a connect/transport-class failure (M1b).
    /// No-op if the name is not registered. Notifies subscribers.
    ///
    /// The peer is omitted from [`routing_descriptors`](Self::routing_descriptors) until
    /// [`mark_healthy`](Self::mark_healthy) or half-open expiry of the stored mark Instant.
    pub fn mark_degraded(&self, name: &str) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if !state.mcps.contains_key(name) {
            return;
        }
        state.degraded.insert(name.to_string(), crate::clock::now());
        state.last_updated = crate::clock::now();
        let _ = self.updated.send(());
    }

    /// Clear degraded status after a successful connect or successful tool invoke (recovery).
    pub fn mark_healthy(&self, name: &str) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if state.degraded.remove(name).is_some() {
            state.last_updated = crate::clock::now();
            let _ = self.updated.send(());
        }
    }

    /// Drop degraded entries whose mark Instant is older than `degraded_ttl` (half-open).
    /// Returns whether any entry was removed (and subscribers were notified).
    fn purge_expired_degraded(&self) -> bool {
        let now = crate::clock::now();
        {
            let state = self.inner.read().unwrap_or_else(|e| e.into_inner());
            let any_expired = state
                .degraded
                .values()
                .any(|at| now.saturating_duration_since(*at) >= state.degraded_ttl);
            if !any_expired {
                return false;
            }
        }
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let ttl = state.degraded_ttl;
        let before = state.degraded.len();
        state
            .degraded
            .retain(|_, at| now.saturating_duration_since(*at) < ttl);
        if state.degraded.len() != before {
            state.last_updated = crate::clock::now();
            let _ = self.updated.send(());
            true
        } else {
            false
        }
    }

    /// Whether `name` is currently marked degraded for routing (after half-open expiry purge).
    pub fn is_degraded(&self, name: &str) -> bool {
        self.purge_expired_degraded();
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .degraded
            .contains_key(name)
    }

    /// Return a snapshot of all registered descriptors (including degraded peers).
    ///
    /// Use for zone/authority checks. For **dispatcher routing**, prefer
    /// [`routing_descriptors`](Self::routing_descriptors).
    pub fn descriptors(&self) -> Vec<McpDescriptor> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .mcps
            .values()
            .cloned()
            .collect()
    }

    /// Descriptors eligible for dispatcher / classifier routing — **excludes** degraded peers.
    ///
    /// This is the M1b production filter: a peer marked degraded after connect/transport failure
    /// must not appear as a healthy routing target until [`mark_healthy`](Self::mark_healthy)
    /// or half-open expiry of the stored mark Instant.
    pub fn routing_descriptors(&self) -> Vec<McpDescriptor> {
        self.purge_expired_degraded();
        let state = self.inner.read().unwrap_or_else(|e| e.into_inner());
        state
            .mcps
            .values()
            .filter(|d| !state.degraded.contains_key(&d.name))
            .cloned()
            .collect()
    }

    /// Look up a single descriptor by name.
    pub fn get(&self, name: &str) -> Option<McpDescriptor> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .mcps
            .get(name)
            .cloned()
    }

    /// Subscribe to catalog changes. The returned receiver yields `()` every time
    /// the catalog is modified (register/deregister).
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<()> {
        self.updated.subscribe()
    }

    /// Whether the catalog is currently empty.
    pub fn is_empty(&self) -> bool {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .mcps
            .is_empty()
    }

    /// Number of registered descriptors.
    pub fn len(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .mcps
            .len()
    }

    /// `(mcp_name, consequence)` pairs for every registered descriptor — the shape
    /// `RiskGatedToolRuntime`'s consequence check and `Orchestrator`'s runtime-level gate consume.
    pub fn consequence_catalog(&self) -> Vec<(String, Consequence)> {
        self.descriptors()
            .iter()
            .map(|d| (d.name.clone(), d.consequence))
            .collect()
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
            provenance: None,
            ..Default::default()
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
            provenance: None,
            ..Default::default()
        });

        // Re-register with updated fields.
        catalog.register(McpDescriptor {
            name: "mcp".into(),
            description: "v2".into(),
            consequence: Consequence::External,
            provenance: None,
            ..Default::default()
        });

        let desc = catalog.get("mcp").unwrap();
        assert_eq!(desc.description, "v2");
        assert_eq!(desc.consequence, Consequence::External);
    }

    // ── M1b degraded / routing_descriptors ────────────────────────────────

    #[test]
    fn degraded_peer_is_omitted_from_routing_but_stays_in_full_descriptors() {
        let catalog = CapabilityCatalog::new();
        catalog.register(sample_descriptor("weather"));
        catalog.register(sample_descriptor("tasks"));

        catalog.mark_degraded("weather");
        assert!(catalog.is_degraded("weather"));
        assert!(!catalog.is_degraded("tasks"));

        let full: Vec<_> = catalog.descriptors().into_iter().map(|d| d.name).collect();
        assert!(full.contains(&"weather".into()) && full.contains(&"tasks".into()));

        let routing: Vec<_> = catalog
            .routing_descriptors()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert!(
            !routing.contains(&"weather".into()),
            "degraded peer must not be a routing target: {routing:?}"
        );
        assert!(
            routing.contains(&"tasks".into()),
            "healthy peer must remain routable: {routing:?}"
        );
    }

    #[test]
    fn mark_healthy_restores_routing_eligibility() {
        let catalog = CapabilityCatalog::new();
        catalog.register(sample_descriptor("weather"));
        catalog.mark_degraded("weather");
        assert!(catalog.routing_descriptors().is_empty());

        catalog.mark_healthy("weather");
        assert!(!catalog.is_degraded("weather"));
        let routing: Vec<_> = catalog
            .routing_descriptors()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(routing, vec!["weather".to_string()]);
    }

    #[test]
    fn mark_degraded_unknown_name_is_noop() {
        let catalog = CapabilityCatalog::new();
        catalog.mark_degraded("ghost");
        assert!(!catalog.is_degraded("ghost"));
        assert!(catalog.routing_descriptors().is_empty());
    }

    #[test]
    fn half_open_ttl_reincludes_peer_in_routing() {
        let catalog = CapabilityCatalog::new();
        catalog.register(sample_descriptor("weather"));
        catalog.mark_degraded("weather");
        assert!(catalog.is_degraded("weather"));
        assert!(
            catalog.routing_descriptors().is_empty(),
            "still hard-closed within half-open window"
        );

        // Zero TTL: any mark is already expired on the next purge.
        catalog.set_degraded_half_open_ttl(Duration::ZERO);
        assert!(
            !catalog.is_degraded("weather"),
            "half-open must clear degraded after TTL"
        );
        let routing: Vec<_> = catalog
            .routing_descriptors()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(
            routing,
            vec!["weather".to_string()],
            "half-open peer must re-enter routing so recovery is designed, not accidental"
        );
    }

    // ── zone_write_restriction ────────────────────────────────────────────
    // Shared between the dispatcher's pre-flight guard and RiskGatedToolRuntime — see the
    // function's own doc comment. Each individual case here used to live duplicated (in spirit)
    // across both call sites' own test suites; it's tested once, here, now.

    fn vault_descriptor() -> McpDescriptor {
        McpDescriptor {
            name: "vault".into(),
            description: "git-tracked vault".into(),
            consequence: Consequence::Reversible,
            provenance: None,
            default_zone: Some("tasks".into()),
            tool_zones: vec![("write_review".into(), Some("reviews".into()))],
            zone_from_arg: None,
            write_tools: Vec::new(),
        }
    }

    /// A path-addressed MCP: one write tool that can land in any zone, plus a read that must NOT be
    /// made to look like a write just because it also carries a path.
    fn path_addressed_descriptor() -> McpDescriptor {
        McpDescriptor {
            name: "turbovault".into(),
            description: "path-addressed vault".into(),
            consequence: Consequence::Reversible,
            provenance: None,
            default_zone: None,
            tool_zones: Vec::new(),
            zone_from_arg: Some("path".into()),
            write_tools: vec!["write_note".into(), "delete_note".into()],
        }
    }

    #[test]
    fn a_path_addressed_write_takes_its_zone_from_the_argument() {
        // The whole reason `write_target` exists: `default_zone` cannot describe this MCP. The SAME
        // tool writes to a different zone on every call, so a fixed declaration would authorize a
        // write to `decisions/` under a `Write(tasks)` capability.
        let d = path_addressed_descriptor();
        assert_eq!(
            write_target(
                &d,
                "write_note",
                &serde_json::json!({"path": "decisions/x.md"})
            ),
            WriteTarget::Zone("decisions".into())
        );
        assert_eq!(
            write_target(&d, "write_note", &serde_json::json!({"path": "tasks/y.md"})),
            WriteTarget::Zone("tasks".into())
        );
    }

    #[test]
    fn a_read_on_a_path_addressed_mcp_is_not_a_write() {
        // `read_note` also carries a path. Without `write_tools`, it would demand a Write capability.
        let d = path_addressed_descriptor();
        assert_eq!(
            write_target(
                &d,
                "read_note",
                &serde_json::json!({"path": "finance/secret.md"})
            ),
            WriteTarget::NotAWrite
        );
    }

    #[test]
    fn a_write_whose_zone_cannot_be_determined_fails_closed() {
        // A write we cannot place is a write we cannot authorize. Collapsing this into "not a write"
        // is exactly the bug F1 was — a guard that says nothing when it does not know.
        let d = path_addressed_descriptor();
        assert!(matches!(
            write_target(&d, "write_note", &serde_json::json!({})),
            WriteTarget::Undeterminable(_)
        ));
        assert!(
            matches!(
                write_target(&d, "write_note", &serde_json::json!({"path": "loose.md"})),
                WriteTarget::Undeterminable(_)
            ),
            "a bare filename names no zone"
        );
    }

    #[test]
    fn restricted_zone_is_flagged() {
        let restriction = zone_write_restriction(
            "vault",
            "write_review",
            &[vault_descriptor()],
            &[("reviews".to_string(), WriteClass::ProposalOnly)],
        );
        assert_eq!(restriction, Some("reviews".to_string()));
    }

    #[test]
    fn agent_writable_zone_is_not_restricted() {
        let restriction = zone_write_restriction(
            "vault",
            "write_review",
            &[vault_descriptor()],
            &[("reviews".to_string(), WriteClass::AgentWritable)],
        );
        assert_eq!(restriction, None);
    }

    #[test]
    fn unlisted_zone_fails_safe_to_restricted() {
        // "reviews" isn't in zone_write_classes at all — must fail safe (ProposalOnly default),
        // not silently pass just because nothing was configured for it.
        let restriction = zone_write_restriction(
            "vault",
            "write_review",
            &[vault_descriptor()],
            &[("tasks".to_string(), WriteClass::AgentWritable)],
        );
        assert_eq!(restriction, Some("reviews".to_string()));
    }

    #[test]
    fn mcp_not_in_catalog_is_not_restricted() {
        // The capability guard (checked separately by each caller) already rejects an ungranted
        // MCP — this function isn't the place that catches that.
        let restriction = zone_write_restriction(
            "some-other-mcp",
            "anything",
            &[vault_descriptor()],
            &[("reviews".to_string(), WriteClass::ProposalOnly)],
        );
        assert_eq!(restriction, None);
    }

    #[test]
    fn tool_not_opted_into_zone_tracking_is_not_restricted() {
        // "tasks-mcp" has no default_zone/tool_zones at all, so resolve_zone returns None
        // regardless of what's restricted elsewhere.
        let untracked = McpDescriptor {
            name: "tasks-mcp".into(),
            description: "task ops".into(),
            consequence: Consequence::Reversible,
            provenance: None,
            ..Default::default()
        };
        let restriction = zone_write_restriction(
            "tasks-mcp",
            "add",
            &[untracked],
            &[("reviews".to_string(), WriteClass::ProposalOnly)],
        );
        assert_eq!(restriction, None);
    }

    #[test]
    fn consequence_catalog_returns_all_consequences() {
        let catalog = CapabilityCatalog::new();
        let d1 = McpDescriptor {
            name: "weather".into(),
            consequence: Consequence::ReadOnly,
            ..sample_descriptor("weather")
        };
        let d2 = McpDescriptor {
            name: "email".into(),
            consequence: Consequence::External,
            ..sample_descriptor("email")
        };
        catalog.register(d1);
        catalog.register(d2);

        let catalog_result = catalog.consequence_catalog();
        assert_eq!(catalog_result.len(), 2);
        assert!(catalog_result.contains(&("weather".into(), Consequence::ReadOnly)));
        assert!(catalog_result.contains(&("email".into(), Consequence::External)));
    }

    #[test]
    fn write_target_empty_segment_is_not_a_zone() {
        let d = path_addressed_descriptor();
        // path starting with `//` → first split segments are empty.
        // The correct `&&` skips empties to find "foo". A `||` mutation would match the
        // empty string first and return `Zone("")` — a zone we cannot authorize.
        let result = write_target(
            &d,
            "write_note",
            &serde_json::json!({"path": "//foo/bar.md"}),
        );
        assert_eq!(
            result,
            WriteTarget::Zone("foo".into()),
            "empty segments must be skipped to find the real zone: got {result:?}"
        );
    }

    /// When purge_expired_degraded removes entries, it should send a notification. The
    /// `!=` → `==` mutation on line 300 inverts this check.
    #[test]
    fn degraded_purge_sends_notification_on_expiry() {
        let catalog = CapabilityCatalog::new();
        catalog.register(sample_descriptor("weather"));
        catalog.mark_degraded("weather");

        // Subscribe after mark_degraded — rx sees the latest value.
        let rx = catalog.subscribe();

        catalog.set_degraded_half_open_ttl(Duration::ZERO);
        // is_degraded calls purge_expired_degraded internally.
        // With correct `!=`, it sends a notification after removing entries.
        let _ = catalog.is_degraded("weather");

        assert!(
            rx.has_changed().unwrap(),
            "purge must notify subscribers when entries are removed"
        );
    }

    /// Freeze time, mark degraded, advance by exactly TTL — verifies the half-open boundary.
    #[test]
    fn degraded_entry_purged_at_exact_ttl_boundary() {
        let catalog = CapabilityCatalog::new();
        catalog.register(sample_descriptor("weather"));

        let t0 = std::time::Instant::now();
        crate::clock::test_freeze_at(t0);
        catalog.mark_degraded("weather");
        assert!(catalog.is_degraded("weather"));

        let ttl = Duration::from_secs(5);
        catalog.set_degraded_half_open_ttl(ttl);
        crate::clock::test_advance(ttl);

        assert!(
            !catalog.is_degraded("weather"),
            "must be purged at exact TTL boundary"
        );

        crate::clock::test_thaw();
    }
}
