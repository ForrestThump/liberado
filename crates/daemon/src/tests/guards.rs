//! Tests for guard conformance: capability gap, high consequence, and sweeping destructive gates.

use liberado_common::{
    BlockReason, Capability, CapabilitySet, Consequence, Delivery, DispatchAction,
    DispatchDecision, McpDescriptor, ProposalSigner, RiskWaiverSet, ToolCall,
};
use liberado_config_loader::DispatchTuning;
use liberado_dispatcher::guards::evaluate;
use liberado_executor::{RiskGatedToolRuntime, ToolRuntime};
use liberado_provider::{ToolDef, ToolInvocation};
use std::sync::Arc;

#[tokio::test]
async fn guard_conformance_capability_gap_agrees_both_sides() {
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("tasks-mcp".into())]);
    let catalog = vec![McpDescriptor {
        name: "email-mcp".into(),
        description: "email".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
    }];

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: vec![ToolCall {
                tool: "email-mcp:send".into(),
                args: serde_json::json!({}),
            }],
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "test".into(),
    };
    let req = liberado_dispatcher::DispatchRequest {
        goal: "send an email".into(),
        catalog,
        capabilities: caps.clone(),
        reaction_depth: 0,
        zone_write_classes: Vec::new(),
        risk_waivers: RiskWaiverSet::empty(),
    };
    assert_eq!(
        evaluate(&decision, &req, &DispatchTuning::default(), 4),
        Some(BlockReason::CapabilityGap),
        "dispatcher: ungranted MCP must be CapabilityGap"
    );

    struct NoopRt;
    #[async_trait::async_trait]
    impl ToolRuntime for NoopRt {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    let rt = RiskGatedToolRuntime::new(
        Arc::new(NoopRt),
        caps,
        vec![("email-mcp".into(), Consequence::Reversible)],
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        "send an email".into(),
        "t1-guards-cap".into(),
        ProposalSigner::random(),
        "default",
        RiskWaiverSet::empty(),
    );
    let runtime_result = rt
        .invoke(&ToolInvocation::new(
            "c1",
            "email-mcp:send",
            serde_json::json!({}),
        ))
        .await;
    assert!(
        runtime_result.is_err(),
        "runtime must also reject ungranted MCP"
    );
}

#[tokio::test]
async fn guard_conformance_consequence_agrees_on_external_mcp() {
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("email".into())]);
    let catalog = vec![McpDescriptor {
        name: "email".into(),
        description: "send email".into(),
        consequence: Consequence::External,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
    }];

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: vec![ToolCall {
                tool: "email:send".into(),
                args: serde_json::json!({}),
            }],
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "test".into(),
    };
    let req = liberado_dispatcher::DispatchRequest {
        goal: "email my boss".into(),
        catalog,
        capabilities: caps.clone(),
        reaction_depth: 0,
        zone_write_classes: Vec::new(),
        risk_waivers: RiskWaiverSet::empty(),
    };
    assert_eq!(
        evaluate(&decision, &req, &DispatchTuning::default(), 4),
        Some(BlockReason::HighConsequence),
        "dispatcher: External MCP must be HighConsequence"
    );

    struct NoopRt2;
    #[async_trait::async_trait]
    impl ToolRuntime for NoopRt2 {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    let rt = RiskGatedToolRuntime::new(
        Arc::new(NoopRt2),
        caps,
        vec![("email".into(), Consequence::External)],
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        "email my boss".into(),
        "t1-guards-consequence".into(),
        ProposalSigner::random(),
        "default",
        RiskWaiverSet::empty(),
    );
    let runtime_result = rt
        .invoke(&ToolInvocation::new(
            "c1",
            "email:send",
            serde_json::json!({}),
        ))
        .await;
    assert!(
        runtime_result.is_ok() && runtime_result.unwrap().contains("PROPOSAL"),
        "runtime must downgrade External MCP to a proposal"
    );
}

#[tokio::test]
async fn guard_conformance_magnitude_agrees_on_sweeping_destructive() {
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("vault".into())]);
    let catalog = vec![McpDescriptor {
        name: "vault".into(),
        description: "git-tracked vault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
    }];
    let goal = "delete all of my notes and erase everything";

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: vec![ToolCall {
                tool: "vault:write".into(),
                args: serde_json::json!({}),
            }],
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "test".into(),
    };
    let req = liberado_dispatcher::DispatchRequest {
        goal: goal.into(),
        catalog,
        capabilities: caps.clone(),
        reaction_depth: 0,
        zone_write_classes: Vec::new(),
        risk_waivers: RiskWaiverSet::empty(),
    };
    assert_eq!(
        evaluate(&decision, &req, &DispatchTuning::default(), 4),
        Some(BlockReason::HighConsequence),
        "dispatcher: sweeping-destructive goal must be HighConsequence"
    );

    struct NoopRt3;
    #[async_trait::async_trait]
    impl ToolRuntime for NoopRt3 {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    let rt = RiskGatedToolRuntime::new(
        Arc::new(NoopRt3),
        caps,
        vec![("vault".into(), Consequence::Reversible)],
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        goal.into(),
        "t1-guards-magnitude".into(),
        ProposalSigner::random(),
        "default",
        RiskWaiverSet::empty(),
    );
    let runtime_result = rt
        .invoke(&ToolInvocation::new(
            "c1",
            "vault:write",
            serde_json::json!({}),
        ))
        .await;
    assert!(
        runtime_result.is_ok() && runtime_result.unwrap().contains("PROPOSAL"),
        "runtime must downgrade sweeping-destructive call to a proposal"
    );
}
