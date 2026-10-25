//! Survivor tests for the face agent's `delegate` bridge (D-e capability strip, report shape).

use super::*;
use liberado_common::{
    Capability, CapabilityCatalog, Delivery, DispatchAction, DispatchDecision, ProposalSigner, Zone,
};
use liberado_dispatch_pack::DispatchPack;
use liberado_executor::{RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL};
use liberado_orchestrator::Orchestrator;
use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};
use liberado_session::{
    DomainPackRunner, GoalResult, GoalSessionHub, GoalSessionStore, PackError, TerminalKind,
};
use std::sync::Arc;

struct NoopFactory;
#[async_trait]
impl RuntimeFactory for NoopFactory {
    async fn runtime_for(
        &self,
        _allowed_mcps: &[String],
        _provenance: liberado_common::WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        unreachable!("the pack's worker never builds a runtime here")
    }
}

/// A hub hosting the dispatch pack whose worker immediately submits a success.
async fn hub_with_succeeding_pack() -> Arc<GoalSessionHub> {
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "test".into(),
    };
    let pack_dispatcher = liberado_dispatcher::Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "pack-dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&decision).unwrap(),
            )],
        )),
        liberado_config_loader::DispatchTuning::default(),
        4,
    );
    let orchestrator = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "pack-exec",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c1",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({
                    "outcome": "succeeded",
                    "summary": "found 3 open tasks",
                }),
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
    .with_pool("default", pack_dispatcher, orchestrator);
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    Arc::new(hub)
}

/// D-e: the delegated session runs **without** AskHuman even when the dispatcher grant carries
/// it — and keeps every non-AskHuman capability. An inverted retain hands the subagent *only*
/// AskHuman (a chat-turn that can block on a human it cannot relay) and strips its real powers.
#[tokio::test]
async fn delegate_strips_askhuman_but_keeps_the_rest_of_the_grant() {
    // `delegate` writes its journal via `LIBERADO_DATA_DIR`; pin it to a tempdir under the
    // shared process-wide lock so nothing lands in the crate directory.
    let _env = crate::dispatch_journal::survivor_tests::data_dir_lock().await;
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("LIBERADO_DATA_DIR", data.path()) };

    let hub = hub_with_succeeding_pack().await;
    let caller = CapabilitySet::from_iter([
        Capability::ExecuteMcp("tasks-mcp".into()),
        Capability::AskHuman,
    ]);
    let bridge = DispatchBridge {
        hub: hub.clone(),
        dispatcher_capabilities: caller.clone(),
    };
    let deferral = AtomicBool::new(false);

    let report = bridge
        .delegate(
            "count my open tasks",
            Some("parent-chat"),
            &deferral,
            &caller,
        )
        .await
        .expect("delegation succeeds");

    // Compact-report contract for the face turn.
    assert!(
        report.starts_with("RESULT (Succeeded):\nfound 3 open tasks"),
        "{report}"
    );
    assert!(report.contains("\n[session: "), "{report}");
    assert!(report.contains("[dispatch journal: "), "{report}");
    assert!(report.contains("chat-delegate-"), "{report}");
    assert!(report.contains("parent chat: parent-chat"), "{report}");
    assert!(!deferral.load(Ordering::Relaxed), "nothing deferred");

    // The hosted session record is the audit trail of what the subagent may do.
    let rows = hub.list().await;
    assert_eq!(rows.len(), 1);
    let grant = &rows[0].grant;
    assert!(
        grant
            .capabilities
            .capabilities
            .iter()
            .any(|c| matches!(c, Capability::ExecuteMcp(m) if m == "tasks-mcp")),
        "non-AskHuman capabilities survive: {:?}",
        grant.capabilities
    );
    assert!(
        !grant.grants_ask_human(),
        "AskHuman must be stripped from a delegated session"
    );
    let origin = rows[0]
        .goal
        .origin
        .as_ref()
        .expect("delegate has an origin");
    assert_eq!(origin.conversation_id.as_deref(), Some("parent-chat"));
    assert!(
        origin
            .correlation_id
            .as_deref()
            .unwrap()
            .starts_with("chat-delegate-"),
        "journal stitching survives"
    );

    unsafe { std::env::remove_var("LIBERADO_DATA_DIR") };
}

/// Succeeds only when the child grant can write the `tasks` zone.
/// A read-only caller must come back refused.
struct WriteProbePack;

#[async_trait]
impl DomainPackRunner for WriteProbePack {
    fn domain_id(&self) -> &str {
        "dispatch"
    }

    async fn run(
        &self,
        _session_id: &str,
        _goal: &liberado_session::GoalSpec,
        ctx: &liberado_session::PackContext<'_>,
        _events: tokio::sync::mpsc::Sender<liberado_session::SessionEvent>,
        _inputs: liberado_session::InputChannel,
        _cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        let write = Capability::Write(Zone::vault("tasks"));
        if ctx.can(&write) {
            Ok(GoalResult {
                terminal: TerminalKind::Succeeded,
                summary: "wrote the task".into(),
                artifacts: Vec::new(),
                diagnostics: serde_json::Value::Null,
            })
        } else {
            Ok(GoalResult {
                terminal: TerminalKind::Failed,
                summary: "refused: write not granted".into(),
                artifacts: Vec::new(),
                diagnostics: serde_json::Value::Null,
            })
        }
    }
}

/// A research (read-only) caller that asks `delegate` to write a task is refused.
/// The child grant is the caller's grant narrowed to the dispatcher ceiling.
#[tokio::test]
async fn a_read_only_caller_cannot_write_through_delegate() {
    let _env = crate::dispatch_journal::survivor_tests::data_dir_lock().await;
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("LIBERADO_DATA_DIR", data.path()) };

    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(WriteProbePack));
    let hub = Arc::new(hub);

    let research = CapabilitySet::from_iter([Capability::Read(Zone::vault("tasks"))]);
    let ceiling = CapabilitySet::from_iter([
        Capability::Read(Zone::vault("tasks")),
        Capability::Write(Zone::vault("tasks")),
        Capability::ExecuteMcp("tasks-mcp".into()),
    ]);
    assert!(
        !research.contains(&Capability::Write(Zone::vault("tasks"))),
        "the research caller cannot write tasks"
    );
    assert!(ceiling.contains(&Capability::Write(Zone::vault("tasks"))));

    let bridge = DispatchBridge {
        hub: hub.clone(),
        dispatcher_capabilities: ceiling,
    };
    let deferral = AtomicBool::new(false);
    let report = bridge
        .delegate(
            "write a task: buy milk",
            Some("research-chat"),
            &deferral,
            &research,
        )
        .await
        .expect("the bridge returns a report");

    assert!(
        report.contains("refused"),
        "a read-only research caller must not write through delegate: {report}"
    );
    assert!(
        !report.contains("wrote the task"),
        "the write must not succeed: {report}"
    );
    let rows = hub.list().await;
    assert_eq!(rows.len(), 1);
    assert!(
        !rows[0]
            .grant
            .capabilities
            .contains(&Capability::Write(Zone::vault("tasks"))),
        "child grant must not include Write: {:?}",
        rows[0].grant.capabilities
    );

    unsafe { std::env::remove_var("LIBERADO_DATA_DIR") };
}
