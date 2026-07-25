//! Policy section: zones and grants.

use liberado_common::{Capability, CapabilitySet, WriteClass};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Policy — the central, auditable security surface (Decision 4 / 5).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    /// Per-zone write classes. An *unlisted* zone is treated as `proposal_only` (fail safe).
    pub zones: Vec<ZonePolicy>,
    /// Base capability grants per component (narrowed, never widened, at dispatch).
    pub grants: Vec<Grant>,
    /// Names of secrets referenced by components (resolved from env/systemd, never inlined).
    pub secret_refs: Vec<String>,
}

impl Policy {
    /// The write class declared for `zone`, or the fail-safe default if unlisted.
    pub fn write_class(&self, zone: &str) -> WriteClass {
        self.zones
            .iter()
            .find(|z| z.zone == zone)
            .map(|z| z.write_class)
            .unwrap_or_default()
    }

    /// The capability set granted to `component` — the union of every [`Grant`] whose `component`
    /// matches, narrowed to just that slice of authority (this narrowing is itself the ceiling; a
    /// dispatch further narrows within it, never outside it — Decision 4). Two components are
    /// meaningful today: `"main-agent"` (the chat-facing tool surface, `ChatSessions`) and
    /// `"dispatcher"` (the ceiling the guard pipeline and `ExecuteDirect`/`DispatchSubagent`
    /// execution check against, `configure_daemon`). A grant can list either, both, or neither —
    /// an MCP granted only to `"dispatcher"` is reachable via dispatch-routed execution but never
    /// appears directly in chat.
    pub fn capabilities_for(&self, component: &str) -> CapabilitySet {
        self.grants
            .iter()
            .filter(|g| g.component == component)
            .flat_map(|g| g.capabilities.iter().cloned())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZonePolicy {
    /// Zone name (a vault folder, or a named external zone).
    pub zone: String,
    pub write_class: WriteClass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    /// Component the grant applies to (MCP/hook/subagent role name).
    pub component: String,
    pub capabilities: Vec<Capability>,
}
