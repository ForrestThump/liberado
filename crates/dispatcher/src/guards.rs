//! The deterministic guard pipeline (`liberado-dispatch-logic-spec.md` §6).
//!
//! Guards run **after** the classifier, in pure code, and can only move a decision toward *less*
//! autonomy — never more. This is what makes the right behavior emergent and the wrong behavior
//! cheap: a misclassification can waste tokens, but it cannot escalate past a guard into an
//! unsafe action. Because the guards are deterministic, the entire safety surface is exactly
//! assertable (Decision 16) — only the classifier's *quality* is probabilistic, never its safety.
//!
//! v1 scope: capability, reaction-depth, and confidence-floor guards (all cleanly modelable and
//! mapping to a `Clarify` downgrade). The consequence-gate and zone-write-class guards (§6 #2/#3)
//! are deferred to the slice that adds proposal production + tool→zone resolution; they are noted
//! where they would slot in.

use liberado_common::config::DispatchTuning;
use liberado_common::{BlockReason, DispatchAction, DispatchDecision, mcp_of};

use crate::DispatchRequest;

/// Evaluate the guards against a classified decision. Returns the [`BlockReason`] of the first
/// (highest-priority) violation, or `None` if the decision passes unchanged. The caller downgrades
/// to a `Clarify` carrying this reason.
///
/// Priority order — most fundamental first, so the reported reason is the most actionable:
/// capability gap → reaction-depth limit → confidence floor.
pub(crate) fn evaluate(
    decision: &DispatchDecision,
    req: &DispatchRequest,
    tuning: &DispatchTuning,
    max_reaction_depth: u32,
) -> Option<BlockReason> {
    // A Clarify is already the most conservative action — nothing to downgrade.
    if matches!(decision.action, DispatchAction::Clarify { .. }) {
        return None;
    }

    // (1) Capability check — never auto-widen (Decision 4 invariant). Every MCP the action would
    // invoke must be granted in the active capability set.
    for mcp in referenced_mcps(&decision.action) {
        if !req.capabilities.grants_mcp(mcp) {
            return Some(BlockReason::CapabilityGap);
        }
    }

    // §6 #2 (zone write-class) and #3 (consequence gate) would slot in here, downgrading writes to
    // `proposal_only`/`human_only` zones and high-consequence actions to a proposal. Deferred.

    // (4) Reaction-depth guard — halt runaway background cascades.
    if req.reaction_depth >= max_reaction_depth {
        return Some(BlockReason::DepthLimit);
    }

    // (5) Confidence floor — below the bar, ask rather than act. The write threshold is applied
    // conservatively to any action-taking decision (read/write tiering needs per-tool metadata,
    // deferred); `Clarify` was already excluded above.
    if decision.confidence < tuning.clarify_threshold_write {
        return Some(BlockReason::LowConfidence);
    }

    None
}

/// The MCPs an action would invoke. The tool-name convention is `"<mcp>:<tool>"`; a bare name is
/// treated as the MCP itself.
fn referenced_mcps(action: &DispatchAction) -> Vec<&str> {
    match action {
        // Pre-flight check over the classifier's opening move. The real boundary is runtime: the
        // executor only offers tools the capability set permits, so an adaptive call it makes later
        // is enforced there even though it isn't visible to this guard.
        DispatchAction::ExecuteDirect { seed_calls } => {
            seed_calls.iter().map(|c| mcp_of(&c.tool)).collect()
        }
        DispatchAction::DispatchSubagent { allowed_mcps, .. } => {
            allowed_mcps.iter().map(String::as_str).collect()
        }
        DispatchAction::Clarify { .. } => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::McpDescriptor;
    use liberado_common::{Capability, CapabilitySet, ToolCall, Zone};

    fn req(capabilities: CapabilitySet, reaction_depth: u32) -> DispatchRequest {
        DispatchRequest {
            goal: "do the thing".into(),
            catalog: vec![McpDescriptor {
                name: "tasks-mcp".into(),
                description: "task ops".into(),
            }],
            capabilities,
            reaction_depth,
        }
    }

    fn execute_direct(tool: &str, confidence: f32) -> DispatchDecision {
        DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: vec![ToolCall {
                    tool: tool.into(),
                    args: serde_json::json!({}),
                }],
            },
            confidence,
            rationale: "test".into(),
        }
    }

    fn granted(mcp: &str) -> CapabilitySet {
        CapabilitySet::from_iter([
            Capability::ExecuteMcp(mcp.into()),
            // a zone read, just to show unrelated caps don't matter
            Capability::Read(Zone::vault("tasks")),
        ])
    }

    #[test]
    fn high_confidence_granted_call_passes_through() {
        let d = execute_direct("tasks-mcp:add", 0.95);
        assert_eq!(
            evaluate(
                &d,
                &req(granted("tasks-mcp"), 0),
                &DispatchTuning::default(),
                4
            ),
            None
        );
    }

    #[test]
    fn ungranted_mcp_is_a_capability_gap() {
        let d = execute_direct("email-mcp:send", 0.95);
        assert_eq!(
            evaluate(
                &d,
                &req(granted("tasks-mcp"), 0),
                &DispatchTuning::default(),
                4
            ),
            Some(BlockReason::CapabilityGap)
        );
    }

    #[test]
    fn bare_tool_name_is_treated_as_mcp_name() {
        let d = execute_direct("tasks-mcp", 0.95);
        assert_eq!(
            evaluate(
                &d,
                &req(granted("tasks-mcp"), 0),
                &DispatchTuning::default(),
                4
            ),
            None
        );
    }

    #[test]
    fn reaction_depth_limit_downgrades() {
        let d = execute_direct("tasks-mcp:add", 0.95);
        // At the cap, even a granted high-confidence call is halted.
        assert_eq!(
            evaluate(
                &d,
                &req(granted("tasks-mcp"), 4),
                &DispatchTuning::default(),
                4
            ),
            Some(BlockReason::DepthLimit)
        );
    }

    #[test]
    fn low_confidence_downgrades() {
        let d = execute_direct("tasks-mcp:add", 0.5); // below default write threshold 0.7
        assert_eq!(
            evaluate(
                &d,
                &req(granted("tasks-mcp"), 0),
                &DispatchTuning::default(),
                4
            ),
            Some(BlockReason::LowConfidence)
        );
    }

    #[test]
    fn capability_gap_outranks_low_confidence() {
        // Both a capability gap and low confidence apply; the more fundamental one is reported.
        let d = execute_direct("email-mcp:send", 0.1);
        assert_eq!(
            evaluate(
                &d,
                &req(granted("tasks-mcp"), 0),
                &DispatchTuning::default(),
                4
            ),
            Some(BlockReason::CapabilityGap)
        );
    }

    #[test]
    fn subagent_requires_all_allowed_mcps_granted() {
        let d = DispatchDecision {
            action: DispatchAction::DispatchSubagent {
                goal: "review".into(),
                capabilities: CapabilitySet::empty(),
                allowed_mcps: vec!["tasks-mcp".into(), "decisions-mcp".into()],
                success_criteria: vec![],
                artifact_target: None,
                model: None,
                correlation_id: "c1".into(),
            },
            confidence: 0.95,
            rationale: "test".into(),
        };
        // Only tasks-mcp granted → the missing decisions-mcp is a capability gap.
        assert_eq!(
            evaluate(
                &d,
                &req(granted("tasks-mcp"), 0),
                &DispatchTuning::default(),
                4
            ),
            Some(BlockReason::CapabilityGap)
        );
    }

    #[test]
    fn clarify_is_never_downgraded() {
        let d = DispatchDecision {
            action: DispatchAction::Clarify {
                questions: vec!["which?".into()],
                what_blocked: BlockReason::Ambiguous,
            },
            confidence: 0.0, // would trip the confidence floor if it applied
            rationale: "test".into(),
        };
        assert_eq!(
            evaluate(
                &d,
                &req(CapabilitySet::empty(), 9),
                &DispatchTuning::default(),
                4
            ),
            None
        );
    }
}
