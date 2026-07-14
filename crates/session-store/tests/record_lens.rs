//! The `SessionRecordStore` conformance suite — run against **both** implementations.
//!
//! This is the sibling of [`conversation_lens`](./conversation_lens.rs), and it exists for the same
//! reason, one layer up.
//!
//! `SessionRecordStore` is the **kernel lens**: the hub, every pack, resume, the alert path and the
//! parked-session machinery all speak it and nothing else. It has **two** implementations —
//! `liberado_session::GoalSessionStore` (in-memory) and `liberado_session_store::SessionStore` (the
//! durable JSONL store the daemon actually boots). Until now there was no shared contract test
//! between them, and **every hub and pack test in the codebase runs against the in-memory one**.
//!
//! That is the exact shape of the most expensive bug class in this repo
//! (`docs/architecture/failure-modes.md` §1): a suite of load-bearing invariants pointed at an
//! implementation production never reaches, while the one doing the real work goes unverified. It
//! cost three separate defects last time. This suite is the check that the two agree — so a hub test
//! passing on the in-memory store means something about the daemon.
//!
//! Every test below runs twice, once per implementation, and asserts identical observable behaviour.

use std::sync::Arc;

use liberado_session::{
    GoalResult, GoalSessionRecord, GoalSessionStore, GoalSpec, SessionEvent, SessionEventKind,
    SessionRecordStore, SessionStatus, TerminalKind, TurnAuthor,
};
use liberado_session_store::SessionStore;
use tempfile::TempDir;

/// The two implementations, behind the trait the kernel actually uses. A test that takes this and
/// asserts on it cannot accidentally be written against only one of them.
async fn implementations() -> Vec<(&'static str, Arc<dyn SessionRecordStore>, Option<TempDir>)> {
    let dir = tempfile::tempdir().unwrap();
    let durable = SessionStore::open(dir.path()).await;
    vec![
        (
            "GoalSessionStore (in-memory — what every hub/pack test uses)",
            Arc::new(GoalSessionStore::new()) as Arc<dyn SessionRecordStore>,
            None,
        ),
        (
            "SessionStore (JSONL — what the daemon actually boots)",
            Arc::new(durable) as Arc<dyn SessionRecordStore>,
            Some(dir),
        ),
    ]
}

fn spec(id: &str, description: &str) -> GoalSpec {
    GoalSpec {
        id: Some(id.into()),
        description: description.into(),
        success_criteria: vec![],
        domain: liberado_session::DomainHint::Coding,
        max_turns: 0,
        max_idle_secs: None,
        origin: None,
        profile: None,
        payload: serde_json::Value::Null,
    }
}

/// A session id both stores accept. The JSONL store parses ids as ULIDs; the in-memory one takes any
/// string. A conformance suite has to use an id that is legal for *both*, or it is only testing one.
fn id() -> String {
    ulid::Ulid::new().to_string()
}

#[tokio::test]
async fn a_session_round_trips() {
    for (name, store, _d) in implementations().await {
        let sid = id();
        store.insert(GoalSessionRecord::new(spec(&sid, "build a cli"))).await;

        let got = store
            .get(&sid)
            .await
            .unwrap_or_else(|| panic!("{name}: inserted session must be readable back"));
        assert_eq!(got.id, sid, "{name}");
        assert_eq!(got.goal.description, "build a cli", "{name}");
        assert!(!got.awaiting_input, "{name}: a fresh session awaits nothing");
        assert!(
            store.list().await.iter().any(|r| r.id == sid),
            "{name}: and it must appear in the list"
        );
    }
}

#[tokio::test]
async fn turns_read_back_what_was_written_in_order() {
    // The read side of the transcript is what resume runs on (E6-c). It was write-only until
    // 2026-07-14 — nobody had needed to remember. If the two stores disagree here, a resume that
    // works in a unit test does something different on your machine.
    for (name, store, _d) in implementations().await {
        let sid = id();
        store.insert(GoalSessionRecord::new(spec(&sid, "goal"))).await;

        store.append_turn(&sid, TurnAuthor::User, "the goal".into()).await;
        store
            .append_turn(&sid, TurnAuthor::Assistant, "a question?".into())
            .await;
        store.append_turn(&sid, TurnAuthor::User, "an answer".into()).await;

        let turns = store.turns(&sid).await;
        let said: Vec<&str> = turns.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            said,
            vec!["the goal", "a question?", "an answer"],
            "{name}: the transcript must read back in the order it was written"
        );
        assert!(
            matches!(turns[1].0, TurnAuthor::Assistant),
            "{name}: and it must remember WHO said each thing — pairing a question with its answer \
             is exactly what resume does with this"
        );
    }
}

#[tokio::test]
async fn turns_are_empty_for_a_session_that_has_said_nothing() {
    for (name, store, _d) in implementations().await {
        let sid = id();
        store.insert(GoalSessionRecord::new(spec(&sid, "goal"))).await;
        assert!(
            store.turns(&sid).await.is_empty(),
            "{name}: no turns recorded means no turns read back — resume must not invent an answer"
        );
        assert!(
            store.turns(&id()).await.is_empty(),
            "{name}: and an unknown session yields nothing rather than panicking"
        );
    }
}

#[tokio::test]
async fn awaiting_input_is_derived_from_the_event_stream_not_set_by_hand() {
    // `awaiting_input` is the flag every surface badges "needs you" on, and the alert path fires on
    // it. It must be a *consequence* of the events, in both stores, or a session can silently look
    // answerable when it is not (or the reverse).
    for (name, store, _d) in implementations().await {
        let sid = id();
        store.insert(GoalSessionRecord::new(spec(&sid, "goal"))).await;

        store
            .push_event(SessionEvent::new(
                &sid,
                SessionEventKind::AwaitingInput {
                    prompt: "which one?".into(),
                    options: vec![],
                },
            ))
            .await;
        assert!(
            store.get(&sid).await.unwrap().awaiting_input,
            "{name}: an AwaitingInput event must raise the flag"
        );

        store
            .push_event(SessionEvent::new(
                &sid,
                SessionEventKind::HumanInput {
                    text: "that one".into(),
                },
            ))
            .await;
        assert!(
            !store.get(&sid).await.unwrap().awaiting_input,
            "{name}: and the answer must clear it"
        );
    }
}

#[tokio::test]
async fn events_read_back_in_order() {
    for (name, store, _d) in implementations().await {
        let sid = id();
        store.insert(GoalSessionRecord::new(spec(&sid, "goal"))).await;
        for i in 0..5 {
            store
                .push_event(SessionEvent::new(
                    &sid,
                    SessionEventKind::Progress {
                        message: format!("step {i}"),
                    },
                ))
                .await;
        }
        let events = store.events(&sid).await.unwrap_or_default();
        assert_eq!(events.len(), 5, "{name}");
        // `can_resume` scans this history for "did the build start?". Order and completeness are the
        // whole basis of that decision.
        let msgs: Vec<String> = events
            .iter()
            .filter_map(|e| match &e.kind {
                SessionEventKind::Progress { message } => Some(message.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(msgs, ["step 0", "step 1", "step 2", "step 3", "step 4"], "{name}");
    }
}

#[tokio::test]
async fn finish_is_terminal_and_records_the_result() {
    for (name, store, _d) in implementations().await {
        let sid = id();
        store.insert(GoalSessionRecord::new(spec(&sid, "goal"))).await;
        store.set_status(&sid, SessionStatus::Running).await;

        store
            .finish(
                &sid,
                SessionStatus::Succeeded,
                GoalResult {
                    terminal: TerminalKind::Succeeded,
                    summary: "done".into(),
                    artifacts: vec!["a.rs".into()],
                    diagnostics: serde_json::json!({"k": 1}),
                },
            )
            .await;

        let rec = store.get(&sid).await.unwrap();
        assert_eq!(rec.status, SessionStatus::Succeeded, "{name}");
        assert!(rec.status.is_terminal(), "{name}");
        let result = rec
            .result
            .unwrap_or_else(|| panic!("{name}: a finished session must carry its result"));
        assert_eq!(result.summary, "done", "{name}");
        assert_eq!(result.artifacts, ["a.rs"], "{name}");
    }
}

#[tokio::test]
async fn a_live_subscriber_is_counted_and_an_absent_one_is_not() {
    // This is the whole basis of "nobody is watching, so ping their phone" (E5). If the two stores
    // disagree, the notification either never fires or fires while you are looking at the screen.
    for (name, store, _d) in implementations().await {
        let sid = id();
        store.insert(GoalSessionRecord::new(spec(&sid, "goal"))).await;

        assert_eq!(
            store.live_subscriber_count(&sid).await,
            0,
            "{name}: nobody is watching a session nobody subscribed to"
        );

        let sub = store.subscribe(&sid).await;
        assert!(sub.is_some(), "{name}: a known session must be subscribable");
        assert_eq!(
            store.live_subscriber_count(&sid).await,
            1,
            "{name}: a live subscriber must be visible to the alert path"
        );

        drop(sub);
        assert_eq!(
            store.live_subscriber_count(&sid).await,
            0,
            "{name}: and dropping the stream must make the session unwatched again — otherwise the \
             ping is suppressed forever by a browser tab that closed an hour ago"
        );
    }
}

#[tokio::test]
async fn an_unknown_session_is_absent_rather_than_a_panic() {
    for (name, store, _d) in implementations().await {
        let missing = id();
        assert!(store.get(&missing).await.is_none(), "{name}");
        assert!(store.events(&missing).await.is_none(), "{name}");
        assert!(store.subscribe(&missing).await.is_none(), "{name}");
        assert_eq!(store.live_subscriber_count(&missing).await, 0, "{name}");
        // Writes to a session that does not exist must be inert, not fatal — the hub can race a
        // teardown against a late event.
        store
            .append_turn(&missing, TurnAuthor::User, "hello".into())
            .await;
        store.set_status(&missing, SessionStatus::Running).await;
    }
}
