//! Split from `lib.rs` for module-health boundaries.

use super::*;
use liberado_common::{
    Capability, Consequence, Delivery, Depth, RiskWaiverSet, ToolCall, WriteClass,
};
use liberado_provider::{CompletionResponse, MockProvider, ResponseFormat};
use std::sync::Mutex;

#[path = "lib_tests_waiver.rs"]
mod waiver_tests;

/// One recorded `record_tool_selection` call: (goal, chosen MCP, offered MCP names).
type RecordedSelection = (String, Option<String>, Vec<String>);

struct MockGuidance {
    hits: Vec<GuidanceHit>,
    recorded: Mutex<Vec<RecordedSelection>>,
}
impl MockGuidance {
    fn with_hits(hits: Vec<GuidanceHit>) -> Arc<Self> {
        Arc::new(Self {
            hits,
            recorded: Mutex::new(Vec::new()),
        })
    }
}
#[async_trait::async_trait]
impl ToolGuidanceSource for MockGuidance {
    async fn search_tool_guidance(&self, _goal: &str) -> Vec<GuidanceHit> {
        self.hits.clone()
    }

    async fn save_tool_guidance(
        &self,
        directive: &str,
        task_type: Option<String>,
        tools_used: Vec<String>,
    ) {
        self.recorded
            .lock()
            .unwrap()
            .push((directive.to_string(), task_type, tools_used));
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
fn request(capabilities: CapabilitySet, reaction_depth: u32) -> DispatchRequest {
    DispatchRequest {
        goal: "add milk to the shopping list".into(),
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
fn caps(mcp: &str) -> CapabilitySet {
    CapabilitySet::from_iter([Capability::ExecuteMcp(mcp.into())])
}
#[tokio::test]
async fn granted_execute_direct_passes_through_at_temp_zero_json() {
    let mock = scripted(&execute_direct("tasks-mcp:add", 0.95));
    let dispatcher = Dispatcher::new(mock.clone(), DispatchTuning::default(), 4);

    let out = dispatcher
        .dispatch(&request(caps("tasks-mcp"), 0))
        .await
        .unwrap();
    assert!(matches!(out.action, DispatchAction::ExecuteDirect { .. }));

    // Classification ran at temperature 0 in structured-output mode.
    let sent = mock.last_request().unwrap();
    assert_eq!(sent.temperature, Some(0.0));
    assert!(matches!(sent.response_format, ResponseFormat::Json { .. }));
}
#[tokio::test]
async fn narrow_direct_tools_default_keeps_relevant_mcps() {
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: vec!["tasks-mcp".into()],
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "test".into(),
    };
    let mock = scripted(&decision);
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let out = dispatcher
        .dispatch(&request(caps("tasks-mcp"), 0))
        .await
        .unwrap();
    match out.action {
        DispatchAction::ExecuteDirect { relevant_mcps, .. } => {
            assert_eq!(relevant_mcps, vec!["tasks-mcp".to_string()])
        }
        other => panic!("expected ExecuteDirect, got {other:?}"),
    }
}
#[tokio::test]
async fn narrow_direct_tools_off_clears_relevant_mcps() {
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: vec!["tasks-mcp".into()],
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "test".into(),
    };
    let mock = scripted(&decision);
    let tuning = DispatchTuning {
        narrow_direct_tools: false,
        ..DispatchTuning::default()
    };
    let dispatcher = Dispatcher::new(mock, tuning, 4);

    let out = dispatcher
        .dispatch(&request(caps("tasks-mcp"), 0))
        .await
        .unwrap();
    match out.action {
        DispatchAction::ExecuteDirect { relevant_mcps, .. } => {
            assert!(relevant_mcps.is_empty(), "expected relevant_mcps cleared")
        }
        other => panic!("expected ExecuteDirect, got {other:?}"),
    }
}
#[tokio::test]
async fn ungranted_mcp_is_downgraded_to_capability_gap() {
    // email-mcp must be *in the catalog* so sanitize keeps the seed; only the grant is missing.
    let mock = scripted(&execute_direct("email-mcp:send", 0.95));
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);
    let mut req = request(caps("tasks-mcp"), 0);
    req.catalog.push(McpDescriptor {
        name: "email-mcp".into(),
        description: "mail".into(),
        consequence: Consequence::External,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
    });

    let out = dispatcher.dispatch(&req).await.unwrap();
    match out.action {
        DispatchAction::Clarify { what_blocked, .. } => {
            assert_eq!(what_blocked, BlockReason::CapabilityGap)
        }
        other => panic!("expected Clarify(CapabilityGap), got {other:?}"),
    }
}
#[tokio::test]
async fn low_confidence_is_downgraded() {
    let mock = scripted(&execute_direct("tasks-mcp:add", 0.4));
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let out = dispatcher
        .dispatch(&request(caps("tasks-mcp"), 0))
        .await
        .unwrap();
    match out.action {
        DispatchAction::Clarify { what_blocked, .. } => {
            assert_eq!(what_blocked, BlockReason::LowConfidence)
        }
        other => panic!("expected Clarify(LowConfidence), got {other:?}"),
    }
}
#[tokio::test]
async fn user_message_places_catalog_before_goal_for_cache_reuse() {
    let mut req = request(caps("tasks-mcp"), 0);
    // A writable zone so the (stable) zone block is present and its position is assertable.
    req.zone_write_classes = vec![("tasks".into(), WriteClass::AgentWritable)];
    let mock = scripted(&execute_direct("tasks-mcp:add", 0.95));
    let dispatcher = Dispatcher::new(mock.clone(), DispatchTuning::default(), 4);
    dispatcher.dispatch(&req).await.unwrap();
    let sent = mock.last_request().unwrap();
    let user_message = &sent.messages[1].content;
    let cat_pos = user_message
        .find("Available MCPs:")
        .expect("catalog header missing");
    let goal_pos = user_message.find("Goal:").expect("goal header missing");
    assert!(
        cat_pos < goal_pos,
        "catalog must appear before the goal so the stable MCP listing is in the shared \
             prefix; cache hit is otherwise ~22% vs ~76% elsewhere. \
             got catalog at {cat_pos}, goal at {goal_pos}"
    );
    // The vault zone list is stable too — it must sit inside the same prefix, not after the
    // goal. Fixing only the catalog leaves the identical mistake one block further down.
    let zone_pos = user_message
        .find("Vault zones")
        .expect("this fixture declares a writable zone, so the block must be present");
    assert!(
        zone_pos < goal_pos,
        "the zone list is identical on every call and belongs before the varying goal; \
             got zones at {zone_pos}, goal at {goal_pos}"
    );
}
#[tokio::test]
async fn prompt_includes_vault_zones_when_writable() {
    let mut req = request(caps("tasks-mcp"), 0);
    req.zone_write_classes = vec![("tasks".into(), WriteClass::AgentWritable)];
    let mock = scripted(&execute_direct("tasks-mcp:add", 0.95));
    let dispatcher = Dispatcher::new(mock.clone(), DispatchTuning::default(), 4);
    dispatcher.dispatch(&req).await.unwrap();
    let sent = mock.last_request().unwrap();
    let user_message = &sent.messages[1].content;
    assert!(
        user_message.contains("Vault zones"),
        "prompt must include vault zones when writable zones exist"
    );
}
#[tokio::test]
async fn prompt_excludes_vault_zones_when_not_writable() {
    let req = request(caps("tasks-mcp"), 0);
    let mock = scripted(&execute_direct("tasks-mcp:add", 0.95));
    let dispatcher = Dispatcher::new(mock.clone(), DispatchTuning::default(), 4);
    dispatcher.dispatch(&req).await.unwrap();
    let sent = mock.last_request().unwrap();
    let user_message = &sent.messages[1].content;
    assert!(
        !user_message.contains("Vault zones"),
        "prompt must not include vault zones when no writable zones exist"
    );
}
#[tokio::test]
async fn high_consequence_concrete_action_is_downgraded_to_propose() {
    // A granted, confident ExecuteDirect whose seed call hits an External MCP: the consequence
    // gate must turn it into a Propose carrying the call — NOT a Clarify (Decision 11 emit path).
    let request = DispatchRequest {
        goal: "email my boss the update".into(),
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
        capabilities: caps("email"),
        reaction_depth: 0,
        zone_write_classes: Vec::new(),
        risk_waivers: RiskWaiverSet::empty(),
    };
    let mock = scripted(&execute_direct("email:send", 0.95));
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let out = dispatcher.dispatch(&request).await.unwrap();
    match out.action {
        DispatchAction::Propose {
            proposed_action: liberado_common::ProposedAction::ToolCalls(calls),
            ..
        } => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].tool, "email:send");
        }
        other => panic!("expected Propose(ToolCalls), got {other:?}"),
    }
}
#[tokio::test]
async fn zone_restricted_concrete_action_is_downgraded_to_propose() {
    // A granted, confident, Reversible-consequence ExecuteDirect whose seed call targets a
    // ProposalOnly zone: the zone-write-class guard (§6 #2) must turn it into a Propose
    // carrying the call, the exact same treatment as HighConsequence gets — not a bare
    // Clarify. See guards.rs's own unit tests for the guard-level isolation of this case.
    let request = DispatchRequest {
        goal: "write a review note".into(),
        catalog: vec![McpDescriptor {
            name: "vault".into(),
            description: "git-tracked vault".into(),
            consequence: Consequence::Reversible,
            provenance: None,
            default_zone: None,
            tool_zones: vec![("write_review".into(), Some("reviews".into()))],
            zone_from_arg: None,
            write_tools: Vec::new(),
        }],
        capabilities: caps("vault"),
        reaction_depth: 0,
        zone_write_classes: vec![("reviews".into(), liberado_common::WriteClass::ProposalOnly)],
        risk_waivers: RiskWaiverSet::empty(),
    };
    let mock = scripted(&execute_direct("vault:write_review", 0.95));
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let out = dispatcher.dispatch(&request).await.unwrap();
    match out.action {
        DispatchAction::Propose {
            proposed_action: liberado_common::ProposedAction::ToolCalls(calls),
            ..
        } => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].tool, "vault:write_review");
        }
        other => panic!("expected Propose(ToolCalls), got {other:?}"),
    }
}
#[tokio::test]
async fn high_consequence_subagent_is_downgraded_to_propose() {
    // A DispatchSubagent whose allowed_mcps touches an External MCP: the consequence gate must
    // turn it into a Propose(Subagent) — NOT a Clarify — since a restated goal is always
    // concrete enough to propose (unlike an empty-seed ExecuteDirect).
    let request = DispatchRequest {
        goal: "summarize this week's reviews and email the boss".into(),
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
        capabilities: caps("email"),
        reaction_depth: 0,
        zone_write_classes: Vec::new(),
        risk_waivers: RiskWaiverSet::empty(),
    };
    let decision = DispatchDecision {
        action: DispatchAction::DispatchSubagent {
            goal: "summarize this week's reviews and email the boss".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: vec!["email".into()],
            success_criteria: vec!["the boss received the summary".into()],
            artifact_target: None,
            model: None,
            correlation_id: "c1".into(),
            delivery: Delivery::Summarize,
            depth: Depth::Normal,
        },
        confidence: 0.9,
        rationale: "open-ended, touches an external MCP".into(),
    };
    let mock = scripted(&decision);
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let out = dispatcher.dispatch(&request).await.unwrap();
    match out.action {
        DispatchAction::Propose {
            proposed_action:
                liberado_common::ProposedAction::Subagent {
                    goal,
                    allowed_mcps,
                    success_criteria,
                    ..
                },
            ..
        } => {
            assert_eq!(goal, "summarize this week's reviews and email the boss");
            assert_eq!(allowed_mcps, vec!["email".to_string()]);
            assert_eq!(
                success_criteria,
                vec!["the boss received the summary".to_string()]
            );
        }
        other => panic!("expected Propose(Subagent), got {other:?}"),
    }
}
#[tokio::test]
async fn deep_reaction_is_halted() {
    let mock = scripted(&execute_direct("tasks-mcp:add", 0.95));
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    // A confidently-classified, fully-granted call is still halted at the depth cap.
    let out = dispatcher
        .dispatch(&request(caps("tasks-mcp"), 4))
        .await
        .unwrap();
    match out.action {
        DispatchAction::Clarify { what_blocked, .. } => {
            assert_eq!(what_blocked, BlockReason::DepthLimit)
        }
        other => panic!("expected Clarify(DepthLimit), got {other:?}"),
    }
}
#[tokio::test]
async fn subagent_passes_through_when_all_mcps_granted() {
    let decision = DispatchDecision {
        action: DispatchAction::DispatchSubagent {
            goal: "review recent decisions".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: vec!["tasks-mcp".into()],
            success_criteria: vec!["a review note exists".into()],
            artifact_target: Some("reviews/".into()),
            model: None,
            correlation_id: "c1".into(),
            delivery: Delivery::Summarize,
            depth: Depth::Normal,
        },
        confidence: 0.85,
        rationale: "open-ended".into(),
    };
    let mock = scripted(&decision);
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let out = dispatcher
        .dispatch(&request(caps("tasks-mcp"), 0))
        .await
        .unwrap();
    assert!(matches!(
        out.action,
        DispatchAction::DispatchSubagent { .. }
    ));
}
#[test]
fn goal_hash_differs_across_distinct_goals() {
    assert_ne!(goal_hash("task A"), goal_hash("different task"));
}
#[tokio::test]
async fn ensure_correlation_mints_an_id_when_empty() {
    let decision = DispatchDecision {
        action: DispatchAction::DispatchSubagent {
            goal: "review recent decisions".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: vec!["tasks-mcp".into()],
            success_criteria: vec!["a review note exists".into()],
            artifact_target: None,
            model: None,
            correlation_id: String::new(),
            delivery: Delivery::Summarize,
            depth: Depth::Normal,
        },
        confidence: 0.85,
        rationale: "open-ended".into(),
    };
    let mock = scripted(&decision);
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let out = dispatcher
        .dispatch(&request(caps("tasks-mcp"), 0))
        .await
        .unwrap();
    match out.action {
        DispatchAction::DispatchSubagent { correlation_id, .. } => {
            assert!(
                !correlation_id.is_empty(),
                "ensure_correlation should have filled an empty correlation_id"
            );
            assert!(
                correlation_id.starts_with("sub:"),
                "correlation_id should start with 'sub:', got {correlation_id}"
            );
        }
        other => panic!("expected DispatchSubagent, got {other:?}"),
    }
}
fn interactive_caps() -> CapabilitySet {
    let mut caps = CapabilitySet::empty();
    caps.grant(Capability::AskHuman);
    caps
}
#[tokio::test]
async fn malformed_output_degrades_to_clarify() {
    let mock = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::text("this is not valid json"),
            CompletionResponse::text("still not valid json"),
        ],
    ));
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let out = dispatcher
        .dispatch(&request(interactive_caps(), 0))
        .await
        .unwrap();
    match out.action {
        DispatchAction::Clarify { what_blocked, .. } => {
            assert_eq!(what_blocked, BlockReason::UnusableOutput)
        }
        other => panic!("expected Clarify, got {other:?}"),
    }
    assert_eq!(out.confidence, 0.0);
}
#[tokio::test]
async fn structured_output_retries_once_before_giving_up() {
    let good = r#"{"action":{"ExecuteDirect":{"seed_calls":[],"relevant_mcps":[]}},
                       "confidence":0.9,"rationale":"ok"}"#;
    let mock = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::text("not valid json"),
            CompletionResponse::text(good),
        ],
    ));
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let out = dispatcher
        .dispatch(&request(interactive_caps(), 0))
        .await
        .unwrap();
    assert!(
        matches!(out.action, DispatchAction::ExecuteDirect { .. }),
        "the retry's good reply should be used, got {:?}",
        out.action
    );
}
#[tokio::test]
async fn an_unclassifiable_goal_for_an_unattended_actor_is_not_a_question() {
    let mock = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::text("garbage"),
            CompletionResponse::text("garbage again"),
        ],
    ));
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let out = dispatcher
        .dispatch(&request(CapabilitySet::empty(), 0)) // no AskHuman — a cron
        .await
        .unwrap();
    match out.action {
        DispatchAction::Clarify {
            what_blocked,
            questions,
        } => {
            assert_eq!(what_blocked, BlockReason::Unattended);
            let all = questions.join("\n");
            // The explanation is appended...
            assert!(all.contains("unattended"), "{all}");
            assert!(all.contains("no one to ask"), "{all}");
            // ...and the original diagnosis survives it. Whoever reads this hours later needs
            // to know *what* could not be routed, not merely that something could not be.
            assert!(all.contains("could not be read as a decision"), "{all}");
        }
        other => panic!("expected the Unattended disposition, got {other:?}"),
    }
}
#[tokio::test]
async fn genuine_provider_failure_propagates() {
    // An empty mock yields MockExhausted (a real provider failure, not malformed output).
    let mock = Arc::new(MockProvider::new("mock"));
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let err = dispatcher
        .dispatch(&request(CapabilitySet::empty(), 0))
        .await
        .unwrap_err();
    assert!(matches!(err, DispatchError::Provider(_)));
}
#[tokio::test]
async fn provider_transport_error_propagates_to_dispatch_error() {
    let mock = Arc::new(MockProvider::new("mock"));
    mock.push_error(ProviderError::Transport("connection refused".into()));
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let err = dispatcher
        .dispatch(&request(CapabilitySet::empty(), 0))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        DispatchError::Provider(ProviderError::Transport(_))
    ));
}
#[tokio::test]
async fn provider_rate_limit_error_propagates_to_dispatch_error() {
    let mock = Arc::new(MockProvider::new("mock"));
    mock.push_error(ProviderError::RateLimited);
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let err = dispatcher
        .dispatch(&request(CapabilitySet::empty(), 0))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        DispatchError::Provider(ProviderError::RateLimited)
    ));
}
#[tokio::test]
async fn high_confidence_guidance_short_circuits_classification() {
    // An empty-script mock would fail (MockExhausted) if classify() were ever called — proves
    // the short-circuit genuinely skips the LLM call, not just that it produces the same
    // answer classification would have.
    let mock = Arc::new(MockProvider::new("mock"));
    let guidance = MockGuidance::with_hits(vec![GuidanceHit {
        content: "Use tasks-mcp for shopping list items".into(),
        tools_used: vec!["tasks-mcp".into()],
        score: 0.95,
    }]);
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4).with_guidance(guidance);

    let out = dispatcher
        .dispatch(&request(caps("tasks-mcp"), 0))
        .await
        .unwrap();
    match out.action {
        DispatchAction::ExecuteDirect {
            seed_calls,
            relevant_mcps,
            ..
        } => {
            assert!(seed_calls.is_empty(), "short-circuit must not invent args");
            assert_eq!(relevant_mcps, vec!["tasks-mcp".to_string()]);
        }
        other => panic!("expected ExecuteDirect, got {other:?}"),
    }
}
#[tokio::test]
async fn low_confidence_guidance_falls_through_to_full_classification() {
    // Guidance below guidance_match_floor must NOT short-circuit — and critically, must not
    // narrow the catalog either: the classifier still sees every MCP, exactly the failure mode
    // the removed verb-keyword advisor had (silently dropping tools it didn't recognize).
    let mock = scripted(&execute_direct("tasks-mcp:add", 0.9));
    let guidance = MockGuidance::with_hits(vec![GuidanceHit {
        content: "maybe use tasks-mcp".into(),
        tools_used: vec!["tasks-mcp".into()],
        score: 0.5, // below the default 0.8 floor
    }]);
    let dispatcher =
        Dispatcher::new(mock.clone(), DispatchTuning::default(), 4).with_guidance(guidance);

    let out = dispatcher
        .dispatch(&request(caps("tasks-mcp"), 0))
        .await
        .unwrap();
    assert!(matches!(out.action, DispatchAction::ExecuteDirect { .. }));
    // The classifier was actually invoked (a real request went to the provider), and the
    // catalog it saw was the full, untouched one from `request()`.
    let sent = mock.last_request().unwrap();
    let user_message = &sent.messages[1].content;
    assert!(
        user_message.contains("tasks-mcp"),
        "catalog must not be narrowed"
    );
}
#[tokio::test]
async fn a_toolless_guidance_hit_never_short_circuits() {
    // A hit with no tools_used can't become relevant_mcps at all — must fall through even at
    // maximal confidence, rather than short-circuiting to an empty-catalog ExecuteDirect.
    let mock = scripted(&execute_direct("tasks-mcp:add", 0.9));
    let guidance = MockGuidance::with_hits(vec![GuidanceHit {
        content: "general advice, no specific tool".into(),
        tools_used: Vec::new(),
        score: 1.0,
    }]);
    let dispatcher =
        Dispatcher::new(mock.clone(), DispatchTuning::default(), 4).with_guidance(guidance);

    let out = dispatcher
        .dispatch(&request(caps("tasks-mcp"), 0))
        .await
        .unwrap();
    assert!(matches!(out.action, DispatchAction::ExecuteDirect { .. }));
    assert!(
        mock.last_request().is_some(),
        "classifier must have been called"
    );
}
#[tokio::test]
async fn score_at_the_guidance_floor_does_short_circuit() {
    let mock = Arc::new(MockProvider::new("mock"));
    let guidance = MockGuidance::with_hits(vec![GuidanceHit {
        content: "maybe use tasks-mcp".into(),
        tools_used: vec!["tasks-mcp".into()],
        score: 0.8, // exactly at the default guidance_match_floor
    }]);
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4).with_guidance(guidance);

    let out = dispatcher
        .dispatch(&request(caps("tasks-mcp"), 0))
        .await
        .unwrap();
    match out.action {
        DispatchAction::ExecuteDirect { relevant_mcps, .. } => {
            assert_eq!(relevant_mcps, vec!["tasks-mcp".to_string()]);
        }
        other => panic!("expected ExecuteDirect, got {other:?}"),
    }
}
#[tokio::test]
async fn prompt_includes_guidance_hits_when_present() {
    let mock = scripted(&execute_direct("tasks-mcp:add", 0.9));
    let guidance = MockGuidance::with_hits(vec![GuidanceHit {
        content: "Use tasks-mcp for shopping list items".into(),
        tools_used: vec!["tasks-mcp".into()],
        score: 0.5, // below 0.8 floor — short-circuit doesn't fire, classify runs with hits
    }]);
    let dispatcher =
        Dispatcher::new(mock.clone(), DispatchTuning::default(), 4).with_guidance(guidance);
    dispatcher
        .dispatch(&request(caps("tasks-mcp"), 0))
        .await
        .unwrap();
    let sent = mock.last_request().unwrap();
    let user_message = &sent.messages[1].content;
    assert!(
        user_message.contains("Relevant past guidance"),
        "prompt must include guidance section when hits are present"
    );
}
#[tokio::test]
async fn prompt_excludes_guidance_hits_when_absent() {
    let mock = scripted(&execute_direct("tasks-mcp:add", 0.9));
    let dispatcher = Dispatcher::new(mock.clone(), DispatchTuning::default(), 4);
    dispatcher
        .dispatch(&request(caps("tasks-mcp"), 0))
        .await
        .unwrap();
    let sent = mock.last_request().unwrap();
    let user_message = &sent.messages[1].content;
    assert!(
        !user_message.contains("Relevant past guidance"),
        "prompt must not include guidance section with no guidance source"
    );
}
#[tokio::test]
async fn guidance_short_circuit_still_passes_through_the_guard_pipeline() {
    // A confident guidance hit naming an MCP that is *in the catalog* but not granted must
    // still CapabilityGap — short-circuit never bypasses guards. (email-mcp is in catalog
    // here so sanitize keeps it; if it were unknown it would be dropped as classifier noise.)
    let mock = Arc::new(MockProvider::new("mock"));
    let guidance = MockGuidance::with_hits(vec![GuidanceHit {
        content: "Use email-mcp for this".into(),
        tools_used: vec!["email-mcp".into()],
        score: 0.99,
    }]);
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4).with_guidance(guidance);

    let mut req = request(caps("tasks-mcp"), 0);
    req.catalog.push(McpDescriptor {
        name: "email-mcp".into(),
        description: "send mail".into(),
        consequence: Consequence::External,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
    });
    let out = dispatcher.dispatch(&req).await.unwrap();
    match out.action {
        DispatchAction::Clarify { what_blocked, .. } => {
            assert_eq!(what_blocked, BlockReason::CapabilityGap)
        }
        other => panic!("expected Clarify(CapabilityGap), got {other:?}"),
    }
}
#[test]
fn sanitize_rewrites_tool_shaped_relevant_mcps_and_drops_bare_unknowns() {
    let catalog = vec![McpDescriptor {
        name: "turbovault".into(),
        description: "vault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
    }];
    let mut d = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: vec![
                ToolCall {
                    tool: "turbovault:list_tasks".into(),
                    args: serde_json::json!({}),
                },
                ToolCall {
                    tool: "list_tasks".into(), // bare — not a catalog MCP
                    args: serde_json::json!({}),
                },
            ],
            relevant_mcps: vec![
                "turbovault:list_tasks".into(),
                "list_tasks".into(),
                "turbovault".into(),
            ],
            delivery: Delivery::Summarize,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    sanitize_decision_mcps(&mut d, &catalog);
    match d.action {
        DispatchAction::ExecuteDirect {
            seed_calls,
            relevant_mcps,
            ..
        } => {
            assert_eq!(relevant_mcps, vec!["turbovault".to_string()]);
            assert_eq!(seed_calls.len(), 1);
            assert_eq!(seed_calls[0].tool, "turbovault:list_tasks");
        }
        other => panic!("expected ExecuteDirect, got {other:?}"),
    }
}
#[test]
fn sanitize_empty_relevant_mcps_means_full_grant_not_capability_gap() {
    // Dogfood 01KX9S39 shape: inventing `list_tasks` as an MCP must not block when
    // turbovault is granted — after drop, empty relevant_mcps is "no narrowing".
    let catalog = vec![McpDescriptor {
        name: "turbovault".into(),
        description: "vault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
    }];
    let mut d = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: vec!["list_tasks".into()],
            delivery: Delivery::Summarize,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    sanitize_decision_mcps(&mut d, &catalog);
    match &d.action {
        DispatchAction::ExecuteDirect { relevant_mcps, .. } => {
            assert!(relevant_mcps.is_empty());
        }
        other => panic!("expected ExecuteDirect, got {other:?}"),
    }
    // Capability guard would not trip: no referenced MCPs.
    let req = DispatchRequest {
        goal: "list tasks".into(),
        catalog: catalog.clone(),
        capabilities: caps("turbovault"),
        reaction_depth: 0,
        zone_write_classes: Vec::new(),
        risk_waivers: RiskWaiverSet::empty(),
    };
    assert!(guards::evaluate(&d, &req, &DispatchTuning::default(), 4).is_none());
}
#[tokio::test]
async fn record_outcome_saves_a_directive_for_execute_direct() {
    let guidance = MockGuidance::with_hits(vec![]);
    let mock = Arc::new(MockProvider::new("mock"));
    let dispatcher =
        Dispatcher::new(mock, DispatchTuning::default(), 4).with_guidance(guidance.clone());

    let decision = execute_direct("tasks-mcp:add", 0.9);
    dispatcher
        .record_outcome("add milk to the list", &decision)
        .await;

    let recorded = guidance.recorded.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].0.contains("add milk to the list"));
}
#[tokio::test]
async fn record_outcome_is_a_noop_for_clarify() {
    let guidance = MockGuidance::with_hits(vec![]);
    let mock = Arc::new(MockProvider::new("mock"));
    let dispatcher =
        Dispatcher::new(mock, DispatchTuning::default(), 4).with_guidance(guidance.clone());

    let decision = DispatchDecision {
        action: DispatchAction::Clarify {
            questions: vec!["which one?".into()],
            what_blocked: BlockReason::Ambiguous,
        },
        confidence: 0.4,
        rationale: "test".into(),
    };
    dispatcher.record_outcome("some goal", &decision).await;

    assert!(guidance.recorded.lock().unwrap().is_empty());
}
#[tokio::test]
async fn record_outcome_dispatch_subagent() {
    let guidance = MockGuidance::with_hits(vec![]);
    let mock = Arc::new(MockProvider::new("mock"));
    let dispatcher =
        Dispatcher::new(mock, DispatchTuning::default(), 4).with_guidance(guidance.clone());

    let decision = DispatchDecision {
        action: DispatchAction::DispatchSubagent {
            goal: "review recent decisions".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: vec!["decisions-mcp".into(), "tasks-mcp".into()],
            success_criteria: vec!["done".into()],
            artifact_target: None,
            model: None,
            correlation_id: "c1".into(),
            delivery: Delivery::Summarize,
            depth: Depth::Normal,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    dispatcher
        .record_outcome("review decisions", &decision)
        .await;

    let recorded = guidance.recorded.lock().unwrap();
    assert_eq!(recorded.len(), 1, "DispatchSubagent should be recorded");
    assert!(
        recorded[0].0.contains("review decisions"),
        "directive should mention the goal"
    );
}
#[tokio::test]
async fn record_outcome_subagent_without_allowed_mcps_is_a_noop() {
    let guidance = MockGuidance::with_hits(vec![]);
    let mock = Arc::new(MockProvider::new("mock"));
    let dispatcher =
        Dispatcher::new(mock, DispatchTuning::default(), 4).with_guidance(guidance.clone());

    let decision = DispatchDecision {
        action: DispatchAction::DispatchSubagent {
            goal: "review recent decisions".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: Vec::new(),
            success_criteria: vec!["done".into()],
            artifact_target: None,
            model: None,
            correlation_id: "c1".into(),
            delivery: Delivery::Summarize,
            depth: Depth::Normal,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    dispatcher
        .record_outcome("review decisions", &decision)
        .await;

    assert!(
        guidance.recorded.lock().unwrap().is_empty(),
        "DispatchSubagent with empty allowed_mcps must not record"
    );
}
#[tokio::test]
async fn record_outcome_empty_execute_direct_is_a_noop() {
    let guidance = MockGuidance::with_hits(vec![]);
    let mock = Arc::new(MockProvider::new("mock"));
    let dispatcher =
        Dispatcher::new(mock, DispatchTuning::default(), 4).with_guidance(guidance.clone());

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    dispatcher.record_outcome("list tasks", &decision).await;

    assert!(
        guidance.recorded.lock().unwrap().is_empty(),
        "empty ExecuteDirect must not record anything"
    );
}
#[tokio::test]
async fn record_outcome_with_relevant_mcps() {
    let guidance = MockGuidance::with_hits(vec![]);
    let mock = Arc::new(MockProvider::new("mock"));
    let dispatcher =
        Dispatcher::new(mock, DispatchTuning::default(), 4).with_guidance(guidance.clone());

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: vec!["tasks-mcp".into()],
            delivery: Delivery::Summarize,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    dispatcher.record_outcome("list tasks", &decision).await;

    let recorded = guidance.recorded.lock().unwrap();
    assert_eq!(recorded.len(), 1, "should record from relevant_mcps");
    assert!(
        recorded[0].0.contains("tasks-mcp"),
        "directive should include the MCP name"
    );
}
#[test]
fn a_reply_shaped_by_the_schema_deserializes() {
    let cases = [
        serde_json::json!({
            "action": {
                "ExecuteDirect": {
                    "relevant_mcps": ["turbovault"],
                }
            },
            "confidence": 0.95,
            "rationale": "single lookup",
        }),
        // Empty collections are the common terse case and must still decode.
        serde_json::json!({
            // Serde still accepts `seed_calls` even though the schema no longer offers it, so a
            // persisted decision from before this change still decodes.
            "action": { "ExecuteDirect": { "seed_calls": [], "relevant_mcps": [] } },
            "confidence": 0.5,
            "rationale": "let the executor decide",
        }),
        serde_json::json!({
            "action": {
                "DispatchSubagent": {
                    "goal": "research X",
                    "allowed_mcps": ["liberado-spider-mcp"],
                    "success_criteria": ["a written summary"],
                }
            },
            "confidence": 0.8,
            "rationale": "multi-step",
        }),
        serde_json::json!({
            "action": {
                "Clarify": { "questions": ["which vault?"], "what_blocked": "ambiguous" }
            },
            "confidence": 0.2,
            "rationale": "ambiguous target",
        }),
    ];

    for case in cases {
        let decoded: Result<DispatchDecision, _> = serde_json::from_value(case.clone());
        assert!(
            decoded.is_ok(),
            "schema-shaped reply must deserialize: {case}\nerror: {:?}",
            decoded.unwrap_err()
        );
    }
}
#[test]
fn every_block_reason_the_schema_offers_is_a_real_one() {
    let schema = decision_schema();
    let offered = schema["properties"]["action"]["anyOf"]
        .as_array()
        .expect("anyOf")
        .iter()
        .find_map(|v| v["properties"]["Clarify"]["properties"]["what_blocked"]["enum"].as_array())
        .expect("Clarify offers what_blocked");

    for value in offered {
        let name = value.as_str().expect("string");
        let decoded: Result<BlockReason, _> = serde_json::from_value(serde_json::json!(name));
        assert!(
            decoded.is_ok(),
            "schema offers unknown BlockReason '{name}'"
        );
    }
}
#[test]
fn the_schema_satisfies_strict_mode_rules() {
    fn check(node: &serde_json::Value, path: &str) {
        if node["type"] == "object" {
            assert_eq!(
                node["additionalProperties"],
                serde_json::json!(false),
                "{path}: strict mode requires additionalProperties:false"
            );
            let props: Vec<&String> = node["properties"]
                .as_object()
                .map(|o| o.keys().collect())
                .unwrap_or_default();
            let required: Vec<&str> = node["required"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            for p in &props {
                assert!(
                    required.contains(&p.as_str()),
                    "{path}: strict mode requires every property in `required`, missing '{p}'"
                );
            }
        }
        match node {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    check(v, &format!("{path}.{k}"));
                }
            }
            serde_json::Value::Array(items) => {
                for (i, v) in items.iter().enumerate() {
                    check(v, &format!("{path}[{i}]"));
                }
            }
            _ => {}
        }
    }
    check(&decision_schema(), "root");
}
