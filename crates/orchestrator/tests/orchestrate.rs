//! Tests that the orchestrator maps each decision to the right execution: tasks built, provenance
//! correlation chosen per action, and Clarify short-circuiting before any execution.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use liberado_common::{
    BlockReason, Capability, CapabilitySet, Consequence, DispatchAction, DispatchDecision,
    Outcome, Proposal, ProposalStatus, ProposedAction, ToolCall, WriteProvenance,
};
use liberado_executor::{SUBMIT_REPORT_TOOL, ToolRuntime};
use liberado_orchestrator::{Disposition, Orchestrator, RuntimeFactory, RuntimeSetupError, SubDispatch};
use liberado_provider::{CompletionResponse, MockProvider, ToolDef, ToolInvocation};

/// A runtime that offers no tools (so the scripted model goes straight to `submit_report`).
struct MockRuntime;

#[async_trait]
impl ToolRuntime for MockRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Ok("ok".to_string())
    }
}

type Calls = Arc<Mutex<Vec<(Vec<String>, WriteProvenance)>>>;

/// Records every `runtime_for` call so tests can assert what scope/provenance the orchestrator
/// derived from the decision. Cloneable (shared `calls`) so a handle can outlive the move into the
/// orchestrator.
#[derive(Clone, Default)]
struct RecordingFactory {
    calls: Calls,
}

#[async_trait]
impl RuntimeFactory for RecordingFactory {
    async fn runtime_for(
        &self,
        allowed_mcps: &[String],
        provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        self.calls
            .lock()
            .unwrap()
            .push((allowed_mcps.to_vec(), provenance));
        Ok(Box::new(MockRuntime))
    }
}

/// A runtime that records every `invoke` so `execute_approved` tests can assert the exact approved
/// calls ran. Shares its log so a handle survives the move into the factory.
#[derive(Clone, Default)]
struct RecordingRuntime {
    invoked: Arc<Mutex<Vec<ToolInvocation>>>,
}

#[async_trait]
impl ToolRuntime for RecordingRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        self.invoked.lock().unwrap().push(call.clone());
        Ok("ok".to_string())
    }
}

/// A factory that hands out clones of one [`RecordingRuntime`], so the test can read back what the
/// orchestrator invoked.
struct RecordingRuntimeFactory {
    runtime: RecordingRuntime,
}

#[async_trait]
impl RuntimeFactory for RecordingRuntimeFactory {
    async fn runtime_for(
        &self,
        _allowed_mcps: &[String],
        _provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        Ok(Box::new(self.runtime.clone()))
    }
}

fn submit_report_response() -> CompletionResponse {
    CompletionResponse::tool_calls(vec![ToolInvocation::new(
        "c",
        SUBMIT_REPORT_TOOL,
        serde_json::json!({ "outcome": "succeeded", "summary": "done" }),
    )])
}

fn orchestrator(script: Vec<CompletionResponse>, capabilities: CapabilitySet) -> (Calls, Orchestrator) {
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let factory = RecordingFactory::default();
    let calls = factory.calls.clone();
    let orch = Orchestrator::new(provider, factory, capabilities, Vec::new(), std::env::temp_dir());
    (calls, orch)
}

#[tokio::test]
async fn execute_direct_scopes_the_runtime_to_the_granted_mcps() {
    // ExecuteDirect scopes to exactly what `capabilities` grants — an empty allow-list would mean
    // "every registered MCP" to the factory, which is the bug this test guards against.
    let capabilities =
        CapabilitySet::from_iter([Capability::ExecuteMcp("tasks-mcp".into())]);
    let (calls, orch) = orchestrator(vec![submit_report_response()], capabilities);

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
        },
        confidence: 0.9,
        rationale: "simple".into(),
    };

    let disposition = orch
        .run(decision, "tidy the inbox", "vault-change:inbox/x.md:abc123")
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
    let (calls, orch) = orchestrator(vec![submit_report_response()], capabilities);

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: vec!["tasks-mcp".into()],
        },
        confidence: 0.9,
        rationale: "simple".into(),
    };

    orch.run(decision, "add milk to my list", "trigger-1")
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
    let (calls, orch) = orchestrator(vec![submit_report_response()], CapabilitySet::empty());

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
        },
        confidence: 0.9,
        rationale: "simple".into(),
    };
    let disposition = orch
        .run(decision, "tidy the inbox", "trigger-1")
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
    let (calls, orch) = orchestrator(vec![submit_report_response()], CapabilitySet::empty());

    let decision = DispatchDecision {
        action: DispatchAction::DispatchSubagent {
            goal: "summarize recent decisions".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: vec!["tasks-mcp".into(), "memory-mcp".into()],
            success_criteria: vec!["a summary note exists".into()],
            artifact_target: None,
            model: None,
            correlation_id: "subagent-42".into(),
        },
        confidence: 0.8,
        rationale: "multi-step".into(),
    };

    let disposition = orch
        .run(
            decision,
            "outer goal is ignored for subagents",
            "trigger-xyz",
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
    let (calls, orch) = orchestrator(vec![], CapabilitySet::empty());

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
        .run(decision, "email the boss", "vault-change:inbox/x.md:abc")
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
    let (calls, orch) = orchestrator(vec![], CapabilitySet::empty());

    let decision = DispatchDecision {
        action: DispatchAction::Clarify {
            questions: vec!["which project?".into()],
            what_blocked: BlockReason::Ambiguous,
        },
        confidence: 0.3,
        rationale: "ambiguous".into(),
    };

    let disposition = orch
        .run(decision, "ambiguous goal", "trigger-1")
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
    let runtime = RecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        Vec::<CompletionResponse>::new(),
    ));
    let orch = Orchestrator::new(
        provider,
        RecordingRuntimeFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        std::env::temp_dir(),
    );

    let call = ToolCall {
        tool: "email:send".into(),
        args: serde_json::json!({ "to": "boss@example.com", "body": "hi" }),
    };
    let mut proposal = Proposal::pending(
        "vault-change:inbox/x.md:abc",
        "vault-change:inbox/x.md:abc",
        "liberado",
        ProposedAction::ToolCalls(vec![call.clone()]),
        "the note asks to email the boss",
    );
    proposal.status = ProposalStatus::Approved;

    let report = orch.execute_approved(&proposal).await.expect("execute");
    assert_eq!(report.outcome, Outcome::Succeeded);

    // The runtime saw exactly the approved call, with the approved args.
    let invoked = invoked.lock().unwrap();
    assert_eq!(invoked.len(), 1);
    assert_eq!(invoked[0].name, "email:send");
    assert_eq!(invoked[0].arguments, call.args);
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
    let runtime = RecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let proposals_dir = tempfile::TempDir::new().unwrap();
    let capabilities = CapabilitySet::from_iter([Capability::ExecuteMcp("dangerous-mcp".into())]);
    let orch = Orchestrator::new(
        provider,
        RecordingRuntimeFactory { runtime },
        capabilities,
        vec![("dangerous-mcp".into(), Consequence::External)],
        proposals_dir.path().to_path_buf(),
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
        .run(decision, "clean up the vault", "trigger-1")
        .await
        .expect("run");

    // The downgrade is a tool *result*, not an abort — the run still completes normally.
    assert!(matches!(disposition, Disposition::Reported(_)));

    // The real tool never ran.
    assert!(
        invoked.lock().unwrap().is_empty(),
        "a high-consequence adaptive call must not reach the real tool"
    );

    // A proposal file was written instead.
    let written = std::fs::read_dir(proposals_dir.path().join("proposals"))
        .expect("proposals dir should exist")
        .count();
    assert_eq!(written, 1, "exactly one proposal file should be written");
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
    let runtime = RecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let capabilities = CapabilitySet::from_iter([Capability::ExecuteMcp("safe-mcp".into())]);
    let orch = Orchestrator::new(
        provider,
        RecordingRuntimeFactory { runtime },
        capabilities,
        Vec::new(),
        std::env::temp_dir(),
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
        .run(decision, "do a safe thing", "trigger-1")
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
    let runtime = RecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let ceiling = CapabilitySet::from_iter([
        Capability::ExecuteMcp("tasks-mcp".into()),
        Capability::ExecuteMcp("other-mcp".into()),
    ]);
    let orch = Orchestrator::new(
        provider,
        RecordingRuntimeFactory { runtime },
        ceiling,
        Vec::new(),
        std::env::temp_dir(),
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
        },
        confidence: 0.8,
        rationale: "multi-step".into(),
    };

    let disposition = orch
        .run(decision, "outer goal ignored", "trigger-1")
        .await
        .expect("run");

    assert!(matches!(disposition, Disposition::Reported(_)));
    assert!(
        invoked.lock().unwrap().is_empty(),
        "the subagent's own (narrower) capabilities must gate it, not the orchestrator's raw ceiling"
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
    let runtime = RecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let capabilities = CapabilitySet::from_iter([Capability::ExecuteMcp("dangerous-mcp".into())]);
    let orch = Orchestrator::new(
        provider,
        RecordingRuntimeFactory { runtime },
        capabilities,
        vec![("dangerous-mcp".into(), Consequence::External)],
        std::env::temp_dir(),
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
    let runtime = RecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        Vec::<CompletionResponse>::new(),
    ));
    let orch = Orchestrator::new(
        provider,
        RecordingRuntimeFactory { runtime },
        CapabilitySet::empty(),
        vec![("email-mcp".into(), Consequence::External)],
        std::env::temp_dir(),
    );

    let call = ToolCall {
        tool: "email-mcp:send".into(),
        args: serde_json::json!({ "to": "boss@example.com" }),
    };
    let mut proposal = Proposal::pending(
        "id-1",
        "id-1",
        "liberado",
        ProposedAction::ToolCalls(vec![call.clone()]),
        "approved email",
    );
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
