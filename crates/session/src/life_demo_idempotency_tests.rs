//! Idempotent-start test split from `life_demo.rs` for its module-health boundaries.

use std::sync::Arc;

use crate::SessionEventKind;
use crate::goal::{DomainHint, GoalSpec};
use crate::hub::GoalSessionHub;
use crate::life_demo::LifeOpsDemoRunner;
use crate::store::GoalSessionStore;

#[tokio::test]
async fn repeated_client_goal_id_does_not_start_a_second_run() {
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(LifeOpsDemoRunner));
    let hub = Arc::new(hub);
    let goal = GoalSpec {
        id: Some("stable-repair-command".into()),
        description: "capture one durable note".into(),
        success_criteria: vec![],
        domain: DomainHint::Life,
        max_turns: 0,
        max_idle_secs: None,
        origin: None,
        profile: None,
        payload: serde_json::json!({}),
    };

    let first = hub.start(goal.clone()).await.unwrap();
    let second = hub.start(goal).await.unwrap();

    assert_eq!(first, second);
    assert_eq!(hub.list().await.len(), 1);
    let snapshot = hub.await_terminal(&first).await.unwrap();
    let run_starts = snapshot
        .events
        .iter()
        .filter(|event| matches!(event.kind, SessionEventKind::RoleStarted { .. }))
        .count();
    assert_eq!(run_starts, 1, "the pack must run only once");
}

#[tokio::test]
async fn concurrent_client_goal_id_starts_one_run() {
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(LifeOpsDemoRunner));
    let hub = Arc::new(hub);
    let goal = GoalSpec {
        id: Some("concurrent-stable-repair-command".into()),
        description: "capture one durable note".into(),
        success_criteria: vec![],
        domain: DomainHint::Life,
        max_turns: 0,
        max_idle_secs: None,
        origin: None,
        profile: None,
        payload: serde_json::json!({}),
    };

    let (first, second) = tokio::join!(hub.start(goal.clone()), hub.start(goal));
    let first = first.unwrap();
    let second = second.unwrap();

    assert_eq!(first, second);
    assert_eq!(hub.list().await.len(), 1);
    let snapshot = hub.await_terminal(&first).await.unwrap();
    let run_starts = snapshot
        .events
        .iter()
        .filter(|event| matches!(event.kind, SessionEventKind::RoleStarted { .. }))
        .count();
    assert_eq!(run_starts, 1, "the pack must run only once");
}
