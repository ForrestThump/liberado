//! Tests for pool authority segregation.

use super::super::*;
use super::test_fixtures::*;
use liberado_common::{
    Capability, CapabilityCatalog, CapabilitySet, Delivery, DispatchAction, DispatchDecision,
    Event, EventPayload, Guard, RiskWaiver, RiskWaiverSet,
};
use liberado_config_loader::DispatchTuning;
use liberado_dispatcher::Dispatcher;
use liberado_executor::SUBMIT_REPORT_TOOL;
use liberado_orchestrator::{Disposition, Orchestrator};
use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};
use liberado_test_support::CallRecordingFactory;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;

#[tokio::test]
async fn risk_waivers_reach_pools_regardless_of_builder_order() {
    let waiver = RiskWaiver {
        mcp: "tasks-mcp".into(),
        match_tools: None,
        match_zones: None,
        guard: Guard::Magnitude,
    };
    let waivers = RiskWaiverSet {
        waivers: [waiver].into_iter().collect(),
    };
    let dispatcher = || {
        Dispatcher::new(
            Arc::new(MockProvider::with_script("dispatch", [])),
            DispatchTuning::default(),
            4,
        )
    };

    let (before, _dir) = temp_daemon().await;
    let before = before
        .with_risk_waivers(waivers.clone())
        .with_pool_dispatcher(
            "before",
            dispatcher(),
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
        );
    assert!(
        before.pools["before"]
            .dispatcher
            .as_ref()
            .unwrap()
            .risk_waivers
            .covers(Guard::Magnitude, "tasks-mcp:list", None)
    );

    let (after, _dir) = temp_daemon().await;
    let after = after
        .with_pool_dispatcher(
            "after",
            dispatcher(),
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
        )
        .with_risk_waivers(waivers);
    assert!(
        after.pools["after"]
            .dispatcher
            .as_ref()
            .unwrap()
            .risk_waivers
            .covers(Guard::Magnitude, "tasks-mcp:list", None)
    );
}

#[tokio::test]
async fn pools_are_authority_segregated() {
    // The direct proof that named pools (Decision 18 checkpoint #3) aren't just routed but
    // actually authority-segregated: two pools, two schedules, both decisions asking to call
    // the SAME MCP — but only "granted-pool" was actually given that capability. If pools
    // shared authority (e.g. a bug reusing one capability set for both), "blocked-pool" would
    // reach the real runtime too; it must not.
    let (daemon, _dir) = temp_daemon().await;

    // Both pools' dispatchers classify identically: ExecuteDirect against "shared-mcp".
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: vec!["shared-mcp".into()],
            delivery: Delivery::Summarize,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    let decision_json = serde_json::to_string(&decision).unwrap();
    let dispatcher_for = || {
        Dispatcher::new(
            Arc::new(MockProvider::with_script(
                "dispatch",
                [CompletionResponse::text(decision_json.clone())],
            )),
            DispatchTuning::default(),
            4,
        )
    };

    // granted-pool: actually holds the "shared-mcp" capability, so its orchestrator's
    // ExecuteDirect scoping resolves a non-empty `allowed_mcps` and reaches the real factory.
    let granted_capabilities =
        CapabilitySet::from_iter([Capability::ExecuteMcp("shared-mcp".into())]);
    let granted_factory = CallRecordingFactory::default();
    let granted_calls = granted_factory.calls.clone();
    let granted_exec = Arc::new(MockProvider::with_script(
        "exec-granted",
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c1",
                "shared-mcp:do_thing",
                serde_json::json!({}),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c2",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "granted pool acted" }),
            )]),
        ],
    ));
    let granted_orch = Orchestrator::new(
        granted_exec,
        granted_factory,
        granted_capabilities.clone(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        liberado_common::ProposalSigner::random(),
        "granted-pool",
    );

    // blocked-pool: an EMPTY capability set — the dispatcher's own pre-flight capability guard
    // (`guards::evaluate`'s `CapabilityGap` check, run against THIS pool's own capabilities)
    // catches the reference to "shared-mcp" before the decision ever reaches an orchestrator,
    // downgrading it to Clarify — so `blocked_exec`/`blocked_factory` below must NEVER be
    // touched at all. That's the segregation proof: the identical decision that runs for real
    // in granted-pool never even reaches execution in blocked-pool.
    let blocked_factory = CallRecordingFactory::default();
    let blocked_calls = blocked_factory.calls.clone();
    let blocked_exec = Arc::new(MockProvider::with_script("exec-blocked", []));
    let blocked_orch = Orchestrator::new(
        blocked_exec,
        blocked_factory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        liberado_common::ProposalSigner::random(),
        "blocked-pool",
    );

    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_pool_dispatcher(
            "granted-pool",
            dispatcher_for(),
            Arc::new(CapabilityCatalog::new()),
            granted_capabilities,
        )
        .with_pool_orchestrator("granted-pool", granted_orch)
        .with_pool_dispatcher(
            "blocked-pool",
            dispatcher_for(),
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
        )
        .with_pool_orchestrator("blocked-pool", blocked_orch);

    // Inject one event per pool directly (the same seam `liberado-server`'s webhook handler
    // and `liberado-cron` both use) — deterministic, no dependence on real-time cron ticking.
    let sender = daemon.event_sender();

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(200)).await;
    sender
        .send(Event::trigger(
            "Trigger",
            "test:granted",
            "test:granted:1",
            EventPayload {
                pool: Some("granted-pool".into()),
                ..Default::default()
            },
        ))
        .unwrap();
    sender
        .send(Event::trigger(
            "Trigger",
            "test:blocked",
            "test:blocked:1",
            EventPayload {
                pool: Some("blocked-pool".into()),
                ..Default::default()
            },
        ))
        .unwrap();

    let mut outcomes_by_pool = std::collections::HashMap::new();
    for _ in 0..2 {
        let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for a reaction")
            .expect("reaction channel closed");
        outcomes_by_pool.insert(reaction.event.payload.pool.clone(), reaction.outcome);
    }

    // granted-pool: authorized for "shared-mcp" — the decision runs for real.
    match outcomes_by_pool.get(&Some("granted-pool".to_string())) {
        Some(ReactionOutcome::Acted(Disposition::Reported(_))) => {}
        Some(o) => panic!("expected granted-pool to reach Reported, got {}", o.label()),
        None => panic!("no reaction recorded for granted-pool"),
    }

    // blocked-pool: an identical decision naming the same MCP, but this pool was never
    // granted it — the dispatcher's own pre-flight guard catches it and downgrades to
    // Clarify, never reaching an orchestrator/runtime at all.
    match outcomes_by_pool.get(&Some("blocked-pool".to_string())) {
        Some(ReactionOutcome::Acted(Disposition::Clarify { what_blocked, .. })) => {
            assert_eq!(*what_blocked, liberado_common::BlockReason::CapabilityGap);
        }
        Some(o) => panic!(
            "expected blocked-pool to be guard-downgraded to Clarify, got {}",
            o.label()
        ),
        None => panic!("no reaction recorded for blocked-pool"),
    }

    // The load-bearing assertion: granted-pool's own capability actually reached the real
    // runtime; blocked-pool's identical request never did, despite an identical decision.
    assert_eq!(
        granted_calls.lock().unwrap().len(),
        1,
        "granted-pool must reach the real runtime for a call it's actually authorized for"
    );
    assert!(
        blocked_calls.lock().unwrap().is_empty(),
        "blocked-pool must NEVER reach the real runtime for an MCP it wasn't granted, even \
             though the decision asked for the exact same call granted-pool made"
    );

    handle.abort();
}
