//! Split from `remediation.rs`: kills the baseline campaign's survivors.
//!
//! An actionable review must spend a real coding run and describe what it
//! addressed; only a nothing-to-do review skips the run.

use super::*;
use crate::CoderRunResult;
use liberado_coder_core::Remedy;
use liberado_common::Outcome;

struct SucceedingBackend;

#[async_trait::async_trait]
impl CoderBackend for SucceedingBackend {
    fn name(&self) -> &str {
        "succeeding"
    }
    async fn run(&self, _request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
        Ok(CoderRunResult {
            backend: "succeeding".into(),
            outcome: Outcome::Succeeded,
            summary: "fixed".into(),
            files_changed: vec!["src/lib.rs".into()],
            file_changes: Vec::new(),
            validation_notes: None,
            critic_verdict: None,
            gate_votes: Vec::new(),
            trace_path: None,
            diff_findings: Vec::new(),
            session_findings: Vec::new(),
            remediation: None,
            diagnostics: serde_json::json!({}),
        })
    }
}

fn actionable_finding(kind: &str) -> SessionFinding {
    SessionFinding {
        kind: kind.into(),
        quote: "all tests pass".into(),
        why: "no test ran in the trace".into(),
        remedy: Remedy::Repair,
    }
}

fn base_request() -> CoderRunRequest {
    serde_json::from_value(serde_json::json!({
        "task": {"id": "t-9", "description": "original task"},
        "workspace": {"root": "/tmp/ws", "base_ref": "main"},
        "config": {
            "backend": "loop",
            "planner": {"model": "p"},
            "coder": {"model": "c"},
            "critic": {"model": "cr"},
            "session_critic": { "enabled": true, "remediation": true }
        }
    }))
    .expect("request json")
}

#[tokio::test]
async fn actionable_findings_trigger_one_remediation_run() {
    let findings = vec![
        actionable_finding("abandoned_finding"),
        SessionFinding {
            remedy: Remedy::Retract,
            ..actionable_finding("unsupported_claim")
        },
    ];
    let record = run_remediation(
        &SucceedingBackend,
        &base_request(),
        &findings,
        "agent/remediation-t-9-x".into(),
    )
    .await
    .expect("the run happened");
    let record = record.expect("actionable findings produce a record");
    assert_eq!(record.branch, "agent/remediation-t-9-x");
    assert_eq!(record.outcome, Outcome::Succeeded);
    assert_eq!(record.summary, "fixed");
    // Only the actionable finding is claimed as addressed.
    assert_eq!(record.addressed, vec!["abandoned_finding".to_string()]);
}

/// The remediation run inherits verifiers but never its own session critic —
/// a critic reviewing a run spawned to satisfy it is a feedback loop.
#[test]
fn the_remediation_request_disables_its_own_critic() {
    let base = base_request();
    let derived = remediation_request(&base, "fix it".into());
    assert!(!derived.config.session_critic.enabled);
    assert!(!derived.config.session_critic.remediation);
    assert_eq!(derived.attempt, 0);
    assert!(derived.prior_feedback.is_empty());
}
