//! Tests for daemon session profiles, joinable background sessions, and notifier delivery.

use super::super::*;
use super::test_fixtures::*;
use liberado_common::{
    Capability, CapabilityCatalog, CapabilitySet, Delivery, DispatchAction, DispatchDecision,
    Event, EventPayload, ProposalSigner, WriteProvenance,
};
use liberado_config_loader::DispatchTuning;
use liberado_dispatch_pack::{DISPATCH_DOMAIN, DispatchPack};
use liberado_dispatcher::Dispatcher;
use liberado_executor::{RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL, ToolRuntime};
use liberado_orchestrator::Orchestrator;
use liberado_provider::{CompletionResponse, MockProvider, ToolDef, ToolInvocation};
use liberado_session::{GoalSessionHub, GoalSessionStore, SessionStatus, TerminalKind, Visibility};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;

#[tokio::test]
async fn unknown_session_profile_does_not_start_session() {
    let (daemon, _dir) = temp_daemon().await;
    let grant_dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "unused",
            Vec::<CompletionResponse>::new(),
        )),
        DispatchTuning::default(),
        4,
    );
    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        std::env::temp_dir(),
    );
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);

    let mut known = HashMap::new();
    known.insert(
        "interactive-cron".into(),
        CapabilitySet::from_iter([Capability::AskHuman]),
    );

    let daemon = daemon
        .with_dispatcher(
            grant_dispatcher,
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
            Vec::new(),
        )
        .with_session_profile_caps(known)
        .with_goal_hub(hub.clone());

    let sender = daemon.event_sender();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));
    tokio::time::sleep(Duration::from_millis(50)).await;

    sender
        .send(Event::trigger(
            "CronFired",
            "cron:bad-profile",
            "cron:bad-profile:1",
            EventPayload {
                summary: Some("should not run".into()),
                data: serde_json::json!({ "profile": "typo-not-in-map" }),
                ..Default::default()
            },
        ))
        .unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");
    assert!(
        matches!(reaction.outcome, ReactionOutcome::Observed),
        "unknown profile must be Observed, not Dispatched; got {}",
        reaction.outcome.label()
    );
    assert!(
        hub.list().await.is_empty(),
        "no hosted session may be created for an unknown profile"
    );

    handle.abort();
}

#[tokio::test]
async fn known_session_profile_still_dispatches() {
    let (daemon, _dir) = temp_daemon().await;
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "profile ok".into(),
    };
    let dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&decision).unwrap(),
            )],
        )),
        DispatchTuning::default(),
        4,
    );
    let orchestrator = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "exec",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "ok" }),
            )])],
        )),
        NoopFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );
    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        std::env::temp_dir(),
    )
    .with_pool("default", dispatcher, orchestrator);
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);

    let grant_dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "unused",
            Vec::<CompletionResponse>::new(),
        )),
        DispatchTuning::default(),
        4,
    );
    let mut known = HashMap::new();
    known.insert(
        "interactive-cron".into(),
        CapabilitySet::from_iter([Capability::AskHuman]),
    );

    let daemon = daemon
        .with_dispatcher(
            grant_dispatcher,
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
            Vec::new(),
        )
        .with_session_profile_caps(known)
        .with_goal_hub(hub.clone());

    let sender = daemon.event_sender();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));
    tokio::time::sleep(Duration::from_millis(50)).await;

    sender
        .send(Event::trigger(
            "CronFired",
            "cron:good-profile",
            "cron:good-profile:1",
            EventPayload {
                summary: Some("profile-gated goal".into()),
                data: serde_json::json!({ "profile": "interactive-cron" }),
                ..Default::default()
            },
        ))
        .unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");
    let session_id = match reaction.outcome {
        ReactionOutcome::Dispatched { session_id } => session_id,
        other => panic!("known profile must dispatch, got {}", other.label()),
    };
    let snap = hub
        .snapshot(&session_id)
        .await
        .expect("session must exist on hub");
    assert_eq!(
        snap.session.goal.profile.as_deref(),
        Some("interactive-cron")
    );
    assert!(
        snap.session.grant.capabilities.grants_ask_human(),
        "the session grant must inherit the profile's AskHuman capability: got {:?}",
        snap.session.grant.capabilities
    );
    assert_eq!(
        snap.session.grant.profile.as_deref(),
        Some("interactive-cron"),
        "the grant's profile field must echo the resolved profile"
    );
    assert!(
        snap.session.grant.overrides.is_null(),
        "no profile-supplied overrides were configured; the grant's overrides must be Value::Null"
    );

    handle.abort();
}

#[tokio::test]
async fn l9_cron_event_becomes_joinable_dispatched_session() {
    let (daemon, _dir) = temp_daemon().await;

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "L9 routine cron".into(),
    };
    let dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&decision).unwrap(),
            )],
        )),
        DispatchTuning::default(),
        4,
    );
    let orchestrator = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "exec",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "L9 brief delivered" }),
            )])],
        )),
        NoopFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        std::env::temp_dir(),
    )
    .with_pool("default", dispatcher, orchestrator);
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);

    let grant_dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "unused",
            Vec::<CompletionResponse>::new(),
        )),
        DispatchTuning::default(),
        4,
    );
    let daemon = daemon
        .with_dispatcher(
            grant_dispatcher,
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
            Vec::new(),
        )
        .with_goal_hub(hub.clone());

    let sender = daemon.event_sender();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));
    tokio::time::sleep(Duration::from_millis(50)).await;

    sender
        .send(Event::trigger(
            "CronFired",
            "cron:l9-morning",
            "cron:l9-morning:t1",
            EventPayload {
                summary: Some("L9 morning brief goal".into()),
                ..Default::default()
            },
        ))
        .unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for L9 reaction")
        .expect("reaction channel closed");
    assert_eq!(reaction.event.source, "cron:l9-morning");

    let session_id = match reaction.outcome {
        ReactionOutcome::Dispatched { session_id } => session_id,
        other => panic!(
            "L9 requires Dispatched {{ session_id }}, got {}",
            other.label()
        ),
    };

    let live = hub
        .snapshot(&session_id)
        .await
        .expect("L9: Dispatched session_id must be joinable on the hub");
    assert_eq!(
        live.session.goal.domain.as_str(),
        DISPATCH_DOMAIN,
        "reaction sessions run under the dispatch domain pack"
    );
    assert_eq!(live.session.visibility, Visibility::Background);
    assert_eq!(
        live.session.goal.description, "L9 morning brief goal",
        "session must carry the event goal text, not an empty shell"
    );

    let snap = hub
        .await_terminal(&session_id)
        .await
        .expect("L9 session should reach terminal");
    assert_eq!(snap.session.status, SessionStatus::Succeeded);
    assert_eq!(
        snap.session.result.as_ref().unwrap().summary,
        "L9 brief delivered"
    );
    assert_eq!(
        snap.session.result.as_ref().unwrap().terminal,
        TerminalKind::Succeeded
    );

    handle.abort();
}

#[tokio::test]
async fn l9_webhook_event_becomes_joinable_dispatched_session() {
    struct WebhookL9Runtime;
    #[async_trait::async_trait]
    impl ToolRuntime for WebhookL9Runtime {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    struct WebhookL9Factory;
    #[async_trait::async_trait]
    impl RuntimeFactory for WebhookL9Factory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            Ok(Box::new(WebhookL9Runtime))
        }
    }

    let (daemon, _dir) = temp_daemon().await;

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "webhook task".into(),
    };
    let dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&decision).unwrap(),
            )],
        )),
        DispatchTuning::default(),
        4,
    );
    let orchestrator = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "exec",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "webhook task done" }),
            )])],
        )),
        WebhookL9Factory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        std::env::temp_dir(),
    )
    .with_pool("default", dispatcher, orchestrator);
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);

    let grant_dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "unused",
            Vec::<CompletionResponse>::new(),
        )),
        DispatchTuning::default(),
        4,
    );
    let daemon = daemon
        .with_dispatcher(
            grant_dispatcher,
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
            Vec::new(),
        )
        .with_goal_hub(hub.clone());

    let sender = daemon.event_sender();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));
    tokio::time::sleep(Duration::from_millis(50)).await;

    sender
        .send(Event::trigger(
            "WebhookFired",
            "webhook:nightly-backup",
            "webhook:nightly-backup:t1",
            EventPayload {
                summary: Some("back up the vault".into()),
                ..Default::default()
            },
        ))
        .unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for webhook reaction")
        .expect("reaction channel closed");
    assert_eq!(reaction.event.source, "webhook:nightly-backup");

    let session_id = match reaction.outcome {
        ReactionOutcome::Dispatched { session_id } => session_id,
        other => panic!("expected Dispatched for webhook, got {}", other.label()),
    };

    let snap = hub.await_terminal(&session_id).await.expect("terminal");
    let row = &snap.session;

    assert_eq!(row.visibility, Visibility::Background);
    assert_eq!(row.goal.description, "back up the vault");
    assert_eq!(row.status, SessionStatus::Succeeded);
    assert_eq!(
        row.result.as_ref().unwrap().terminal,
        TerminalKind::Succeeded
    );
    assert!(
        row.result.as_ref().unwrap().summary.contains("webhook"),
        "session summary must contain the webhook work: {:?}",
        row.result.as_ref().unwrap().summary
    );
    assert_eq!(
        row.goal.domain.as_str(),
        DISPATCH_DOMAIN,
        "webhook reaction sessions run under the dispatch domain"
    );

    handle.abort();
}

#[tokio::test]
async fn l9_webhook_session_triggers_notifier_deliver_cron() {
    let recorded_calls: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let notifier = Arc::new(RecordingNotifier {
        calls: recorded_calls.clone(),
    });

    let (daemon, _dir) = temp_daemon().await;

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "notify task".into(),
    };
    let dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&decision).unwrap(),
            )],
        )),
        DispatchTuning::default(),
        4,
    );
    let orchestrator = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "exec",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "notified task done" }),
            )])],
        )),
        NoopFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        std::env::temp_dir(),
    )
    .with_pool("default", dispatcher, orchestrator);
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);

    let grant_dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "unused",
            Vec::<CompletionResponse>::new(),
        )),
        DispatchTuning::default(),
        4,
    );
    let daemon = daemon
        .with_dispatcher(
            grant_dispatcher,
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
            Vec::new(),
        )
        .with_goal_hub(hub.clone())
        .with_notifier(notifier);

    let sender = daemon.event_sender();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));
    tokio::time::sleep(Duration::from_millis(50)).await;

    sender
        .send(Event::trigger(
            "CronFired",
            "cron:notify-test",
            "cron:notify-test:t1",
            EventPayload {
                summary: Some("verify notifier delivery".into()),
                ..Default::default()
            },
        ))
        .unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    let session_id = match reaction.outcome {
        ReactionOutcome::Dispatched { session_id } => session_id,
        other => panic!("expected Dispatched, got {}", other.label()),
    };

    let snap = hub.await_terminal(&session_id).await.expect("terminal");
    assert_eq!(
        snap.session.status,
        SessionStatus::Succeeded,
        "notifier-test session should succeed, got {:?}: result={:?}",
        snap.session.status,
        snap.session.result
    );

    tokio::time::sleep(Duration::from_millis(100)).await;

    let recorded = recorded_calls.lock().unwrap();
    assert_eq!(
        recorded.len(),
        1,
        "deliver_cron must be called exactly once, got {:?}",
        *recorded
    );
    assert!(
        recorded[0].contains("notify-test"),
        "notifier message must contain the schedule name, got: {:?}",
        recorded[0]
    );

    handle.abort();
}
