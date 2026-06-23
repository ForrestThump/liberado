//! Tests that the orchestrator maps each decision to the right execution: tasks built, provenance
//! correlation chosen per action, and Clarify short-circuiting before any execution.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use liberado_common::{
    BlockReason, CapabilitySet, DispatchAction, DispatchDecision, Outcome, WriteProvenance,
};
use liberado_executor::{SUBMIT_REPORT_TOOL, ToolRuntime};
use liberado_orchestrator::{Disposition, Orchestrator, RuntimeFactory, RuntimeSetupError};
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

fn submit_report_response() -> CompletionResponse {
    CompletionResponse::tool_calls(vec![ToolInvocation::new(
        "c",
        SUBMIT_REPORT_TOOL,
        serde_json::json!({ "outcome": "succeeded", "summary": "done" }),
    )])
}

fn orchestrator(script: Vec<CompletionResponse>) -> (Calls, Orchestrator) {
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let factory = RecordingFactory::default();
    let calls = factory.calls.clone();
    let orch = Orchestrator::new(provider, factory);
    (calls, orch)
}

#[tokio::test]
async fn execute_direct_runs_and_adopts_the_trigger_correlation() {
    let (calls, orch) = orchestrator(vec![submit_report_response()]);

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
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
    assert!(allowed.is_empty(), "ExecuteDirect sees no narrowed catalog");
    assert_eq!(prov.source, "liberado-executor");
    // ExecuteDirect acts in the reaction's name → adopts the triggering correlation.
    assert_eq!(
        prov.correlation_id.as_deref(),
        Some("vault-change:inbox/x.md:abc123")
    );
}

#[tokio::test]
async fn dispatch_subagent_uses_its_own_correlation_and_allowed_mcps() {
    let (calls, orch) = orchestrator(vec![submit_report_response()]);

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
async fn clarify_short_circuits_without_executing() {
    // No scripted responses + no runtime: proves nothing ran.
    let (calls, orch) = orchestrator(vec![]);

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
