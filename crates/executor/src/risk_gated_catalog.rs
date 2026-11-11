//! MCP catalog lookups for [`super::RiskGatedToolRuntime`] (consequence + descriptor resolution).

use liberado_common::{Consequence, McpDescriptor, WriteTarget, bare_tool_name, write_target};
use liberado_provider::ToolInvocation;

use super::RiskGatedToolRuntime;

impl RiskGatedToolRuntime {
    /// The consequence of an MCP, looked up in the live catalog or the boot-time snapshot.
    ///
    /// `None` is fail-closed: the MCP is capability-granted but undeclared in whichever catalog
    /// this gate consults, so its consequence is unknowable and the call is refused rather than run
    /// with a default `ReadOnly`.
    pub(super) fn consequence_of(&self, mcp_name: &str) -> Option<Consequence> {
        if let Some(cat) = &self.live_catalog {
            return cat.get(mcp_name).map(|d| d.consequence);
        }
        self.consequence_catalog
            .iter()
            .find(|(name, _)| name == mcp_name)
            .map(|(_, c)| *c)
    }

    /// The declaration this call is judged against — the live catalog when one is attached,
    /// else the boot-time zone catalog, with the consequence catalog as a final fallback.
    pub(super) fn descriptor_of(&self, mcp_name: &str) -> Option<McpDescriptor> {
        self.live_descriptor(mcp_name)
            .or_else(|| self.zone_snapshot_descriptor(mcp_name))
            .or_else(|| self.synthesised_descriptor(mcp_name))
    }

    pub(super) fn write_target_of(&self, mcp_name: &str, call: &ToolInvocation) -> WriteTarget {
        self.descriptor_of(mcp_name).map_or_else(
            || WriteTarget::Undeterminable(undeclared_mcp_write_message(mcp_name)),
            |d| write_target(&d, bare_tool_name(&call.name), &call.arguments),
        )
    }

    pub(super) fn require_consequence(
        &self,
        call: &ToolInvocation,
        mcp_name: &str,
    ) -> Result<Consequence, String> {
        match self.consequence_of(mcp_name) {
            Some(c) => Ok(c),
            None => {
                self.authority_decision(
                    "consequence_catalog",
                    "refused",
                    call,
                    None,
                    &format!(
                        "a declared consequence for MCP '{mcp_name}' — register the MCP in \
                         topology.toml (name + consequence + tool classification) before any \
                         of its tools may be invoked"
                    ),
                );
                tracing::warn!(
                    mcp = %mcp_name,
                    "tool call refused: MCP is capability-granted but undeclared in the consequence catalog"
                );
                Err(format!(
                    "not authorized: MCP '{mcp_name}' is capability-granted but is not \
                     declared in the gate's consequence catalog (live or boot-time) — \
                     refusing the call; the operator must register the MCP (its name, \
                     consequence, and tool classification) in topology.toml before any of \
                     its tools may be invoked"
                ))
            }
        }
    }

    fn live_descriptor(&self, mcp_name: &str) -> Option<McpDescriptor> {
        self.live_catalog.as_ref().and_then(|cat| cat.get(mcp_name))
    }

    fn zone_snapshot_descriptor(&self, mcp_name: &str) -> Option<McpDescriptor> {
        self.zone_catalog
            .iter()
            .find(|d| d.name == mcp_name)
            .cloned()
    }

    fn synthesised_descriptor(&self, mcp_name: &str) -> Option<McpDescriptor> {
        self.consequence_catalog
            .iter()
            .find(|(name, _)| name == mcp_name)
            .map(|(_, c)| McpDescriptor {
                name: mcp_name.to_string(),
                description: format!(
                    "synthesised from consequence entry for '{mcp_name}' (no zone catalog entry)"
                ),
                consequence: *c,
                ..McpDescriptor::default()
            })
    }
}

fn undeclared_mcp_write_message(mcp_name: &str) -> String {
    format!(
        "MCP '{mcp_name}' is capability-granted but is not declared in the gate's \
         catalog (live, zone, or consequence) — refusing the call; the operator must \
         register the MCP (its name, consequence, and tool classification) in \
         topology.toml before any of its tools may be invoked"
    )
}
