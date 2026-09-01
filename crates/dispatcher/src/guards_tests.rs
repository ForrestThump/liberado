use super::*;
use crate::McpDescriptor;
use liberado_common::{
    Capability, CapabilitySet, Consequence, Delivery, Depth, RiskWaiverSet, ToolCall, WriteClass,
    Zone,
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
        risk_waivers: RiskWaiverSet::empty(),
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
        risk_waivers: RiskWaiverSet::empty(),
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
        risk_waivers: RiskWaiverSet::empty(),
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
        risk_waivers: RiskWaiverSet::empty(),
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
            zone_from_arg: None,
            write_tools: Vec::new(),
        }],
        capabilities: granted("vault"),
        reaction_depth: 0,
        zone_write_classes: zone_write_classes
            .into_iter()
            .map(|(z, wc)| (z.to_string(), wc))
            .collect(),
        risk_waivers: RiskWaiverSet::empty(),
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

// --- Risk waiver (#3) --------------------------------------------------------------------
//
// The magnitude heuristic gates goals whose text reads as "sweeping destructive". A waiver
// declared in `policy.toml` and matching every (tool, zone) the action would touch suppresses
// this gate. The capability gate above remains the authority boundary — a waiver without a
// matching grant is still refused.

fn req_with_waivers(
    capabilities: CapabilitySet,
    reaction_depth: u32,
    waivers: Vec<liberado_common::RiskWaiver>,
) -> DispatchRequest {
    let mut r = req(capabilities, reaction_depth);
    r.risk_waivers = RiskWaiverSet {
        waivers: waivers.into_iter().collect(),
    };
    r
}

fn waiver_for(
    mcp: &str,
    tools: Option<Vec<&str>>,
    zones: Option<Vec<&str>>,
) -> liberado_common::RiskWaiver {
    liberado_common::RiskWaiver {
        mcp: mcp.into(),
        match_tools: tools.map(|t| t.into_iter().map(String::from).collect()),
        match_zones: zones.map(|z| z.into_iter().map(String::from).collect()),
        guard: liberado_common::Guard::Magnitude,
    }
}

/// The live false positive: "Read Tasks/Main.md in full. Then write it back with exactly these
/// changes: … Remove the line containing … completely. Keep everything else in the file
/// exactly as-is." — the literal session `01M087A4ZTAV965HWEQHSSN6RR` (2026-08-17). A waiver
/// covering the read+write pair lets it through.
#[test]
fn waiver_suppresses_magnitude_for_targeted_read_then_write() {
    let goal = "Read Tasks/Main.md in full. Then write it back with exactly these changes: \
                Remove the line containing 'X' completely. Keep everything else in the file \
                exactly as-is.";
    let mut r = req(granted("tasks-mcp"), 0);
    r.goal = goal.into();
    r.zone_write_classes = vec![("tasks".into(), WriteClass::AgentWritable)];
    r.catalog = vec![McpDescriptor {
        name: "tasks-mcp".into(),
        description: "task ops".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: Some("tasks".into()),
        tool_zones: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
    }];
    let d = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: vec![
                ToolCall {
                    tool: "tasks-mcp:read".into(),
                    args: serde_json::json!({}),
                },
                ToolCall {
                    tool: "tasks-mcp:write".into(),
                    args: serde_json::json!({}),
                },
            ],
            relevant_mcps: vec![],
            delivery: Delivery::Summarize,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    // Without a waiver: the magnitude heuristic still fires.
    let blocked = evaluate(&d, &r, &DispatchTuning::default(), 4);
    assert_eq!(
        blocked,
        Some(BlockReason::HighConsequence),
        "the goal must trip the magnitude gate before any waiver exists"
    );

    // With a waiver covering both tools: the gate is suppressed and the action passes.
    let waived = req_with_waivers(
        granted("tasks-mcp"),
        0,
        vec![waiver_for("tasks-mcp", Some(vec!["read", "write"]), None)],
    );
    let mut waived = waived;
    waived.goal = r.goal.clone();
    waived.catalog = r.catalog.clone();
    waived.zone_write_classes = r.zone_write_classes.clone();
    assert_eq!(
        evaluate(&d, &waived, &DispatchTuning::default(), 4),
        None,
        "a covering waiver must suppress the magnitude gate"
    );
}

#[test]
fn whole_mcp_waiver_covers_adaptive_direct_scope_without_seed_calls() {
    let mut r = req_with_waivers(
        granted("tasks-mcp"),
        0,
        vec![waiver_for("tasks-mcp", None, None)],
    );
    r.goal = "Remove the obsolete line but keep everything else exactly as-is.".into();
    let d = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: vec!["tasks-mcp".into()],
            delivery: Delivery::Summarize,
        },
        confidence: 0.9,
        rationale: "adaptive task edit".into(),
    };

    assert_eq!(evaluate(&d, &r, &DispatchTuning::default(), 4), None);
}

#[test]
fn tool_filtered_waiver_does_not_cover_unknown_adaptive_calls() {
    let mut r = req_with_waivers(
        granted("tasks-mcp"),
        0,
        vec![waiver_for("tasks-mcp", Some(vec!["read_note"]), None)],
    );
    r.goal = "Remove every obsolete task.".into();
    let d = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: vec!["tasks-mcp".into()],
            delivery: Delivery::Summarize,
        },
        confidence: 0.9,
        rationale: "adaptive task edit".into(),
    };

    assert_eq!(
        evaluate(&d, &r, &DispatchTuning::default(), 4),
        Some(BlockReason::HighConsequence)
    );
}

/// A waiver that only covers the read tool does NOT cover the write — the magnitude heuristic
/// still fires because the action has a non-waived surface.
#[test]
fn partial_waiver_does_not_suppress_magnitude() {
    let goal = "Remove everything and keep nothing else.";
    let mut r = req(granted("tasks-mcp"), 0);
    r.goal = goal.into();
    r.zone_write_classes = vec![("tasks".into(), WriteClass::AgentWritable)];
    let d = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: vec![
                ToolCall {
                    tool: "tasks-mcp:read".into(),
                    args: serde_json::json!({}),
                },
                ToolCall {
                    tool: "tasks-mcp:write".into(),
                    args: serde_json::json!({}),
                },
            ],
            relevant_mcps: vec![],
            delivery: Delivery::Summarize,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    let partial = req_with_waivers(
        granted("tasks-mcp"),
        0,
        vec![waiver_for("tasks-mcp", Some(vec!["read"]), None)],
    );
    let mut partial = partial;
    partial.goal = r.goal.clone();
    partial.catalog = r.catalog.clone();
    partial.zone_write_classes = r.zone_write_classes.clone();
    assert_eq!(
        evaluate(&d, &partial, &DispatchTuning::default(), 4),
        Some(BlockReason::HighConsequence),
        "partial coverage: the write tool's reach is not waived, so the heuristic still fires"
    );
}

/// Waivers do not grant authority. A waiver for an MCP the agent cannot invoke is caught by
/// the capability guard above, before the magnitude gate runs.
#[test]
fn waiver_does_not_grant_authority() {
    let goal = "Do all the things.";
    let mut r = req(granted("other-mcp"), 0);
    r.goal = goal.into();
    r.catalog = vec![
        McpDescriptor {
            name: "other-mcp".into(),
            description: "other".into(),
            consequence: Consequence::Reversible,
            provenance: None,
            default_zone: None,
            tool_zones: Vec::new(),
            zone_from_arg: None,
            write_tools: Vec::new(),
        },
        McpDescriptor {
            name: "forbidden-mcp".into(),
            description: "forbidden".into(),
            consequence: Consequence::Reversible,
            provenance: None,
            default_zone: None,
            tool_zones: Vec::new(),
            zone_from_arg: None,
            write_tools: Vec::new(),
        },
    ];
    let d = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: vec![ToolCall {
                tool: "forbidden-mcp:do_it".into(),
                args: serde_json::json!({}),
            }],
            relevant_mcps: vec![],
            delivery: Delivery::Summarize,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    let waived = req_with_waivers(
        granted("other-mcp"),
        0,
        vec![waiver_for("forbidden-mcp", None, None)],
    );
    let mut waived = waived;
    waived.goal = r.goal.clone();
    waived.catalog = r.catalog.clone();
    assert_eq!(
        evaluate(&d, &waived, &DispatchTuning::default(), 4),
        Some(BlockReason::CapabilityGap),
        "a waiver does not bypass the capability grant; the agent still cannot reach the MCP"
    );
}

/// An `ExecuteDirect` with no seed calls ("let the executor decide every step") has no
/// `(tool, zone)` targets for the magnitude waiver to match — so an empty waiver set must
/// not accidentally waive the gate. The previous mutation `|| targets.is_empty()` would
/// have done exactly that, and only this test would have caught it.
#[test]
fn empty_target_list_does_not_waive_magnitude() {
    let goal = "Delete all my notes and remove everything else.";
    let mut r = req(granted("tasks-mcp"), 0);
    r.goal = goal.into();
    r.zone_write_classes = vec![("tasks".into(), WriteClass::AgentWritable)];
    let d = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    // No waivers configured: must still trip.
    assert_eq!(
        evaluate(&d, &r, &DispatchTuning::default(), 4),
        Some(BlockReason::HighConsequence),
        "no seed calls means no waiver targets; the magnitude gate still fires on the goal"
    );
}
