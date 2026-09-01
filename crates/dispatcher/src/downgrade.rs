//! Approvable-guard downgrade path. Split from `lib.rs` so adding AdaptiveGoal
//! does not push the crate root past its cyclomatic baseline.

use liberado_common::{
    ApprovedGuard, BlockReason, CapabilitySet, Delivery, DispatchAction, DispatchDecision,
    ProposedAction,
};

use crate::guards;
use crate::{
    DispatchRequest, downgrade_to_clarify, downgrade_to_propose_subagent,
    downgrade_to_propose_tool_calls,
};

pub(crate) fn downgrade_approvable(
    action: DispatchAction,
    req: &DispatchRequest,
    violation: guards::GuardViolation,
    confidence: f32,
    rationale: String,
    reason: BlockReason,
) -> DispatchDecision {
    match action {
        DispatchAction::ExecuteDirect {
            seed_calls,
            relevant_mcps,
            delivery,
        } => {
            if !seed_calls.is_empty() {
                return downgrade_to_propose_tool_calls(seed_calls, confidence, rationale);
            }
            if let Some(approved_guard) = approved_guard(violation.guard) {
                return downgrade_to_propose_adaptive_goal(
                    req.goal.clone(),
                    req.capabilities.clone(),
                    relevant_mcps,
                    delivery,
                    approved_guard,
                    confidence,
                    rationale,
                );
            }
        }
        DispatchAction::DispatchSubagent {
            goal,
            capabilities,
            allowed_mcps,
            success_criteria,
            ..
        } => {
            return downgrade_to_propose_subagent(
                goal,
                capabilities,
                allowed_mcps,
                success_criteria,
                confidence,
                rationale,
            );
        }
        _ => {}
    }
    downgrade_to_clarify(confidence, reason)
}

fn approved_guard(guard: guards::GuardKind) -> Option<ApprovedGuard> {
    match guard {
        guards::GuardKind::Consequence => Some(ApprovedGuard::Consequence),
        guards::GuardKind::Magnitude => Some(ApprovedGuard::Magnitude),
        guards::GuardKind::ZoneWriteClass => Some(ApprovedGuard::ZoneWriteClass),
        guards::GuardKind::AskHumanCapability
        | guards::GuardKind::McpGrant
        | guards::GuardKind::ReactionDepth
        | guards::GuardKind::ConfidenceFloor => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn downgrade_to_propose_adaptive_goal(
    goal: String,
    capabilities: CapabilitySet,
    relevant_mcps: Vec<String>,
    delivery: Delivery,
    approved_guard: ApprovedGuard,
    confidence: f32,
    rationale: String,
) -> DispatchDecision {
    DispatchDecision {
        action: DispatchAction::Propose {
            proposed_action: ProposedAction::AdaptiveGoal {
                goal,
                capabilities,
                relevant_mcps,
                delivery,
                approved_guard,
            },
            rationale: rationale.clone(),
        },
        confidence,
        rationale,
    }
}
