//! Split from `completion_gate.rs`: kills the baseline campaign's survivors.
//!
//! Covers reviewer naming, vote flattening, kind labels, contract rendering,
//! the trace/fanout observers, refutation-history formatting, and the
//! best-effort strategist extraction chain.

use super::*;
use liberado_coder_core::CoderTask;
use liberado_provider::CompletionResponse;
use liberado_session::{GateVerdict, ReviewVote};

fn scripted_request(content: &str) -> Arc<dyn Provider> {
    let provider = liberado_provider::MockProvider::new("mock");
    provider.push(CompletionResponse::text(content));
    Arc::new(provider)
}

struct FixedFactory(Option<Arc<dyn Provider>>);

impl CoderProviderFactory for FixedFactory {
    fn provider_for(
        &self,
        _role: &str,
        _config: &CoderRoleConfig,
    ) -> Result<Arc<dyn Provider>, CoderError> {
        match &self.0 {
            Some(p) => Ok(Arc::clone(p)),
            None => Err(CoderError::Setup("no critic provider configured".into())),
        }
    }
}

fn gate_request() -> CoderRunRequest {
    serde_json::from_value(serde_json::json!({
        "task": {
            "id": "t",
            "description": "add --version",
            "context": "the CLI crate",
            "success_criteria": ["prints a semver", "exits zero"]
        },
        "workspace": {"root": "/tmp/ws", "base_ref": "main"},
        "config": {
            "backend": "loop",
            "planner": {"model": "p"},
            "coder": {"model": "c"},
            "critic": {"model": "cr"}
        }
    }))
    .expect("request json")
}

#[test]
fn the_reviewer_reports_its_configured_name() {
    let mut reviewer = ModelReviewer {
        name: "fresh-1".into(),
        ..reviewer()
    };
    reviewer.name = "gate-1".into();
    assert_eq!(reviewer.name(), "gate-1");
}

fn reviewer() -> ModelReviewer {
    ModelReviewer {
        name: "r".to_string(),
        provider: Arc::new(liberado_provider::MockProvider::new("mock")),
        role: CoderRoleConfig::default(),
        instructions: "you are a reviewer".to_string(),
    }
}

#[test]
fn flatten_votes_carries_every_field() {
    let outcome = GateOutcome {
        verdict: GateVerdict::Refuted {
            issues: vec!["late".into()],
        },
        votes: vec![
            RecordedVote {
                reviewer: "gatekeeper".into(),
                kind: ReviewerKind::Gatekeeper,
                vote: ReviewVote::Approve,
                coerced_from: None,
            },
            RecordedVote {
                reviewer: "fresh-2".into(),
                kind: ReviewerKind::Fresh,
                vote: ReviewVote::Refute {
                    issues: vec!["no tests".into(), "wrong shape".into()],
                },
                coerced_from: Some("backend down".into()),
            },
        ],
    };
    let records = flatten_votes(&outcome);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].reviewer, "gatekeeper");
    assert_eq!(records[0].kind, "gatekeeper");
    assert!(records[0].approved);
    assert!(records[0].issues.is_empty());
    assert!(!records[0].coerced);
    assert_eq!(records[1].reviewer, "fresh-2");
    assert_eq!(records[1].kind, "fresh");
    assert!(!records[1].approved);
    assert_eq!(records[1].issues, vec!["no tests", "wrong shape"]);
    assert!(
        records[1].coerced,
        "the substitution must survive transport"
    );
}

#[test]
fn every_reviewer_kind_has_its_label() {
    assert_eq!(kind_label(ReviewerKind::Gatekeeper), "gatekeeper");
    assert_eq!(kind_label(ReviewerKind::Fresh), "fresh");
    assert_eq!(kind_label(ReviewerKind::Strategist), "strategist");
}

#[test]
fn contract_summary_renders_task_criteria_and_context() {
    let out = contract_summary(&gate_request());
    assert!(out.contains("Task:\nadd --version"), "{out}");
    assert!(out.contains("- prints a semver"), "{out}");
    assert!(out.contains("- exits zero"), "{out}");
    assert!(out.contains("Task context:\nthe CLI crate"), "{out}");
}

#[test]
fn a_contract_without_criteria_says_so() {
    let mut request = gate_request();
    request.task = CoderTask::new("t", "bare task");
    let out = contract_summary(&request);
    assert!(out.contains("(none listed)"), "{out}");
}

#[test]
fn the_trace_observer_records_each_vote_and_flags_coercion() {
    let log: EventLog = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observer = TraceObserver { events: &log };
    observer.on_vote(&RecordedVote {
        reviewer: "g".into(),
        kind: ReviewerKind::Gatekeeper,
        vote: ReviewVote::Approve,
        coerced_from: None,
    });
    observer.on_vote(&RecordedVote {
        reviewer: "f".into(),
        kind: ReviewerKind::Fresh,
        vote: ReviewVote::Refute {
            issues: vec!["x".into()],
        },
        coerced_from: Some("timeout".into()),
    });
    let events = log.lock().expect("lock");
    assert_eq!(events.len(), 3, "{events:?}");
    assert!(matches!(
        &events[0],
        CoderEvent::CriticVerdict {
            verdict: CriticVerdict::Acceptable,
            ..
        }
    ));
    assert!(matches!(
        &events[1],
        CoderEvent::CriticVerdict {
            verdict: CriticVerdict::NeedsRevision { issues },
            ..
        } if issues == &vec!["x".to_string()]
    ));
    assert!(
        matches!(
            &events[2],
            CoderEvent::LoopGuardTriggered { guard, action, .. }
            if guard == "gate_reviewer_unavailable:f" && action == "counted_as_refuting"
        ),
        "a coerced vote must be distinguishable from a genuine rejection"
    );
}

#[test]
fn the_fanout_observer_feeds_both_sides() {
    let log_a: EventLog = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log_b: EventLog = Arc::new(std::sync::Mutex::new(Vec::new()));
    let fanout = FanoutObserver {
        a: &TraceObserver { events: &log_a },
        b: &TraceObserver { events: &log_b },
    };
    fanout.on_vote(&RecordedVote {
        reviewer: "g".into(),
        kind: ReviewerKind::Gatekeeper,
        vote: ReviewVote::Approve,
        coerced_from: None,
    });
    assert_eq!(log_a.lock().expect("lock").len(), 1, "side A saw nothing");
    assert_eq!(log_b.lock().expect("lock").len(), 1, "side B saw nothing");
}

#[tokio::test]
async fn run_strategist_returns_the_directive_it_extracts() {
    let providers = FixedFactory(Some(scripted_request("  switch to a config struct  ")));
    let directive = run_strategist(&providers, &gate_request(), &["old issue".into()])
        .await
        .expect("strategist is best-effort, not fatal");
    assert_eq!(directive.as_deref(), Some("switch to a config struct"));
}

#[tokio::test]
async fn run_strategist_without_a_provider_stays_best_effort() {
    let providers = FixedFactory(None);
    let directive = run_strategist(&providers, &gate_request(), &[])
        .await
        .expect("an unavailable strategist must not fail the attempt");
    assert_eq!(directive, None);
}

#[test]
fn refutation_history_renders_newest_first_bounded() {
    assert_eq!(format_refutation_history(&[]), "(none recorded)");
    let all: Vec<String> = (0..15).map(|i| format!("issue-{i}")).collect();
    let rendered = format_refutation_history(&all);
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines.len(), PRIOR_REFUTATIONS_MAX, "{rendered}");
    assert_eq!(
        lines[0], "- issue-3",
        "oldest kept entry is the first cut victim's successor"
    );
    assert_eq!(
        lines.last(),
        Some(&"- issue-14"),
        "the most recent complaint must be present"
    );
}

#[tokio::test]
async fn complete_and_extract_trims_and_skips_blanks() {
    let completion = CompletionRequest::new(vec![Message::user("propose")]);
    let directive = complete_and_extract(
        scripted_request("  REAL DIRECTIVE  ").as_ref(),
        completion.clone(),
        2,
    )
    .await
    .expect("best-effort");
    assert_eq!(directive.as_deref(), Some("REAL DIRECTIVE"));

    let empty = complete_and_extract(scripted_request("").as_ref(), completion.clone(), 2)
        .await
        .expect("best-effort");
    assert_eq!(empty, None, "an empty answer is no directive");

    let blank = complete_and_extract(scripted_request("   \n\t ").as_ref(), completion.clone(), 2)
        .await
        .expect("best-effort");
    assert_eq!(blank, None, "whitespace-only padding is no directive");

    let failing = liberado_provider::MockProvider::new("mock");
    failing.push_error(liberado_provider::ProviderError::MockExhausted);
    let errored = complete_and_extract(&failing, completion, 2)
        .await
        .expect("best-effort");
    assert_eq!(errored, None, "a strategist outage continues without one");
}
