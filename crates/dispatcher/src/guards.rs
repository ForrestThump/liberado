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

use liberado_common::CONSEQUENCE_GATE;
use liberado_common::{
    BlockReason, Consequence, DispatchAction, DispatchDecision, bare_tool_name, instruction_scope,
    is_sweeping_destructive, mcp_of, zone_write_restriction,
};
use liberado_config_loader::DispatchTuning;

use crate::DispatchRequest;

/// Evaluate the guards against a classified decision. Returns the [`BlockReason`] of the first
/// (highest-priority) violation, or `None` if the decision passes unchanged. The caller downgrades
/// to a `Clarify` carrying this reason.
///
/// Priority order — most fundamental first, so the reported reason is the most actionable:
/// capability gap → reaction-depth limit → confidence floor.
pub fn evaluate(
    decision: &DispatchDecision,
    req: &DispatchRequest,
    tuning: &DispatchTuning,
    max_reaction_depth: u32,
) -> Option<BlockReason> {
    // A Clarify is the most conservative action — *when somebody can answer it*.
    //
    // For an actor holding no `AskHuman` capability there is no one, so a question is not a
    // conservative fallback, it is a dead end: the run is spent producing something delivered to
    // nobody. The capability model already says this ("structurally unable to block on a person who
    // isn't there") and the homelab's `dispatcher` grant already omits `AskHuman` — this guard is
    // the dispatcher finally reading a constraint that was declared all along, rather than a new
    // rule. A live evening-debrief cron burned a run on "how should I proceed?" at 01:55.
    if matches!(decision.action, DispatchAction::Clarify { .. }) {
        if !req.capabilities.grants_ask_human() {
            return blocked(
                "ask_human_capability",
                BlockReason::Unattended,
                "Clarify requires an interlocutor and this actor holds no AskHuman capability",
            );
        }
        return None;
    }

    // (1) Capability check — never auto-widen (Decision 4 invariant). Everything the action would
    // invoke must be granted in the active capability set.
    //
    // Checked at whatever precision the reference carries: a seed call names a concrete
    // `<mcp>:<tool>`, so it is checked against the tool-level grant; a `relevant_mcps` /
    // `allowed_mcps` hint names only a server, so only the server can be checked. Collapsing seed
    // calls to their MCP (which this used to do) would pass a partial grant's *ungranted* tools
    // pre-flight and leave the refusal to `RiskGatedToolRuntime` — safe, since that gate does ask the
    // tool-level question, but it burns a dispatch turn to arrive at an error we could name here.
    for reference in referenced_grants(&decision.action) {
        let authorized = match reference.contains(':') {
            true => req.capabilities.grants_tool(reference),
            false => req.capabilities.grants_mcp(reference),
        };
        if !authorized {
            tracing::warn!(
                reference,
                action = %decision.action,
                "capability gap: action references something not in the dispatcher grant"
            );
            return blocked(
                "mcp_grant",
                BlockReason::CapabilityGap,
                &format!("action references '{reference}', which the grant does not include"),
            );
        }
    }

    // (2) Consequence gate (§6 #3) — a permitted action that would touch something irreversible or
    // external (an email/message, an unversioned delete) needs human confirmation, even at high
    // confidence. A git-tracked vault write is `Reversible` and passes; `External`/`Irreversible`
    // does not.
    let consequence = max_consequence(&decision.action, req);
    if consequence >= CONSEQUENCE_GATE {
        return blocked(
            "consequence",
            BlockReason::HighConsequence,
            &format!("an MCP in scope is rated {consequence:?} (gate {CONSEQUENCE_GATE:?})"),
        );
    }

    // (2b) Zone-write-class gate (§6 #2) — a permitted, low-*general*-consequence action can still
    // target a *specific* vault zone the human has restricted (`proposal_only`/`human_only`).
    // Pre-flight scope only (this checks `ExecuteDirect`'s seed calls — see `zone_restricted`'s own
    // doc comment); the real, always-enforced boundary for every call including adaptive ones is
    // `RiskGatedToolRuntime`.
    if zone_restricted(&decision.action, req) {
        return blocked(
            "zone_write_class",
            BlockReason::ZoneRestricted,
            "a seed call targets a zone that is not directly agent-writable",
        );
    }

    // (3) Magnitude gate — a *sweeping destructive* action is high-stakes by reach even when each
    // change is reversible ("delete all my notes" in a git-tracked vault). Read from the goal: it's
    // the tool-independent signal available pre-execution, and (unlike a specific tool name) it
    // survives the model routing the work to a subagent. Liberado owns this classification because
    // MCP tools don't declare their own risk. Per-call, args-aware enforcement is a later layer.
    // Scoped to the *instruction* — a goal that merely narrates a past deletion in a trailing
    // `Context:` section is not asking for one. See `instruction_scope`'s doc comment for the
    // live false positive that motivated this.
    let instruction = instruction_scope(&req.goal);
    if is_sweeping_destructive(instruction) {
        return blocked(
            "magnitude",
            BlockReason::HighConsequence,
            &format!(
                "instruction reads as sweeping+destructive ({} of {} goal chars scanned)",
                instruction.len(),
                req.goal.len()
            ),
        );
    }

    // (4) Reaction-depth guard — halt runaway background cascades.
    if req.reaction_depth >= max_reaction_depth {
        return blocked(
            "reaction_depth",
            BlockReason::DepthLimit,
            &format!("depth {} >= max {max_reaction_depth}", req.reaction_depth),
        );
    }

    // (5) Confidence floor — below the bar, ask rather than act. The write threshold is applied
    // conservatively to any action-taking decision (read/write tiering needs per-tool metadata,
    // deferred); `Clarify` was already excluded above.
    if decision.confidence < tuning.clarify_threshold_write {
        return blocked(
            "confidence_floor",
            BlockReason::LowConfidence,
            &format!(
                "confidence {:.2} < threshold {:.2}",
                decision.confidence, tuning.clarify_threshold_write
            ),
        );
    }

    None
}

/// Name the guard that just fired, alongside the [`BlockReason`] it produces.
///
/// `BlockReason` is a coarse wire type deliberately shared by more than one check: the consequence
/// gate and the magnitude gate both return `HighConsequence`, and they are different problems with
/// different fixes. When a live proposal appeared with `downgrade=HighConsequence`, telling the two
/// apart required re-running the heuristic offline against the goal text — the log could not answer
/// it. `guard=` closes that, and matches the field name `RiskGatedToolRuntime::authority_decision`
/// uses on the runtime side, so one grep covers both enforcement points.
fn blocked(guard: &'static str, reason: BlockReason, detail: &str) -> Option<BlockReason> {
    tracing::warn!(guard, ?reason, detail = %detail, "pre-flight guard blocked the action");
    Some(reason)
}

/// The MCPs an action would invoke. The tool-name convention is `"<mcp>:<tool>"`; a bare name is
/// treated as the MCP itself.
/// Everything an action would invoke, each at the precision the action states it: a qualified
/// `"<mcp>:<tool>"` for a concrete seed call, a bare MCP name for a scope hint.
///
/// The caller distinguishes them by the `:` and asks the matching question. Kept as one list because
/// the guard's job is "is all of this granted", and splitting it would invite checking one list and
/// forgetting the other.
fn referenced_grants(action: &DispatchAction) -> Vec<&str> {
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
            ..
        } => seed_calls
            // The tool name whole, not `mcp_of` it: this is the one place the action is specific
            // enough to check a per-tool grant, and discarding that was the precision loss.
            .iter()
            .map(|c| c.tool.as_str())
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
    referenced_grants(action)
        .into_iter()
        // `mcp_of` is mandatory here, not cosmetic: consequence is declared per MCP, and
        // `referenced_grants` now yields qualified `<mcp>:<tool>` names for seed calls. Comparing
        // those against `d.name` matches nothing, and an unmatched entry contributes
        // `Consequence::ReadOnly` — so omitting this would quietly disarm the consequence gate for
        // every `ExecuteDirect`, the exact actions it exists to catch.
        .map(mcp_of)
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
    use liberado_common::{
        Capability, CapabilitySet, Consequence, Delivery, Depth, ToolCall, Zone,
    };

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
                zone_from_arg: None,
                write_tools: Vec::new(),
            }],
            capabilities,
            reaction_depth,
            zone_write_classes: Vec::new(),
        }
    }

    fn clarify_decision() -> DispatchDecision {
        DispatchDecision {
            action: DispatchAction::Clarify {
                questions: vec!["which one?".into()],
                what_blocked: BlockReason::Ambiguous,
            },
            confidence: 0.9,
            rationale: "test".into(),
        }
    }

    /// A cron holds no `AskHuman`, so a `Clarify` is a dead end: delivered to nobody, run spent.
    /// The homelab's `dispatcher` grant already omitted the capability — the dispatcher just never
    /// read it, and a live evening-debrief burned a run on "how should I proceed?" at 01:55.
    #[test]
    fn an_unattended_actor_may_not_be_asked_to_clarify() {
        let unattended = granted("tasks-mcp"); // no AskHuman
        let reason = evaluate(
            &clarify_decision(),
            &req(unattended, 0),
            &DispatchTuning::default(),
            5,
        );
        assert_eq!(reason, Some(BlockReason::Unattended));
    }

    /// A seed call names a concrete tool, so a per-tool grant must be read at that precision here
    /// rather than collapsed to its MCP. Collapsing it passed pre-flight and left the refusal to the
    /// runtime gate — safe, but it spent a dispatch turn to reach an error nameable up front.
    #[test]
    fn a_partial_grant_blocks_an_ungranted_seed_call_at_preflight() {
        let partial = CapabilitySet::from_iter([Capability::ExecuteTool("tasks-mcp:list".into())]);

        let allowed = evaluate(
            &execute_direct("tasks-mcp:list", 0.95),
            &req(partial.clone(), 0),
            &DispatchTuning::default(),
            5,
        );
        assert_eq!(allowed, None, "the granted tool must pass");

        let refused = evaluate(
            &execute_direct("tasks-mcp:delete_all", 0.95),
            &req(partial, 0),
            &DispatchTuning::default(),
            5,
        );
        assert_eq!(
            refused,
            Some(BlockReason::CapabilityGap),
            "another tool on the same MCP is not granted, and the MCP-level question cannot see that"
        );
    }

    /// The consequence gate reads its declaration per MCP, so it has to keep resolving qualified tool
    /// names back to their server. If it stopped, every `ExecuteDirect` would score `ReadOnly` and the
    /// gate would silently pass the actions it exists to catch.
    #[test]
    fn consequence_is_still_resolved_for_a_qualified_seed_call() {
        // `tasks-mcp` is declared `Reversible` in `req`; the catalog is keyed by bare MCP name.
        let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("tasks-mcp".into())]);
        let action = execute_direct("tasks-mcp:list", 0.95);
        assert_eq!(
            max_consequence(&action.action, &req(caps, 0)),
            Consequence::Reversible,
            "a qualified tool name must still resolve to its MCP's declared consequence"
        );
    }

    /// ...and an actor that *can* ask is untouched: Clarify remains the conservative answer there.
    #[test]
    fn an_interactive_actor_may_still_clarify() {
        let mut interactive = granted("tasks-mcp");
        interactive.grant(Capability::AskHuman);
        let reason = evaluate(
            &clarify_decision(),
            &req(interactive, 0),
            &DispatchTuning::default(),
            5,
        );
        assert_eq!(reason, None);
    }

    fn execute_direct(tool: &str, confidence: f32) -> DispatchDecision {
        DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: vec![ToolCall {
                    tool: tool.into(),
                    args: serde_json::json!({}),
                }],
                relevant_mcps: Vec::new(),
                delivery: Delivery::Summarize,
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
                zone_from_arg: None,
                write_tools: Vec::new(),
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
                zone_from_arg: None,
                write_tools: Vec::new(),
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
                zone_from_arg: None,
                write_tools: Vec::new(),
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
                delivery: Delivery::Summarize,
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
                delivery: Delivery::Summarize,
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
                delivery: Delivery::Summarize,
                depth: Depth::Normal,
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

    /// A Clarify skips the *other* guards — confidence floor, depth limit — because asking is
    /// already the conservative answer. Renamed from `clarify_is_never_downgraded`: that was true
    /// unconditionally until the AskHuman guard, and "never" is now wrong. The exemption holds only
    /// when someone can actually answer, so this fixture must grant `AskHuman` to test it.
    #[test]
    fn clarify_skips_the_other_guards_when_a_human_is_reachable() {
        let d = DispatchDecision {
            action: DispatchAction::Clarify {
                questions: vec!["which?".into()],
                what_blocked: BlockReason::Ambiguous,
            },
            confidence: 0.0, // would trip the confidence floor if it applied
            rationale: "test".into(),
        };
        let mut caps = CapabilitySet::empty();
        caps.grant(Capability::AskHuman);
        assert_eq!(
            evaluate(&d, &req(caps, 9), &DispatchTuning::default(), 4),
            None
        );
    }

    /// A `vault` MCP request whose seed call targets `write_review` (declared to write to the
    /// `reviews` zone), granted and `Reversible` (so the consequence gate alone would pass it) —
    /// isolating the zone-write-class guard from the consequence gate it sits next to.
    fn vault_request(
        zone_write_classes: Vec<(&str, liberado_common::WriteClass)>,
    ) -> DispatchRequest {
        DispatchRequest {
            goal: "write a review note".into(),
            catalog: vec![McpDescriptor {
                name: "vault".into(),
                description: "git-tracked vault".into(),
                consequence: Consequence::Reversible,
                provenance: None,
                default_zone: Some("tasks".into()),
                tool_zones: vec![("write_review".into(), Some("reviews".into()))],
                zone_from_arg: None,
                write_tools: Vec::new(),
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

    #[test]
    fn confidence_at_the_write_threshold_is_not_low_confidence() {
        let tuning = DispatchTuning::default();
        let d = execute_direct("tasks-mcp:add", tuning.clarify_threshold_write);
        assert_eq!(
            evaluate(&d, &req(granted("tasks-mcp"), 0), &tuning, 4),
            None
        );
    }
}
