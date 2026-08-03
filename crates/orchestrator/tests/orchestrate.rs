//! Tests that the orchestrator maps each decision to the right execution: tasks built, provenance
//! correlation chosen per action, and Clarify short-circuiting before any execution.

use std::sync::{Arc, Mutex};

use liberado_common::{
    BlockReason, Capability, CapabilitySet, Consequence, Delivery, Depth, DispatchAction,
    DispatchDecision, Outcome, Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall,
    WriteProvenance,
};
use liberado_executor::SUBMIT_REPORT_TOOL;
use liberado_orchestrator::{Disposition, Orchestrator, OrchestratorError, SubDispatch};
use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};
use liberado_test_support::{
    CallRecordingFactory, FailingFactory as ErrorFailingFactory, InvocationRecordingFactory,
    InvocationRecordingRuntime,
};

type Calls = Arc<Mutex<Vec<(Vec<String>, WriteProvenance)>>>;

fn submit_report_response() -> CompletionResponse {
    CompletionResponse::tool_calls(vec![ToolInvocation::new(
        "c",
        SUBMIT_REPORT_TOOL,
        serde_json::json!({ "outcome": "succeeded", "summary": "done" }),
    )])
}

fn orchestrator(
    script: Vec<CompletionResponse>,
    capabilities: CapabilitySet,
) -> (Calls, Orchestrator, CapabilitySet) {
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let factory = CallRecordingFactory::default();
    let calls = factory.calls.clone();
    let orch = Orchestrator::new(
        provider,
        factory,
        capabilities.clone(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );
    (calls, orch, capabilities)
}

#[tokio::test]
async fn execute_direct_scopes_the_runtime_to_the_granted_mcps() {
    // ExecuteDirect scopes to exactly what `capabilities` grants — an empty allow-list would mean
    // "every registered MCP" to the factory, which is the bug this test guards against.
    let capabilities = CapabilitySet::from_iter([Capability::ExecuteMcp("tasks-mcp".into())]);
    let (calls, orch, caps) = orchestrator(vec![submit_report_response()], capabilities);

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
        },
        confidence: 0.9,
        rationale: "simple".into(),
    };

    let disposition = orch
        .run(
            decision,
            "tidy the inbox",
            "vault-change:inbox/x.md:abc123",
            &caps,
        )
        .await
        .expect("run");

    match disposition {
        Disposition::Reported(report) => assert_eq!(report.outcome, Outcome::Succeeded),
        other => panic!("expected Reported, got {other:?}"),
    }

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let (allowed, prov) = &calls[0];
    assert_eq!(allowed, &vec!["tasks-mcp".to_string()]);
    assert_eq!(prov.source, "liberado-executor");
    // ExecuteDirect acts in the reaction's name → adopts the triggering correlation.
    assert_eq!(
        prov.correlation_id.as_deref(),
        Some("vault-change:inbox/x.md:abc123")
    );
}

#[tokio::test]
async fn execute_direct_relevant_mcps_narrows_within_the_granted_ceiling() {
    // Granted two MCPs, but the decision names only one as relevant — the runtime should be
    // scoped to that one, not the full granted set (the token-efficiency narrowing).
    let capabilities = CapabilitySet::from_iter([
        Capability::ExecuteMcp("tasks-mcp".into()),
        Capability::ExecuteMcp("email-mcp".into()),
    ]);
    let (calls, orch, caps) = orchestrator(vec![submit_report_response()], capabilities);

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: vec!["tasks-mcp".into()],
        },
        confidence: 0.9,
        rationale: "simple".into(),
    };

    orch.run(decision, "add milk to my list", "trigger-1", &caps)
        .await
        .expect("run");

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let (allowed, _) = &calls[0];
    assert_eq!(
        allowed,
        &vec!["tasks-mcp".to_string()],
        "relevant_mcps must narrow within the granted ceiling, not replace it"
    );
}

#[tokio::test]
async fn execute_direct_with_zero_grants_never_calls_the_factory() {
    // No ExecuteMcp grants at all: the factory must not be asked for "every registered MCP" (what
    // an empty allow-list would otherwise mean) — it must not be called at all.
    let (calls, orch, caps) = orchestrator(vec![submit_report_response()], CapabilitySet::empty());

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
        },
        confidence: 0.9,
        rationale: "simple".into(),
    };
    let disposition = orch
        .run(decision, "tidy the inbox", "trigger-1", &caps)
        .await
        .expect("run");
    assert!(matches!(disposition, Disposition::Reported(_)));
    assert!(
        calls.lock().unwrap().is_empty(),
        "zero grants must not reach the factory (would mean 'every registered MCP')"
    );
}

#[tokio::test]
async fn dispatch_subagent_uses_its_own_correlation_and_allowed_mcps() {
    let (calls, orch, caps) = orchestrator(vec![submit_report_response()], CapabilitySet::empty());

    let decision = DispatchDecision {
        action: DispatchAction::DispatchSubagent {
            goal: "summarize recent decisions".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: vec!["tasks-mcp".into(), "memory-mcp".into()],
            success_criteria: vec!["a summary note exists".into()],
            artifact_target: None,
            model: None,
            correlation_id: "subagent-42".into(),
            delivery: Delivery::Summarize,
            depth: Depth::Normal,
        },
        confidence: 0.8,
        rationale: "multi-step".into(),
    };

    let disposition = orch
        .run(
            decision,
            "outer goal is ignored for subagents",
            "trigger-xyz",
            &caps,
        )
        .await
        .expect("run");

    assert!(matches!(disposition, Disposition::Reported(_)));

    let calls = calls.lock().unwrap();
    let (allowed, prov) = &calls[0];
    assert_eq!(
        allowed,
        &vec!["tasks-mcp".to_string(), "memory-mcp".to_string()]
    );
    // The subagent uses the classifier-minted correlation, NOT the trigger.
    assert_eq!(prov.correlation_id.as_deref(), Some("subagent-42"));
}

#[tokio::test]
async fn propose_builds_a_pending_proposal_without_executing() {
    // No scripted responses + no runtime: proves nothing ran (the orchestrator only builds the
    // artifact; the daemon writes it).
    let (calls, orch, caps) = orchestrator(vec![], CapabilitySet::empty());

    let action = ProposedAction::ToolCalls(vec![ToolCall {
        tool: "email:send".into(),
        args: serde_json::json!({ "to": "boss@example.com" }),
    }]);
    let decision = DispatchDecision {
        action: DispatchAction::Propose {
            proposed_action: action.clone(),
            rationale: "the note asks to email the boss".into(),
        },
        confidence: 0.95,
        rationale: "guard downgrade".into(),
    };

    let disposition = orch
        .run(
            decision,
            "email the boss",
            "vault-change:inbox/x.md:abc",
            &caps,
        )
        .await
        .expect("run");

    match disposition {
        Disposition::Propose(p) => {
            assert_eq!(p.proposed_action, action);
            assert_eq!(p.correlation_id, "vault-change:inbox/x.md:abc");
            assert_eq!(p.id, "vault-change:inbox/x.md:abc");
            assert_eq!(p.status, ProposalStatus::Pending);
        }
        other => panic!("expected Propose, got {other:?}"),
    }
    assert!(
        calls.lock().unwrap().is_empty(),
        "Propose must not build a runtime or execute"
    );
}

#[tokio::test]
async fn clarify_short_circuits_without_executing() {
    // No scripted responses + no runtime: proves nothing ran.
    let (calls, orch, caps) = orchestrator(vec![], CapabilitySet::empty());

    let decision = DispatchDecision {
        action: DispatchAction::Clarify {
            questions: vec!["which project?".into()],
            what_blocked: BlockReason::Ambiguous,
        },
        confidence: 0.3,
        rationale: "ambiguous".into(),
    };

    let disposition = orch
        .run(decision, "ambiguous goal", "trigger-1", &caps)
        .await
        .expect("run");

    match disposition {
        Disposition::Clarify {
            questions,
            what_blocked,
        } => {
            assert_eq!(questions, vec!["which project?".to_string()]);
            assert_eq!(what_blocked, BlockReason::Ambiguous);
        }
        other => panic!("expected Clarify, got {other:?}"),
    }
    assert!(
        calls.lock().unwrap().is_empty(),
        "Clarify must not build a runtime or execute"
    );
}

#[tokio::test]
async fn execute_approved_runs_the_exact_calls_without_a_classifier_or_guard() {
    // No scripted provider responses: execute_approved must NOT go through any classifier/guard — it
    // runs the approved calls straight against the runtime. A scripted call would panic if consumed.
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        Vec::<CompletionResponse>::new(),
    ));
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        provider,
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let call = ToolCall {
        tool: "email:send".into(),
        args: serde_json::json!({ "to": "boss@example.com", "body": "hi" }),
    };
    let proposal = Proposal::pending(
        "vault-change:inbox/x.md:abc",
        "vault-change:inbox/x.md:abc",
        "liberado",
        ProposedAction::ToolCalls(vec![call.clone()]),
        "the note asks to email the boss",
    );
    let mut proposal = signer.sign(proposal).into_proposal();
    proposal.status = ProposalStatus::Approved;

    let report = orch.execute_approved(&proposal).await.expect("execute");
    assert_eq!(report.outcome, Outcome::Succeeded);

    // The runtime saw exactly the approved call, with the approved args.
    let invoked = invoked.lock().unwrap();
    assert_eq!(invoked.len(), 1);
    assert_eq!(invoked[0].name, "email:send");
    assert_eq!(invoked[0].arguments, call.args);
}

#[tokio::test]
async fn execute_approved_rejects_a_proposal_with_no_valid_signature() {
    // A proposal that was never signed (or was tampered with after signing) must not execute, even
    // though it's status: approved — the integrity check runs before anything else in
    // execute_approved.
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        Vec::<CompletionResponse>::new(),
    ));
    let orch = Orchestrator::new(
        provider,
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let mut proposal = Proposal::pending(
        "forged-1",
        "forged-1",
        "liberado",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "email:send".into(),
            args: serde_json::json!({ "to": "boss@example.com" }),
        }]),
        "never signed",
    );
    proposal.status = ProposalStatus::Approved;
    // proposal.integrity is left empty — never signed by this orchestrator's signer.

    let report = orch
        .execute_approved(&proposal)
        .await
        .expect("execute_approved");
    assert_eq!(
        report.outcome,
        Outcome::Failed,
        "an unsigned/forged proposal must not report success"
    );
    assert!(
        invoked.lock().unwrap().is_empty(),
        "the real tool must never be invoked for a proposal that fails integrity verification"
    );
}

// ------------------------------------------------------------------
// Runtime-level gating: the executor's *adaptive* (non-seed) tool calls must get the same
// capability/consequence checking the dispatcher's pre-flight guard only ever applied to the
// decision's seed call.
// ------------------------------------------------------------------

#[tokio::test]
async fn execute_direct_downgrades_a_high_consequence_adaptive_call() {
    // No seed_calls: the model's first real turn calling this tool *is* the adaptive call — the
    // same code path a genuine mid-loop call takes, never pre-vetted by the dispatcher's guard.
    let script = vec![
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c1",
            "dangerous-mcp:delete_everything",
            serde_json::json!({"path": "important.md"}),
        )]),
        submit_report_response(),
    ];
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let proposals_dir = tempfile::TempDir::new().unwrap();
    let capabilities = CapabilitySet::from_iter([Capability::ExecuteMcp("dangerous-mcp".into())]);
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        provider,
        InvocationRecordingFactory { runtime },
        capabilities.clone(),
        vec![("dangerous-mcp".into(), Consequence::External)],
        Vec::new(),
        Vec::new(),
        proposals_dir.path().to_path_buf(),
        signer.clone(),
        "default",
    );

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
        },
        confidence: 0.9,
        rationale: "looked simple".into(),
    };

    let disposition = orch
        .run(decision, "clean up the vault", "trigger-1", &capabilities)
        .await
        .expect("run");

    // The downgrade is a tool *result*, not an abort — the run still completes normally.
    assert!(matches!(disposition, Disposition::Reported(_)));

    // The real tool never ran.
    assert!(
        invoked.lock().unwrap().is_empty(),
        "a high-consequence adaptive call must not reach the real tool"
    );

    // A proposal file was written instead, and it's signed with the orchestrator's own signer.
    let mut entries = std::fs::read_dir(proposals_dir.path().join("proposals"))
        .expect("proposals dir should exist");
    let entry = entries
        .next()
        .expect("exactly one proposal file should be written")
        .unwrap();
    assert!(
        entries.next().is_none(),
        "exactly one proposal file should be written"
    );
    let content = std::fs::read_to_string(entry.path()).unwrap();
    let written_proposal = Proposal::from_note(&content).unwrap();
    assert!(
        signer.verify(&written_proposal),
        "the written proposal must verify against the orchestrator's own signer"
    );
}

#[tokio::test]
async fn execute_direct_rejects_an_out_of_capability_adaptive_call() {
    // Granted only "safe-mcp" — so `allowed_mcps` derivation lets *some* execution proceed — but the
    // model's first real turn (adaptive, not seeded) calls an entirely different, ungranted MCP.
    let script = vec![
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c1",
            "other-mcp:do_something",
            serde_json::json!({}),
        )]),
        submit_report_response(),
    ];
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let capabilities = CapabilitySet::from_iter([Capability::ExecuteMcp("safe-mcp".into())]);
    let orch = Orchestrator::new(
        provider,
        InvocationRecordingFactory { runtime },
        capabilities.clone(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
        },
        confidence: 0.9,
        rationale: "looked simple".into(),
    };

    let disposition = orch
        .run(decision, "do a safe thing", "trigger-1", &capabilities)
        .await
        .expect("run");

    assert!(matches!(disposition, Disposition::Reported(_)));
    assert!(
        invoked.lock().unwrap().is_empty(),
        "an ungranted adaptive call must not reach the real tool"
    );
}

#[tokio::test]
async fn dispatch_subagent_gates_with_the_narrowed_capability_set() {
    // The orchestrator's own ceiling grants both MCPs; the decision's own (narrower) capabilities
    // only grant "tasks-mcp". The gate must use the narrowed intersection, not the raw ceiling.
    let script = vec![
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c1",
            "other-mcp:do_something",
            serde_json::json!({}),
        )]),
        submit_report_response(),
    ];
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let ceiling = CapabilitySet::from_iter([
        Capability::ExecuteMcp("tasks-mcp".into()),
        Capability::ExecuteMcp("other-mcp".into()),
    ]);
    let orch = Orchestrator::new(
        provider,
        InvocationRecordingFactory { runtime },
        ceiling.clone(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let decision = DispatchDecision {
        action: DispatchAction::DispatchSubagent {
            goal: "narrow task".into(),
            capabilities: CapabilitySet::from_iter([Capability::ExecuteMcp("tasks-mcp".into())]),
            allowed_mcps: vec!["other-mcp".into()],
            success_criteria: vec![],
            artifact_target: None,
            model: None,
            correlation_id: "sub-1".into(),
            delivery: Delivery::Summarize,
            depth: Depth::Normal,
        },
        confidence: 0.8,
        rationale: "multi-step".into(),
    };

    let disposition = orch
        .run(decision, "outer goal ignored", "trigger-1", &ceiling)
        .await
        .expect("run");

    assert!(matches!(disposition, Disposition::Reported(_)));
    assert!(
        invoked.lock().unwrap().is_empty(),
        "the subagent's own (narrower) capabilities must gate it, not the orchestrator's raw ceiling"
    );
}

#[tokio::test]
async fn dispatch_subagent_empty_capabilities_derives_gate_from_allowed_mcps() {
    // Classifier default: capabilities empty, allowed_mcps names the scoped MCP. The risk gate
    // must allow that MCP (ceiling ∩ allowed_mcps), not block everything with an empty set.
    let script = vec![
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c1",
            "turbovault:list_tasks",
            serde_json::json!({}),
        )]),
        submit_report_response(),
    ];
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let ceiling = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::ExecuteMcp("other-mcp".into()),
    ]);
    let orch = Orchestrator::new(
        provider,
        InvocationRecordingFactory { runtime },
        ceiling.clone(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let decision = DispatchDecision {
        action: DispatchAction::DispatchSubagent {
            goal: "list vault tasks".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: vec!["turbovault".into()],
            success_criteria: vec![],
            artifact_target: None,
            model: None,
            correlation_id: "sub-vault-1".into(),
            delivery: Delivery::Summarize,
            depth: Depth::Normal,
        },
        confidence: 0.85,
        rationale: "vault work".into(),
    };

    let disposition = orch
        .run(decision, "outer goal ignored", "trigger-1", &ceiling)
        .await
        .expect("run");

    assert!(matches!(disposition, Disposition::Reported(_)));
    let names: Vec<String> = invoked
        .lock()
        .unwrap()
        .iter()
        .map(|c| c.name.clone())
        .collect();
    assert_eq!(
        names,
        vec!["turbovault:list_tasks".to_string()],
        "empty decision.capabilities must still permit allowed_mcps that the ceiling grants"
    );
}

#[tokio::test]
async fn dispatch_subagent_empty_capabilities_still_cannot_widen_past_ceiling() {
    // allowed_mcps names an MCP the ceiling does not grant — derivation must not invent authority.
    let script = vec![
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c1",
            "ungranted-mcp:secret",
            serde_json::json!({}),
        )]),
        submit_report_response(),
    ];
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let ceiling = CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]);
    let orch = Orchestrator::new(
        provider,
        InvocationRecordingFactory { runtime },
        ceiling.clone(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let decision = DispatchDecision {
        action: DispatchAction::DispatchSubagent {
            goal: "should not run ungranted tools".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: vec!["ungranted-mcp".into()],
            success_criteria: vec![],
            artifact_target: None,
            model: None,
            correlation_id: "sub-no-widen".into(),
            delivery: Delivery::Summarize,
            depth: Depth::Normal,
        },
        confidence: 0.8,
        rationale: "test".into(),
    };

    let disposition = orch
        .run(decision, "outer goal ignored", "trigger-1", &ceiling)
        .await
        .expect("run");

    assert!(matches!(disposition, Disposition::Reported(_)));
    assert!(
        invoked.lock().unwrap().is_empty(),
        "allowed_mcps outside the ceiling must not pass the risk gate"
    );
}

#[tokio::test]
async fn dispatch_parallel_gates_each_sub_dispatch() {
    let script = vec![
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c1",
            "dangerous-mcp:wipe",
            serde_json::json!({}),
        )]),
        submit_report_response(),
    ];
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let capabilities = CapabilitySet::from_iter([Capability::ExecuteMcp("dangerous-mcp".into())]);
    let orch = Orchestrator::new(
        provider,
        InvocationRecordingFactory { runtime },
        capabilities.clone(),
        vec![("dangerous-mcp".into(), Consequence::External)],
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let sub_dispatches = vec![SubDispatch {
        goal: "do the dangerous thing".into(),
        allowed_mcps: vec!["dangerous-mcp".into()],
        success_criteria: vec![],
        correlation_id: "sub-a".into(),
        label: "A".into(),
    }];

    let report = orch
        .dispatch_parallel(sub_dispatches, 1)
        .await
        .expect("dispatch_parallel");

    assert_eq!(report.outcome, Outcome::Succeeded);
    assert!(
        invoked.lock().unwrap().is_empty(),
        "a high-consequence sub-dispatch call must not reach the real tool"
    );
}

#[tokio::test]
async fn execute_approved_bypasses_gating_by_design() {
    // Even with a consequence catalog that WOULD downgrade this call through the gate, an approved
    // proposal must execute directly — approval is already the authorization (see
    // `execute_approved`'s doc comment). Re-gating it would create an approve -> re-propose loop.
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        Vec::<CompletionResponse>::new(),
    ));
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        provider,
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        vec![("email-mcp".into(), Consequence::External)],
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let call = ToolCall {
        tool: "email-mcp:send".into(),
        args: serde_json::json!({ "to": "boss@example.com" }),
    };
    let proposal = Proposal::pending(
        "id-1",
        "id-1",
        "liberado",
        ProposedAction::ToolCalls(vec![call.clone()]),
        "approved email",
    );
    let mut proposal = signer.sign(proposal).into_proposal();
    proposal.status = ProposalStatus::Approved;

    let report = orch.execute_approved(&proposal).await.expect("execute");
    assert_eq!(report.outcome, Outcome::Succeeded);

    let invoked = invoked.lock().unwrap();
    assert_eq!(
        invoked.len(),
        1,
        "an approved high-consequence call must actually execute, not be re-gated"
    );
    assert_eq!(invoked[0].name, "email-mcp:send");
}

#[tokio::test]
async fn execute_approved_subagent_dispatches_the_approved_goal() {
    // What was approved is the goal + MCP scope (Decision 11's review surface for a Subagent
    // proposal), not fixed calls — the approved run still drives a real adaptive tool loop, ending
    // in submit_report like any other subagent dispatch.
    let script = vec![
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c1",
            "decisions-mcp:list_recent",
            serde_json::json!({}),
        )]),
        submit_report_response(),
    ];
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        provider,
        InvocationRecordingFactory { runtime },
        CapabilitySet::from_iter([Capability::ExecuteMcp("decisions-mcp".into())]),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let proposal = Proposal::pending(
        "review-2026-07-02",
        "review-2026-07-02",
        "liberado",
        ProposedAction::Subagent {
            goal: "review recent decisions".into(),
            capabilities: CapabilitySet::from_iter([Capability::ExecuteMcp(
                "decisions-mcp".into(),
            )]),
            allowed_mcps: vec!["decisions-mcp".into()],
            success_criteria: vec!["a review note exists".into()],
        },
        "open-ended, touches an external-consequence MCP",
    );
    let mut proposal = signer.sign(proposal).into_proposal();
    proposal.status = ProposalStatus::Approved;

    let report = orch.execute_approved(&proposal).await.expect("execute");
    assert_eq!(report.outcome, Outcome::Succeeded);

    let invoked = invoked.lock().unwrap();
    assert_eq!(invoked.len(), 1);
    assert_eq!(invoked[0].name, "decisions-mcp:list_recent");
}

#[tokio::test]
async fn execute_approved_subagent_still_gates_adaptive_calls_outside_its_capabilities() {
    // Unlike ToolCalls (specific calls were the thing reviewed), a Subagent proposal only approved
    // a goal + scope — the subagent's own adaptive calls during execution must still be gated, the
    // same as a live (never-proposed) DispatchSubagent would be.
    let script = vec![
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c1",
            "other-mcp:do_something",
            serde_json::json!({}),
        )]),
        submit_report_response(),
    ];
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let signer = ProposalSigner::random();
    // The orchestrator's own ceiling grants both MCPs; the proposal's own (narrower) capabilities
    // only grant "tasks-mcp" — the gate must use the narrowed intersection.
    let ceiling = CapabilitySet::from_iter([
        Capability::ExecuteMcp("tasks-mcp".into()),
        Capability::ExecuteMcp("other-mcp".into()),
    ]);
    let orch = Orchestrator::new(
        provider,
        InvocationRecordingFactory { runtime },
        ceiling.clone(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let proposal = Proposal::pending(
        "c1",
        "c1",
        "liberado",
        ProposedAction::Subagent {
            goal: "narrow task".into(),
            capabilities: CapabilitySet::from_iter([Capability::ExecuteMcp("tasks-mcp".into())]),
            allowed_mcps: vec!["other-mcp".into()],
            success_criteria: vec![],
        },
        "narrowly scoped",
    );
    let mut proposal = signer.sign(proposal).into_proposal();
    proposal.status = ProposalStatus::Approved;

    orch.execute_approved(&proposal).await.expect("execute");
    assert!(
        invoked.lock().unwrap().is_empty(),
        "the proposal's own (narrower) capabilities must gate it, not the orchestrator's raw ceiling"
    );
}

#[tokio::test]
async fn per_run_capabilities_narrow_the_pool_ceiling_and_never_widen() {
    // E1: a session grant narrower than the pool is genuinely enforced. The pool grants two MCPs;
    // the per-run grant only grants one. The runtime must be scoped to that one — and a per-run
    // grant that *adds* an MCP the pool lacks must not invent authority.
    let pool = CapabilitySet::from_iter([
        Capability::ExecuteMcp("tasks-mcp".into()),
        Capability::ExecuteMcp("email-mcp".into()),
    ]);
    let (calls, orch, _pool_caps) = orchestrator(
        vec![submit_report_response(), submit_report_response()],
        pool,
    );

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
        },
        confidence: 0.9,
        rationale: "routine".into(),
    };

    // Narrower per-run grant: only tasks-mcp.
    let session_grant = CapabilitySet::from_iter([Capability::ExecuteMcp("tasks-mcp".into())]);
    orch.run(decision.clone(), "list tasks", "sess-1", &session_grant)
        .await
        .expect("run");
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].0,
            vec!["tasks-mcp".to_string()],
            "session grant must narrow the pool ceiling"
        );
    }

    // Wider-looking per-run grant: claims vault-mcp the pool never held — must not appear.
    let spoofed = CapabilitySet::from_iter([
        Capability::ExecuteMcp("tasks-mcp".into()),
        Capability::ExecuteMcp("email-mcp".into()),
        Capability::ExecuteMcp("vault-mcp".into()),
    ]);
    orch.run(decision, "list tasks", "sess-2", &spoofed)
        .await
        .expect("run");
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        let mut allowed = calls[1].0.clone();
        allowed.sort();
        assert_eq!(
            allowed,
            vec!["email-mcp".to_string(), "tasks-mcp".to_string()],
            "per-run capabilities cannot widen past the pool ceiling"
        );
    }
}

use async_trait::async_trait;
use liberado_executor::{RuntimeFactory, RuntimeSetupError, ToolRuntime};
use liberado_provider::ToolDef;

struct FailingRuntime;

#[async_trait]
impl ToolRuntime for FailingRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Err("simulated failure".into())
    }
}

struct FailingFactory;

#[async_trait]
impl RuntimeFactory for FailingFactory {
    async fn runtime_for(
        &self,
        _allowed_mcps: &[String],
        _provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        Ok(Box::new(FailingRuntime))
    }
}

#[tokio::test]
async fn execute_approved_tool_calls_dedup_mcp_names() {
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        Vec::<CompletionResponse>::new(),
    ));
    let factory = CallRecordingFactory::default();
    let calls = factory.calls.clone();
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        provider,
        factory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let call = ToolCall {
        tool: "email:send".into(),
        args: serde_json::json!({}),
    };
    let proposal = Proposal::pending(
        "hash1",
        "hash1",
        "liberado",
        ProposedAction::ToolCalls(vec![call.clone(), call.clone()]),
        "duplicate calls",
    );
    let mut proposal = signer.sign(proposal).into_proposal();
    proposal.status = ProposalStatus::Approved;

    let report = orch.execute_approved(&proposal).await.expect("execute");
    assert_eq!(report.outcome, Outcome::Succeeded);

    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, vec!["email".to_string()]);
}

#[tokio::test]
async fn execute_approved_tool_calls_all_failed_is_failed_outcome() {
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        Vec::<CompletionResponse>::new(),
    ));
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        provider,
        FailingFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let call = ToolCall {
        tool: "email:send".into(),
        args: serde_json::json!({}),
    };
    let proposal = Proposal::pending(
        "hash1",
        "hash1",
        "liberado",
        ProposedAction::ToolCalls(vec![call]),
        "failing call",
    );
    let mut proposal = signer.sign(proposal).into_proposal();
    proposal.status = ProposalStatus::Approved;

    let report = orch.execute_approved(&proposal).await.expect("execute");
    assert_eq!(report.outcome, Outcome::Failed);
}

#[tokio::test]
async fn execute_approved_refuses_past_deadline_without_invoking_tools() {
    use chrono::{Duration as ChronoDuration, Utc};

    let provider = Arc::new(MockProvider::with_script(
        "mock",
        Vec::<CompletionResponse>::new(),
    ));
    let factory = CallRecordingFactory::default();
    let calls = factory.calls.clone();
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        provider,
        factory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let call = ToolCall {
        tool: "email:send".into(),
        args: serde_json::json!({}),
    };
    let mut proposal = Proposal::pending(
        "hash-expired",
        "hash-expired",
        "liberado",
        ProposedAction::ToolCalls(vec![call]),
        "too late",
    );
    proposal.expires = Some(Utc::now() - ChronoDuration::hours(1));
    let mut proposal = signer.sign(proposal).into_proposal();
    proposal.status = ProposalStatus::Approved;

    let report = orch.execute_approved(&proposal).await.expect("execute");
    assert_eq!(report.outcome, Outcome::Failed);
    assert_eq!(
        report.summary,
        liberado_orchestrator::EXPIRED_PROPOSAL_REFUSAL_SUMMARY,
        "must use the exact refusal summary for daemon matching"
    );
    assert!(
        calls.lock().unwrap().is_empty(),
        "expired proposal must not open a tool runtime"
    );
}

#[tokio::test]
async fn execute_approved_tool_calls_partial_failure_is_partial_outcome() {
    use liberado_provider::ToolInvocation; // shadow to avoid conflict with FailingRuntime

    struct MixedFactory {
        fail_on: String,
    }

    #[async_trait]
    impl RuntimeFactory for MixedFactory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            Ok(Box::new(MixedRuntime {
                fail_on: self.fail_on.clone(),
            }))
        }
    }

    struct MixedRuntime {
        fail_on: String,
    }

    #[async_trait]
    impl ToolRuntime for MixedRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
            if call.name == self.fail_on {
                Err("intentional failure".into())
            } else {
                Ok("ok".into())
            }
        }
    }

    let provider = Arc::new(MockProvider::with_script(
        "mock",
        Vec::<CompletionResponse>::new(),
    ));
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        provider,
        MixedFactory {
            fail_on: "failing-tool".into(),
        },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let calls = vec![
        ToolCall {
            tool: "ok-tool".into(),
            args: serde_json::json!({}),
        },
        ToolCall {
            tool: "failing-tool".into(),
            args: serde_json::json!({}),
        },
    ];
    let proposal = Proposal::pending(
        "hash1",
        "hash1",
        "liberado",
        ProposedAction::ToolCalls(calls),
        "mixed results",
    );
    let mut proposal = signer.sign(proposal).into_proposal();
    proposal.status = ProposalStatus::Approved;

    let report = orch.execute_approved(&proposal).await.expect("execute");
    assert_eq!(report.outcome, Outcome::PartiallySucceeded);
}

// --- Report delivery ----------------------------------------------------------------------------
//
// The motivating failure: a deep-research subagent produced a long write-up, the face agent
// ingested it and paraphrased it back out, and the human's actual report was never written down.
// `Delivery::Vault` makes the orchestrator file it directly — one tool call, no model reads the
// body on the way there — and hand the face agent a receipt it has nothing to restate from.

/// The full path, end to end: a read-only subagent finishes, and the orchestrator itself performs
/// the write. This is the test that would have caught the original bug.
#[tokio::test]
async fn a_read_only_subagent_report_is_written_to_the_vault_by_the_orchestrator() {
    let script = vec![CompletionResponse::tool_calls(vec![ToolInvocation::new(
        "c",
        SUBMIT_REPORT_TOOL,
        serde_json::json!({
            "outcome": "succeeded",
            // A realistic write-up, not a toy string: delivery now verifies the report actually
            // looks like the document it is about to be filed as, so a two-line fixture would be
            // (correctly) refused and this test would assert nothing about the happy path.
            "summary": "# Graph Engineering — Survey\n\n\
                        ## Findings\n\n\
                        The graph engineering literature splits three ways.\n\n\
                        - Storage engines optimise for traversal locality.\n\
                        - Query planners diverge on whether to materialise intermediate paths.\n\
                        - Streaming systems treat the graph as a changelog rather than a snapshot.\n\n\
                        ## Detail\n\n\
                        Each camp reports incomparable benchmarks, which is the main obstacle to \
                        drawing conclusions across them. The storage-first work measures cold \
                        traversal; the planner work measures warm repeated queries.\n\n\
                        ## Open questions\n\n\
                        Whether the streaming formulation subsumes the other two remains unsettled.",
        }),
    )])];
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let ceiling = CapabilitySet::from_iter([
        Capability::ExecuteMcp("search".into()),
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Write(liberado_common::Zone::vault("Learning")),
    ]);
    let orch = Orchestrator::new(
        provider,
        InvocationRecordingFactory { runtime },
        ceiling.clone(),
        vec![
            ("search".into(), Consequence::ReadOnly),
            ("turbovault".into(), Consequence::Reversible),
        ],
        Vec::new(),
        vec![(
            "Learning".into(),
            liberado_common::WriteClass::AgentWritable,
        )],
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    )
    .with_report_sink(liberado_orchestrator::ReportSink::new(
        "turbovault",
        "write_note",
        "path",
        "content",
    ));

    let decision = DispatchDecision {
        action: DispatchAction::DispatchSubagent {
            goal: "research graph engineering".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: vec!["search".into()],
            success_criteria: vec![],
            artifact_target: None,
            model: None,
            correlation_id: "sub-research-1".into(),
            delivery: Delivery::Vault {
                path: "Learning/graph-engineering.md".into(),
            },
            depth: Depth::Deep,
        },
        confidence: 0.9,
        rationale: "read-only research".into(),
    };

    let disposition = orch
        .run(decision, "outer goal ignored", "trigger-1", &ceiling)
        .await
        .expect("run");

    let calls = invoked.lock().unwrap().clone();
    let write = calls
        .iter()
        .find(|c| c.name == "turbovault:write_note")
        .expect("the orchestrator must have written the report itself");
    assert_eq!(
        write.arguments["path"], "Learning/graph-engineering.md",
        "the declared `path_arg` carries the destination"
    );
    let content = write.arguments["content"].as_str().unwrap();
    assert!(
        content.contains("The graph engineering literature splits three ways."),
        "the report body is filed verbatim, not paraphrased"
    );
    assert!(
        content.contains("liberado_correlation: sub-research-1"),
        "the note must join back to the dispatch that produced it"
    );

    let Disposition::Reported(report) = disposition else {
        panic!("expected a Reported disposition");
    };
    // The saving is that the face agent has nothing to restate.
    assert!(
        !report.summary.contains("splits three ways"),
        "the receipt must not carry the body — that is the entire point"
    );
    assert!(report.summary.contains("Learning/graph-engineering.md"));
    assert_eq!(report.artifacts, vec!["Learning/graph-engineering.md"]);
}

/// A subagent that could have *acted* never bypasses the face agent, however the dispatcher routed
/// it. Guard #1 is a property of the MCP grant, checked after the model, not a promise from it.
#[tokio::test]
async fn delivery_is_refused_when_the_subagent_could_have_acted() {
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        vec![submit_report_response()],
    ));
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let ceiling = CapabilitySet::from_iter([
        Capability::ExecuteMcp("email".into()),
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Write(liberado_common::Zone::vault("Learning")),
    ]);
    let orch = Orchestrator::new(
        provider,
        InvocationRecordingFactory { runtime },
        ceiling.clone(),
        vec![
            ("email".into(), Consequence::External),
            ("turbovault".into(), Consequence::Reversible),
        ],
        Vec::new(),
        vec![(
            "Learning".into(),
            liberado_common::WriteClass::AgentWritable,
        )],
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    )
    .with_report_sink(liberado_orchestrator::ReportSink::new(
        "turbovault",
        "write_note",
        "path",
        "content",
    ));

    let decision = DispatchDecision {
        action: DispatchAction::DispatchSubagent {
            goal: "email the summary".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: vec!["email".into()],
            success_criteria: vec![],
            artifact_target: None,
            model: None,
            correlation_id: "sub-acts-1".into(),
            delivery: Delivery::Vault {
                path: "Learning/x.md".into(),
            },
            depth: Depth::Deep,
        },
        confidence: 0.9,
        rationale: "acts on the world".into(),
    };

    let disposition = orch
        .run(decision, "outer goal", "trigger-1", &ceiling)
        .await
        .expect("run");

    assert!(
        !invoked
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.name == "turbovault:write_note"),
        "a dispatch that can act must not file its own report"
    );
    let Disposition::Reported(report) = disposition else {
        panic!("expected a Reported disposition");
    };
    assert_eq!(report.summary, "done", "the body goes to the face agent");
    assert!(report.artifacts.is_empty());
}

#[tokio::test]
async fn factory_setup_error_is_surfaced_by_orchestrator() {
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [submit_report_response()],
    ));
    let capabilities = CapabilitySet::from_iter([Capability::ExecuteMcp("tasks-mcp".into())]);

    let orch = Orchestrator::new(
        provider,
        ErrorFailingFactory::new("MCP launch failed"),
        capabilities.clone(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: vec!["tasks-mcp".into()],
        },
        confidence: 0.9,
        rationale: "should fail".into(),
    };

    let err = orch
        .run(decision, "test goal", "trigger-1", &capabilities)
        .await
        .unwrap_err();
    assert!(
        matches!(err, OrchestratorError::Runtime(_)),
        "expected Runtime error, got {err:?}"
    );
}

// ------------------------------------------------------------------
// Round 3 §1 — delegated findings reach the face (relay contract on Summarize)
// ------------------------------------------------------------------

/// Long findings a research subagent should put in `summary` when it has the relay contract.
const RESEARCH_FINDINGS: &str = "\
## Belt vs chain: findings

- Carbon belts need an eccentric BB or similar tensioning (CyclingAbout, 135000 km testing).
- Chain drives remain lighter and cheaper for most riders (BikeRadar comparison 2024).
- Gates Carbon Drive is the dominant belt system; tension windows are tighter than chains.

Sources: CyclingAbout long-term test, BikeRadar belt-drive review, Hackaday tensioning notes.";

/// Status-line shape the model produces under the default "short" schema alone — what the face
/// used to receive, and what invented 7872 chars of specifics from priors.
const STATUS_LINE: &str = "Comprehensive comparison of belt vs chain drive bicycles, \
synthesized from authoritative sources including CyclingAbout and BikeRadar.";

fn research_ceiling() -> CapabilitySet {
    CapabilitySet::from_iter([
        Capability::ExecuteMcp("search".into()),
        Capability::ExecuteMcp("spider".into()),
    ])
}

fn research_catalog() -> Vec<(String, Consequence)> {
    vec![
        ("search".into(), Consequence::ReadOnly),
        ("spider".into(), Consequence::ReadOnly),
        ("email".into(), Consequence::External),
    ]
}

fn long_findings_report() -> CompletionResponse {
    CompletionResponse::tool_calls(vec![ToolInvocation::new(
        "c",
        SUBMIT_REPORT_TOOL,
        serde_json::json!({
            "outcome": "succeeded",
            "summary": RESEARCH_FINDINGS,
            "artifacts": [],
            "new_high_signal_facts": ["belt needs eccentric BB"],
        }),
    )])
}

/// R6: a research `DispatchSubagent` with `Delivery::Summarize` (the chat `delegate` shape) must
/// put the **relay contract** into the system prompt the subagent actually receives — asserted on
/// the provider's recorded request, not on `output_contract`'s return value, which is what
/// `e0fde79`'s own tests already cover.
///
/// Scope, stated honestly (R5): this proves the contract is **delivered**. It cannot prove the
/// subagent then writes more, because a scripted mock ignores its prompt. "Findings reach the face"
/// is only observable against a real model — see the live check in the PR body.
///
/// R7 wrong implementation excluded: *no contract on Summarize* (only vault path gets a directive)
/// — the first completion's system prompt would lack "Anything you leave out is gone", and the
/// seam would keep discarding research.
#[tokio::test]
async fn research_summarize_subagent_gets_the_relay_contract_in_its_live_prompt() {
    let provider = Arc::new(MockProvider::with_script("mock", [long_findings_report()]));
    let factory = CallRecordingFactory::default();
    let ceiling = research_ceiling();
    let orch = Orchestrator::new(
        provider.clone(),
        factory,
        ceiling.clone(),
        research_catalog(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let decision = DispatchDecision {
        action: DispatchAction::DispatchSubagent {
            goal: "compare belt drive vs chain drive for touring".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: vec!["search".into(), "spider".into()],
            success_criteria: vec!["findings with sources".into()],
            artifact_target: None,
            model: None,
            correlation_id: "chat-delegate-test-1".into(),
            delivery: Delivery::Summarize, // chat delegate path — no vault path
            depth: Depth::Deep,
        },
        confidence: 0.9,
        rationale: "research".into(),
    };

    let disposition = orch
        .run(
            decision,
            "compare belt vs chain",
            "parent-chat-corr",
            &ceiling,
        )
        .await
        .expect("run");

    // Contract was actually appended to the subagent (not only defined as a pure function).
    let req = provider
        .last_request()
        .expect("subagent must have been called");
    let system_blob: String = req
        .messages
        .iter()
        .filter(|m| m.role == liberado_provider::Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        system_blob.contains("Anything you leave out is gone"),
        "research Summarize must append relay_directive to the live task; system was:\n{system_blob}"
    );
    assert!(
        !system_blob.contains("FINISHED DOCUMENT"),
        "chat relay must not use the vault finished-document contract"
    );

    let Disposition::Reported(report) = disposition else {
        panic!("expected Reported, got {disposition:?}");
    };
    // A long summary survives to the report unmangled — the orchestrator does not truncate or
    // re-summarise what the subagent filed.
    //
    // This is deliberately **not** claimed as "the findings reach the face". `MockProvider` returns
    // its script regardless of the prompt it is given, so the findings here are guaranteed by the
    // fixture, not by the contract: with `relay_directive()` replaced by `String::new()` these
    // assertions still pass. No in-process test can show that a prompt *causes* a model to write
    // more — the provable property is the one above, that the contract reaches the subagent.
    assert!(
        report.summary.contains("eccentric BB")
            && report.summary.contains("135000")
            && report.summary.contains("BikeRadar"),
        "a long filed summary must survive to the report intact; got:\n{}",
        report.summary
    );
    assert!(
        report.summary.len() > STATUS_LINE.len() * 2,
        "fixture check: this summary must be substantially longer than the status-line shape, or \
         the assertion above proves nothing about truncation"
    );
}

/// Vault delivery still gets the document contract (unchanged), not the relay wording.
#[tokio::test]
async fn vault_delivery_subagent_still_gets_document_contract_not_relay() {
    let provider = Arc::new(MockProvider::with_script("mock", [long_findings_report()]));
    let factory = CallRecordingFactory::default();
    let ceiling = CapabilitySet::from_iter([
        Capability::ExecuteMcp("search".into()),
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Write(liberado_common::Zone::vault("Learning")),
    ]);
    let orch = Orchestrator::new(
        provider.clone(),
        factory,
        ceiling.clone(),
        vec![
            ("search".into(), Consequence::ReadOnly),
            ("turbovault".into(), Consequence::Reversible),
        ],
        Vec::new(), // zone_catalog unused when live_catalog absent
        vec![(
            "Learning".into(),
            liberado_common::WriteClass::AgentWritable,
        )],
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    )
    .with_report_sink(liberado_orchestrator::ReportSink::new(
        "turbovault",
        "write_note",
        "path",
        "content",
    ));

    let decision = DispatchDecision {
        action: DispatchAction::DispatchSubagent {
            goal: "research and file to Learning/x.md".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: vec!["search".into()],
            success_criteria: vec![],
            artifact_target: None,
            model: None,
            correlation_id: "vault-sub-1".into(),
            delivery: Delivery::Vault {
                path: "Learning/x.md".into(),
            },
            depth: Depth::Deep,
        },
        confidence: 0.9,
        rationale: "file it".into(),
    };

    let _ = orch
        .run(decision, "outer", "t1", &ceiling)
        .await
        .expect("run");

    let req = provider.last_request().expect("subagent called");
    let system_blob: String = req
        .messages
        .iter()
        .filter(|m| m.role == liberado_provider::Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        system_blob.contains("FINISHED DOCUMENT") && system_blob.contains("Learning/x.md"),
        "vault path must still get delivery_directive; got:\n{system_blob}"
    );
    assert!(
        !system_blob.contains("will trim or relay"),
        "vault path must not get the chat relay contract"
    );
}

/// R7: action subagents must not be told to write an essay — they produce their own artifact.
#[tokio::test]
async fn acting_subagent_gets_no_output_contract() {
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [submit_report_response()],
    ));
    let factory = CallRecordingFactory::default();
    let ceiling = CapabilitySet::from_iter([Capability::ExecuteMcp("email".into())]);
    let orch = Orchestrator::new(
        provider.clone(),
        factory,
        ceiling.clone(),
        vec![("email".into(), Consequence::External)],
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let decision = DispatchDecision {
        action: DispatchAction::DispatchSubagent {
            goal: "send the follow-up email".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: vec!["email".into()],
            success_criteria: vec![],
            artifact_target: None,
            model: None,
            correlation_id: "act-1".into(),
            delivery: Delivery::Summarize,
            depth: Depth::Normal,
        },
        confidence: 0.9,
        rationale: "act".into(),
    };

    let _ = orch
        .run(decision, "outer", "t1", &ceiling)
        .await
        .expect("run");

    let req = provider.last_request().expect("subagent called");
    let system_blob: String = req
        .messages
        .iter()
        .filter(|m| m.role == liberado_provider::Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !system_blob.contains("Anything you leave out is gone")
            && !system_blob.contains("FINISHED DOCUMENT"),
        "acting dispatch must not get research/vault output contracts; got:\n{system_blob}"
    );
}

/// Pins the **known gap**: `ExecuteDirect` gets no output contract, and its baseline instructions
/// ask for a *"concise, high-signal result"* — the same "short" contract that caused the seam bug.
/// A chat `delegate` the classifier judges simple lands here, so this path can still return an
/// unshaped summary.
///
/// It is left this way deliberately, and this test exists so that stays a decision rather than an
/// accident. `ExecuteDirect` carries no `Delivery`, so the orchestrator cannot tell a chat relay
/// from a cron brief or a vault reaction; appending `relay_directive` unconditionally would tell
/// every read-only direct execution to write a document, and that is the `orchestrator` role —
/// 92.8% of token spend. The fix needs a destination, not a directive.
///
/// **If you make `ExecuteDirect` destination-aware, this test should start failing.** Change it
/// then, and see `delegated-work-is-discarded-at-the-seam.md`.
#[tokio::test]
async fn execute_direct_gets_no_output_contract_today() {
    let provider = Arc::new(MockProvider::with_script("mock", [long_findings_report()]));
    let ceiling = research_ceiling();
    let orch = Orchestrator::new(
        provider.clone(),
        CallRecordingFactory::default(),
        ceiling.clone(),
        research_catalog(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            // Read-only MCPs: research-shaped, so this is the case that *would* qualify for the
            // relay contract if direct execution knew where its report was going.
            relevant_mcps: vec!["search".into()],
        },
        confidence: 0.9,
        rationale: "few steps suffice".into(),
    };

    let _ = orch
        .run(
            decision,
            "compare belt vs chain",
            "chat-delegate-direct",
            &ceiling,
        )
        .await
        .expect("run");

    let req = provider.last_request().expect("executor called");
    let system_blob: String = req
        .messages
        .iter()
        .filter(|m| m.role == liberado_provider::Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !system_blob.contains("Anything you leave out is gone"),
        "documented gap: ExecuteDirect has no relay contract. If you fixed that, update this test \
         and the seam doc; got:\n{system_blob}"
    );
    assert!(
        system_blob.contains("concise, high-signal"),
        "and its baseline instructions still ask for concision — the shape of the original bug"
    );
}
