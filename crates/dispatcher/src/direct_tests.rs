//! Split from `lib_tests.rs` for module-health boundaries.
//!
//! `direct = true` (the `[[schedules]]` opt-in flag for cron goals that should bypass the
//! router) lives in a sibling module so this file stays at its cyclomatic / functions /
//! ploc baseline. The tests are short and isolated — they pin the dispatcher behavior the
//! PR promises. Re-declares the helpers it needs rather than reaching into the parent's
//! private namespace.

use super::*;
use liberado_common::{
    Capability, CapabilitySet, Consequence, Delivery, DispatchAction, McpDescriptor, RiskWaiverSet,
    ToolCall,
};
use liberado_provider::{CompletionResponse, MockProvider};
use std::sync::Arc;

fn req(capabilities: CapabilitySet, reaction_depth: u32, direct: bool) -> DispatchRequest {
    DispatchRequest {
        goal: "do the thing".into(),
        catalog: vec![McpDescriptor {
            name: "turbovault".into(),
            description: "vault ops".into(),
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
        direct,
    }
}

fn scripted(decision: &DispatchDecision) -> Arc<MockProvider> {
    Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text(
            serde_json::to_string(decision).unwrap(),
        )],
    ))
}

fn execute_direct_with_seed(tool: &str, confidence: f32) -> DispatchDecision {
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

fn caps(mcp: &str) -> CapabilitySet {
    CapabilitySet::from_iter([Capability::ExecuteMcp(mcp.into())])
}

/// `direct = true` must skip the router entirely and synthesize an `ExecuteDirect`
/// decision with confidence 1.0 and no MCP narrowing. The mock provider has an empty
/// scripted completion list — any provider call would fail, so the assertion succeeding
/// is itself proof no router call landed.
#[tokio::test]
async fn direct_dispatch_bypasses_the_router_and_synthesizes_execute_direct() {
    let mock = Arc::new(MockProvider::with_script("mock", Vec::new()));
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);
    let mut req = req(caps("turbovault"), 1, true);
    req.goal = "do something specific".into();

    let out = dispatcher.dispatch(&req).await.unwrap();
    match &out.action {
        DispatchAction::ExecuteDirect {
            seed_calls,
            relevant_mcps,
            ..
        } => {
            assert!(seed_calls.is_empty(), "direct must not seed tool calls");
            assert!(
                relevant_mcps.is_empty(),
                "direct must not narrow MCPs (executor decides)"
            );
        }
        other => panic!("expected ExecuteDirect, got {other:?}"),
    }
    assert_eq!(out.confidence, 1.0, "direct must claim full confidence");
}

/// `direct = false` (the default) must still go through the router — this regression
/// guard pairs with the test above so a stray default-true can never sneak in.
#[tokio::test]
async fn direct_false_routes_through_the_router() {
    let mock = scripted(&execute_direct_with_seed("tasks-mcp:add", 0.95));
    let dispatcher = Dispatcher::new(mock.clone(), DispatchTuning::default(), 4);
    let req = req(caps("tasks-mcp"), 0, false);

    let out = dispatcher.dispatch(&req).await.unwrap();
    assert!(matches!(out.action, DispatchAction::ExecuteDirect { .. }));
    let sent = mock.last_request().unwrap();
    assert_eq!(
        sent.temperature,
        Some(0.0),
        "router must be called when direct = false"
    );
}

/// `direct = true` must still flow through guards. A sweeping goal + low confidence is
/// not reachable here (confidence is hard-coded to 1.0), but the magnitude guard
/// should still run — set the goal text to something that *would* trip it and assert
/// the guard downgraded to Propose. The router would have caught the same case, so
/// the bypass must not weaken it.
#[tokio::test]
async fn direct_dispatch_still_runs_the_magnitude_guard() {
    let mock = Arc::new(MockProvider::with_script("mock", Vec::new()));
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);
    let mut req = req(caps("turbovault"), 0, true);
    req.goal = "delete all of my notes".into();

    let out = dispatcher.dispatch(&req).await.unwrap();
    assert!(
        matches!(out.action, DispatchAction::Propose { .. }),
        "magnitude guard must still downgrade sweeping goals, got {:?}",
        out.action
    );
}
