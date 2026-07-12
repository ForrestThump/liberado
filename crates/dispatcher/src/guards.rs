//! The deterministic guard pipeline (`liberado-dispatch-logic-spec.md` §6).
//!
//! Guards run **after** the classifier, in pure code, and can only move a decision toward *less*
//! autonomy — never more. This is what makes the right behavior emergent and the wrong behavior
//! cheap: a misclassification can waste tokens, but it cannot escalate past a guard into an
//! unsafe action. Because the guards are deterministic, the entire safety surface is exactly
//! assertable (Decision 16) — only the classifier's *quality* is probabilistic, never its safety.
//!
//! Scope: capability, consequence-gate, zone-write-class, magnitude, reaction-depth, and
//! confidence-floor guards. (This comment previously listed the consequence gate, then the
//! zone-write-class gate, as deferred — stale as of whenever each actually shipped; the code below
//! has implemented both for a while by the time you're reading this.)
//!
//! ## Keeping this in sync with the runtime guard (`liberado-executor`'s `RiskGatedToolRuntime`)
//!
//! This is the pre-flight half of a two-part safety net; `RiskGatedToolRuntime` is the runtime half
//! that applies the equivalent checks to every *adaptive* (non-seed) call, since a call this guard
//! never saw at dispatch time still has to pass something before it runs. The zone-write-class
//! check is unified (`liberado_common::zone_write_restriction`) so it can't drift between the two.
//! The capability/consequence/magnitude checks are NOT unified — different shapes at each site for
//! good reason — but if you add a **new** guard here, check whether `risk_gated.rs` needs the
//! runtime equivalent, and vice versa.

use liberado_common::{
    BlockReason, Consequence, DispatchAction, DispatchDecision, bare_tool_name,
    is_sweeping_destructive, mcp_of, zone_write_restriction,
};
use liberado_config_loader::DispatchTuning;

use crate::DispatchRequest;

/// At or above this consequence, a direct action is gated to a `Clarify` for confirmation. Set to
/// `Irreversible` so anything that can't be undone — and everything `External` — needs a human, while
/// `Reversible` (git-tracked) writes and `ReadOnly` lookups flow.
const CONSEQUENCE_GATE: Consequence = Consequence::Irreversible;

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
            tracing::warn!(
                mcp,
                action = %decision.action,
                "capability gap: action references MCP not in the dispatcher grant"
            );
            return Some(BlockReason::CapabilityGap);
        }
    }

    // (2) Consequence gate (§6 #3) — a permitted action that would touch something irreversible or
    // external (an email/message, an unversioned delete) needs human confirmation, even at high
    // confidence. A git-tracked vault write is `Reversible` and passes; `External`/`Irreversible`
    // does not.
    if max_consequence(&decision.action, req) >= CONSEQUENCE_GATE {
        return Some(BlockReason::HighConsequence);
    }

    // (2b) Zone-write-class gate (§6 #2) — a permitted, low-*general*-consequence action can still
    // target a *specific* vault zone the human has restricted (`proposal_only`/`human_only`).
    // Pre-flight scope only (this checks `ExecuteDirect`'s seed calls — see `zone_restricted`'s own
    // doc comment); the real, always-enforced boundary for every call including adaptive ones is
    // `RiskGatedToolRuntime`.
    if zone_restricted(&decision.action, req) {
        return Some(BlockReason::ZoneRestricted);
    }

    // (3) Magnitude gate — a *sweeping destructive* action is high-stakes by reach even when each
    // change is reversible ("delete all my notes" in a git-tracked vault). Read from the goal: it's
    // the tool-independent signal available pre-execution, and (unlike a specific tool name) it
    // survives the model routing the work to a subagent. Liberado owns this classification because
    // MCP tools don't declare their own risk. Per-call, args-aware enforcement is a later layer.
    if is_sweeping_destructive(&req.goal) {
        return Some(BlockReason::HighConsequence);
    }

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
        // Pre-flight check over the classifier's opening move (`seed_calls`) AND its narrowing hint
        // (`relevant_mcps`, if the model populated one) — a hallucinated or out-of-scope name in
        // either gets caught here, the same capability-gap protection `DispatchSubagent.allowed_mcps`
        // already gets below. The real boundary is still runtime: the executor only offers tools the
        // capability set permits, so an adaptive call it makes later is enforced there too, even
        // though it isn't visible to this pre-flight guard.
        DispatchAction::ExecuteDirect {
            seed_calls,
            relevant_mcps,
        } => seed_calls
            .iter()
            .map(|c| mcp_of(&c.tool))
            .chain(relevant_mcps.iter().map(String::as_str))
            .collect(),
        DispatchAction::DispatchSubagent { allowed_mcps, .. } => {
            allowed_mcps.iter().map(String::as_str).collect()
        }
        // Clarify carries no calls; Propose is a post-guard output the guards never receive.
        DispatchAction::Clarify { .. } | DispatchAction::Propose { .. } => Vec::new(),
    }
}

/// The highest consequence among the MCPs an action would touch, looked up from the catalog. An MCP
/// the catalog doesn't describe contributes nothing (`ReadOnly`). Like the capability check, this is
/// a pre-flight read of the action's declared scope; runtime gating of an `ExecuteDirect`'s adaptive
/// calls is a separate, later boundary.
fn max_consequence(action: &DispatchAction, req: &DispatchRequest) -> Consequence {
    referenced_mcps(action)
        .into_iter()
        .filter_map(|mcp| {
            req.catalog
                .iter()
                .find(|d| d.name == mcp)
                .map(|d| d.consequence)
        })
        .max()
        .unwrap_or_default()
}

/// Whether any of `action`'s seed calls target a zone whose declared `WriteClass` doesn't allow a
/// direct agent write (`ProposalOnly`/`HumanOnly`) — the zone-write-class guard (§6 #2).
///
/// Only `ExecuteDirect`'s seed calls are checked — the same pre-flight scope `max_consequence`
/// above already accepts: a `DispatchSubagent`'s adaptive calls aren't known yet at dispatch time,
/// and this is a check, not the boundary (`RiskGatedToolRuntime` is, for every call including
/// adaptive ones — see `liberado_common::zone_write_restriction`'s own doc comment, the shared
/// determination logic both this guard and that runtime call so the two can't silently drift
/// apart on what counts as restricted).
fn zone_restricted(action: &DispatchAction, req: &DispatchRequest) -> bool {
    let DispatchAction::ExecuteDirect { seed_calls, .. } = action else {
        return false;
    };
    seed_calls.iter().any(|call| {
        zone_write_restriction(
            mcp_of(&call.tool),
            bare_tool_name(&call.tool),
            &req.catalog,
            &req.zone_write_classes,
        )
        .is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::McpDescriptor;
    use liberado_common::{Capability, CapabilitySet, Consequence, ToolCall, Zone};

    fn req(capabilities: CapabilitySet, reaction_depth: u32) -> DispatchRequest {
        DispatchRequest {
            goal: "do the thing".into(),
            catalog: vec![McpDescriptor {
                name: "tasks-mcp".into(),
                description: "task ops".into(),
                consequence: Consequence::Reversible,
                provenance: None,
                default_zone: None,
                tool_zones: Vec::new(),
            }],
            capabilities,
            reaction_depth,
            zone_write_classes: Vec::new(),
        }
    }

    fn execute_direct(tool: &str, confidence: f32) -> DispatchDecision {
        DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: vec![ToolCall {
                    tool: tool.into(),
                    args: serde_json::json!({}),
                }],
                relevant_mcps: Vec::new(),
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
    fn external_action_is_gated_by_consequence() {
        // Granted and confident — but it would send a message out of the system. Confirm first.
        let request = DispatchRequest {
            goal: "email my boss".into(),
            catalog: vec![McpDescriptor {
                name: "email".into(),
                description: "send email".into(),
                consequence: Consequence::External,
                provenance: None,
                default_zone: None,
                tool_zones: Vec::new(),
            }],
            capabilities: granted("email"),
            reaction_depth: 0,
            zone_write_classes: Vec::new(),
        };
        let d = execute_direct("email:send", 0.95);
        assert_eq!(
            evaluate(&d, &request, &DispatchTuning::default(), 4),
            Some(BlockReason::HighConsequence)
        );
    }

    #[test]
    fn reversible_git_tracked_write_is_not_gated() {
        // A write to a git-tracked vault is recoverable — reversibility is the safety net, so the
        // consequence gate lets it flow even at the same confidence the email was blocked at.
        let request = DispatchRequest {
            goal: "write a note".into(),
            catalog: vec![McpDescriptor {
                name: "vault".into(),
                description: "git-tracked Obsidian vault".into(),
                consequence: Consequence::Reversible,
                provenance: None,
                default_zone: None,
                tool_zones: Vec::new(),
            }],
            capabilities: granted("vault"),
            reaction_depth: 0,
            zone_write_classes: Vec::new(),
        };
        let d = execute_direct("vault:write", 0.95);
        assert_eq!(evaluate(&d, &request, &DispatchTuning::default(), 4), None);
    }

    #[test]
    fn sweeping_destructive_goal_is_gated_by_magnitude() {
        // The eval's case: a git-tracked vault (Reversible, so the consequence gate passes), but the
        // goal is sweeping-destructive — the magnitude gate must still downgrade it.
        let request = DispatchRequest {
            goal: "delete all of my notes".into(),
            catalog: vec![McpDescriptor {
                name: "vault".into(),
                description: "git-tracked vault".into(),
                consequence: Consequence::Reversible,
                provenance: None,
                default_zone: None,
                tool_zones: Vec::new(),
            }],
            capabilities: granted("vault"),
            reaction_depth: 0,
            zone_write_classes: Vec::new(),
        };
        let d = execute_direct("vault:delete", 0.95);
        assert_eq!(
            evaluate(&d, &request, &DispatchTuning::default(), 4),
            Some(BlockReason::HighConsequence)
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
    fn execute_direct_requires_relevant_mcps_granted() {
        // seed_calls references a granted MCP, but relevant_mcps names one that isn't — the
        // narrowing hint gets the same capability-gap protection as seed_calls and allowed_mcps.
        let d = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: vec![ToolCall {
                    tool: "tasks-mcp:add".into(),
                    args: serde_json::json!({}),
                }],
                relevant_mcps: vec!["tasks-mcp".into(), "email-mcp".into()],
            },
            confidence: 0.95,
            rationale: "test".into(),
        };
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
    fn execute_direct_with_only_granted_relevant_mcps_passes() {
        let d = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
                relevant_mcps: vec!["tasks-mcp".into()],
            },
            confidence: 0.95,
            rationale: "test".into(),
        };
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

    /// A `vault` MCP request whose seed call targets `write_review` (declared to write to the
    /// `reviews` zone), granted and `Reversible` (so the consequence gate alone would pass it) —
    /// isolating the zone-write-class guard from the consequence gate it sits next to.
    fn vault_request(zone_write_classes: Vec<(&str, liberado_common::WriteClass)>) -> DispatchRequest {
        DispatchRequest {
            goal: "write a review note".into(),
            catalog: vec![McpDescriptor {
                name: "vault".into(),
                description: "git-tracked vault".into(),
                consequence: Consequence::Reversible,
                provenance: None,
                default_zone: Some("tasks".into()),
                tool_zones: vec![("write_review".into(), Some("reviews".into()))],
            }],
            capabilities: granted("vault"),
            reaction_depth: 0,
            zone_write_classes: zone_write_classes
                .into_iter()
                .map(|(z, wc)| (z.to_string(), wc))
                .collect(),
        }
    }

    #[test]
    fn write_to_a_proposal_only_zone_is_zone_restricted() {
        use liberado_common::WriteClass;
        let d = execute_direct("vault:write_review", 0.95);
        let request = vault_request(vec![("reviews", WriteClass::ProposalOnly)]);
        assert_eq!(
            evaluate(&d, &request, &DispatchTuning::default(), 4),
            Some(BlockReason::ZoneRestricted)
        );
    }

    #[test]
    fn write_to_an_agent_writable_zone_passes() {
        use liberado_common::WriteClass;
        let d = execute_direct("vault:write_review", 0.95);
        let request = vault_request(vec![("reviews", WriteClass::AgentWritable)]);
        assert_eq!(evaluate(&d, &request, &DispatchTuning::default(), 4), None);
    }

    #[test]
    fn unlisted_zone_fails_safe_to_zone_restricted() {
        // "reviews" isn't in zone_write_classes at all -- must fail safe (ProposalOnly), not
        // silently pass just because nothing was configured.
        use liberado_common::WriteClass;
        let d = execute_direct("vault:write_review", 0.95);
        let request = vault_request(vec![("tasks", WriteClass::AgentWritable)]);
        assert_eq!(
            evaluate(&d, &request, &DispatchTuning::default(), 4),
            Some(BlockReason::ZoneRestricted)
        );
    }

    #[test]
    fn a_tool_not_opted_into_zone_tracking_is_not_zone_restricted() {
        // "add" isn't in vault's `tool_zones` and there's a `default_zone`, so it inherits
        // "tasks" -- this specifically checks a tool from a *different*, zone-untracked MCP
        // (tasks-mcp, no default_zone/tool_zones at all) isn't affected by the vault-only
        // zone_write_classes above -- it should pass regardless of what zones are restricted.
        use liberado_common::WriteClass;
        let d = execute_direct("tasks-mcp:add", 0.95);
        let request = DispatchRequest {
            zone_write_classes: vec![("reviews".to_string(), WriteClass::ProposalOnly)],
            ..req(granted("tasks-mcp"), 0)
        };
        assert_eq!(evaluate(&d, &request, &DispatchTuning::default(), 4), None);
    }
}
