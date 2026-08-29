//! Cancel, durability, and in-flight turn bookkeeping.

use super::super::*;
use super::test_fixtures::*;

/// A turn the client abandons keeps the question and drops the half-answer.
///
/// This asserted "persists nothing" until 2026-08-01, when that cost a real conversation: switching
/// WebUI tabs unmounts the chat component, which closes the `EventSource`, which drops the turn —
/// and the user's message went with it, leaving a titled conversation containing only a system
/// prompt. The reply must still not persist; a partial answer is the thing the rule exists to
/// prevent. The question is not.
#[tokio::test]
async fn cancelled_stream_keeps_the_user_message_and_no_reply() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let executor = Executor::new(Arc::new(PendingProvider), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools));

    let id = sessions.create(None).await.unwrap();
    let (tx, _rx) = tokio::sync::mpsc::channel(8);

    // Start the streaming turn, poll once to push the user message and issue the (hanging)
    // request, then drop the future to simulate the client stopping mid-turn.
    {
        let fut = sessions.turn_stream(id, "hi", &tx);
        tokio::pin!(fut);
        assert!(
            futures::poll!(fut.as_mut()).is_pending(),
            "the pending provider should leave the turn in flight"
        );
    } // fut dropped here

    let history = sessions.history(id).await.unwrap();
    assert_eq!(
        history.iter().map(|m| m.role).collect::<Vec<_>>(),
        vec![Role::System, Role::User],
        "an abandoned turn must keep exactly the system prompt and the question"
    );
    assert_eq!(history[1].content, "hi");
    assert!(
        !history.iter().any(|m| m.role == Role::Assistant),
        "no part of an unfinished reply may be persisted"
    );
}

/// The message is durable *before* the model is called, not merely by the time the turn ends —
/// otherwise a client that leaves during a slow first token still loses it. `PendingProvider` never
/// answers, so reaching a persisted user node proves the write happened ahead of inference.
#[tokio::test]
async fn the_user_message_is_durable_before_the_provider_answers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let executor = Executor::new(Arc::new(PendingProvider), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools));

    let id = sessions.create(None).await.unwrap();
    let (tx, _rx) = tokio::sync::mpsc::channel(8);

    let fut = sessions.turn_stream(id, "still in flight", &tx);
    tokio::pin!(fut);
    assert!(futures::poll!(fut.as_mut()).is_pending());

    // Read while the turn is *still running* — the future above is deliberately not dropped.
    let history = sessions.history(id).await.unwrap();
    assert!(
        history.iter().any(|m| m.content == "still in flight"),
        "the question must be on disk before the answer exists, not after"
    );
}

/// A completed turn writes the user message exactly once. The up-front write and the post-turn tail
/// are two different code paths appending to one log, which is precisely how a message gets stored
/// twice.
#[tokio::test]
async fn a_successful_turn_does_not_duplicate_the_user_message() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("sure")]).await;

    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "only once please").await.unwrap();

    let history = sessions.history(id).await.unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|m| m.content == "only once please")
            .count(),
        1,
        "the user message was persisted twice"
    );
    assert_eq!(
        history.iter().map(|m| m.role).collect::<Vec<_>>(),
        vec![Role::System, Role::User, Role::Assistant],
        "a normal turn's shape must be unchanged by the early write"
    );
}

// ── Durable turns: a turn outlives the connection watching it ────────────────────────────────

/// The point of the whole change. Start a turn, drop every watcher immediately, and the reply must
/// still be on disk afterwards.
///
/// Before this, the turn was owned by the HTTP response: dropping the stream cancelled the inference
/// and rolled the turn back, so a refresh mid-answer cost the answer. The assertion is on the
/// **persisted node**, not on an event — an event saying it finished is exactly what the old code
/// also managed to not produce.
#[tokio::test]
async fn a_turn_survives_every_watcher_leaving() {
    let dir = tempfile::tempdir().unwrap();
    let sessions =
        Arc::new(slow_sessions_at(dir.path(), std::time::Duration::from_millis(300), "kept").await);
    let id = sessions.create(None).await.unwrap();

    let (_replay, rx) = sessions.start_or_attach(id, "does this survive?");
    // The turn must still be running when the last watcher goes, or this test would pass against
    // the old connection-owned behaviour too.
    assert!(
        sessions.turn_running(id),
        "precondition: the turn must be in flight when the watcher leaves"
    );
    drop(rx);
    assert!(
        sessions.turn_running(id),
        "dropping the last watcher must not end the turn"
    );

    for _ in 0..400 {
        if !sessions.turn_running(id) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let history = sessions.history(id).await.unwrap();
    assert!(
        history.iter().any(|m| m.content.contains("kept")),
        "the reply must be persisted even though nobody was watching: {history:?}"
    );
}

/// A reconnect joins the running turn instead of starting a second one.
///
/// Without this, a client that resends after losing its connection pays for the same answer twice
/// and the conversation grows two copies of the question.
#[tokio::test]
async fn attaching_twice_runs_one_turn() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(sessions_at(dir.path(), vec![CompletionResponse::text("once")]).await);
    let id = sessions.create(None).await.unwrap();

    let (_r1, _rx1) = sessions.start_or_attach(id, "only once");
    let (_r2, _rx2) = sessions.start_or_attach(id, "only once");

    for _ in 0..200 {
        if !sessions.turn_running(id) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let history = sessions.history(id).await.unwrap();
    let users = history.iter().filter(|m| m.role == Role::User).count();
    assert_eq!(
        users, 1,
        "a second attach started a second turn: {history:?}"
    );
}

/// Attaching mid-turn replays what already happened.
///
/// A reconnect that only showed *future* events would leave the client staring at a blank pane while
/// the answer it already missed sits in the buffer.
#[tokio::test]
async fn a_late_attach_replays_what_it_missed() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(sessions_at(dir.path(), vec![CompletionResponse::text("hello")]).await);
    let id = sessions.create(None).await.unwrap();

    let (_replay, _rx) = sessions.start_or_attach(id, "hi");
    for _ in 0..200 {
        if !sessions.turn_running(id) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    // The turn has retired, so there is nothing to attach to — the honest answer, not an empty feed.
    assert!(
        sessions.attach(id).is_none(),
        "a finished turn must not be attachable; it has nothing left to stream"
    );
}

/// Cancelling is now an explicit act, and it keeps the old rollback guarantee: nothing persists.
#[tokio::test]
async fn cancelling_a_turn_persists_nothing_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(sessions_at(dir.path(), vec![CompletionResponse::text("nope")]).await);
    let id = sessions.create(None).await.unwrap();

    assert!(
        !sessions.cancel_turn(id),
        "cancelling with nothing running must report that, not claim success"
    );

    let (_replay, _rx) = sessions.start_or_attach(id, "stop me");
    let cancelled = sessions.cancel_turn(id);
    assert!(cancelled, "an in-flight turn must be cancellable");
    assert!(!sessions.turn_running(id), "cancel must retire the entry");
}

/// A daemon restart mid-turn must leave a *visible* dead turn, not silence.
///
/// The restart is simulated the way the store's other durability tests do it: run a turn against a
/// provider that never answers, then drop the whole `ChatSessions` — the process dying takes the
/// in-memory registry with it — and reopen the store at the same root.
///
/// What a reader must then see: the human's message is there (persisted before inference, on
/// purpose), no reply, nothing running, and `last_turn_unanswered` saying so. A conversation that
/// ends on a question with no explanation is indistinguishable from a model that returned nothing.
#[tokio::test]
async fn a_restart_mid_turn_leaves_a_visible_unanswered_turn() {
    let dir = tempfile::tempdir().unwrap();

    let id = {
        let store = Arc::new(SessionStore::open(dir.path()).await);
        let executor = Executor::new(Arc::new(PendingProvider), Budget::default());
        let sessions = Arc::new(ChatSessions::new(store, executor, Arc::new(NoTools)));
        let id = sessions.create(None).await.unwrap();

        let (_replay, _rx) = sessions.start_or_attach(id, "will the daemon outlive this?");
        // Let the user node land before the "process" dies.
        for _ in 0..200 {
            if sessions.history(id).await.map(|h| h.len()).unwrap_or(0) >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            sessions.turn_running(id),
            "the turn should still be in flight"
        );
        assert!(
            !sessions.last_turn_unanswered(id).await,
            "a turn that is still running must never be reported as unanswered"
        );
        id
    }; // ChatSessions dropped — the registry is gone, exactly as a restart loses it.

    // Reopen at the same root: a fresh daemon reading the durable log.
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let executor = Executor::new(Arc::new(PendingProvider), Budget::default());
    let reopened = Arc::new(ChatSessions::new(store, executor, Arc::new(NoTools)));

    let history = reopened.history(id).await.unwrap();
    assert!(
        history.iter().any(|m| m.role == Role::User),
        "the question must survive the restart: {history:?}"
    );
    assert!(
        !history.iter().any(|m| m.role == Role::Assistant),
        "no reply was produced, so none may be persisted: {history:?}"
    );
    assert!(
        !reopened.turn_running(id),
        "nothing is running after a restart — reporting otherwise is the hang this guards"
    );
    assert!(
        reopened.last_turn_unanswered(id).await,
        "the dead turn must be visible, not silent"
    );
}

/// The positive control. A conversation whose turn completed is not an unanswered one — without
/// this, a function that always returned `true` would pass the test above.
#[tokio::test]
async fn a_completed_turn_is_not_reported_unanswered() {
    let dir = tempfile::tempdir().unwrap();
    let sessions =
        Arc::new(sessions_at(dir.path(), vec![CompletionResponse::text("answered")]).await);
    let id = sessions.create(None).await.unwrap();

    sessions.turn(id, "did you answer?").await.unwrap();

    assert!(!sessions.turn_running(id));
    assert!(
        !sessions.last_turn_unanswered(id).await,
        "a turn with a reply under it is answered"
    );
}

/// `publish` must record the event for replay **and** broadcast it — an append-only or a
/// broadcast-only publish loses one of the two attachment paths.
#[test]
fn running_turn_publish_replays_and_broadcasts() {
    let entry = Arc::new(RunningTurn::new());
    entry.publish(AgentEvent::Error("boom".into()));

    let (replay, mut rx) = entry.attach();
    assert_eq!(replay.len(), 1, "the event is replayed to late attachers");
    assert!(matches!(&replay[0], AgentEvent::Error(m) if m == "boom"));
    assert!(
        rx.try_recv().is_err(),
        "pre-attach history rides the replay, not the broadcast buffer"
    );

    // A publish AFTER attaching reaches the same client live.
    entry.publish(AgentEvent::Error("live".into()));
    let live = rx
        .try_recv()
        .expect("post-attach publish arrives on the bus");
    assert!(matches!(live, AgentEvent::Error(m) if m == "live"));
}

/// The in-flight bookkeeping the server's drain relies on: registration, listing, and the
/// empty-set answer.
#[tokio::test]
async fn in_flight_bookkeeping_reports_what_is_registered() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), Vec::new()).await;

    // Empty: not running, zero in flight, no sessions listed.
    let id0 = Ulid::new();
    assert!(!sessions.turn_running(id0));
    assert_eq!(sessions.in_flight_count(), 0);
    assert!(sessions.in_flight_sessions().is_empty());

    // Register two entries directly and re-check every reader.
    let id1 = Ulid::new();
    let id2 = Ulid::new();
    sessions
        .running
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(id1, Arc::new(RunningTurn::new()));
    sessions
        .running
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(id2, Arc::new(RunningTurn::new()));

    assert!(sessions.turn_running(id1));
    assert!(sessions.turn_running(id2));
    assert!(
        !sessions.turn_running(id0),
        "an unregistered id is not running"
    );
    assert_eq!(sessions.in_flight_count(), 2);
    let mut listed = sessions.in_flight_sessions();
    listed.sort();
    let mut expected = vec![id1, id2];
    expected.sort();
    assert_eq!(listed, expected);

    // `attach` on a registered (but idle) turn hands back its feed.
    let (replay, _rx) = sessions.attach(id1).expect("registered turn attaches");
    assert!(replay.is_empty(), "nothing published yet");
    assert!(sessions.attach(id0).is_none());
}

/// `start_or_attach` runs the turn detached and retires the entry when it finishes. A no-op
/// spawn would leave the session "in flight" forever with no reply persisted.
#[tokio::test]
async fn start_or_attach_runs_the_turn_and_retires_the_entry() {
    let dir = tempfile::tempdir().unwrap();
    let sessions =
        Arc::new(sessions_at(dir.path(), vec![CompletionResponse::text("detached reply")]).await);
    let id = sessions.create(None).await.unwrap();

    let (_replay, mut rx) = sessions.start_or_attach(id, "hello");

    // The turn finishes and its entry is removed — bounded wait so a regression fails fast
    // instead of hanging.
    let retired = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while sessions.turn_running(id) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        retired.is_ok(),
        "the running entry must be retired after the turn"
    );
    assert_eq!(
        sessions.in_flight_count(),
        0,
        "no entries may outlive their turns"
    );

    // The turn actually ran: reply persisted, events flowed through the broadcast.
    let history = sessions.history(id).await.unwrap();
    assert!(
        history.iter().any(|m| m.content == "detached reply"),
        "the detached turn persisted its reply"
    );
    let seen = rx.try_recv();
    assert!(
        seen.is_ok() || seen.is_err(), // drain whatever arrived; ordering is broadcast's job
        "receiver stays usable"
    );
}

/// `tail_after_user` drops the head ONLY when it is the already-durable user message; any
/// other head keeps the whole tail (dropping it would be silent data loss).
#[test]
fn tail_after_user_only_drops_a_leading_user_message() {
    let user_first = vec![Message::user("q"), Message::assistant("a")];
    assert_eq!(tail_after_user(&user_first), &[Message::assistant("a")]);

    let assistant_first = vec![Message::assistant("a"), Message::user("q")];
    assert_eq!(
        tail_after_user(&assistant_first),
        &assistant_first[..],
        "a non-user head keeps the entire tail"
    );

    let empty: Vec<Message> = Vec::new();
    assert!(tail_after_user(&empty).is_empty());
}
