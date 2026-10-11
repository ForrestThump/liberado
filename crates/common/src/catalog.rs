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
    /// Path-addressed MCPs: tools that are **explicitly read-only** by declaration.
    ///
    /// Why this exists alongside `write_tools` and `vault_wide_writes`: a path-addressed MCP like
    /// TurboVault often grows new tools in upgrades, and `write_tools` is a **deny** list ("only
    /// these names write"). Unlisted tools were treated as `NotAWrite` — fine when the server
    /// itself is the source of truth, **unsafe** when the operator's config is the source of truth
    /// and the live upgrade outpaces a `write_tools` refresh: an undeclared writer gets a free
    /// pass. The live bug `01M3ZB89WK54HGNG6ATJ9FPJMX` (2026-10-11) was exactly that: `edit_note`
    /// was added to TurboVault, the deployed `topology.toml` did not include it in `write_tools`,
    /// and the agent's `Write(Vault("conformance"))`-less grant sailed through with no approval
    /// card. Therefore: every tool on a path-addressed MCP must now appear in **one** of
    /// `write_tools`, `read_tools`, or `vault_wide_writes`; tools in **none** are `Undeterminable`
    /// and fail closed at the runtime gate.
    pub read_tools: Vec<String>,
    /// Path-addressed MCPs: tools that mutate vault-wide state without a single target zone
    /// (`begin_fanout` / `commit_fanout` / `abandon_fanout`, `set_active_vault`,
    /// `vector_reindex`, …). The gate treats these as writes that need any zone write authority
    /// (a per-tool grant or `Write(*)`); a session holding only reads cannot start a fanout.
    pub vault_wide_writes: Vec<String>,
}

/// What a tool call writes to, resolved against its MCP's declaration *and its arguments*.
///
/// Deliberately a multi-state answer, not `Option<String>`: "this is a write whose zone I cannot
/// determine" is a real, distinct outcome (a path-addressed write tool called with no path), and it
/// must **not** collapse into "not a write". That collapse is exactly the class of bug F1 was — a
/// guard that says nothing when it does not know, and is therefore silently absent.
///
/// The four- (now five-) state split separates **read vs unknown vs write** so each one can be
/// handled distinctly:
/// - `NotAWrite` — confirmed read, no authority check needed.
/// - `Zone(z)` — write to one zone, check `Write(z)`.
/// - `VaultWide` — write to the vault as a whole, check any zone write.
/// - `Undeterminable(why)` — could not classify (no path arg, undeclared tool); fail closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteTarget {
    /// Not a zone write: a declared read, or an MCP that writes nothing.
    NotAWrite,
    /// Writes to this zone. The caller must hold `Capability::Write(Zone::vault(zone))`.
    Zone(String),
    /// Writes to vault-wide state with no single target zone (fanout, vault switch,
    /// index rebuild). The caller must hold a `Capability::Write(Zone)` for **some** zone, or
    /// a per-tool grant for this exact tool.
    VaultWide,
    /// It *is* a write, but the target zone could not be determined — an undeclared tool on a
    /// path-addressed MCP, a missing/empty path arg on a declared writer. **Fail closed** —
    /// refuse, with `0` explaining the gap and how to close it.
    Undeterminable(String),
}

/// Resolve what `bare_tool_name` on `descriptor` writes to, given the call's `arguments`.
///
/// Two declaration styles, because two kinds of MCP exist:
///
/// * **Fixed-zone** (a tasks MCP whose every tool touches `tasks/`): `default_zone` + per-tool
///   `tool_zones` overrides. Zone depends only on the tool name.
/// * **Path-addressed** (TurboVault): `zone_from_arg` + a **complete** classification of every
///   known tool in one of `write_tools`, `read_tools`, or `vault_wide_writes`. Zone depends on
///   the call's arguments for the writers, and is independent for the other two classes.
///
/// The path-addressed case changed shape on 2026-10-11 (live bug `01M3ZB89WK54HGNG6ATJ9FPJMX`):
/// before, a tool **not** listed in `write_tools` was silently classified as `NotAWrite`, so a
/// declared write tool the operator forgot to list (e.g. `edit_note` after a turbovault upgrade)
/// passed every guard as a read. The new rule is fail-closed: tools in none of the three lists
/// are `Undeterminable("undeclared tool…")`, refuse the call, and the error message names the
/// MCP/tool and the three lists the operator must classify it under. This means a stale config
/// can no longer silently reopen the hole — the worst case is a refused call with an actionable
/// message instead of a silent write.
pub fn write_target(
    descriptor: &McpDescriptor,
    bare_tool_name: &str,
    arguments: &serde_json::Value,
) -> WriteTarget {
    // Path-addressed MCP — the classification is exhaustive: every tool must appear in exactly one
    // of `write_tools`, `read_tools`, `vault_wide_writes`. The fixed-zone branch below still
    // resolves tools not listed in `tool_zones` through `default_zone` / `None` — that is the
    // long-standing behavior for fixed-zone MCPs and is preserved here.
    if let Some(arg) = &descriptor.zone_from_arg {
        // Vault-wide writes are not path-addressed: the tool mutates vault state without naming
        // a single zone. Check this first so a tool that happens to carry a path arg AND mutate
        // vault state is treated correctly.
        if descriptor
            .vault_wide_writes
            .iter()
            .any(|t| t == bare_tool_name)
        {
            return WriteTarget::VaultWide;
        }
        // Explicit read: not a write. `read_tools` is opt-in — most deployments use the
        // fixed-zone `tool_zones` for reads, and on a path-addressed MCP the deny-by-default rule
        // means an operator who doesn't list a tool is alerted rather than waved through.
        if descriptor.read_tools.iter().any(|t| t == bare_tool_name) {
            return WriteTarget::NotAWrite;
        }
        // Declared writer: resolve the zone from the path arg, fail closed if the arg is missing
        // or doesn't name a zone.
        if descriptor.write_tools.iter().any(|t| t == bare_tool_name) {
            let Some(path) = arguments.get(arg).and_then(|v| v.as_str()) else {
                return WriteTarget::Undeterminable(format!(
                    "'{bare_tool_name}' writes, and its zone comes from the '{arg}' argument, \
                     which is missing or not a string"
                ));
            };
            // The zone is the leading path segment: `tasks/foo.md` -> `tasks`. A bare filename
            // has no zone, and a write with no zone is a write we cannot authorize.
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
        // Undeclared tool on a path-addressed MCP — fail closed. The denial text names the three
        // lists so the operator knows exactly where to add the tool.
        return WriteTarget::Undeterminable(format!(
            "'{bare_tool_name}' on '{}' is undeclared — name it in one of `write_tools`, \
             `read_tools`, or `vault_wide_writes` (in topology.toml) before this MCP can be \
             called; silently treating it as a read is the F1 bug",
            descriptor.name
        ));
    }

    // Fixed-zone MCP (or no zone tracking at all). Existing behavior preserved: a tool resolves
    // through `resolve_zone`, which returns `None` for tools the MCP has not opted into zone
    // tracking for — and `None` here means "not a zone-write concern", not "unknown write", so
    // `NotAWrite` is the correct answer.
    match resolve_zone(descriptor, bare_tool_name) {
        Some(zone) => WriteTarget::Zone(zone),
        None => WriteTarget::NotAWrite,
    }
}

/// Whether this call names **exactly one** write target, known from the declaration rather than
/// guessed from text.
///
/// True only for the **path-addressed** style (`zone_from_arg` + `write_tools`) with the path
/// argument actually present and naming a zone. There, the call carries its own target: one path,
/// one note, bounded reach — and no amount of prose in the payload changes that.
///
/// False for the **fixed-zone** style, deliberately. There the zone comes from the *tool name*, so
/// a resolved [`WriteTarget::Zone`] says which zone is touched but nothing about how much of it —
/// "delete everything in tasks/" resolves the same as "write one task". Reach genuinely is unknown,
/// and the text heuristic remains the only signal available.
///
/// # Why this exists
///
/// [`Magnitude`](crate::Magnitude) measures *reach*, and reach is usually structural. The risk gate
/// was estimating it by scanning the whole arguments blob for destructive words, which for a write
/// tool means scanning **the content being saved** — so a research report discussing organizations
/// being "destroyed" and coalitions blocking "every" reform read as an order to delete everything,
/// and a requested write-up became a permission prompt (homelab, `prop-1785557626819756862`,
/// 2026-08-01). The payload is data, not an instruction; this is how the gate tells them apart.
pub fn names_single_write_target(
    descriptor: &McpDescriptor,
    bare_tool_name: &str,
    arguments: &serde_json::Value,
) -> bool {
    descriptor.zone_from_arg.is_some()
        && matches!(
            write_target(descriptor, bare_tool_name, arguments),
            WriteTarget::Zone(_)
        )
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
            ..Default::default()
        }
    }

    /// A path-addressed MCP: one write tool that can land in any zone, plus a read that must NOT
    /// be made to look like a write just because it also carries a path.
    ///
    /// `read_note` is now declared in `read_tools` — pre-2026-10-11 the unlisted-read path
    /// returned `NotAWrite` directly, which is the correct answer for THIS descriptor shape (it
    /// has a `read_tools` list and `read_note` is in it). Tests that exercise the *undeclared*
    /// path now build their own descriptor or call `write_target` on a tool name no list covers.
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
            read_tools: vec!["read_note".into()],
            ..Default::default()
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

    // The 2026-10-11 live bug pinned here. This is the regression test for the hole:
    // an undeclared tool on a path-addressed MCP was silently treated as a read, so an
    // `edit_note` call (added to turbovault since the deployed topology was written) sailed
    // through every gate under any grant that held `ExecuteMcp("turbovault")`. The new rule
    // fails closed, naming the MCP, the tool, and the three lists the operator must use to
    // classify it.
    #[test]
    fn an_undeclared_tool_on_a_path_addressed_mcp_fails_closed_with_a_remediation_hint() {
        let d = path_addressed_descriptor();
        let result = write_target(&d, "edit_note", &serde_json::json!({"path": "X/y.md"}));
        match result {
            WriteTarget::Undeterminable(why) => {
                assert!(
                    why.contains("edit_note") && why.contains("turbovault"),
                    "the refusal must name the tool and the MCP: {why}"
                );
                assert!(
                    why.contains("write_tools")
                        && why.contains("read_tools")
                        && why.contains("vault_wide_writes"),
                    "the refusal must point at the three classification lists: {why}"
                );
            }
            other => panic!("undeclared tool must be Undeterminable, got {other:?}"),
        }

        // And the no-arg shape: an undeclared writer called with NO path arg is also
        // Undeterminable, with the same actionable message.
        let no_path = write_target(&d, "edit_note", &serde_json::json!({}));
        assert!(
            matches!(no_path, WriteTarget::Undeterminable(_)),
            "an undeclared tool is undeclared regardless of its arguments: {no_path:?}"
        );
    }

    #[test]
    fn a_vault_wide_write_resolves_to_vaultwide_not_zone() {
        // `begin_fanout` mutates git/fanout state, not a single note. There is no zone to resolve
        // from arguments, and `VaultWide` is the third class of path-addressed writer.
        let mut d = path_addressed_descriptor();
        d.vault_wide_writes = vec!["begin_fanout".into(), "commit_fanout".into()];
        let result = write_target(&d, "begin_fanout", &serde_json::json!({}));
        assert_eq!(
            result,
            WriteTarget::VaultWide,
            "a fanout mutator is VaultWide, not Zone or Undeterminable: {result:?}"
        );
        assert_eq!(
            write_target(&d, "commit_fanout", &serde_json::json!({})),
            WriteTarget::VaultWide,
        );
        // Sanity: `write_note` on the same MCP still resolves its zone from the path arg, because
        // the three classifications do NOT swallow the original write-tool resolution.
        assert_eq!(
            write_target(&d, "write_note", &serde_json::json!({"path": "tasks/x.md"})),
            WriteTarget::Zone("tasks".into())
        );
    }

    #[test]
    fn a_tool_listed_explicitly_as_read_is_not_a_write() {
        // `read_tools` is the explicit read declaration. A tool named there is `NotAWrite` even
        // when the same descriptor has `write_tools` — the lists are independent.
        let d = path_addressed_descriptor();
        assert_eq!(
            write_target(&d, "read_note", &serde_json::json!({"path": "X/y.md"})),
            WriteTarget::NotAWrite,
        );
    }

    #[test]
    fn fixed_zone_mcp_unchanged_an_unlisted_tool_is_not_a_write() {
        // Fixed-zone MCPs retain the original behavior: a tool with no zone mapping (no entry in
        // `tool_zones`, no `default_zone` to inherit) resolves to `NotAWrite` rather than
        // `Undeterminable`. This is a deliberate carve-out because fixed-zone MCPs use
        // `default_zone` for an MCP-wide declaration, not a deny-list — a fixed-zone MCP that
        // hasn't opted a tool into zone tracking is "this tool does not write a vault zone."
        let d = McpDescriptor {
            name: "tasks-mcp".into(),
            description: "task ops".into(),
            consequence: Consequence::Reversible,
            provenance: None,
            default_zone: None, // explicitly: this MCP has no zone tracking
            ..Default::default()
        };
        assert_eq!(
            write_target(&d, "anything", &serde_json::json!({})),
            WriteTarget::NotAWrite,
        );
    }

    // The proptest invariant: a path-addressed MCP's classification is exhaustive (every tool in
    // exactly one of write_tools/read_tools/vault_wide_writes classifies; unclassified is
    // `Undeterminable`). This is what makes the 2026-10-11 bug structurally impossible: an upgrade
    // that adds a new tool to turbovault without updating the operator's topology.toml surfaces
    // as `Undeterminable` at the gate, never as `NotAWrite`.
    #[test]
    fn a_path_addressed_classification_is_exhaustive() {
        let d = path_addressed_descriptor();
        // Every tool in write_tools: resolves to Zone or Undeterminable (depending on path arg).
        for tool in &d.write_tools {
            let r = write_target(&d, tool, &serde_json::json!({"path": "tasks/x.md"}));
            assert!(
                matches!(r, WriteTarget::Zone(_)),
                "writer {tool} with path arg must be Zone: {r:?}"
            );
        }
        // Every tool in read_tools: NotAWrite, regardless of arguments.
        for tool in &d.read_tools {
            let r = write_target(&d, tool, &serde_json::json!({"path": "tasks/x.md"}));
            assert!(
                matches!(r, WriteTarget::NotAWrite),
                "reader {tool} with path arg must be NotAWrite: {r:?}"
            );
        }
        // Every tool in vault_wide_writes: VaultWide, regardless of arguments.
        for tool in &d.vault_wide_writes {
            let r = write_target(&d, tool, &serde_json::json!({}));
            assert!(
                matches!(r, WriteTarget::VaultWide),
                "vault-wide writer {tool} must be VaultWide: {r:?}"
            );
        }
        // A tool in NONE of the three: Undeterminable (fail closed).
        let r = write_target(
            &d,
            "brand_new_tool_added_in_a_future_release",
            &serde_json::json!({}),
        );
        assert!(
            matches!(r, WriteTarget::Undeterminable(_)),
            "an undeclared tool must be Undeterminable: {r:?}"
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
        let clock = crate::clock::test_freeze_at(t0);
        catalog.mark_degraded("weather");
        assert!(catalog.is_degraded("weather"));

        let ttl = Duration::from_secs(5);
        catalog.set_degraded_half_open_ttl(ttl);
        clock.advance(ttl);

        assert!(
            !catalog.is_degraded("weather"),
            "must be purged at exact TTL boundary"
        );
    }
}

#[cfg(test)]
mod proptest_tests {
    use std::slice;

    use proptest::prelude::*;
    use serde_json::Value;

    use super::*;

    // ── Generation primitives ─────────────────────────────────────────────

    /// A tool name as it appears in `write_tools` / `tool_zones` keys.
    const TOOL_NAME: &str = "[a-zA-Z0-9_/-]{1,20}";

    /// A vault zone name: a plain 1–20 char alphanumeric string.
    const ZONE_NAME: &str = "[a-zA-Z0-9]{1,20}";

    /// A zone-name wrapper so generation carries intent. `McpDescriptor` stores zones as plain
    /// `String`; this newtype only exists to give the strategy a name.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Zone(String);

    impl Arbitrary for Zone {
        type Parameters = ();
        type Strategy = BoxedStrategy<Zone>;

        fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
            ZONE_NAME.prop_map(Zone).boxed()
        }
    }

    impl Arbitrary for WriteClass {
        type Parameters = ();
        type Strategy = BoxedStrategy<WriteClass>;

        fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
            prop_oneof![
                Just(WriteClass::HumanOnly),
                Just(WriteClass::AgentWritable),
                Just(WriteClass::ProposalOnly),
                Just(WriteClass::Shared),
            ]
            .boxed()
        }
    }

    impl Arbitrary for McpDescriptor {
        type Parameters = ();
        type Strategy = BoxedStrategy<McpDescriptor>;

        fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
            // `write_tools` is only consulted by `write_target` inside the `zone_from_arg`
            // (path-addressed) branch. A fixed-zone descriptor (`zone_from_arg: None`) ignores
            // `write_tools` and resolves through `resolve_zone`, which reports `NotAWrite` for a
            // tool with no default/tool zone. Correlating the two keeps "a declared write tool
            // is never `NotAWrite`" a genuine invariant rather than a vacuous one.
            let zone_from_arg_strategy = prop_oneof![
                2 => Just(None::<String>),
                3 => "[a-zA-Z0-9_-]{1,20}".prop_map(Some),
            ];

            zone_from_arg_strategy
                .prop_flat_map(|zone_from_arg| {
                    let write_tools: BoxedStrategy<Vec<String>> = match &zone_from_arg {
                        Some(_) => proptest::collection::vec(TOOL_NAME, 0..5).boxed(),
                        None => Just(Vec::<String>::new()).boxed(),
                    };
                    let read_tools: BoxedStrategy<Vec<String>> = match &zone_from_arg {
                        Some(_) => proptest::collection::vec(TOOL_NAME, 0..5).boxed(),
                        None => Just(Vec::<String>::new()).boxed(),
                    };
                    let vault_wide_writes: BoxedStrategy<Vec<String>> = match &zone_from_arg {
                        Some(_) => proptest::collection::vec(TOOL_NAME, 0..3).boxed(),
                        None => Just(Vec::<String>::new()).boxed(),
                    };
                    (
                        "[a-zA-Z0-9_-]{1,20}",  // name
                        "[a-zA-Z0-9_ -]{0,40}", // description
                        prop_oneof![
                            1 => Just(None::<String>),
                            2 => any::<Zone>().prop_map(|z| Some(z.0)),
                        ],
                        proptest::collection::vec(
                            (
                                TOOL_NAME,
                                prop_oneof![
                                    1 => Just(None::<String>),
                                    2 => any::<Zone>().prop_map(|z| Some(z.0)),
                                ],
                            ),
                            0..3,
                        ),
                        write_tools,
                        read_tools,
                        vault_wide_writes,
                    )
                        .prop_map(
                            move |(
                                name,
                                description,
                                default_zone,
                                tool_zones,
                                write_tools,
                                read_tools,
                                vault_wide_writes,
                            )| {
                                McpDescriptor {
                                    name,
                                    description,
                                    consequence: Consequence::Reversible,
                                    provenance: None,
                                    default_zone,
                                    tool_zones,
                                    zone_from_arg: zone_from_arg.clone(),
                                    write_tools,
                                    read_tools,
                                    vault_wide_writes,
                                }
                            },
                        )
                        .boxed()
                })
                .boxed()
        }
    }

    /// A `serde_json::Value` strategy. `any::<serde_json::Value>()` requires proptest's optional
    /// `serde_json` feature (not enabled here), so build one by hand: scalar leaves plus
    /// recursive arrays/objects. String leaves deliberately allow `/` and `.` so `write_target`'s
    /// path-segment logic is exercised.
    fn arb_json_value() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(Value::from),
            any::<f64>()
                .prop_filter("finite", |f| f.is_finite())
                .prop_map(Value::from),
            "[a-zA-Z0-9_/.-]{0,30}".prop_map(Value::String),
        ];
        leaf.prop_recursive(4, 16, 8, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
                proptest::collection::hash_map("[a-zA-Z0-9_-]{0,12}", inner, 0..5).prop_map(|m| {
                    let mut map = serde_json::Map::new();
                    for (k, v) in m {
                        map.insert(k, v);
                    }
                    Value::Object(map)
                },),
            ]
        })
    }

    /// A write class that does **not** allow direct agent writes. Test 2's guard-equivalence
    /// property asserts `(Some(zone), Some(zone))` for every resolved zone — the fail-closed
    /// behavior this guard is specified to have. The "agent may write, so no restriction"
    /// outcome is a separate behavior already pinned by the unit tests in `tests`.
    fn restricted_write_class() -> impl Strategy<Value = WriteClass> {
        prop_oneof![Just(WriteClass::HumanOnly), Just(WriteClass::ProposalOnly)]
    }

    // ── Test 1: write_target never collapses a declared write tool to NotAWrite ──

    fn write_target_never_notawrite(desc: McpDescriptor, tool: String, args: Value) -> bool {
        if desc.write_tools.contains(&tool) {
            !matches!(write_target(&desc, &tool, &args), WriteTarget::NotAWrite)
        } else {
            true
        }
    }

    proptest! {
        #[test]
        fn proptest_write_target_never_notawrite(
            desc in any::<McpDescriptor>(),
            tool in TOOL_NAME,
            args in arb_json_value(),
        ) {
            prop_assert!(write_target_never_notawrite(desc, tool, args));
        }
    }

    // ── Test 2: zone_write_restriction agrees with resolve_zone ──────────

    /// `zone_write_restriction` looks its descriptor up by name in a *slice* catalog; the spec
    /// check passes the single generated descriptor as that one-entry catalog
    /// (`std::slice::from_ref`, adapting the real `&[McpDescriptor]` signature).
    fn zone_write_restriction_agrees(
        mcp: String,
        tool: String,
        desc: McpDescriptor,
        zone_classes: Vec<(String, WriteClass)>,
    ) -> bool {
        let zone = resolve_zone(&desc, &tool);
        let r = zone_write_restriction(&mcp, &tool, slice::from_ref(&desc), &zone_classes);
        match (zone, r) {
            (None, None) => true,
            (Some(z), Some(rz)) => z == rz,
            _ => false,
        }
    }

    proptest! {
        #[test]
        fn proptest_zone_write_restriction_agrees_with_resolve_zone(
            // `mcp` must name the descriptor actually in the catalog, or the lookup is a
            // no-op and the equivalence degenerates to "always `None`".
            (mcp, desc) in any::<McpDescriptor>()
                .prop_map(|desc| (desc.name.clone(), desc)),
            tool in TOOL_NAME,
            // Restricted classes only, so a resolved zone always yields `Some(zone)` — exactly
            // the `(Some(z), Some(rz))` arm.
            zone_classes in proptest::collection::vec(
                (TOOL_NAME, restricted_write_class()),
                0..4,
            ),
        ) {
            prop_assert!(zone_write_restriction_agrees(mcp, tool, desc, zone_classes));
        }
    }

    // ── Test 3: classification is exhaustive for path-addressed MCPs ───────────────
    //
    // The 2026-10-11 live bug, pinned at the type level: a tool not in write_tools was NotAWrite
    // (fail OPEN). The new rule is "fail CLOSED on undeclared tools" — every tool resolves to
    // one of NotAWrite / Zone / VaultWide / Undeterminable, and Undeterminable is the answer for
    // any tool not in any of the three classification lists.
    fn classification_is_exhaustive(desc: McpDescriptor, tool: String, args: Value) -> bool {
        if desc.zone_from_arg.is_none() {
            // Fixed-zone MCPs keep the older "NotAWrite for the unlisted" semantics.
            return matches!(
                write_target(&desc, &tool, &args),
                WriteTarget::NotAWrite | WriteTarget::Zone(_)
            );
        }
        // Path-addressed: any of the four outcomes is acceptable; the structural property is
        // that NotAWrite is reachable only via the read_tools list — never by accident through
        // omission from write_tools.
        match write_target(&desc, &tool, &args) {
            WriteTarget::NotAWrite => {
                // Read classification must come from the explicit list.
                desc.read_tools.contains(&tool)
            }
            WriteTarget::VaultWide => desc.vault_wide_writes.contains(&tool),
            WriteTarget::Zone(_) | WriteTarget::Undeterminable(_) => true,
        }
    }

    proptest! {
        #[test]
        fn proptest_path_addressed_classification_is_exhaustive(
            desc in any::<McpDescriptor>(),
            tool in TOOL_NAME,
            args in arb_json_value(),
        ) {
            prop_assert!(classification_is_exhaustive(desc, tool, args));
        }
    }
}
