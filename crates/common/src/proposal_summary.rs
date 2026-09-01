//! Adaptive-goal summary text. Kept beside [`crate::proposal`] so `ProposedAction::summary`
//! stays a straight match over variants.

pub(super) fn dispatch_or_adaptive_summary(
    action: &super::ProposedAction,
    goal: &str,
    mcps: &[String],
) -> String {
    let joined = mcps.join(", ");
    match action {
        super::ProposedAction::AdaptiveGoal { approved_guard, .. } => {
            format!(
                "run approved adaptive goal: {goal} (mcps: {joined}; approved guard: {approved_guard:?})"
            )
        }
        _ => format!("dispatch a subagent for: {goal} (mcps: {joined})"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ApprovedGuard, Proposal, ProposedAction};
    use crate::capability::{Capability, CapabilitySet};

    #[test]
    fn note_round_trips_an_approved_adaptive_goal_scope() {
        let p = Proposal::pending(
            "prop-adaptive-1",
            "prop-adaptive-1",
            "liberado",
            ProposedAction::AdaptiveGoal {
                goal: "delete all archived tasks".into(),
                capabilities: CapabilitySet::from_iter([Capability::ExecuteMcp(
                    "tasks-mcp".into(),
                )]),
                relevant_mcps: vec!["tasks-mcp".into()],
                delivery: crate::Delivery::Summarize,
                approved_guard: ApprovedGuard::Magnitude,
            },
            "The human must approve the exact sweeping goal",
        );
        let back = Proposal::from_note(&p.to_note()).unwrap();
        assert_eq!(back, p);
    }
}
