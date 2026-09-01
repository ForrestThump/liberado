//! Proposal-side risk guards extracted from [`super::RiskGatedToolRuntime`].

use liberado_common::{ApprovedGuard, Consequence, WriteTarget, is_sweeping_destructive};
use liberado_provider::ToolInvocation;

use super::{RiskGatedToolRuntime, proposal_message};

impl RiskGatedToolRuntime {
    fn skips(&self, guard: ApprovedGuard) -> bool {
        self.approved_guard == Some(guard)
    }

    fn magnitude_waived(&self, call: &ToolInvocation, write_target: &WriteTarget) -> bool {
        let zone = match write_target {
            WriteTarget::Zone(name) => Some(name.as_str()),
            WriteTarget::NotAWrite | WriteTarget::Undeterminable(_) => None,
        };
        self.risk_waivers
            .covers(liberado_common::Guard::Magnitude, &call.name, zone)
    }

    /// Consequence, zone-write-class, and magnitude proposal path. `Ok(Some)` is a
    /// human-facing proposal message; `Ok(None)` means the call may run.
    pub(super) async fn proposal_if_risky(
        &self,
        call: &ToolInvocation,
        mcp_name: &str,
        consequence: Consequence,
        write_zone: Option<&str>,
        write_target: &WriteTarget,
    ) -> Result<Option<String>, String> {
        if consequence >= Consequence::Irreversible && !self.skips(ApprovedGuard::Consequence) {
            self.authority_decision(
                "consequence",
                "proposal",
                call,
                None,
                &format!(
                    "an MCP rated below {:?} (this one is {consequence:?})",
                    Consequence::Irreversible
                ),
            );
            let proposal_path = self
                .write_proposal(call, "High-consequence MCP — requires human approval")
                .await?;
            return Ok(Some(proposal_message(&proposal_path)));
        }

        if let Some(zone) = write_zone.filter(|zone| self.zone_is_restricted(zone))
            && !self.skips(ApprovedGuard::ZoneWriteClass)
        {
            self.authority_decision(
                "zone_write_class",
                "proposal",
                call,
                Some(zone),
                &format!("zone '{zone}' declared agent_writable or shared in policy.zones"),
            );
            let proposal_path = self
                .write_proposal(
                    call,
                    &format!("Write targets the '{zone}' zone, which requires human approval"),
                )
                .await?;
            return Ok(Some(proposal_message(&proposal_path)));
        }

        if self.skips(ApprovedGuard::Magnitude) || self.magnitude_waived(call, write_target) {
            return Ok(None);
        }
        if self.payload_or_goal_is_sweeping(call, mcp_name) {
            self.authority_decision(
                "magnitude",
                "proposal",
                call,
                None,
                "arguments/goal without sweeping-destructive phrasing",
            );
            let proposal_path = self
                .write_proposal(
                    call,
                    "Sweeping destructive action — requires human approval",
                )
                .await?;
            return Ok(Some(proposal_message(&proposal_path)));
        }
        Ok(None)
    }

    fn zone_is_restricted(&self, zone: &str) -> bool {
        match self.zone_write_classes.iter().find(|(z, _)| z == zone) {
            Some((_, wc)) => !wc.allows_direct_agent_write(),
            None if self
                .capabilities
                .contains(&liberado_common::Capability::Write(
                    liberado_common::Zone::vault(zone),
                )) =>
            {
                false
            }
            None => !liberado_common::WriteClass::default().allows_direct_agent_write(),
        }
    }

    fn payload_or_goal_is_sweeping(&self, call: &ToolInvocation, mcp_name: &str) -> bool {
        let names_one_target = self.descriptor_of(mcp_name).is_some_and(|d| {
            liberado_common::names_single_write_target(
                &d,
                liberado_common::bare_tool_name(&call.name),
                &call.arguments,
            )
        });
        let full_context = format!(
            "{} {}",
            liberado_common::instruction_scope(&self.goal_context),
            call.name
        );
        let sweeping_payload =
            !names_one_target && is_sweeping_destructive(&call.arguments.to_string());
        sweeping_payload || is_sweeping_destructive(&full_context)
    }
}
