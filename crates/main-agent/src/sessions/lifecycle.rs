//! Conversation lifecycle: create, persist, rehydrate, list, append-note.

use super::super::*;
use super::test_fixtures::*;

#[tokio::test]
async fn persisted_turn_round_trips_to_disk() {
    let dir = tempfile::tempdir().unwrap();

    let id = {
        let sessions = sessions_at(
            dir.path(),
            vec![CompletionResponse::text("Hi! How can I help?")],
        )
        .await;
        let id = sessions.create(None).await.unwrap();
        let reply = sessions.turn(id, "hello").await.unwrap();
        assert_eq!(reply, "Hi! How can I help?");
        id
    };

    // A SECOND ChatSessions over the SAME store root must see the durable history: it round-trips
    // through disk, not an in-process cache.
    let reopened = sessions_at(dir.path(), Vec::new()).await;
    let history = reopened.history(id).await.unwrap();
    assert_eq!(history[0].role, Role::System);
    assert!(
        history.iter().any(|m| m.content == "hello"),
        "user message did not persist"
    );
    assert!(
        history.iter().any(|m| m.content == "Hi! How can I help?"),
        "assistant reply did not persist"
    );
}

#[tokio::test]
async fn append_note_folds_a_goal_session_summary_into_the_conversation() {
    // The return-handoff path (S4/D2): a finished specialist session's summary is appended to the
    // parent conversation and rehydrates as ordinary context on the next load.
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("On it.")]).await;
        let id = sessions.create(None).await.unwrap();
        sessions.turn(id, "build me a CLI").await.unwrap();
        sessions
            .append_note(
                id,
                "[coding session succeeded] build a hello CLI\nOutcome: 1 file written",
            )
            .await
            .unwrap();
        // Must-not-regress (round 3 §1): handoff is Named("goal-session"), never Assistant —
        // model derivation and last_turn_unanswered both key on Author.
        let nodes = sessions.history_nodes(id).await.unwrap();
        assert!(
            nodes.iter().any(|n| {
                matches!(&n.author, Author::Named(name) if name == "goal-session")
                    && n.message.content.contains("[coding session succeeded]")
            }),
            "append_note must author as Named(\"goal-session\"), not Assistant: {nodes:?}"
        );
        assert!(
            nodes
                .iter()
                .filter(|n| n.message.content.contains("[coding session succeeded]"))
                .all(|n| n.model.is_none()),
            "handoff note must not carry a model stamp"
        );
        id
    };

    // Reopen over the same store: the note is durable and in history.
    let reopened = sessions_at(dir.path(), Vec::new()).await;
    let history = reopened.history(id).await.unwrap();
    assert!(
        history
            .iter()
            .any(|m| m.content.contains("[coding session succeeded]")
                && m.content.contains("1 file written")),
        "handoff note did not persist into the conversation"
    );
}

#[tokio::test]
async fn context_carries_across_turns_via_rehydration() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::text("Hi! How can I help?"),
            CompletionResponse::text("You said hello a moment ago."),
        ],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools));

    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "hello").await.unwrap();
    sessions.turn(id, "what did I just say?").await.unwrap();

    // The second turn rehydrated from the store, so its provider request carried the first user
    // message — context survived even though nothing was held in memory between turns.
    let second_request = &provider.received_requests()[1];
    assert!(
        second_request.messages.iter().any(|m| m.content == "hello"),
        "rehydration lost the first user message"
    );
}

#[tokio::test]
async fn list_returns_created_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), Vec::new()).await;

    sessions.create(Some("My chat".into())).await.unwrap();
    let headers = sessions.list().await.unwrap();
    assert!(
        headers
            .iter()
            .any(|h| h.title.as_deref() == Some("My chat")),
        "list did not return the created conversation"
    );
}

/// Transient system messages are injected at the *front* of the view, so slicing the turn's output
/// by a pre-turn length walks back into history and re-persists messages already on disk.
///
/// Latent for as long as the only injector was a profile's optional nudge; the tool manifest runs
/// every turn, which made it certain. Caught by an unrelated compaction test starting to fail —
/// duplicated messages inflated the next load past the compaction trigger.
#[tokio::test]
async fn a_turn_persists_only_its_own_messages_not_the_injected_ones() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(
        dir.path(),
        vec![
            CompletionResponse::text("first answer"),
            CompletionResponse::text("second answer"),
        ],
    )
    .await;
    let id = sessions
        .create_with_grant(
            None,
            SessionGrant {
                profile: Some("terse".into()),
                prompt_append: Some("Be terse.".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    sessions.turn(id, "first question").await.unwrap();
    sessions.turn(id, "second question").await.unwrap();

    let history = sessions.history(id).await.unwrap();
    for probe in ["first question", "first answer", "second question"] {
        assert_eq!(
            history.iter().filter(|m| m.content == probe).count(),
            1,
            "{probe:?} must be stored exactly once; history: {:?}",
            history.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
    }
    // And the per-turn injections are views, never records.
    for injected in ["Be terse.", "available to you on this turn"] {
        assert!(
            !history.iter().any(|m| m.content.contains(injected)),
            "{injected:?} is a per-turn view and must not be persisted"
        );
    }
}
