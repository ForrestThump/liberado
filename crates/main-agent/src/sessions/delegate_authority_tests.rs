//! A delegated worker must not outrank the chat that asked for it.
//!
//! Read-only profiles do not delegate. An explicit delegation still intersects the caller with
//! the dispatcher ceiling.

use super::super::*;
use super::test_fixtures::*;

use liberado_common::{Capability, CapabilityCatalog, CapabilitySet, Zone};
use liberado_dispatch_pack::DispatchPack;
use liberado_orchestrator::Orchestrator;
use liberado_session::{GoalSessionHub, GoalSessionStore};
use std::sync::Arc;

fn read_only_tasks() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Read(Zone::vault("tasks"))])
}

fn dispatcher_ceiling() -> CapabilitySet {
    CapabilitySet::from_iter([
        Capability::Read(Zone::vault("tasks")),
        Capability::Write(Zone::vault("tasks")),
        Capability::ExecuteMcp("tasks-mcp".into()),
        Capability::AskHuman,
    ])
}

/// Hub whose dispatch pack finishes at once, so `delegate` returns during the chat turn.
async fn hub_that_finishes_delegate() -> Arc<GoalSessionHub> {
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "test".into(),
    };
    let pack_dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "pack-dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&decision).unwrap(),
            )],
        )),
        DispatchTuning::default(),
        4,
    );
    let pack_orchestrator = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "pack-exec",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c1",
                liberado_executor::SUBMIT_REPORT_TOOL,
                serde_json::json!({
                    "outcome": "succeeded",
                    "summary": "wrote the task",
                }),
            )])],
        )),
        NoopFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        liberado_common::ProposalSigner::random(),
        "default",
    );
    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        std::env::temp_dir(),
    )
    .with_pool("default", pack_dispatcher, pack_orchestrator);
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    Arc::new(hub)
}

async fn chat_that_tries_to_delegate(
    grant: SessionGrant,
) -> (tempfile::TempDir, ChatSessions, Arc<GoalSessionHub>, Ulid) {
    let dir = tempfile::tempdir().unwrap();
    let hub = hub_that_finishes_delegate().await;
    let chat_provider = Arc::new(MockProvider::with_script(
        "chat",
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "d1",
                crate::DELEGATE_TOOL_NAME,
                serde_json::json!({ "goal": "write a task: buy milk" }),
            )]),
            CompletionResponse::text("I could not write that."),
        ],
    ));
    let sessions = ChatSessions::new(
        Arc::new(SessionStore::open(dir.path()).await),
        Executor::new(chat_provider, liberado_executor::Budget::default()),
        Arc::new(no_tools_runtime()),
    )
    .with_delegation_mode(true)
    .with_goal_hub(hub.clone())
    .with_dispatcher_capabilities(dispatcher_ceiling())
    .with_dispatch(
        Dispatcher::new(
            Arc::new(MockProvider::with_script(
                "unused",
                Vec::<CompletionResponse>::new(),
            )),
            DispatchTuning::default(),
            4,
        ),
        Arc::new(CapabilityCatalog::new()),
    );
    let id = sessions.create_with_grant(None, grant).await.unwrap();
    (dir, sessions, hub, id)
}

fn research_grant(delegation: Option<bool>) -> SessionGrant {
    SessionGrant {
        capabilities: read_only_tasks(),
        profile: Some("researcher".into()),
        delegation,
        ..Default::default()
    }
}

/// A read-only `research` session must not start a worker that can write.
///
/// On the unfixed tree the session inherits daemon delegation, `delegate` runs, and the child
/// receives the dispatcher ceiling (including `Write`).
#[tokio::test]
async fn research_session_delegate_to_write_a_task_is_refused() {
    let (_dir, sessions, hub, id) = chat_that_tries_to_delegate(research_grant(None)).await;
    sessions
        .turn(id, "write a task: buy milk")
        .await
        .expect("the turn finishes; refusal is not a crashed turn");

    let rows = hub.list().await;
    let grants: Vec<String> = rows
        .iter()
        .map(|row| format!("{:?}", row.grant.capabilities))
        .collect();
    assert!(
        rows.is_empty(),
        "a read-only research session must not start a worker; child grants={grants:?}"
    );
}

/// Explicit delegation still cannot widen the caller. The child keeps only the intersection.
#[tokio::test]
async fn delegated_worker_grant_is_caller_intersect_dispatcher() {
    let (_dir, sessions, hub, id) = chat_that_tries_to_delegate(research_grant(Some(true))).await;
    sessions
        .turn(id, "write a task: buy milk")
        .await
        .expect("delegation runs inside the turn");

    let rows = hub.list().await;
    assert_eq!(rows.len(), 1, "an explicit delegate starts one worker");
    let caps = &rows[0].grant.capabilities;
    assert!(
        caps.capabilities
            .iter()
            .any(|c| matches!(c, Capability::Read(z) if z.name() == "tasks")),
        "the caller's read survives: {caps:?}"
    );
    assert!(
        !caps
            .capabilities
            .iter()
            .any(|c| matches!(c, Capability::Write(_))),
        "the child must not gain a write the caller lacks: {caps:?}"
    );
    assert!(
        !caps.grants_ask_human(),
        "AskHuman stays stripped from a delegated worker: {caps:?}"
    );
}
