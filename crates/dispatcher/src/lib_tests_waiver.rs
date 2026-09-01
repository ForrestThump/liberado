use super::*;
use liberado_common::{Consequence, Delivery, DispatchAction, DispatchDecision, RiskWaiverSet};

#[tokio::test]
async fn high_consequence_without_seed_calls_proposes_the_exact_adaptive_goal() {
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
        capabilities: caps("vault"),
        reaction_depth: 0,
        zone_write_classes: Vec::new(),
        risk_waivers: RiskWaiverSet::empty(),
    };
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "test".into(),
    };
    let mock = scripted(&decision);
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let out = dispatcher.dispatch(&request).await.unwrap();
    match out.action {
        DispatchAction::Propose {
            proposed_action:
                liberado_common::ProposedAction::AdaptiveGoal {
                    goal,
                    capabilities,
                    relevant_mcps,
                    delivery,
                    approved_guard,
                },
            ..
        } => {
            assert_eq!(goal, "delete all of my notes");
            assert!(capabilities.grants_mcp("vault"));
            assert!(relevant_mcps.is_empty());
            assert_eq!(delivery, Delivery::Summarize);
            assert_eq!(approved_guard, liberado_common::ApprovedGuard::Magnitude);
        }
        other => panic!("expected Propose(AdaptiveGoal), got {other:?}"),
    }
}

#[tokio::test]
async fn deleting_one_entire_named_line_is_bounded_and_executes_directly() {
    let mut request = request(caps("tasks-mcp"), 0);
    request.goal = "Remove the entire line containing Mom's September birthday gift".into();
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: vec!["tasks-mcp".into()],
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "one exact task line".into(),
    };
    let mock = scripted(&decision);
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let out = dispatcher.dispatch(&request).await.unwrap();
    assert!(matches!(out.action, DispatchAction::ExecuteDirect { .. }));
}
