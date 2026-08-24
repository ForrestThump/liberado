//! Split from `effects.rs` for module-health boundaries.

use super::*;
use liberado_theme::ThemeRegistry;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn make_app(server: &str) -> Arc<Mutex<App>> {
    let mut app = App::new(server.to_string(), ThemeRegistry::new());
    app.settings_path = None; // never touch the user's real config during tests
    Arc::new(Mutex::new(app))
}

fn make_runner(app: Arc<Mutex<App>>, action_tx: mpsc::Sender<Action>) -> EffectRunner {
    EffectRunner {
        app,
        should_quit: Arc::new(AtomicBool::new(false)),
        action_tx,
        client: reqwest::Client::new(),
        stream_state: Arc::new(Mutex::new(StreamState::default())),
    }
}

#[tokio::test]
async fn refresh_conversations_success() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/conversations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "c1",
                "title": "test",
                "created_at": "2025-06-25T12:00:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner.run(Effect::RefreshConversations).await;

    let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
        .await
        .expect("timeout waiting for action")
        .expect("channel closed");

    match action {
        Action::ConversationsUpdate(convs) => {
            assert_eq!(convs.len(), 1);
            assert_eq!(convs[0].id, "c1");
            assert_eq!(convs[0].title, Some("test".into()));
        }
        other => panic!("expected ConversationsUpdate, got {other:?}"),
    }
}

#[tokio::test]
async fn refresh_conversations_error() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/conversations"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner.run(Effect::RefreshConversations).await;

    // The error is only traced, no action sent
    let result = tokio::time::timeout(Duration::from_millis(500), action_rx.recv()).await;
    assert!(result.is_err(), "expected no action but got one");
}

#[tokio::test]
async fn load_conversation_history_success() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/c1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "messages": [
                {"role": "user", "content": "hi"}
            ]
        })))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner
        .run(Effect::LoadConversationHistory("c1".into()))
        .await;

    let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
        .await
        .expect("timeout waiting for action")
        .expect("channel closed");

    match action {
        Action::HistoryLoaded {
            id,
            messages,
            turn_running,
            turn_unanswered,
        } => {
            assert_eq!(id, "c1");
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].role, "user");
            assert_eq!(messages[0].content, "hi");
            assert!(!turn_running);
            assert!(!turn_unanswered);
        }
        other => panic!("expected HistoryLoaded, got {other:?}"),
    }
}

#[tokio::test]
async fn load_conversation_history_not_found() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/c1"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner
        .run(Effect::LoadConversationHistory("c1".into()))
        .await;

    let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
        .await
        .expect("timeout waiting for action")
        .expect("channel closed");

    match action {
        Action::SseFailed(msg) => {
            assert_eq!(msg, "conversation not found");
        }
        other => panic!("expected SseFailed('conversation not found'), got {other:?}"),
    }
}

#[tokio::test]
async fn cancel_stream_aborts_local_reader_without_conversation() {
    let app = make_app("http://localhost:0");
    let (action_tx, _action_rx) = mpsc::channel(256);
    let stream_state = Arc::new(Mutex::new(StreamState::default()));
    let runner = EffectRunner {
        app,
        should_quit: Arc::new(AtomicBool::new(false)),
        action_tx,
        client: reqwest::Client::new(),
        stream_state: stream_state.clone(),
    };

    // Store a real abort handle
    let handle = tokio::spawn(async {}).abort_handle();
    stream_state.lock().handle = Some(handle);

    runner
        .run(Effect::CancelStream { conversation: None })
        .await;

    assert!(
        stream_state.lock().handle.is_none(),
        "handle should be taken after CancelStream"
    );
}

/// Stop must hit the daemon cancel endpoint — local SSE abort alone is not enough for durable turns.
#[tokio::test]
async fn cancel_stream_posts_conversation_cancel() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/conversations/conv-abc/cancel"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, _action_rx) = mpsc::channel(256);
    let stream_state = Arc::new(Mutex::new(StreamState::default()));
    let runner = EffectRunner {
        app,
        should_quit: Arc::new(AtomicBool::new(false)),
        action_tx,
        client: reqwest::Client::new(),
        stream_state: stream_state.clone(),
    };
    let handle =
        tokio::spawn(async { tokio::time::sleep(Duration::from_secs(60)).await }).abort_handle();
    stream_state.lock().handle = Some(handle);

    runner
        .run(Effect::CancelStream {
            conversation: Some("conv-abc".into()),
        })
        .await;

    assert!(stream_state.lock().handle.is_none());
    // wiremock expect(1) asserts the cancel POST was made when the mock drops.
}

#[tokio::test]
async fn select_model_daemon_wide_omits_conversation_field() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/models/select"))
        .and(body_json(serde_json::json!({ "model": "m1" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "models": [],
            "current": "m1",
            "error": null
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner
        .run(Effect::SelectModel {
            model: "m1".into(),
            conversation: None,
        })
        .await;

    let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
        .await
        .expect("timeout")
        .expect("channel closed");
    match action {
        Action::ModelSelected {
            model,
            error: None,
            conversation_scoped: false,
        } => assert_eq!(model, "m1"),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn select_model_with_open_conversation_includes_conversation_field() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/models/select"))
        .and(body_json(serde_json::json!({
            "model": "m2",
            "conversation": "conv-99"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "models": [],
            "current": "m2",
            "error": null
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner
        .run(Effect::SelectModel {
            model: "m2".into(),
            conversation: Some("conv-99".into()),
        })
        .await;

    let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
        .await
        .expect("timeout")
        .expect("channel closed");
    match action {
        Action::ModelSelected {
            model,
            error: None,
            conversation_scoped: true,
        } => assert_eq!(model, "m2"),
        other => panic!("unexpected {other:?}"),
    }
}

/// Reattach must actually reach the daemon and decode what comes back.
///
/// The app-level tests assert only that `Effect::AttachConversationStream` is *emitted*.
/// Gutting the effect's body left all 278 tests passing — so the feature whose absence was
/// the regression had no coverage below the effect boundary at all.
#[tokio::test]
async fn attach_stream_replays_the_running_turn() {
    let mock_server = MockServer::start().await;
    // Replay-then-live, in the shared SSE vocabulary. `token` frames are what a live turn
    // emits; the attach endpoint replays them before continuing.
    let body = "event: token\ndata: re\n\n\
                event: token\ndata: joined\n\n\
                event: session_finished\ndata: {\"status\":\"done\",\"summary\":\"\"}\n\n";
    Mock::given(method("GET"))
        .and(path("/api/conversations/live-7/attach"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner
        .run(Effect::AttachConversationStream("live-7".into()))
        .await;

    let mut tokens = String::new();
    let mut saw_done = false;
    for _ in 0..8 {
        match tokio::time::timeout(Duration::from_secs(2), action_rx.recv()).await {
            Ok(Some(Action::SseToken(t))) => tokens.push_str(&t),
            Ok(Some(Action::SseDone)) => {
                saw_done = true;
                break;
            }
            Ok(Some(Action::SseFailed(e))) => panic!("attach failed: {e}"),
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!("timed out waiting for attach actions; got {tokens:?}"),
        }
    }
    assert_eq!(tokens, "rejoined", "replayed tokens must reach the app");
    assert!(saw_done, "the attach stream must terminate the turn");
}

/// A turn that finishes between reading `turn_running` and the attach request is the expected
/// race, not a fault. It must reload the transcript — the reply is on disk — rather than
/// blaming the user with `[error] attach refused (409)`.
#[tokio::test]
async fn attach_409_reloads_history_instead_of_reporting_an_error() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/done-3/attach"))
        .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
            "error": "no turn is running for this conversation"
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner
        .run(Effect::AttachConversationStream("done-3".into()))
        .await;

    let mut saw_reload = false;
    for _ in 0..4 {
        match tokio::time::timeout(Duration::from_secs(2), action_rx.recv()).await {
            Ok(Some(Action::SseFailed(e))) => {
                panic!("a finished turn must not surface as an error: {e}")
            }
            Ok(Some(Action::ReloadConversationHistory(id))) => {
                assert_eq!(id, "done-3");
                saw_reload = true;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert!(
        saw_reload,
        "409 must re-read the transcript so the reply that just landed is shown"
    );
}

#[tokio::test]
async fn load_history_forwards_turn_lifecycle_flags() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/c-run"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "messages": [{"role": "user", "content": "still going?"}],
            "turn_running": true,
            "turn_unanswered": false
        })))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner
        .run(Effect::LoadConversationHistory("c-run".into()))
        .await;

    let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
        .await
        .expect("timeout")
        .expect("channel closed");
    match action {
        Action::HistoryLoaded {
            id,
            turn_running: true,
            turn_unanswered: false,
            ..
        } => assert_eq!(id, "c-run"),
        other => panic!("expected running HistoryLoaded, got {other:?}"),
    }
}

#[tokio::test]
async fn fork_conversation_posts_the_branch_point_and_reports_the_new_session() {
    // This effect used to log "server support not yet available" and return. It now actually
    // forks — the branch point rides in the body as a turn number, not a node id, because a turn
    // is the thing a human can see and point at.
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sessions/c1/fork"))
        .and(body_json(serde_json::json!({ "after_turn": 2 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "c2",
            "forked_from": "c1",
            "kept_turns": 2,
            "total_turns": 5
        })))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner
        .run(Effect::ForkConversation {
            parent_id: "c1".into(),
            after_turn: Some(2),
        })
        .await;

    let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
        .await
        .expect("timeout waiting for the fork result")
        .expect("channel closed");

    match action {
        Action::Forked(fork) => {
            assert_eq!(fork.id, "c2");
            assert_eq!(fork.kept_turns, 2);
            assert_eq!(fork.total_turns, 5);
        }
        other => panic!("expected Forked, got {other:?}"),
    }
}

#[tokio::test]
async fn a_failed_fork_surfaces_the_servers_own_reason() {
    // "session has no message transcript to fork" is the useful message; a bare 400 is not.
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sessions/g1/fork"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "session g1 has no message transcript to fork"
        })))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner
        .run(Effect::ForkConversation {
            parent_id: "g1".into(),
            after_turn: None,
        })
        .await;

    let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
        .await
        .expect("timeout")
        .expect("channel closed");

    match action {
        Action::SseFailed(msg) => assert!(
            msg.contains("no message transcript"),
            "the server's reason must reach the human, got: {msg}"
        ),
        other => panic!("expected SseFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn quit_sets_should_quit() {
    let should_quit = Arc::new(AtomicBool::new(false));
    let app = make_app("http://localhost:0");
    let (action_tx, _action_rx) = mpsc::channel(256);
    let runner = EffectRunner {
        app,
        should_quit: should_quit.clone(),
        action_tx,
        client: reqwest::Client::new(),
        stream_state: Arc::new(Mutex::new(StreamState::default())),
    };

    runner.run(Effect::Quit).await;

    assert!(
        should_quit.load(Ordering::Relaxed),
        "should_quit should be true after Quit"
    );
}

#[test]
fn sse_text_chunk_decodes_events_into_actions() {
    let mut decoder = SseDecoder::default();
    let (actions, terminal) = sse_actions_from_text(
        &mut decoder,
        "event: token
data: hello

",
    );
    assert!(!terminal);
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], Action::SseToken(_)));
}

#[test]
fn sse_text_chunk_stops_at_session_finished() {
    let mut decoder = SseDecoder::default();
    let text = "event: token\ndata: hi\n\nevent: session_finished\ndata: {\"status\":\"ok\",\"summary\":\"done\"}\n\n";
    let (actions, terminal) = sse_actions_from_text(&mut decoder, text);
    assert!(terminal);
    assert_eq!(actions.len(), 2);
    assert!(matches!(actions[0], Action::SseToken(_)));
    assert!(matches!(actions[1], Action::SseDone));
}

#[test]
fn sse_text_chunk_flags_failed_as_terminal() {
    let mut decoder = SseDecoder::default();
    let (actions, terminal) = sse_actions_from_text(
        &mut decoder,
        "event: failed
data: {\"message\":\"boom\"}

",
    );
    assert!(terminal);
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], Action::SseFailed(_)));
}

#[test]
fn sse_text_chunk_unknown_kind_is_a_benign_noop() {
    let mut decoder = SseDecoder::default();
    let (actions, terminal) = sse_actions_from_text(
        &mut decoder,
        "event: no_such_event
data: x

",
    );
    assert!(!terminal);
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], Action::SseToken(_)));
}

#[test]
fn sse_text_chunk_malformed_known_event_yields_sse_failed() {
    let mut decoder = SseDecoder::default();
    let (actions, terminal) = sse_actions_from_text(
        &mut decoder,
        "event: tool_started
data: not-json

",
    );
    assert!(terminal);
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], Action::SseFailed(_)));
}

#[test]
fn goal_chunk_forwards_token_and_finished() {
    let mut decoder = SseDecoder::default();
    let text = "event: session_started\ndata: {\"domain\":\"coding\",\"description\":\"fix tests\"}\n\nevent: session_finished\ndata: {\"status\":\"ok\",\"summary\":\"done\"}\n\n";
    let (actions, terminal) = EffectRunner::goal_actions_from_chunk(&mut decoder, text);
    assert!(terminal);
    assert_eq!(actions.len(), 2);
    assert!(matches!(actions[0], Action::GoalStreamEvent(_)));
    assert!(matches!(actions[1], Action::GoalStreamEvent(_)));
}

#[test]
fn goal_chunk_token_is_not_terminal() {
    let mut decoder = SseDecoder::default();
    let (actions, terminal) =
        EffectRunner::goal_actions_from_chunk(&mut decoder, "event: token\ndata: hello\n\n");
    assert!(!terminal);
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], Action::GoalStreamEvent(_)));
}

#[test]
fn goal_chunk_unknown_kind_is_benign() {
    let mut decoder = SseDecoder::default();
    let (actions, terminal) =
        EffectRunner::goal_actions_from_chunk(&mut decoder, "event: no_such_event\ndata: x\n\n");
    assert!(!terminal);
    // The chat wire decodes unknown event types to an empty Token no-op (never an error).
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], Action::GoalStreamEvent(_)));
}

// ── run_goal_stream (goal-session SSE drain) ───────────────────────────────

#[tokio::test]
async fn run_goal_stream_emits_chunk_tokens_then_closes_on_eof() {
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let state = Arc::new(Mutex::new(StreamState::default()));
    let dummy = tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    });
    state.lock().goal_handle = Some(dummy.abort_handle());

    let stream = futures::stream::iter(vec![
        Ok::<_, String>(
            b"event: session_started\ndata: {\"domain\":\"g\",\"description\":\"d\"}\n\n".to_vec(),
        ),
        Ok::<_, String>(b"event: token\ndata: hello\n\n".to_vec()),
    ]);
    EffectRunner::run_goal_stream(
        &action_tx,
        &state,
        stream,
        &mut crate::sse::SseDecoder::default(),
    )
    .await;

    let mut msgs = Vec::new();
    while let Ok(m) = action_rx.try_recv() {
        msgs.push(m);
    }
    // Started + one token, then the trailing close on clean EOF.
    assert!(matches!(
        msgs[0].clone(),
        Action::GoalStreamEvent(crate::app::GoalUiEvent::Started { .. })
    ));
    assert!(
        matches!(msgs[1].clone(), Action::GoalStreamEvent(crate::app::GoalUiEvent::Token(t)) if t == "hello")
    );
    assert!(matches!(
        msgs[2].clone(),
        Action::GoalStreamClosed(end) if end.is_none(),
    ));
    assert!(
        state.lock().goal_handle.is_none(),
        "handle cleared once the stream drains"
    );

    dummy.abort();
}

#[tokio::test]
async fn run_goal_stream_terminal_event_clears_handle_without_close() {
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let state = Arc::new(Mutex::new(StreamState::default()));
    let dummy = tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    });
    state.lock().goal_handle = Some(dummy.abort_handle());

    let stream = futures::stream::iter(vec![Ok::<_, String>(
        b"event: session_finished\ndata: {\"status\":\"done\",\"summary\":\"\"}\n\n".to_vec(),
    )]);
    EffectRunner::run_goal_stream(
        &action_tx,
        &state,
        stream,
        &mut crate::sse::SseDecoder::default(),
    )
    .await;

    let mut msgs = Vec::new();
    while let Ok(m) = action_rx.try_recv() {
        msgs.push(m);
    }
    // A terminal Finished short-circuits: the event arrives, no trailing close follows.
    assert!(matches!(
        msgs[0].clone(),
        Action::GoalStreamEvent(crate::app::GoalUiEvent::Finished { .. })
    ));
    // The terminal short-circuit means no trailing close ever appears on the wire.
    assert_eq!(
        msgs.len(),
        1,
        "only the terminal event is emitted (no trailing close)"
    );
    assert!(
        state.lock().goal_handle.is_none(),
        "handle cleared at terminal"
    );
    dummy.abort();
}

#[tokio::test]
async fn run_goal_stream_stream_error_sends_error_close() {
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let state = Arc::new(Mutex::new(StreamState::default()));
    let dummy = tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    });
    state.lock().goal_handle = Some(dummy.abort_handle());

    let stream = futures::stream::iter(vec![Err::<Vec<u8>, _>("boom")]);
    EffectRunner::run_goal_stream(
        &action_tx,
        &state,
        stream,
        &mut crate::sse::SseDecoder::default(),
    )
    .await;

    let mut msgs = Vec::new();
    while let Ok(m) = action_rx.try_recv() {
        msgs.push(m);
    }
    assert!(matches!(
        msgs[0].clone(),
        Action::GoalStreamClosed(message) if message == Some("stream error: boom".into())
    ));
    assert!(
        state.lock().goal_handle.is_none(),
        "handle cleared after stream error"
    );
    dummy.abort();
}

// ── run_async dispatch coverage: one test per previously-unrouted effect ────────────

#[path = "effects_test_util.rs"]
mod effects_test_util;
use effects_test_util::refused_daemon_url;

async fn next_action(action_rx: &mut mpsc::Receiver<Action>) -> Action {
    tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
        .await
        .expect("timeout waiting for action")
        .expect("channel closed")
}

#[tokio::test]
async fn refresh_sessions_populates_the_switcher() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/sessions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "g_01",
                "title": "a goal",
                "goal": {"kind": "coding", "description": "a goal"},
                "status": "running",
                "created_at": "2026-08-24T00:00:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner.run(Effect::RefreshSessions).await;
    match next_action(&mut action_rx).await {
        Action::SessionsUpdate(sessions) => {
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].id, "g_01");
        }
        other => panic!("expected SessionsUpdate, got {other:?}"),
    }
}

#[tokio::test]
async fn fetch_models_reports_the_catalog() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"models": ["m1", "m2"], "current": "m1"})),
        )
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner.run(Effect::FetchModels).await;
    match next_action(&mut action_rx).await {
        Action::ModelsLoaded { models, error } => {
            assert_eq!(models, vec!["m1".to_string(), "m2".to_string()]);
            assert_eq!(error, None);
        }
        other => panic!("expected ModelsLoaded, got {other:?}"),
    }
}

#[tokio::test]
async fn start_chat_stream_streams_tokens_then_done() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat/stream"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "event: session\r\ndata: c1\r\n\r\n",
                "event: token\r\ndata: hel\r\n\r\n",
                "event: token\r\ndata: lo\r\n\r\n",
                "event: session_finished\r\n",
                "data: {\"status\":\"done\",\"summary\":\"\"}\r\n\r\n"
            ),
            "text/event-stream",
        ))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app.clone(), action_tx);

    runner
        .run(Effect::StartChatStream {
            message: "hi".into(),
            session: None,
        })
        .await;
    // Give the spawned stream task a moment to drain; the actions arrive in order.
    let mut seen = Vec::new();
    for _ in 0..4 {
        seen.push(next_action(&mut action_rx).await);
    }
    assert!(
        matches!(seen.last(), Some(Action::SseDone)),
        "the terminal frame ends with SseDone: {seen:?}"
    );
    assert!(
        matches!(&seen[1], Action::SseToken(t) if t == "hel"),
        "{seen:?}"
    );
    assert!(
        matches!(&seen[2], Action::SseToken(t) if t == "lo"),
        "{seen:?}"
    );
    // The stream ended terminally: no live handle remains.
    assert!(runner.stream_state.lock().handle.is_none());
    let _ = app;
}

#[tokio::test]
async fn start_chat_stream_reports_an_error_status_instead_of_decoding_it() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat/stream"))
        .respond_with(ResponseTemplate::new(500).set_body_string("nope"))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner
        .run(Effect::StartChatStream {
            message: "hi".into(),
            session: None,
        })
        .await;
    let action = next_action(&mut action_rx).await;
    assert!(
        matches!(&action, Action::SseFailed(msg) if msg.contains("server returned 500")),
        "{action:?}"
    );
    assert!(runner.stream_state.lock().handle.is_none());
}

#[tokio::test]
async fn start_chat_stream_unreachable_daemon_fails_the_stream() {
    // Nothing listens there, so connect fails on every platform; how fast
    // is platform-dependent (see refused_daemon_url).
    let app = make_app(&refused_daemon_url());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner
        .run(Effect::StartChatStream {
            message: "hi".into(),
            session: None,
        })
        .await;
    let action = tokio::time::timeout(Duration::from_secs(15), action_rx.recv())
        .await
        .expect("the failed stream must report back")
        .expect("channel closed");
    assert!(
        matches!(&action, Action::SseFailed(msg) if msg.contains("could not reach daemon")),
        "{action:?}"
    );
    assert!(runner.stream_state.lock().handle.is_none());
}

#[tokio::test]
async fn join_goal_session_streams_goal_events_until_finished() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/goals/g_7/stream"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "event: progress\r\ndata: {\"message\":\"working\"}\r\n\r\n",
                "event: failed\r\ndata: {\"message\":\"boom\"}\r\n\r\n"
            ),
            "text/event-stream",
        ))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner.run(Effect::JoinGoalSession("g_7".into())).await;
    let mut saw_progress = false;
    loop {
        match next_action(&mut action_rx).await {
            Action::GoalStreamEvent(crate::app::GoalUiEvent::Progress(_)) => saw_progress = true,
            Action::GoalStreamEvent(crate::app::GoalUiEvent::Finished { .. }) => break,
            Action::GoalStreamClosed(_) => break,
            other => panic!("unexpected action {other:?}"),
        }
    }
    assert!(saw_progress, "progress frames reached the app");
}

#[tokio::test]
async fn send_goal_message_surfaces_the_outcome() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/goals/g_9/message"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner
        .run(Effect::SendGoalMessage {
            id: "g_9".into(),
            text: "answer".into(),
        })
        .await;
    match next_action(&mut action_rx).await {
        Action::GoalMessageOutcome(outcome) => assert_eq!(
            outcome,
            crate::api::GoalMessageOutcome::Accepted,
            "a 202 is an accepted outcome: {outcome:?}"
        ),
        other => panic!("expected GoalMessageOutcome, got {other:?}"),
    }
}

#[tokio::test]
async fn spawn_goal_session_hands_back_the_new_id() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/goals"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"session_id": "g_new"})),
        )
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner
        .run(Effect::SpawnGoalSession {
            domain: "life".into(),
            goal: "plan the week".into(),
            origin_conversation: None,
        })
        .await;
    match next_action(&mut action_rx).await {
        Action::GoalSpawned {
            session_id,
            domain,
            description,
        } => {
            assert_eq!(session_id, "g_new");
            assert_eq!(domain, "life");
            assert_eq!(description, "plan the week");
        }
        other => panic!("expected GoalSpawned, got {other:?}"),
    }
}

#[tokio::test]
async fn start_coding_goal_spawns_into_the_coding_domain() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/goals"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"session_id": "g_code"})),
        )
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner
        .run(Effect::StartCodingGoal {
            project: None,
            text: "fix the bug".into(),
            mode: None,
            origin_conversation: None,
        })
        .await;
    match next_action(&mut action_rx).await {
        Action::GoalSpawned {
            session_id, domain, ..
        } => {
            assert_eq!(session_id, "g_code");
            assert_eq!(domain, "coding");
        }
        other => panic!("expected GoalSpawned, got {other:?}"),
    }
}

#[tokio::test]
async fn park_goal_session_reports_the_outcome_as_a_system_message() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/goals/g_3/park"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner.run(Effect::ParkGoalSession("g_3".into())).await;
    match next_action(&mut action_rx).await {
        Action::SystemMessage(msg) => {
            assert!(msg.contains("parked"), "{msg}");
        }
        other => panic!("expected SystemMessage, got {other:?}"),
    }
}

#[tokio::test]
async fn resume_goal_session_without_an_answer_explains_itself() {
    let app = make_app("http://127.0.0.1:1"); // must never be contacted
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app, action_tx);

    runner
        .run(Effect::ResumeGoalSession {
            id: "g_4".into(),
            answer: String::new(),
        })
        .await;
    match next_action(&mut action_rx).await {
        Action::SystemMessage(msg) => assert!(msg.contains("Usage"), "{msg}"),
        other => panic!("expected usage hint, got {other:?}"),
    }
}

#[tokio::test]
async fn leave_goal_session_aborts_a_live_subscription() {
    let app = make_app("http://127.0.0.1:1");
    let (action_tx, mut action_rx) = mpsc::channel(256);
    let runner = make_runner(app.clone(), action_tx);

    // A live handle in the joined state, as if /join had subscribed.
    let handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    runner.stream_state.lock().goal_handle = Some(handle.abort_handle());

    runner.run(Effect::LeaveGoalSession).await;
    // The handle is gone from state and the task was aborted (join errors).
    assert!(runner.stream_state.lock().goal_handle.is_none());
    let joined = tokio::time::timeout(Duration::from_secs(2), handle).await;
    assert!(
        matches!(joined, Ok(Err(_))),
        "the subscription task aborts: {joined:?}"
    );

    // And nothing was ever sent to the app.
    assert!(action_rx.try_recv().is_err());
}
