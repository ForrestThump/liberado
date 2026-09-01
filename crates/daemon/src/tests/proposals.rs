//! Tests for proposal emission, zone restriction downgrades, execution, ledger and signature verification.

use super::super::*;
use super::test_fixtures::*;
use chrono::{Duration as ChronoDuration, Utc};
use liberado_common::{
    ApprovalDecision, ApprovalLedger, Capability, CapabilityCatalog, CapabilitySet, Consequence,
    DEFAULT_POOL, Delivery, DispatchAction, DispatchDecision, GrantScope, McpDescriptor, Outcome,
    Proposal, ProposalSigner, ProposalStatus, ProposedAction, Report, RiskWaiverSet, ToolCall,
    WriteClass, WriteProvenance, session_grants,
};
use liberado_config_loader::DispatchTuning;
use liberado_dispatcher::Dispatcher;
use liberado_executor::{SUBMIT_REPORT_TOOL, ToolRuntime};
use liberado_orchestrator::{Disposition, EXPIRED_PROPOSAL_REFUSAL_SUMMARY, Orchestrator};
use liberado_provider::{
    AgentRole, CompletionResponse, LatencyEvent, LatencyRecorder, MeteredProvider, MockProvider,
    Provider, ToolInvocation,
};
use liberado_session::{GoalSessionHub, GoalSessionStore};
use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;

#[tokio::test]
async fn daemon_emits_a_proposal_for_a_high_consequence_action() {
    let (daemon, dir) = temp_daemon().await;

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: vec![ToolCall {
                tool: "email:send".into(),
                args: serde_json::json!({ "to": "boss@example.com" }),
            }],
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "send the requested email".into(),
    };
    let dispatch_provider = Arc::new(MockProvider::with_script(
        "dispatch",
        [CompletionResponse::text(
            serde_json::to_string(&decision).unwrap(),
        )],
    ));
    let dispatcher = Dispatcher::new(dispatch_provider, DispatchTuning::default(), 4);

    let catalog = CapabilityCatalog::new();
    catalog.register(McpDescriptor {
        name: "email".into(),
        description: "send email".into(),
        consequence: Consequence::External,
        provenance: None,
        ..Default::default()
    });
    let capabilities = CapabilitySet::from_iter([Capability::ExecuteMcp("email".into())]);
    let orchestrator = Orchestrator::new(
        Arc::new(MockProvider::with_script("exec", Vec::new())),
        UnusedFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_dispatcher(dispatcher, Arc::new(catalog), capabilities, Vec::new())
        .with_orchestrator(orchestrator);

    let vault_dir = dir.path().to_path_buf();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::create_dir_all(vault_dir.join("inbox")).unwrap();
    std::fs::write(vault_dir.join("inbox/email-me.md"), "please email the boss").unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out")
        .expect("channel closed");

    let ReactionOutcome::Acted(Disposition::Propose(proposal)) = reaction.outcome else {
        panic!("expected Acted/Propose, got {}", reaction.outcome.label());
    };

    let proposals_dir = vault_dir.join("proposals");
    let entries: Vec<_> = std::fs::read_dir(&proposals_dir)
        .expect("proposals/ should exist")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    assert_eq!(entries.len(), 1, "exactly one proposal note");
    let contents = std::fs::read_to_string(&entries[0]).unwrap();
    let parsed = Proposal::from_note(&contents).expect("proposal note round-trips");
    assert_eq!(&parsed, proposal.as_proposal());
    assert_eq!(parsed.status, ProposalStatus::Pending);
    match parsed.proposed_action {
        ProposedAction::ToolCalls(calls) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].tool, "email:send");
        }
        other => panic!("expected ToolCalls, got {other:?}"),
    }

    handle.abort();
}

#[tokio::test]
async fn daemon_downgrades_a_zone_restricted_seed_call_to_a_proposal() {
    let (daemon, dir) = temp_daemon().await;

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: vec![ToolCall {
                tool: "vault-mcp:write_note".into(),
                args: serde_json::json!({ "path": "reviews/q1.md" }),
            }],
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "file the review note".into(),
    };
    let dispatch_provider = Arc::new(MockProvider::with_script(
        "dispatch",
        [CompletionResponse::text(
            serde_json::to_string(&decision).unwrap(),
        )],
    ));
    let dispatcher = Dispatcher::new(dispatch_provider, DispatchTuning::default(), 4);

    let catalog = CapabilityCatalog::new();
    catalog.register(McpDescriptor {
        name: "vault-mcp".into(),
        description: "vault note writer".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: Some("reviews".into()),
        tool_zones: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
    });
    let capabilities = CapabilitySet::from_iter([Capability::ExecuteMcp("vault-mcp".into())]);
    let zone_write_classes = vec![("reviews".to_string(), WriteClass::ProposalOnly)];
    let orchestrator = Orchestrator::new(
        Arc::new(MockProvider::with_script("exec", Vec::new())),
        UnusedFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_dispatcher(
            dispatcher,
            Arc::new(catalog),
            capabilities,
            zone_write_classes,
        )
        .with_orchestrator(orchestrator);

    let vault_dir = dir.path().to_path_buf();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::create_dir_all(vault_dir.join("inbox")).unwrap();
    std::fs::write(
        vault_dir.join("inbox/review-me.md"),
        "please file this review",
    )
    .unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out")
        .expect("channel closed");

    let ReactionOutcome::Acted(Disposition::Propose(proposal)) = reaction.outcome else {
        panic!("expected Acted/Propose, got {}", reaction.outcome.label());
    };

    let proposals_dir = vault_dir.join("proposals");
    let entries: Vec<_> = std::fs::read_dir(&proposals_dir)
        .expect("proposals/ should exist")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    assert_eq!(entries.len(), 1, "exactly one proposal note");
    let contents = std::fs::read_to_string(&entries[0]).unwrap();
    let parsed = Proposal::from_note(&contents).expect("proposal note round-trips");
    assert_eq!(&parsed, proposal.as_proposal());
    assert_eq!(parsed.status, ProposalStatus::Pending);
    match parsed.proposed_action {
        ProposedAction::ToolCalls(calls) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].tool, "vault-mcp:write_note");
        }
        other => panic!("expected ToolCalls, got {other:?}"),
    }

    handle.abort();
}

#[tokio::test]
async fn daemon_executes_an_approved_proposal() {
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            Vec::<CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_orchestrator(orch)
        .with_proposal_signer(signer.clone());

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let (tx, mut rx) = unbounded_channel();
    approve_in(&dir, "vault-change:test-proposal:abc").await;
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;

    let proposal = Proposal::pending(
        "vault-change:test-proposal:abc",
        "vault-change:test-proposal:abc",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "test task" }),
        }]),
        "a test proposal",
    )
    .with_requested_grant(Capability::Write(liberado_common::Zone::vault("tasks")));
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    std::fs::write(proposals_dir.join("approved.md"), proposal.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    {
        let recorded = invoked.lock().unwrap();
        assert_eq!(
            recorded.len(),
            1,
            "approved proposal must execute the tool call"
        );
        assert_eq!(recorded[0].name, "tasks:create");
    }

    let archived = proposals_dir.join("archive/approved/approved.md");
    let mut contents = None;
    for _ in 0..50 {
        if archived.exists() {
            contents = Some(std::fs::read_to_string(&archived).unwrap());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let contents = contents.expect("approved proposal must be archived under archive/approved/");
    assert!(
        !proposals_dir.join("approved.md").exists(),
        "archived proposal must be removed from the active proposals dir"
    );
    let parsed = Proposal::from_note(&contents).unwrap();
    assert_eq!(parsed.status, ProposalStatus::Done);

    handle.abort();
}

#[tokio::test]
async fn daemon_hub_proposal_lifecycle_applies_grant() {
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "proposal lifecycle" }),
            )])],
        )),
        NoopFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(liberado_session::LifeOpsDemoRunner));
    let hub = Arc::new(hub);

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_orchestrator(orch)
        .with_proposal_signer(signer.clone())
        .with_goal_hub(hub.clone());

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let (tx, mut rx) = unbounded_channel();
    approve_in(&dir, "vault-change:lifecycle:1").await;

    let handle = tokio::spawn(daemon.run(tx));
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut proposal = Proposal::pending(
        "vault-change:lifecycle:1",
        "vault-change:lifecycle:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "lifecycle test" }),
        }]),
        "proposal lifecycle test",
    )
    .with_requested_grant(Capability::Write(liberado_common::Zone::vault("lifecycle")));
    proposal.pool = Some(DEFAULT_POOL.to_string());
    proposal.approved_scope = Some(GrantScope::Session);
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    std::fs::write(proposals_dir.join("lifecycle.md"), proposal.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    let grant = session_grants::session_grant(DEFAULT_POOL);
    assert!(
        !grant.capabilities.is_empty(),
        "hub lifecycle: grant must be non-empty, got {grant:?}"
    );
    assert!(
        grant.contains(&Capability::Write(liberado_common::Zone::vault(
            "lifecycle"
        ))),
        "hub lifecycle: grant must include Write(vault/\"lifecycle\"): {grant:?}"
    );

    let archived = proposals_dir.join("archive/approved/lifecycle.md");
    let mut found = false;
    for _ in 0..50 {
        if archived.exists() {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(found, "proposal must be archived at {archived:?}");
    assert!(
        !proposals_dir.join("lifecycle.md").exists(),
        "archived proposal must be removed from active proposals dir"
    );
    let archived_proposal =
        Proposal::from_note(&std::fs::read_to_string(&archived).unwrap()).unwrap();
    assert_eq!(archived_proposal.status, ProposalStatus::Done);

    handle.abort();
}

#[tokio::test]
async fn daemon_archives_a_rejected_proposal() {
    let signer = ProposalSigner::random();
    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_proposal_signer(signer.clone());

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;

    let proposal = Proposal::pending(
        "vault-change:rejected-proposal:xyz",
        "vault-change:rejected-proposal:xyz",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "denied task" }),
        }]),
        "a rejected proposal",
    );
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Rejected);
    std::fs::write(proposals_dir.join("rejected.md"), proposal.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    let archived = proposals_dir.join("archive/rejected/rejected.md");
    let mut contents = None;
    for _ in 0..50 {
        if archived.exists() {
            contents = Some(std::fs::read_to_string(&archived).unwrap());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let contents = contents.expect("rejected proposal must be archived under archive/rejected/");
    assert!(
        !proposals_dir.join("rejected.md").exists(),
        "archived proposal must be removed from the active proposals dir"
    );
    assert_eq!(
        Proposal::from_note(&contents).unwrap().status,
        ProposalStatus::Rejected
    );

    handle.abort();
}

#[tokio::test]
async fn handle_proposal_change_expires_and_archives_past_deadline_pending() {
    let signer = ProposalSigner::random();
    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon.with_proposal_signer(signer.clone());

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let mut proposal = Proposal::pending(
        "vault-change:stale-pending:1",
        "vault-change:stale-pending:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "too late" }),
        }]),
        "expired pending proposal",
    );
    proposal.expires = Some(Utc::now() - ChronoDuration::hours(1));
    let proposal = signer.sign(proposal);
    let rel = Path::new("proposals/stale-pending.md");
    let prov = WriteProvenance::agent("test", "c1");
    daemon
        .vault
        .write(rel, &proposal.to_note(), None, &prov)
        .await
        .unwrap();

    let outcome = daemon.handle_proposal_change(rel).await.unwrap();
    assert!(
        matches!(outcome, ReactionOutcome::Observed),
        "past-deadline notes are observed, never executed"
    );

    assert!(
        daemon.vault.read(rel).await.is_err(),
        "expired proposal must leave the active proposals/ dir"
    );
    let archived = daemon
        .vault
        .read("proposals/archive/expired/stale-pending.md")
        .await
        .expect("must land under archive/expired/");
    assert_eq!(
        Proposal::from_note(&archived).unwrap().status,
        ProposalStatus::Expired
    );
}

#[test]
fn expired_refusal_matches_exact_orchestrator_summary_only() {
    assert_eq!(
        EXPIRED_PROPOSAL_REFUSAL_SUMMARY,
        "proposal expired — not executed"
    );
    assert!(
        "subagent said the lease expired mid-run".contains("expired")
            && "subagent said the lease expired mid-run" != EXPIRED_PROPOSAL_REFUSAL_SUMMARY
    );
}

#[tokio::test]
async fn handle_proposal_change_expired_refuse_does_not_apply_session_grant() {
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            Vec::<CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let pool = format!("grant-expire-test-{:?}", std::thread::current().id());
    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_orchestrator(orch)
        .with_proposal_signer(signer.clone());

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let mut proposal = Proposal::pending(
        "vault-change:expired-grant:1",
        "vault-change:expired-grant:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "no" }),
        }]),
        "permission request past deadline",
    )
    .with_requested_grant(Capability::Write(liberado_common::Zone::vault("sandbox")));
    proposal.pool = Some(pool.clone());
    proposal.expires = Some(Utc::now() - ChronoDuration::hours(1));
    proposal.approved_scope = Some(GrantScope::Session);
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);

    let rel = Path::new("proposals/expired-grant.md");
    let prov = WriteProvenance::agent("test", "c1");
    daemon
        .vault
        .write(rel, &proposal.to_note(), None, &prov)
        .await
        .unwrap();

    let outcome = daemon.handle_proposal_change(rel).await.unwrap();
    assert!(matches!(outcome, ReactionOutcome::Observed));
    assert!(
        invoked.lock().unwrap().is_empty(),
        "tools must not run on expired refuse"
    );
    assert!(
        session_grants::session_grant(&pool).capabilities.is_empty(),
        "session grant must not persist when execute was refused as expired"
    );
    assert!(daemon.vault.read(rel).await.is_err());
    let archived = daemon
        .vault
        .read("proposals/archive/expired/expired-grant.md")
        .await
        .expect("must archive as expired");
    assert_eq!(
        Proposal::from_note(&archived).unwrap().status,
        ProposalStatus::Expired
    );
}

#[tokio::test]
async fn complete_refusal_lifecycle_archives_only_on_exact_expired_refusal() {
    let (daemon, dir) = temp_daemon().await;
    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let prov = WriteProvenance::agent("test", "c1");

    fn refusal_report(outcome: Outcome, summary: &str) -> Report {
        Report {
            outcome,
            summary: summary.to_string(),
            artifacts: Vec::new(),
            new_high_signal_facts: Vec::new(),
            deferred_to_human: false,
            follow_up: None,
            repeat_calls: 0,
        }
    }

    async fn case(
        daemon: &crate::Daemon,
        prov: &WriteProvenance,
        id: &str,
        outcome: Outcome,
        summary: &str,
        expect_archive_as_expired: bool,
    ) {
        let base = Proposal::pending(
            id,
            id,
            "test",
            ProposedAction::ToolCalls(vec![ToolCall {
                tool: "tasks:create".into(),
                args: serde_json::json!({ "summary": "x" }),
            }]),
            "a proposal",
        );
        let mut signed = ProposalSigner::random().sign(base);
        signed.set_status(ProposalStatus::Approved);
        let mut proposal = signed.into_proposal();

        let rel = Path::new("proposals").join(format!("{id}.md"));
        daemon
            .vault
            .write(&rel, &proposal.to_note(), None, prov)
            .await
            .unwrap();
        let archive_rel = format!("proposals/archive/expired/{id}.md");

        let result = daemon
            .complete_refusal_lifecycle(&rel, &mut proposal, &refusal_report(outcome, summary))
            .await;

        if expect_archive_as_expired {
            assert!(
                matches!(result, Some(crate::ReactionOutcome::Observed)),
                "{id}: exact expired refusal must complete the lifecycle"
            );
            assert!(
                daemon.vault.read(&rel).await.is_err(),
                "{id}: original must be moved"
            );
            let archived = daemon
                .vault
                .read(&archive_rel)
                .await
                .unwrap_or_else(|_| panic!("{id}: must archive as expired"));
            assert_eq!(
                Proposal::from_note(&archived).unwrap().status,
                ProposalStatus::Expired,
                "{id}: archived proposal must carry Expired status"
            );
        } else {
            assert!(
                result.is_none(),
                "{id}: report must not complete the expiry lifecycle"
            );
            assert!(
                daemon.vault.read(&rel).await.is_ok(),
                "{id}: note must be left in place"
            );
            assert!(
                daemon.vault.read(&archive_rel).await.is_err(),
                "{id}: must not archive"
            );
        }
    }

    case(
        &daemon,
        &prov,
        "expire-lifecycle-a",
        Outcome::Failed,
        EXPIRED_PROPOSAL_REFUSAL_SUMMARY,
        true,
    )
    .await;

    case(
        &daemon,
        &prov,
        "expire-lifecycle-b",
        Outcome::Failed,
        "some other failure",
        false,
    )
    .await;

    case(
        &daemon,
        &prov,
        "expire-lifecycle-c",
        Outcome::Succeeded,
        EXPIRED_PROPOSAL_REFUSAL_SUMMARY,
        false,
    )
    .await;
}

#[tokio::test]
async fn handle_proposal_change_does_not_execute_approved_past_deadline() {
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            Vec::<CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_orchestrator(orch)
        .with_proposal_signer(signer.clone());

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let mut proposal = Proposal::pending(
        "vault-change:late-approve:1",
        "vault-change:late-approve:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "should never run" }),
        }]),
        "approved after expiry",
    );
    proposal.expires = Some(Utc::now() - ChronoDuration::hours(1));
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    let rel = Path::new("proposals/late-approve.md");
    let prov = WriteProvenance::agent("test", "c1");
    daemon
        .vault
        .write(rel, &proposal.to_note(), None, &prov)
        .await
        .unwrap();

    let outcome = daemon.handle_proposal_change(rel).await.unwrap();
    assert!(matches!(outcome, ReactionOutcome::Observed));

    assert!(
        invoked.lock().unwrap().is_empty(),
        "past-deadline approved proposal must never invoke tools"
    );
    assert!(daemon.vault.read(rel).await.is_err());
    let archived = daemon
        .vault
        .read("proposals/archive/expired/late-approve.md")
        .await
        .expect("late approve must archive as expired, not approved");
    assert_eq!(
        Proposal::from_note(&archived).unwrap().status,
        ProposalStatus::Expired
    );
}

#[tokio::test]
async fn approved_subagent_execution_is_attributed_to_the_proposal_correlation() {
    #[derive(Default)]
    struct CapturingRecorder {
        events: std::sync::Mutex<Vec<LatencyEvent>>,
    }
    impl LatencyRecorder for CapturingRecorder {
        fn record(&self, event: LatencyEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    const CORRELATION: &str = "vault-change:Learning/Attribution.md:deadbeef";

    let rec = Arc::new(CapturingRecorder::default());
    let inner: Arc<dyn Provider> = Arc::new(MockProvider::with_script(
        "mock",
        (0..24).map(|i| CompletionResponse::text(format!("step {i}"))),
    ));
    let metered_orchestrator = MeteredProvider::wrap(
        inner.clone(),
        AgentRole::Orchestrator,
        rec.clone() as Arc<dyn LatencyRecorder>,
    );
    let metered_subagent = MeteredProvider::wrap(
        inner,
        AgentRole::Subagent,
        rec.clone() as Arc<dyn LatencyRecorder>,
    );

    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        metered_orchestrator,
        InvocationRecordingFactory {
            runtime: InvocationRecordingRuntime::default(),
        },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    )
    .with_subagent_provider(metered_subagent);

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_orchestrator(orch)
        .with_proposal_signer(signer.clone());
    std::fs::create_dir_all(dir.path().join("proposals")).unwrap();
    approve_in(&dir, CORRELATION).await;

    let proposal = Proposal::pending(
        CORRELATION,
        CORRELATION,
        "test",
        ProposedAction::Subagent {
            goal: "summarise the note".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: Vec::new(),
            success_criteria: Vec::new(),
        },
        "an approved subagent",
    );
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    let rel = Path::new("proposals/attributed.md");
    let prov = WriteProvenance::agent("test", CORRELATION);
    daemon
        .vault
        .write(rel, &proposal.to_note(), None, &prov)
        .await
        .unwrap();

    daemon.handle_proposal_change(rel).await.unwrap();

    let events = rec.events.lock().unwrap();
    assert!(
        !events.is_empty(),
        "an approved subagent must reach the provider — otherwise this test proves nothing"
    );
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(
            ev.correlation, CORRELATION,
            "event[{i}] must carry the proposal's correlation, not \"-\""
        );
        assert_eq!(
            ev.role, "subagent",
            "event[{i}] is delegated subagent work and must not merge into the orchestrator role"
        );
    }
}

#[tokio::test]
async fn daemon_does_not_execute_a_pending_proposal() {
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let orch = Orchestrator::new(
        std::sync::Arc::new(MockProvider::with_script(
            "mock",
            Vec::<CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_orchestrator(orch);

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;

    let proposal = Proposal::pending(
        "pending-test",
        "pending-correlation",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "should-not-run" }),
        }]),
        "a pending proposal",
    );
    std::fs::write(proposals_dir.join("pending-test.md"), proposal.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    let recorded = invoked.lock().unwrap();
    assert!(
        recorded.is_empty(),
        "pending proposal must NOT invoke any tool"
    );

    let contents = std::fs::read_to_string(proposals_dir.join("pending-test.md")).unwrap();
    let parsed = Proposal::from_note(&contents).unwrap();
    assert_eq!(parsed.status, ProposalStatus::Pending);

    handle.abort();
}

#[tokio::test]
async fn daemon_rejects_an_approved_proposal_with_a_bad_integrity_signature() {
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let daemon_signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            Vec::<CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        daemon_signer.clone(),
        "default",
    );

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_orchestrator(orch)
        .with_proposal_signer(daemon_signer);

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;

    let forging_signer = ProposalSigner::random();
    let proposal = Proposal::pending(
        "forged-test",
        "forged-correlation",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "should-not-run" }),
        }]),
        "a forged approval",
    );
    let mut proposal = forging_signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    std::fs::write(proposals_dir.join("forged.md"), proposal.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    assert!(
        invoked.lock().unwrap().is_empty(),
        "a proposal with a bad integrity signature must NOT invoke any tool, even though \
             status is approved"
    );

    let contents = std::fs::read_to_string(proposals_dir.join("forged.md")).unwrap();
    let parsed = Proposal::from_note(&contents).unwrap();
    assert_eq!(
        parsed.status,
        ProposalStatus::Approved,
        "a rejected-for-integrity proposal must not be marked Done"
    );

    handle.abort();
}

#[tokio::test]
async fn forged_proposal_with_a_ledger_approval_still_does_not_execute() {
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let orch_signer = ProposalSigner::random();
    let daemon_signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            Vec::<CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        orch_signer.clone(),
        "default",
    );

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_orchestrator(orch)
        .with_proposal_signer(daemon_signer)
        .with_approval_ledger(test_ledger(&dir));

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    approve_in(&dir, "forged-but-approved:1").await;

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;

    let proposal = Proposal::pending(
        "forged-but-approved:1",
        "forged-but-approved:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "should-not-run" }),
        }]),
        "a forged but ledger-approved proposal",
    )
    .with_requested_grant(Capability::Write(liberado_common::Zone::vault("tasks")));
    let mut proposal = orch_signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    std::fs::write(proposals_dir.join("forged-approved.md"), proposal.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    assert!(
        invoked.lock().unwrap().is_empty(),
        "a forged proposal must not run even when the ledger says Approved; \
         a mutant that drops reject_if_tampered would let the tool call through"
    );

    handle.abort();
}

#[tokio::test]
async fn approved_proposal_without_a_ledger_does_not_execute() {
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            Vec::<CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_orchestrator(orch)
        .with_proposal_signer(signer.clone());

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;

    let proposal = Proposal::pending(
        "no-ledger:1",
        "no-ledger:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "should-not-run" }),
        }]),
        "an approved but un-ledgered proposal",
    )
    .with_requested_grant(Capability::Write(liberado_common::Zone::vault("tasks")));
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    std::fs::write(proposals_dir.join("no-ledger.md"), proposal.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    assert!(
        invoked.lock().unwrap().is_empty(),
        "a proposal claiming Approved without a ledger entry must not execute; \
         a mutant that drops refuse_without_ledger_approval would let it through"
    );

    handle.abort();
}

#[tokio::test]
async fn rejected_proposal_in_ledger_does_not_execute() {
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            Vec::<CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_orchestrator(orch)
        .with_proposal_signer(signer.clone())
        .with_approval_ledger(test_ledger(&dir));

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    test_ledger(&dir)
        .record("rejected-by-human:1", ApprovalDecision::Rejected, "test")
        .await
        .unwrap();

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;

    let proposal = Proposal::pending(
        "rejected-by-human:1",
        "rejected-by-human:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "should-not-run" }),
        }]),
        "an approved note but rejected by human",
    )
    .with_requested_grant(Capability::Write(liberado_common::Zone::vault("tasks")));
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    std::fs::write(proposals_dir.join("rejected.md"), proposal.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    assert!(
        invoked.lock().unwrap().is_empty(),
        "a proposal whose ledger says Rejected must not execute even if the note says Approved; \
         a mutant that drops refuse_without_ledger_approval would let it through"
    );

    handle.abort();
}

#[tokio::test]
async fn runtime_gated_downgrade_lands_in_the_vault_and_executes_once_approved() {
    let (daemon, dir) = temp_daemon().await;
    let vault_path = dir.path().to_path_buf();
    let signer = ProposalSigner::random();

    let inner: Arc<dyn ToolRuntime> = Arc::new(InvocationRecordingRuntime::default());
    let gated = liberado_executor::RiskGatedToolRuntime::new(
        inner,
        CapabilitySet::from_iter([Capability::ExecuteMcp("dangerous-mcp".into())]),
        vec![("dangerous-mcp".into(), Consequence::External)],
        Vec::new(),
        Vec::new(),
        vault_path.clone(),
        "clean up the vault".into(),
        "runtime-gate-test".into(),
        signer.clone(),
        "default",
    );
    let call = ToolInvocation::new(
        "c1",
        "dangerous-mcp:wipe",
        serde_json::json!({ "path": "everything" }),
    );
    let downgrade_msg = gated.invoke(&call).await.expect("downgrade is Ok, not Err");
    assert!(downgrade_msg.contains("PROPOSAL CREATED"));

    let proposals_dir = vault_path.join("proposals");
    let mut entries: Vec<_> = std::fs::read_dir(&proposals_dir)
        .expect("the runtime-level downgrade must have created the vault's proposals/ dir")
        .filter_map(Result::ok)
        .collect();
    assert_eq!(entries.len(), 1, "exactly one proposal file should exist");
    let proposal_path = entries.remove(0).path();
    let written = Proposal::from_note(&std::fs::read_to_string(&proposal_path).unwrap())
        .expect("proposal note round-trips");
    assert_eq!(written.status, ProposalStatus::Pending);
    assert!(signer.verify(&written), "the downgrade must be signed");

    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            Vec::<CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_orchestrator(orch)
        .with_proposal_signer(signer);

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));
    tokio::time::sleep(Duration::from_millis(300)).await;

    approve_in(&dir, &written.id).await;
    let mut approved = written;
    approved.status = ProposalStatus::Approved;
    std::fs::write(&proposal_path, approved.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    let recorded = invoked.lock().unwrap();
    assert_eq!(
        recorded.len(),
        1,
        "approving the runtime-gated proposal must actually execute it"
    );
    assert_eq!(recorded[0].name, "dangerous-mcp:wipe");

    handle.abort();
}

#[tokio::test]
async fn handle_proposal_change_active_failed_not_expired_does_not_enter_expiry_path() {
    let signer = ProposalSigner::random();
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c1",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "failed", "summary": "something went wrong" }),
            )])],
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_orchestrator(orch)
        .with_proposal_signer(signer.clone());

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let mut proposal = Proposal::pending(
        "vault-change:gen-fail:1",
        "vault-change:gen-fail:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "test" }),
        }]),
        "generic failure test",
    )
    .with_requested_grant(Capability::Write(liberado_common::Zone::vault("sandbox")));
    proposal.pool = Some(DEFAULT_POOL.to_string());
    proposal.expires = Some(Utc::now() + ChronoDuration::hours(1));
    proposal.approved_scope = Some(GrantScope::Session);
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);

    let rel = Path::new("proposals/gen-fail.md");
    let prov = WriteProvenance::agent("test", "c1");
    daemon
        .vault
        .write(rel, &proposal.to_note(), None, &prov)
        .await
        .unwrap();

    approve_in(&dir, "vault-change:gen-fail:1").await;

    let _outcome = daemon.handle_proposal_change(rel).await.unwrap();
    assert!(
        !invoked.lock().unwrap().is_empty(),
        "tools should have run — the orchestrator returned Failed but not with the expiry refusal"
    );
    let grant = session_grants::session_grant(DEFAULT_POOL);
    assert!(
        !grant.capabilities.is_empty(),
        "session grant must be applied when the failed outcome is not the expiry refusal"
    );
}

#[tokio::test]
async fn an_approved_note_without_a_recorded_decision_does_not_execute() {
    let signer = ProposalSigner::random();
    let (daemon, dir) = temp_daemon().await;
    let ledger_dir = tempfile::tempdir().unwrap();
    let ledger = ApprovalLedger::new(ledger_dir.path());
    let daemon = daemon
        .with_proposal_signer(signer.clone())
        .with_approval_ledger(ledger.clone());

    std::fs::create_dir_all(dir.path().join("proposals")).unwrap();

    let proposal = Proposal::pending(
        "prop-forged",
        "corr-forged",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "not authorised" }),
        }]),
        "an agent flipped this",
    );
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    assert!(
        daemon.signer.verify(&proposal),
        "the forged note must pass integrity — that is precisely why it needed a second check"
    );

    let rel = Path::new("proposals/prop-forged.md");
    daemon
        .vault
        .write(
            rel,
            &proposal.to_note(),
            None,
            &WriteProvenance::agent("test", "c1"),
        )
        .await
        .unwrap();

    assert!(
        matches!(
            daemon.handle_proposal_change(rel).await.unwrap(),
            ReactionOutcome::Observed
        ),
        "an approved-looking note with no human decision behind it must not execute"
    );

    ledger
        .record("prop-forged", ApprovalDecision::Approved, "telegram")
        .await
        .unwrap();
    assert_eq!(
        ledger.decision_for("prop-forged").await,
        Some(ApprovalDecision::Approved),
        "the recorded decision is what the daemon consults"
    );
}

#[tokio::test]
async fn a_daemon_with_no_ledger_executes_nothing() {
    let signer = ProposalSigner::random();
    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon.with_proposal_signer(signer.clone());
    std::fs::create_dir_all(dir.path().join("proposals")).unwrap();

    let proposal = Proposal::pending(
        "prop-noledger",
        "corr-noledger",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "x" }),
        }]),
        "no ledger attached",
    );
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);

    let rel = Path::new("proposals/prop-noledger.md");
    daemon
        .vault
        .write(rel, &proposal.to_note(), None, &WriteProvenance::human())
        .await
        .unwrap();

    assert!(
        matches!(
            daemon.handle_proposal_change(rel).await.unwrap(),
            ReactionOutcome::Observed
        ),
        "no ledger means no authority to execute under"
    );
}

#[tokio::test]
async fn a_permission_request_also_needs_a_recorded_decision() {
    let signer = ProposalSigner::random();
    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon.with_proposal_signer(signer.clone());
    std::fs::create_dir_all(dir.path().join("proposals")).unwrap();

    let proposal = Proposal::pending(
        "perm-gated",
        "corr-perm",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "needs permission" }),
        }]),
        "a permission request",
    )
    .with_requested_grant(Capability::Write(liberado_common::Zone::vault("sandbox")));
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);

    let rel = Path::new("proposals/perm-gated.md");
    daemon
        .vault
        .write(rel, &proposal.to_note(), None, &WriteProvenance::human())
        .await
        .unwrap();

    assert!(
        matches!(
            daemon.handle_proposal_change(rel).await.unwrap(),
            ReactionOutcome::Observed
        ),
        "an approved permission note with no recorded decision must not run the blocked call"
    );

    approve_in(&dir, "perm-gated").await;
    assert_eq!(
        test_ledger(&dir).decision_for("perm-gated").await,
        Some(ApprovalDecision::Approved),
    );
}

#[tokio::test]
async fn notify_executed_sends_a_notification() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let notifier = Arc::new(SpyNotifier {
        calls: calls.clone(),
    });
    let (daemon, _dir) = temp_daemon().await;
    let daemon = daemon.with_notifier(notifier);

    let proposal = Proposal::pending(
        "notify:1",
        "notify:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "did a thing" }),
        }]),
        "the human's proposal ran",
    );
    let mut signed = ProposalSigner::random().sign(proposal);
    signed.set_status(liberado_common::ProposalStatus::Approved);
    let proposal = signed.into_proposal();

    let report = Report {
        outcome: Outcome::Succeeded,
        summary: "ran successfully".into(),
        artifacts: Vec::new(),
        new_high_signal_facts: Vec::new(),
        deferred_to_human: false,
        follow_up: None,
        repeat_calls: 0,
    };

    daemon.notify_executed(&proposal, &report).await;

    let logged = calls.lock().unwrap();
    assert!(
        !logged.is_empty(),
        "notify_executed must send a notification to the human"
    );
    assert!(
        logged[0].contains("proposal executed"),
        "notification must report that the proposal executed"
    );
}
