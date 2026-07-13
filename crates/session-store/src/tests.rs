//! What convergence has to actually deliver, asserted.

use liberado_conversation_store::{Author, ConversationStore, NewNode};
use liberado_provider::Message;
use liberado_session::{
    DomainHint, GoalResult, GoalSessionRecord, GoalSpec, SessionEvent, SessionEventKind,
    SessionOrigin, SessionRecordStore, SessionStatus, TerminalKind,
};

use crate::{NewSession, SessionStore, Visibility};

fn goal_spec(description: &str) -> GoalSpec {
    GoalSpec {
        id: None,
        description: description.into(),
        success_criteria: vec![],
        domain: DomainHint::Life,
        max_turns: 0,
        max_idle_secs: None,
        origin: None,
        profile: None,
        payload: serde_json::json!({}),
    }
}

fn user_node(parent: Option<ulid::Ulid>, text: &str) -> NewNode {
    NewNode {
        parent_id: parent,
        author: Author::User,
        message: Message::user(text),
    }
}

#[tokio::test]
async fn a_chat_and_a_goal_session_live_in_one_store_and_one_list() {
    // The whole point of D7. Before this, these were two stores, two id spaces, and two endpoints
    // the client had to glue together.
    let store = SessionStore::new();

    let chat = store
        .create_session(NewSession {
            title: Some("weekly planning".into()),
            goal: None,
            ..Default::default()
        })
        .await;

    let goal = store
        .create_session(NewSession {
            goal: Some(goal_spec("capture a note")),
            ..Default::default()
        })
        .await;

    let all = store.list_sessions().await;
    assert_eq!(all.len(), 2, "one list holds both");

    // Terminality IS goal.is_some() — the single attribute that distinguishes them.
    assert!(!chat.is_terminal_kind(), "a chat never runs to terminal");
    assert!(goal.is_terminal_kind(), "a goal session does");
}

#[tokio::test]
async fn the_kernel_lens_sees_goal_sessions_and_the_chat_lens_sees_everything() {
    let store = SessionStore::new();
    store
        .create_session(NewSession {
            title: Some("a chat".into()),
            goal: None,
            ..Default::default()
        })
        .await;
    store
        .create_session(NewSession {
            goal: Some(goal_spec("a goal")),
            ..Default::default()
        })
        .await;

    // The kernel's `GoalSessionRecord` simply cannot represent a goal-less session, so its lens
    // shows only the goal-bearing one. That is a property of the *type*, not a second store.
    let records = SessionRecordStore::list(&store).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].goal.description, "a goal");

    // The chat lens shows every session — including the goal session, which has a transcript too.
    // That is what lets one switcher list everything.
    let convs = ConversationStore::list(&store).await.unwrap();
    assert_eq!(convs.len(), 2);
    assert!(
        convs.iter().any(|c| c.title.as_deref() == Some("a goal")),
        "a goal session's title falls back to its goal, so it reads sensibly in a list"
    );
}

#[tokio::test]
async fn origin_is_a_real_edge_not_a_string() {
    // The S4 return handoff had to name the parent conversation by String, because the session
    // kernel was forbidden from depending on the conversation store — two id spaces that could only
    // point at each other by name. One store, one id space, one Ulid.
    let store = SessionStore::new();
    let parent = store
        .create_session(NewSession {
            title: Some("parent chat".into()),
            ..Default::default()
        })
        .await;

    let child = store
        .create_session(NewSession {
            goal: Some(goal_spec("spawned from the chat")),
            parent_session: Some(parent.id),
            visibility: Visibility::Foreground,
            ..Default::default()
        })
        .await;

    assert_eq!(child.parent_session, Some(parent.id));

    // ...and the session tree is walkable, which is what makes a subagent/cron run inspectable.
    let kids = store.children_of(parent.id).await;
    assert_eq!(kids.len(), 1);
    assert_eq!(kids[0].id, child.id);
}

#[tokio::test]
async fn a_background_session_is_an_attribute_not_a_subtype() {
    // A cron used to fire into the void — it wasn't a session at all, so nothing could show it.
    let store = SessionStore::new();
    let cron = store
        .create_session(NewSession {
            goal: Some(goal_spec("nightly summary")),
            visibility: Visibility::Background,
            ..Default::default()
        })
        .await;

    assert_eq!(cron.visibility, Visibility::Background);
    // It is an ordinary session: it lists, it has a transcript, it can be opened.
    assert_eq!(store.list_sessions().await.len(), 1);
}

#[tokio::test]
async fn one_log_holds_both_message_nodes_and_pack_events() {
    // The case that proves these were never two things: an interactive coding session's intake Q&A
    // are *turns*, and its tool calls are *observations*. Both belong to one session.
    let store = SessionStore::new();
    let s = store
        .create_session(NewSession {
            goal: Some(goal_spec("build a CLI")),
            ..Default::default()
        })
        .await;
    let id = s.id.to_string();

    // A pack event...
    SessionRecordStore::push_event(
        &store,
        SessionEvent::new(
            &id,
            SessionEventKind::ToolStarted {
                name: "write_file".into(),
                args_preview: "src/main.rs".into(),
            },
        ),
    )
    .await;

    // ...and a provider-replayable turn, in the same session.
    let node = ConversationStore::append(&store, s.id, user_node(None, "use Rust"))
        .await
        .unwrap();

    assert_eq!(
        SessionRecordStore::events(&store, &id).await.unwrap().len(),
        1
    );
    let path = ConversationStore::leaf_path(&store, s.id, None)
        .await
        .unwrap();
    assert_eq!(path.len(), 1);
    assert_eq!(path[0].id, node.id);
}

#[tokio::test]
async fn the_message_dag_supports_branching_from_any_node() {
    // Forking (the next slice) is additive precisely because of this: `leaf_path(conv, Some(node))`
    // reconstructs the context *prior to a split point*, so a branch inherits exactly the history
    // that preceded it and nothing after.
    let store = SessionStore::new();
    let s = store.create_session(NewSession::default()).await;

    let a = ConversationStore::append(&store, s.id, user_node(None, "root"))
        .await
        .unwrap();
    let b = ConversationStore::append(&store, s.id, user_node(Some(a.id), "down branch 1"))
        .await
        .unwrap();
    // Branch from `a` again — a *second* child, so `a` now has two children.
    let c = ConversationStore::append(&store, s.id, user_node(Some(a.id), "down branch 2"))
        .await
        .unwrap();

    let kids = ConversationStore::children(&store, s.id, a.id)
        .await
        .unwrap();
    assert_eq!(kids.len(), 2, "a is a branch point");

    // Each branch sees its own history, and neither sees the other.
    let p1 = ConversationStore::leaf_path(&store, s.id, Some(b.id))
        .await
        .unwrap();
    let p2 = ConversationStore::leaf_path(&store, s.id, Some(c.id))
        .await
        .unwrap();
    assert_eq!(p1.len(), 2);
    assert_eq!(p2.len(), 2);
    assert_eq!(p1[0].id, a.id);
    assert_eq!(p2[0].id, a.id);
    assert_ne!(
        p1[1].id, p2[1].id,
        "the branches diverge after the split point"
    );
}

#[tokio::test]
async fn a_durable_store_rehydrates_chats_and_goal_sessions_together() {
    let dir = tempfile::tempdir().unwrap();

    let chat_id;
    let goal_id;
    {
        let store = SessionStore::open(dir.path()).await;
        let chat = store
            .create_session(NewSession {
                title: Some("kept".into()),
                ..Default::default()
            })
            .await;
        chat_id = chat.id;
        ConversationStore::append(&store, chat.id, user_node(None, "hello"))
            .await
            .unwrap();

        let goal = store
            .create_session(NewSession {
                goal: Some(goal_spec("finished work")),
                ..Default::default()
            })
            .await;
        goal_id = goal.id;
        SessionRecordStore::push_event(
            &store,
            SessionEvent::new(
                goal_id.to_string(),
                SessionEventKind::Progress {
                    message: "did a thing".into(),
                },
            ),
        )
        .await;
        SessionRecordStore::finish(
            &store,
            &goal_id.to_string(),
            SessionStatus::Succeeded,
            GoalResult {
                terminal: TerminalKind::Succeeded,
                summary: "done".into(),
                artifacts: vec![],
                diagnostics: serde_json::json!({}),
            },
        )
        .await;
    }

    // Reopen: both come back, from one directory.
    let store = SessionStore::open(dir.path()).await;
    assert_eq!(store.list_sessions().await.len(), 2);

    let chat = store.session(chat_id).await.unwrap();
    assert_eq!(chat.title.as_deref(), Some("kept"));
    let path = ConversationStore::leaf_path(&store, chat_id, None)
        .await
        .unwrap();
    assert_eq!(path.len(), 1, "the chat's turns survived");

    let goal = store.session(goal_id).await.unwrap();
    assert_eq!(goal.status, SessionStatus::Succeeded);
    assert_eq!(goal.result.unwrap().summary, "done");
    assert_eq!(
        SessionRecordStore::events(&store, &goal_id.to_string())
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn a_goal_session_killed_mid_run_is_coerced_but_a_chat_is_not() {
    let dir = tempfile::tempdir().unwrap();

    let chat_id;
    let goal_id;
    {
        let store = SessionStore::open(dir.path()).await;
        chat_id = store.create_session(NewSession::default()).await.id;
        goal_id = store
            .create_session(NewSession {
                goal: Some(goal_spec("interrupted")),
                ..Default::default()
            })
            .await
            .id;
        SessionRecordStore::set_status(&store, &goal_id.to_string(), SessionStatus::Running).await;
        // ...and the daemon dies here.
    }

    let store = SessionStore::open(dir.path()).await;
    // No pack is running it any more and packs aren't resumable, so leaving it `Running` would be a
    // lie the UI would render forever.
    assert_eq!(
        store.session(goal_id).await.unwrap().status,
        SessionStatus::Failed
    );
    // A chat has no terminal state to coerce to — an open chat is simply open.
    assert_eq!(
        store.session(chat_id).await.unwrap().status,
        SessionStatus::Running
    );
}

#[tokio::test]
async fn awaiting_input_is_derived_from_the_transcript_and_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let id;
    {
        let store = SessionStore::open(dir.path()).await;
        id = store
            .create_session(NewSession {
                goal: Some(goal_spec("asks a question")),
                ..Default::default()
            })
            .await
            .id;
        SessionRecordStore::push_event(
            &store,
            SessionEvent::new(
                id.to_string(),
                SessionEventKind::AwaitingInput {
                    prompt: "which?".into(),
                    options: vec![],
                },
            ),
        )
        .await;
        assert!(store.session(id).await.unwrap().awaiting_input);
    }

    // Derived on replay from the same events — it can never drift from the transcript.
    let store = SessionStore::open(dir.path()).await;
    // (Coerced to Failed because it was mid-run, but the awaiting flag is cleared by that coercion:
    // a terminal session is not waiting for anyone.)
    let h = store.session(id).await.unwrap();
    assert_eq!(h.status, SessionStatus::Failed);
    assert!(!h.awaiting_input);
}

#[tokio::test]
async fn a_title_change_is_an_append_not_a_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let id;
    {
        let store = SessionStore::open(dir.path()).await;
        id = store.create_session(NewSession::default()).await.id;
        ConversationStore::set_title(&store, id, "renamed".into())
            .await
            .unwrap();
    }
    // Replay takes the last header line it sees, so the rewrite is idempotent and the log keeps its
    // one invariant: never mutate what was already written.
    let store = SessionStore::open(dir.path()).await;
    assert_eq!(
        store.session(id).await.unwrap().title.as_deref(),
        Some("renamed")
    );
}

#[tokio::test]
async fn the_kernel_can_insert_a_record_and_read_it_back_through_its_own_lens() {
    // The hub talks only to `SessionRecordStore`; this is the path it actually uses.
    let store = SessionStore::new();
    let mut spec = goal_spec("from the kernel");
    spec.origin = Some(SessionOrigin {
        conversation_id: Some(ulid::Ulid::new().to_string()),
        correlation_id: Some("corr-1".into()),
    });
    let record = GoalSessionRecord::new(spec);
    let id = record.id.clone();

    SessionRecordStore::insert(&store, record).await;

    let back = SessionRecordStore::get(&store, &id).await.expect("found");
    assert_eq!(back.goal.description, "from the kernel");
    // The stringly origin is resolved into a real parent edge on the way in.
    let header = store.session(id.parse().unwrap()).await.unwrap();
    assert!(header.parent_session.is_some());
    assert_eq!(header.correlation_id.as_deref(), Some("corr-1"));
}

#[tokio::test]
async fn a_background_session_survives_the_kernel_lens_in_both_directions() {
    // The regression that made S5′ step 5 impossible: `insert` stamped every record `Foreground`,
    // because `GoalSessionRecord` had nowhere to *put* a visibility. So a cron could be recorded —
    // and would come back claiming a human had started it. Both directions are asserted here: the
    // way in (record → header) and the way back out (header → record).
    let store = SessionStore::new();
    let mut spec = goal_spec("summarize today's decisions");
    // A cron has a correlation but no parent conversation — the reason `conversation_id` is optional.
    spec.origin = Some(SessionOrigin::from_correlation(
        "cron:nightly:2026-07-13T09:00:00Z",
    ));
    let record = GoalSessionRecord::background(spec, Default::default());
    let id = record.id.clone();

    SessionRecordStore::insert(&store, record).await;

    let header = store.session(id.parse().unwrap()).await.unwrap();
    assert_eq!(header.visibility, Visibility::Background);
    assert_eq!(
        header.correlation_id.as_deref(),
        Some("cron:nightly:2026-07-13T09:00:00Z"),
        "a correlation with no parent conversation must still land on the header"
    );
    assert!(
        header.parent_session.is_none(),
        "nobody spawned a cron from a chat"
    );

    let back = SessionRecordStore::get(&store, &id).await.expect("found");
    assert_eq!(back.visibility, Visibility::Background);
}

#[tokio::test]
async fn a_background_session_shows_up_in_the_one_unified_list() {
    // The actual payoff: a cron firing is a row in the same list as your chats, so it stops firing
    // into the void.
    let store = SessionStore::new();
    store
        .create_session(NewSession {
            title: Some("a chat".into()),
            ..Default::default()
        })
        .await;
    SessionRecordStore::insert(
        &store,
        GoalSessionRecord::background(goal_spec("nightly review"), Default::default()),
    )
    .await;

    let all = store.list_sessions().await;
    assert_eq!(all.len(), 2, "one list, both kinds");
    let bg: Vec<_> = all
        .iter()
        .filter(|h| h.visibility == Visibility::Background)
        .collect();
    assert_eq!(bg.len(), 1);
    assert_eq!(bg[0].goal.as_ref().unwrap().description, "nightly review");
}
