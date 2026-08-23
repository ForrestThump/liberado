// Split continuation of dispatcher lib tests.

#![allow(unused_imports)]

use super::*;
use liberado_common::{Capability, Consequence, Delivery, Depth, ToolCall, WriteClass};
use liberado_provider::{CompletionResponse, MockProvider, ResponseFormat};
use std::sync::Mutex;

#[test]
fn the_schema_does_not_offer_propose() {
    let rendered = decision_schema().to_string();
    assert!(
        !rendered.contains("Propose"),
        "the classifier must not be able to emit Propose"
    );
    for expected in ["ExecuteDirect", "DispatchSubagent", "Clarify"] {
        assert!(rendered.contains(expected), "missing variant {expected}");
    }
}
#[derive(Default, Clone)]
struct Captured(Arc<Mutex<Vec<(tracing::Level, String)>>>);
impl<S: tracing::Subscriber> tracing_subscriber::layer::Layer<S> for Captured {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Msg(String);
        impl tracing::field::Visit for Msg {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    self.0 = format!("{v:?}");
                }
            }
        }
        let mut m = Msg(String::new());
        event.record(&mut m);
        self.0
            .lock()
            .unwrap()
            .push((*event.metadata().level(), m.0));
    }
}
fn with_captured<R>(f: impl FnOnce() -> R) -> (R, Vec<(tracing::Level, String)>) {
    use tracing_subscriber::layer::SubscriberExt as _;
    let captured = Captured::default();
    let sub = tracing_subscriber::registry().with(captured.clone());
    let out = tracing::subscriber::with_default(sub, f);
    let seen = captured.0.lock().unwrap().clone();
    (out, seen)
}
fn decision(action: DispatchAction) -> DispatchDecision {
    DispatchDecision {
        action,
        confidence: 0.9,
        rationale: "test".into(),
    }
}
#[test]
fn classified_decision_logging_fires_for_every_action_variant() {
    use liberado_common::{BlockReason, Delivery};

    let decisions = vec![
        decision(DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        }),
        decision(DispatchAction::DispatchSubagent {
            goal: "sub-goal".into(),
            allowed_mcps: vec!["mcp-a".into()],
            capabilities: CapabilitySet::default(),
            success_criteria: Vec::new(),
            artifact_target: None,
            delivery: Delivery::Summarize,
            depth: Depth::Normal,
            model: None,
            correlation_id: String::new(),
        }),
        decision(DispatchAction::Clarify {
            questions: vec!["which vault?".into()],
            what_blocked: BlockReason::Ambiguous,
        }),
        decision(DispatchAction::Propose {
            proposed_action: ProposedAction::ToolCalls(Vec::new()),
            rationale: "needs approval".into(),
        }),
    ];

    let (_, seen) = with_captured(|| {
        for d in &decisions {
            log_classified_decision(d, "test-model");
        }
    });

    let lines = seen
        .iter()
        .filter(|(l, m)| *l == tracing::Level::INFO && m.contains("classified decision"))
        .count();
    assert_eq!(
        lines,
        decisions.len(),
        "each action variant must emit exactly one pre-guard line, got {seen:?}"
    );
}
#[test]
fn normalize_mcp_list_logs_rewrites_but_not_identity_entries() {
    let known: std::collections::HashSet<&str> = ["turbovault"].into();

    let (out, seen) = with_captured(|| {
        normalize_mcp_list(
            vec![
                "turbovault:list_tasks".into(), // needs rewrite → debug note
                "turbovault".into(),            // already canonical → silent
                "not-an-mcp".into(),            // unknown → warn + drop
            ],
            &known,
            "allowed_mcps",
        )
    });

    assert_eq!(out, vec!["turbovault"]);
    let rewrites = seen
        .iter()
        .filter(|(_, m)| m.contains("normalized MCP reference"))
        .count();
    let drops = seen
        .iter()
        .filter(|(_, m)| m.contains("dropping unknown MCP reference"))
        .count();
    assert_eq!(
        rewrites, 1,
        "exactly the rewritten entry logs a note: {seen:?}"
    );
    assert_eq!(drops, 1, "the unknown entry logs its drop: {seen:?}");
}
