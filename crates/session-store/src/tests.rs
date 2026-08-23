//! What convergence has to actually deliver, asserted.

use liberado_conversation_store::{Author, ConversationStore, NewNode};
use liberado_provider::Message;
use liberado_session::{
    DomainHint, GoalResult, GoalSessionRecord, GoalSpec, SessionEvent, SessionEventKind,
    SessionOrigin, SessionRecordStore, SessionStatus, TerminalKind, TurnAuthor,
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
        model: None,
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
    // E6: an awaiting session is Parked on restart, and the open question remains visible.
    let h = store.session(id).await.unwrap();
    assert_eq!(h.status, SessionStatus::Parked);
    assert!(h.awaiting_input, "the question must survive the restart");
    // B3: restart must leave a marker explaining why the session is parked.
    let events = store.events(&id.to_string()).await.unwrap();
    let has_marker = events.iter().any(|e| {
        matches!(&e.kind, SessionEventKind::Progress { message }
            if message.contains("daemon restarted"))
    });
    assert!(
        has_marker,
        "restart-parked session must carry a Progress marker"
    );
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

// ── Forking (session-focus, after S5′) ───────────────────────────────────────────────────────

/// A three-turn chat: user/assistant × 3, parent-linked into a straight line.
async fn chat_with_turns(store: &SessionStore, turns: &[(&str, &str)]) -> ulid::Ulid {
    let conv = store
        .create_session(NewSession {
            title: Some("original".into()),
            ..Default::default()
        })
        .await
        .id;
    let mut parent = None;
    for (q, a) in turns {
        let u = store.append(conv, user_node(parent, q)).await.unwrap();
        let a = store
            .append(
                conv,
                NewNode {
                    parent_id: Some(u.id),
                    author: Author::Assistant,
                    message: Message::assistant(*a),
                    model: None,
                },
            )
            .await
            .unwrap();
        parent = Some(a.id);
    }
    conv
}

#[tokio::test]
async fn a_fork_copies_the_prefix_rather_than_pointing_at_it() {
    let store = SessionStore::new();
    let conv = chat_with_turns(&store, &[("q1", "a1"), ("q2", "a2")]).await;

    let fork = store
        .fork_session(conv, None, Some("a fork".into()))
        .await
        .unwrap();

    // The fork's own log is self-contained: every node it needs is *in it*, not borrowed from the
    // parent at read time. That is what keeps one session = one greppable, replayable file.
    let copied = store.leaf_path(fork.id, None).await.unwrap();
    let original = store.leaf_path(conv, None).await.unwrap();
    assert_eq!(copied.len(), original.len(), "the whole prefix came across");
    assert_eq!(
        copied
            .iter()
            .map(|n| n.message.content.clone())
            .collect::<Vec<_>>(),
        vec!["q1", "a1", "q2", "a2"],
    );

    // Copies, not the same nodes: fresh ids, belonging to the fork, re-parented onto each other.
    for (c, o) in copied.iter().zip(&original) {
        assert_ne!(c.id, o.id, "a copied node must have its own id");
        assert_eq!(c.conversation_id, fork.id);
    }
    assert!(copied[0].parent_id.is_none(), "the copied root is a root");
    assert_eq!(
        copied[1].parent_id,
        Some(copied[0].id),
        "re-parented onto the copy, not the original"
    );

    // Lineage is still recorded, so the tree stays walkable even though the content stands alone.
    assert_eq!(fork.parent_session, Some(conv));
    assert_eq!(fork.spawned_by, Some(original.last().unwrap().id));
    assert_eq!(store.children_of(conv).await.len(), 1);
}

#[tokio::test]
async fn continuing_the_original_after_forking_does_not_touch_the_fork() {
    // The reason copy semantics were chosen. A fork is a *snapshot*: you go back to the original,
    // keep talking, and the branch you took stays exactly as you left it. Under read-time stitching
    // this is the assertion that would fail.
    let store = SessionStore::new();
    let conv = chat_with_turns(&store, &[("q1", "a1")]).await;
    let fork = store.fork_session(conv, None, None).await.unwrap();

    let leaf = store
        .leaf_path(conv, None)
        .await
        .unwrap()
        .last()
        .unwrap()
        .id;
    store
        .append(conv, user_node(Some(leaf), "a later thought"))
        .await
        .unwrap();

    let forked = store.leaf_path(fork.id, None).await.unwrap();
    assert_eq!(
        forked.len(),
        2,
        "the fork is frozen at the point it was taken"
    );
    assert!(
        !forked
            .iter()
            .any(|n| n.message.content == "a later thought"),
        "the original moved on; the fork must not have"
    );
    // ...and symmetrically, the original kept its own history.
    assert_eq!(store.leaf_path(conv, None).await.unwrap().len(), 3);
}

#[tokio::test]
async fn forking_mid_conversation_keeps_only_the_context_prior_to_the_split() {
    // "Go back to turn 1 and take a different path." The DAG could always reconstruct the prefix
    // before any node — `leaf_path(conv, Some(node))` — nothing had ever *asked* it to.
    let store = SessionStore::new();
    let conv = chat_with_turns(&store, &[("q1", "a1"), ("q2", "a2"), ("q3", "a3")]).await;

    let path = store.leaf_path(conv, None).await.unwrap();
    let after_first_answer = path[1].id; // q1, a1  ← branch here

    let fork = store
        .fork_session(conv, Some(after_first_answer), None)
        .await
        .unwrap();

    let copied = store.leaf_path(fork.id, None).await.unwrap();
    assert_eq!(
        copied
            .iter()
            .map(|n| n.message.content.clone())
            .collect::<Vec<_>>(),
        vec!["q1", "a1"],
        "everything after the split point must be left behind"
    );
    assert_eq!(fork.spawned_by, Some(after_first_answer));
    // The original is untouched and still has all three turns.
    assert_eq!(store.leaf_path(conv, None).await.unwrap().len(), 6);
}

#[tokio::test]
async fn a_fork_does_not_inherit_the_goal_it_was_forked_from() {
    // A goal session runs to a terminal status under a pack. Copying the goal would mint a session
    // claiming to run toward something with no pack running it — permanently `pending`, forever a
    // lie in the switcher. The transcript forks; the mandate does not.
    let store = SessionStore::new();
    let conv = chat_with_turns(&store, &[("q1", "a1")]).await;
    let fork = store.fork_session(conv, None, None).await.unwrap();
    assert!(fork.goal.is_none());
    assert_eq!(fork.status, SessionStatus::Running, "a chat is simply open");
}

#[tokio::test]
async fn forking_a_session_with_no_transcript_is_refused_rather_than_producing_an_empty_chat() {
    // A session in which *nothing was said*. Since packs record their dialogue as turns (#3), this
    // is no longer the ordinary state of a goal session — it is a session that genuinely has no
    // transcript. Handing back an empty conversation would look like the fork had worked.
    let store = SessionStore::new();
    SessionRecordStore::insert(&store, GoalSessionRecord::new(goal_spec("capture a note"))).await;
    let goal_id = SessionRecordStore::list(&store).await[0]
        .id
        .parse()
        .unwrap();

    let err = store
        .fork_session(goal_id, None, None)
        .await
        .expect_err("forking a session that said nothing must fail loudly");
    assert!(format!("{err}").contains("nothing was said"), "got: {err}");
}

#[tokio::test]
async fn a_fork_survives_a_reopen_as_its_own_self_contained_log() {
    // The invariant copy buys: the fork's file stands alone. Reopening the store must rehydrate it
    // without needing the parent at all.
    let dir = tempfile::tempdir().unwrap();
    let fork_id = {
        let store = SessionStore::open(dir.path()).await;
        let conv = chat_with_turns(&store, &[("q1", "a1")]).await;
        store
            .fork_session(conv, None, Some("branch".into()))
            .await
            .unwrap()
            .id
    };

    let reopened = SessionStore::open(dir.path()).await;
    let fork = reopened
        .session(fork_id)
        .await
        .expect("the fork rehydrates");
    assert_eq!(fork.title.as_deref(), Some("branch"));
    let nodes = reopened.leaf_path(fork_id, None).await.unwrap();
    assert_eq!(
        nodes
            .iter()
            .map(|n| n.message.content.clone())
            .collect::<Vec<_>>(),
        vec!["q1", "a1"],
        "the copied transcript is in the fork's own file"
    );
}

// ── Pack turns (debt #3) ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_packs_turn_becomes_a_real_node_in_the_message_dag() {
    // The whole of #3. A pack's dialogue used to be `SessionEvent`s only — which meant it was not
    // searchable (`chat-search` matches message nodes) and the session could not be forked (forking
    // copies a node prefix, and a flat event log has no `parent_id`). `append_turn` closes both at
    // once, because they were always the same gap.
    let store = SessionStore::new();
    let s = store
        .create_session(NewSession {
            goal: Some(goal_spec("build a todo CLI")),
            ..Default::default()
        })
        .await;
    let id = s.id.to_string();

    // A pack asks, a human answers, the pack reports.
    store
        .append_turn(&id, TurnAuthor::Assistant, "Rust or Node?".into())
        .await;
    store
        .append_turn(&id, TurnAuthor::User, "Rust".into())
        .await;
    store
        .append_turn(
            &id,
            TurnAuthor::Assistant,
            "contract frozen (4 verifiers)".into(),
        )
        .await;

    // It is a DAG, parent-linked — not a flat list. That is what forking needs.
    let path = ConversationStore::leaf_path(&store, s.id, None)
        .await
        .unwrap();
    assert_eq!(
        path.iter()
            .map(|n| n.message.content.clone())
            .collect::<Vec<_>>(),
        vec!["Rust or Node?", "Rust", "contract frozen (4 verifiers)"],
    );
    assert!(path[0].parent_id.is_none(), "the first turn is the root");
    assert_eq!(
        path[1].parent_id,
        Some(path[0].id),
        "the store parents each turn itself"
    );
    assert_eq!(path[2].parent_id, Some(path[1].id));

    // The identities survive, so a replay reconstructs who said what.
    assert_eq!(path[0].author, Author::Assistant);
    assert_eq!(path[1].author, Author::User);
}

#[tokio::test]
async fn a_goal_session_can_now_be_forked_because_it_has_turns() {
    // Before #3 this was a 400: "goal sessions record events, not turns". Forking a *coding* session
    // at its freeze point — contract A vs contract B — is the valuable version of forking, and this
    // is the change that makes it representable at all.
    let store = SessionStore::new();
    let s = store
        .create_session(NewSession {
            goal: Some(goal_spec("build a todo CLI")),
            ..Default::default()
        })
        .await;
    let id = s.id.to_string();
    for (who, what) in [
        (TurnAuthor::Assistant, "Rust or Node?"),
        (TurnAuthor::User, "Rust"),
        (TurnAuthor::Assistant, "draft contract A"),
    ] {
        store.append_turn(&id, who, what.into()).await;
    }

    // Branch at the human's answer: keep the negotiation up to that point, drop contract A.
    let path = ConversationStore::leaf_path(&store, s.id, None)
        .await
        .unwrap();
    let fork = store
        .fork_session(s.id, Some(path[1].id), Some("contract B".into()))
        .await
        .expect("a goal session with a transcript is forkable");

    let branched = ConversationStore::leaf_path(&store, fork.id, None)
        .await
        .unwrap();
    assert_eq!(
        branched
            .iter()
            .map(|n| n.message.content.clone())
            .collect::<Vec<_>>(),
        vec!["Rust or Node?", "Rust"],
        "the fork inherits the negotiation and leaves contract A behind"
    );
    assert_eq!(fork.parent_session, Some(s.id));
    // The original still has its own contract A — snapshot semantics hold for goal sessions too.
    assert_eq!(
        ConversationStore::leaf_path(&store, s.id, None)
            .await
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn a_turn_for_an_unknown_session_is_dropped_rather_than_inventing_one() {
    let store = SessionStore::new();
    store
        .append_turn(
            &ulid::Ulid::new().to_string(),
            TurnAuthor::User,
            "into the void".into(),
        )
        .await;
    assert!(store.list_sessions().await.is_empty());
}

// ── delete ───────────────────────────────────────────────────────────────────

/// The point of `delete` is that it is not a hide: the log leaves the disk, so a reopened store
/// does not bring the conversation back. Asserted by reopening, which is the only check that can
/// tell a real delete from an in-memory eviction.
#[tokio::test]
async fn delete_removes_the_log_from_disk_and_it_does_not_come_back() {
    let dir = tempfile::tempdir().unwrap();

    let (kept, gone) = {
        let store = SessionStore::open(dir.path()).await;
        let kept = store
            .create_session(NewSession {
                title: Some("kept".into()),
                ..Default::default()
            })
            .await;
        let gone = store
            .create_session(NewSession {
                title: Some("gone".into()),
                ..Default::default()
            })
            .await;
        ConversationStore::append(&store, gone.id, user_node(None, "secret"))
            .await
            .unwrap();

        let path = dir.path().join(format!("{}.jsonl", gone.id));
        assert!(path.exists(), "the log should exist before deleting");

        ConversationStore::delete(&store, gone.id).await.unwrap();

        assert!(!path.exists(), "delete must remove the log from disk");
        assert_eq!(store.list_sessions().await.len(), 1);
        (kept.id, gone.id)
    };

    // Reopen from the same directory: a soft-delete would resurrect it here.
    let store = SessionStore::open(dir.path()).await;
    let ids: Vec<_> = store
        .list_sessions()
        .await
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, vec![kept], "only the kept session should replay");
    assert!(
        ConversationStore::header(&store, gone).await.is_err(),
        "the deleted conversation must not be readable after a reopen"
    );
}

/// Deleting twice reports `NotFound` rather than silently succeeding — the same contract
/// `set_title` has, so a caller can tell "I removed it" from "it was never there".
#[tokio::test]
async fn deleting_a_missing_conversation_is_not_found() {
    let store = SessionStore::open(tempfile::tempdir().unwrap().path()).await;
    let s = store.create_session(NewSession::default()).await;

    ConversationStore::delete(&store, s.id).await.unwrap();
    let again = ConversationStore::delete(&store, s.id).await;

    assert!(
        matches!(
            again,
            Err(liberado_conversation_store::StoreError::NotFound(_))
        ),
        "second delete should be NotFound, got {again:?}"
    );
}

/// The chat sidebar shows conversations, not the machinery a conversation spawns.
///
/// One store holds every session (D7), so a chat that delegates sits in the same id space as the
/// dispatch sessions it spawns. Until 2026-08-01 the chat lens returned all of them: asking one
/// question put four rows in the sidebar — the question, and three internal agent prompts
/// indistinguishable from it. The session lens must still see everything; that is its job.
#[tokio::test]
async fn the_chat_lens_hides_background_sessions() {
    let store = SessionStore::new();
    let chat = store
        .create_session(NewSession {
            title: Some("what's in the news?".into()),
            ..Default::default()
        })
        .await;
    let dispatched = store
        .create_session(NewSession {
            goal: Some(goal_spec("Get the latest top news headlines right now.")),
            parent_session: Some(chat.id),
            visibility: Visibility::Background,
            ..Default::default()
        })
        .await;

    let sidebar = ConversationStore::list(&store).await.unwrap();
    let ids: Vec<_> = sidebar.iter().map(|h| h.id).collect();
    assert!(
        ids.contains(&chat.id),
        "the human's own chat must be listed"
    );
    assert!(
        !ids.contains(&dispatched.id),
        "a background dispatch session must not appear as a conversation"
    );

    // The session lens is unchanged — hiding it from the sidebar must not make it unobservable.
    let all: Vec<_> = store.list_sessions().await.iter().map(|h| h.id).collect();
    assert!(all.contains(&chat.id) && all.contains(&dispatched.id));
}

/// A foreground session with a parent is still a conversation. The filter keys on visibility, not
/// on having been spawned — a session promoted from a branch, or an interactive specialist a human
/// is attending, has a parent and still belongs in the sidebar.
#[tokio::test]
async fn a_foreground_child_still_lists_as_a_conversation() {
    let store = SessionStore::new();
    let parent = store.create_session(NewSession::default()).await;
    let attended = store
        .create_session(NewSession {
            goal: Some(goal_spec("interactive specialist the human joined")),
            parent_session: Some(parent.id),
            visibility: Visibility::Foreground,
            ..Default::default()
        })
        .await;

    let ids: Vec<_> = ConversationStore::list(&store)
        .await
        .unwrap()
        .iter()
        .map(|h| h.id)
        .collect();
    assert!(
        ids.contains(&attended.id),
        "being spawned is not what disqualifies a session — being unattended is"
    );
}

/// `set_status` must persist the new status so it survives a replay. A mutation that turns the
/// method into a no-op would leave the status at its default (`Pending`), which is invisible until
/// the next `open` — but the in-memory map is also stale, so a direct read catches it too.
///
/// **Mutant campaign note (Debian):** `cargo mutants` on Debian finds 3 misses (50 caught,
/// 53 viable): `write_lock_for` (line 244), `sweep_ephemeral` `delete !` (line 356), and the
/// `delete` guard (line 778). The `set_status` timeout is now caught by this test. The
/// Windows ledger entry (commit `82b2855`) reports 2 survived — the `write_lock_for` miss
/// appears to be platform-dependent (cargo-mutants may classify it differently across hosts).
#[tokio::test]
async fn set_status_persists_the_new_status() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).await;
    let goal = store
        .create_session(NewSession {
            goal: Some(goal_spec("track status changes")),
            ..Default::default()
        })
        .await;

    assert_eq!(
        store.session(goal.id).await.unwrap().status,
        SessionStatus::Pending
    );

    SessionRecordStore::set_status(&store, &goal.id.to_string(), SessionStatus::Running).await;

    assert_eq!(
        store.session(goal.id).await.unwrap().status,
        SessionStatus::Running
    );

    // A terminal status survives a reopen — the Status record was appended to the JSONL log.
    // (Non-terminal statuses are coerced to Failed on replay for goal sessions.)
    SessionRecordStore::set_status(&store, &goal.id.to_string(), SessionStatus::Succeeded).await;
    drop(store);
    let store = SessionStore::open(dir.path()).await;
    assert_eq!(
        store.session(goal.id).await.unwrap().status,
        SessionStatus::Succeeded
    );
}
