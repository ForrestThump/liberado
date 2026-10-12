//! Vault-write authorisation for [`super::RiskGatedToolRuntime`].
//!
//! Split out of `risk_gated.rs` so the invoke path stays under the module-health
//! ceilings. The rules live here; `invoke` calls [`authorize_vault_write`] and
//! either returns the permission-request message or continues to the consequence gate.

use std::path::{Path, PathBuf};

use liberado_common::{
    Capability, ProposedAction, SignedProposal, ToolCall, VAULT_WIDE_ZONE_MARKER, VaultWideGrant,
    WriteTarget, Zone, bare_tool_name,
};
use liberado_provider::ToolInvocation;

use super::RiskGatedToolRuntime;

/// What [`authorize_vault_write`] decided.
pub(super) enum VaultAuth {
    /// Zones the call writes, already checked against the grant. Empty for a read
    /// or a vault-wide write the grant already holds.
    Proceed(Vec<String>),
    /// Permission request raised. `invoke` returns this string as the tool result.
    Message(String),
}

/// `Err` is a hard denial (undeterminable zone, or no human to ask).
pub(super) async fn authorize_vault_write(
    runtime: &RiskGatedToolRuntime,
    call: &ToolInvocation,
    mcp_name: &str,
    write_target: &WriteTarget,
) -> Result<VaultAuth, String> {
    let write_zones = match resolve_write_zones(runtime, call, mcp_name, write_target).await? {
        ZoneDecision::Denied(message) => return Ok(VaultAuth::Message(message)),
        ZoneDecision::Zones(zones) => zones,
    };
    if let Some(message) = refuse_missing_zones(runtime, call, &write_zones).await? {
        return Ok(VaultAuth::Message(message));
    }
    Ok(VaultAuth::Proceed(write_zones))
}

enum ZoneDecision {
    Zones(Vec<String>),
    Denied(String),
}

pub(super) fn refuse_admin_tool(
    runtime: &RiskGatedToolRuntime,
    call: &ToolInvocation,
    mcp_name: &str,
) -> Option<String> {
    let descriptor = runtime.descriptor_of(mcp_name)?;
    let bare = bare_tool_name(&call.name);
    if !descriptor.admin_tools.iter().any(|tool| tool == bare) {
        return None;
    }
    runtime.authority_decision(
        "admin_tool",
        "refused",
        call,
        None,
        "admin tools (e.g. set_active_vault) are not callable from a chat/agent \
         session; the operator must invoke them directly via the daemon config or \
         a hand-shell",
    );
    tracing::warn!(
        tool = %call.name,
        "tool call refused: admin tool invoked through chat/agent"
    );
    Some(format!(
        "not authorized: '{}' is an admin-only operation; the runtime does not \
         route admin tools through chat/agent sessions, and there is no human to \
         ask on this surface. The operator can invoke it directly through the \
         daemon config or a hand-shell.",
        call.name
    ))
}

async fn resolve_write_zones(
    runtime: &RiskGatedToolRuntime,
    call: &ToolInvocation,
    mcp_name: &str,
    write_target: &WriteTarget,
) -> Result<ZoneDecision, String> {
    match write_target {
        WriteTarget::Undeterminable(why) => {
            runtime.authority_decision(
                "write_zone_resolution",
                "refused",
                call,
                None,
                "a resolvable target zone (declare zone_from_arg/write_tools / \
                 write_path_args, or pass a zone-qualified path)",
            );
            tracing::warn!(mcp = %mcp_name, %why, "tool call refused: undeterminable write zone");
            Err(format!("not authorized: {why}"))
        }
        WriteTarget::VaultWide => vault_wide_decision(runtime, call).await,
        WriteTarget::Zone(zone) => Ok(ZoneDecision::Zones(vec![zone.clone()])),
        WriteTarget::Zones(zones) => Ok(ZoneDecision::Zones(zones.clone())),
        WriteTarget::NotAWrite => Ok(ZoneDecision::Zones(Vec::new())),
    }
}

async fn vault_wide_decision(
    runtime: &RiskGatedToolRuntime,
    call: &ToolInvocation,
) -> Result<ZoneDecision, String> {
    if runtime
        .capabilities
        .holds_vault_wide_grant(VAULT_WIDE_ZONE_MARKER)
    {
        return Ok(ZoneDecision::Zones(Vec::new()));
    }
    runtime.authority_decision(
        "vault_wide_write_capability",
        if runtime.can_ask_human() {
            "permission_request"
        } else {
            "refused"
        },
        call,
        None,
        "the explicit vault-wide marker grant \
         Write(Vault(\"vault-wide-any\")) — a fanout / vault switch \
         / index rebuild is a step-change in authority, so it is \
         gated by an opt-in grant, NOT by Write(<zone>) for some specific zone",
    );
    if runtime.can_ask_human() {
        let path = runtime.write_vault_wide_permission_request(call).await?;
        return Ok(ZoneDecision::Denied(vault_wide_permission_request_message(
            &path,
        )));
    }
    Err(format!(
        "not authorized: '{}' mutates vault-wide state (fanout / index / \
         vault switch), and this session's grant does not hold the explicit \
         vault-wide marker `Write(Vault(\"vault-wide-any\"))`. Holding \
         `Write(Tasks)` alone authorises rearranging Tasks, not switching \
         the active vault for the whole process — add the marker to the \
         grant, narrow the tool list to a per-tool ExecuteTool grant, or \
         scrap the operation.",
        call.name
    ))
}

async fn refuse_missing_zones(
    runtime: &RiskGatedToolRuntime,
    call: &ToolInvocation,
    write_zones: &[String],
) -> Result<Option<String>, String> {
    let missing_zones: Vec<String> = write_zones
        .iter()
        .filter(|zone| {
            !runtime
                .capabilities
                .contains(&Capability::Write(Zone::vault((*zone).as_str())))
        })
        .cloned()
        .collect();
    if missing_zones.is_empty() {
        return Ok(None);
    }
    runtime.authority_decision(
        "write_capability",
        if runtime.can_ask_human() {
            "permission_request"
        } else {
            "refused"
        },
        call,
        Some(missing_zones.first().map(String::as_str).unwrap_or("")),
        &missing_zone_requirement(&missing_zones),
    );
    if runtime.can_ask_human() {
        let first_zone = missing_zones.first().map(String::as_str).unwrap_or("");
        let path = runtime
            .write_permission_request(call, &missing_zones)
            .await?;
        return Ok(Some(permission_request_message(
            &path,
            first_zone,
            &missing_zones,
        )));
    }
    Err(zone_denial(call, &missing_zones))
}

fn missing_zone_requirement(missing_zones: &[String]) -> String {
    if missing_zones.len() == 1 {
        format!("Write(Vault(\"{}\"))", missing_zones[0])
    } else {
        format!(
            "Write(Vault(\"<zone>\")) for each of [{}]",
            missing_zones.join(", ")
        )
    }
}

fn zone_denial(call: &ToolInvocation, missing_zones: &[String]) -> String {
    let which = if missing_zones.len() == 1 {
        format!("the zone '{}'", missing_zones[0])
    } else {
        "those zones (each independently)".to_string()
    };
    format!(
        "not authorized: '{}' writes to zone(s) [{}], and this session's \
         grant does not include Write for {which}. Calling an MCP is not permission \
         to write with it.",
        call.name,
        missing_zones.join(", "),
    )
}

impl RiskGatedToolRuntime {
    pub(super) async fn write_vault_wide_permission_request(
        &self,
        call: &ToolInvocation,
    ) -> Result<PathBuf, String> {
        let proposal_id = super::permission_request_id();
        let proposals_subdir = self.proposals_dir.join(liberado_common::PROPOSALS_DIR);
        let proposal_path = proposals_subdir.join(format!("{proposal_id}.md"));
        let proposal = self.sign_vault_wide_permission_proposal(&proposal_id, call);
        if let Err(e) = tokio::fs::create_dir_all(&proposals_subdir).await {
            tracing::error!(path = %proposals_subdir.display(), error = %e, "failed to create proposals directory");
            return Err(format!(
                "could not create the proposals directory at {}: {e} — no permission request was saved",
                proposals_subdir.display()
            ));
        }
        if let Err(e) = tokio::fs::write(&proposal_path, proposal.to_note()).await {
            tracing::error!(path = %proposal_path.display(), error = %e, "failed to write permission request");
            return Err(format!(
                "could not save the permission request at {}: {e} — the action was NOT executed",
                proposal_path.display()
            ));
        }
        tracing::info!(path = %proposal_path.display(), tool = %call.name, "vault-wide permission request written");
        self.notify_vault_wide_permission_request(&proposal_id, call)
            .await;
        Ok(proposal_path)
    }

    fn sign_vault_wide_permission_proposal(
        &self,
        proposal_id: &str,
        call: &ToolInvocation,
    ) -> SignedProposal {
        let mut proposal = liberado_common::Proposal::pending(
            proposal_id,
            &self.correlation_base,
            "liberado-chat",
            ProposedAction::ToolCalls(vec![ToolCall {
                tool: call.name.clone(),
                args: call.arguments.clone(),
            }]),
            format!(
                "Permission request: '{}' mutates vault-wide state (fanout / index / vault \
                 switch). It needs the explicit vault-wide marker grant \
                 Write(Vault(\"{VAULT_WIDE_ZONE_MARKER}\")) — a `Write(<zone>)` for some specific zone is not \
                 enough, because vault-wide mutators affect every concurrent session, not just the \
                 one the grant covers.",
                call.name,
            ),
        )
        .with_requested_grant(Capability::Write(Zone::vault(VAULT_WIDE_ZONE_MARKER)));
        proposal.pool = Some(self.pool_name.clone());
        self.stamp_permission_origin(&mut proposal);
        self.signer.sign(proposal)
    }

    async fn notify_vault_wide_permission_request(&self, proposal_id: &str, call: &ToolInvocation) {
        let message = vault_wide_permission_ask_message(&call.name);
        self.notify_on_surface(proposal_id, &message).await;
    }
}

pub(super) fn permission_ask_message(tool: &str, missing_zones: &[String]) -> String {
    if missing_zones.len() == 1 {
        format!(
            "Liberado needs permission.\n'{tool}' wants to write zone '{zone}', which its grant \
             doesn't include.\nApprove once, for this session, or everywhere?",
            zone = missing_zones[0],
        )
    } else {
        let list = format!("[{}]", missing_zones.join(", "));
        format!(
            "Liberado needs permission.\n'{tool}' wants to write to multiple zones {list}, none \
             of which its grant currently covers.\nApproving will raise another request for the \
             next zone in turn (a multi-zone write takes one card per zone).\nApprove once, for \
             this session, or everywhere?",
        )
    }
}

pub(super) fn permission_request_message(
    path: &Path,
    first_zone: &str,
    missing_zones: &[String],
) -> String {
    let zone_summary = if missing_zones.len() == 1 {
        format!("zone '{first_zone}'")
    } else {
        format!(
            "every zone in [{}] (the daemon grants '{}' first; the next call will request the rest)",
            missing_zones.join(", "),
            first_zone
        )
    };
    format!(
        "PERMISSION REQUESTED — the action was NOT executed. It needs Write access to {zone_summary}, \
         which this session's grant doesn't include. A request was sent for approval \
         (once / this session / everywhere). Saved at: {}. Do not retry or invent a result; the \
         action runs automatically once the human approves.",
        path.display()
    )
}

fn vault_wide_permission_ask_message(tool: &str) -> String {
    format!(
        "Liberado needs permission.\n'{tool}' mutates vault-wide state (fanout, vault switch, \
         index rebuild). It needs the explicit vault-wide grant \
         Write(Vault(\"{VAULT_WIDE_ZONE_MARKER}\")) to authorise that — a Write(<zone>) for some specific zone is not \
         enough. Approve once, for this session, or everywhere?",
    )
}

fn vault_wide_permission_request_message(path: &Path) -> String {
    format!(
        "PERMISSION REQUESTED — the action was NOT executed. It mutates vault-wide state \
         (fanout / index / vault switch), and this session's grant currently does not hold the \
         explicit vault-wide marker `Write(Vault(\"{VAULT_WIDE_ZONE_MARKER}\"))` to authorise that. A request was \
         sent for approval (once / this session / everywhere). Saved at: {}. Do not retry or \
         invent a result; the action runs automatically once the human approves.",
        path.display(),
    )
}
