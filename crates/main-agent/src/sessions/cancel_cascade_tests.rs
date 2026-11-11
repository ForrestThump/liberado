//! Cancelling a chat turn stops the work that turn started.
//!
//! `delegate` blocks the turn on a hub session. Aborting the turn used to drop the
//! `await_terminal` future and nothing else, leaving the child running with its own grant.

use super::super::*;
use super::test_fixtures::*;

use liberado_session::{
    DomainPackRunner, GoalResult, GoalSessionHub, GoalSessionStore, PackError, SessionStatus,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Within what the done criterion allows. A pack that honors its cancel token is woken by the
/// watch channel, so the real wait is a scheduler hop; the budget is for a loaded CI box.
const CANCEL_BUDGET: Duration = Duration::from_secs(2);

/// A worker that never finishes on its own. It ends only when its cancel token fires — which is
/// what makes "the child was still running" a state the test actually passes through.
struct NeverFinishesPack;

#[async_trait]
impl DomainPackRunner for NeverFinishesPack {
    fn domain_id(&self) -> &str {
        "dispatch"
    }

    async fn run(
        &self,
        _session_id: &str,
        _goal: &liberado_session::GoalSpec,
        _ctx: &liberado_session::PackContext<'_>,
        _events: tokio::sync::mpsc::Sender<liberado_session::SessionEvent>,
        _inputs: liberado_session::InputChannel,
        mut cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        loop {
            if *cancel.borrow_and_update() {
                return Err(PackError::Cancelled);
            }
            if cancel.changed().await.is_err() {
                return Err(PackError::Cancelled);
            }
        }
    }
}

/// A chat in delegation mode whose face agent calls `delegate` on its first turn.
async fn delegating_chat(dir: &std::path::Path) -> (Arc<ChatSessions>, Arc<GoalSessionHub>, Ulid) {
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(NeverFinishesPack));
    let hub = Arc::new(hub);

    let provider = Arc::new(MockProvider::with_script(
        "chat",
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "d1",
                crate::DELEGATE_TOOL_NAME,
                serde_json::json!({ "goal": "a job that never ends" }),
            )]),
            CompletionResponse::text("done"),
        ],
    ));
    let sessions = Arc::new(
        ChatSessions::new(
            Arc::new(SessionStore::open(dir).await),
            Executor::new(provider, Budget::default()),
            Arc::new(no_tools_runtime()),
        )
        .with_delegation_mode(true)
        .with_goal_hub(hub.clone())
        .with_dispatcher_capabilities(CapabilitySet::from_iter([
            liberado_common::Capability::ExecuteMcp("tasks-mcp".into()),
        ])),
    );
    let id = sessions
        .create_with_grant(
            None,
            SessionGrant {
                capabilities: CapabilitySet::from_iter([liberado_common::Capability::ExecuteMcp(
                    "tasks-mcp".into(),
                )]),
                profile: Some("delegating-chat".into()),
                delegation: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    (sessions, hub, id)
}

/// Poll until the hub has a session, or give up. Returns its id.
async fn wait_for_child(hub: &GoalSessionHub) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(record) = hub.list().await.into_iter().next() {
            return record.id;
        }
        assert!(
            Instant::now() < deadline,
            "no delegated session ever started"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// **Done test (A0c).** Cancel a chat turn while it is blocked in `delegate`; the child ends as
/// `Cancelled` within two seconds. Without the cascade it stays `Running` forever.
#[tokio::test]
async fn cancelling_a_chat_turn_cancels_the_session_it_delegated() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, hub, id) = delegating_chat(dir.path()).await;

    let (_replay, _rx) = sessions.start_or_attach(id, "do the thing");
    let child = wait_for_child(&hub).await;
    let before = hub.snapshot(&child).await.expect("the child is live");
    assert!(
        !before.session.status.is_terminal(),
        "the child must still be running when the cancel arrives: {:?}",
        before.session.status
    );

    assert!(
        sessions.cancel_turn(id),
        "a turn blocked in delegate is in flight and must be cancellable"
    );

    let deadline = Instant::now() + CANCEL_BUDGET;
    loop {
        let record = hub.snapshot(&child).await.expect("the child record stays");
        if record.session.status == SessionStatus::Cancelled {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the delegated session must end as Cancelled within {CANCEL_BUDGET:?}, \
             it is {:?}",
            record.session.status
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The cascade is keyed by origin, so an unrelated conversation's worker is left alone.
/// A cancel that stopped every live session would pass the test above and be a worse bug.
#[tokio::test]
async fn cancelling_one_chat_leaves_another_chats_worker_running() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, hub, id) = delegating_chat(dir.path()).await;
    let (_replay, _rx) = sessions.start_or_attach(id, "do the thing");
    let mine = wait_for_child(&hub).await;

    // A second worker under a conversation that is not being cancelled.
    let theirs = hub
        .start_background(
            liberado_session::GoalSpec {
                id: None,
                description: "someone else's job".into(),
                success_criteria: Vec::new(),
                domain: liberado_session::DomainHint::from("dispatch"),
                max_turns: 0,
                max_idle_secs: None,
                origin: Some(liberado_session::SessionOrigin::from_conversation("other")),
                profile: None,
                direct: false,
                payload: serde_json::Value::Null,
            },
            SessionGrant::default(),
        )
        .await
        .expect("the second worker starts");

    assert!(sessions.cancel_turn(id));

    let deadline = Instant::now() + CANCEL_BUDGET;
    while hub.snapshot(&mine).await.map(|r| r.session.status) != Some(SessionStatus::Cancelled) {
        assert!(Instant::now() < deadline, "the origin's worker never ended");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let other = hub.snapshot(&theirs).await.expect("the other worker stays");
    assert!(
        !other.session.status.is_terminal(),
        "a worker from another conversation must survive: {:?}",
        other.session.status
    );
}
