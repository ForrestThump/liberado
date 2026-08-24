//! Stdin-dispatch loop tests (moved verbatim from main.rs).

#![allow(unused_imports)]

use super::*;
use crate::provider::catalog_model_ids;
use liberado_provider::MockProvider;
use tempfile::TempDir;

use super::test_support::*;
#[test]
fn extract_prompt_joins_text_blocks() {
    let params = json!({
        "sessionId": "s1",
        "prompt": [
            { "type": "text", "text": "hello " },
            { "type": "text", "text": "world" }
        ]
    });
    assert_eq!(extract_prompt_text(&params).unwrap(), "hello \nworld");
}

#[tokio::test]
async fn a_permission_reply_wakes_the_waiter() {
    let bridge = test_bridge();
    let rx = bridge.permissions.register_waiter("lib-perm-1");
    let msg: JsonRpcIncoming = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":"lib-perm-1","result":{"outcome":{"outcome":"selected","optionId":"once"}}}"#,
        )
        .unwrap();
    assert!(
        apply_client_rpc_reply(&bridge, &msg),
        "a JSON-RPC result with no method is a client reply"
    );
    let reply = rx.await.expect("waiter").expect("ok");
    assert_eq!(
        permission::parse_decision(&reply),
        permission::PermissionDecision::Once
    );
}

#[test]
fn a_session_prompt_is_not_a_permission_reply() {
    let bridge = test_bridge();
    let msg: JsonRpcIncoming =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"session/prompt","params":{}}"#)
            .unwrap();
    assert!(!apply_client_rpc_reply(&bridge, &msg));
}

#[tokio::test]
async fn wait_until_cancelled_resolves_when_flag_set() {
    let (tx, mut rx) = watch::channel(false);
    let waiter = tokio::spawn(async move {
        wait_until_cancelled(&mut rx).await;
    });
    // Give the waiter a chance to park on changed().
    tokio::task::yield_now().await;
    tx.send(true).expect("send cancel");
    tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
        .await
        .expect("cancel wait timed out")
        .expect("join");
}

#[tokio::test]
async fn wait_until_cancelled_sees_already_true() {
    let (_tx, mut rx) = watch::channel(true);
    tokio::time::timeout(
        std::time::Duration::from_millis(200),
        wait_until_cancelled(&mut rx),
    )
    .await
    .expect("must return immediately when already cancelled");
}

#[test]
fn stdin_read_ends_only_on_eof_or_error() {
    assert!(!stdin_read_done(&Ok(Some("{}".into()))));
    assert!(stdin_read_done(&Ok(None)), "EOF ends the reader");
    assert!(
        stdin_read_done(&Err(std::io::Error::other("broken pipe"))),
        "a read error ends the reader"
    );
}

#[test]
fn prompt_slot_check_routes_busy_missing_and_ready() {
    assert!(matches!(
        prompt_slot_check(true, &json!({ "sessionId": "s1" })),
        PromptSlot::Busy
    ));
    assert!(matches!(
        prompt_slot_check(false, &json!({})),
        PromptSlot::MissingSessionId
    ));
    assert!(
        matches!(
            prompt_slot_check(false, &json!({ "sessionId": 7 })),
            PromptSlot::MissingSessionId
        ),
        "a non-string id is invalid params"
    );
    assert!(
        matches!(
            prompt_slot_check(false, &json!({ "sessionId": "" })),
            PromptSlot::MissingSessionId
        ),
        "an empty session id is invalid params"
    );
    assert!(
        matches!(
            prompt_slot_check(false, &json!({ "sessionId": "   " })),
            PromptSlot::Ready(s) if s == "   "
        ),
        "only truly empty ids are rejected here; garbage reaches unknown-session"
    );
    assert!(
        matches!(
            prompt_slot_check(false, &json!({ "sessionId": "s1" })),
            PromptSlot::Ready(s) if s == "s1"
        ),
        "a valid id registers ready with the id carried through"
    );
}

#[tokio::test]
async fn prompt_join_maps_success_error_cancel_and_panic() {
    // Plain success passes through untouched.
    let ok = prompt_join_outcome(Ok(Ok(json!({ "stopReason": "end_turn" })))).unwrap();
    assert_eq!(ok["stopReason"], "end_turn");

    // A prompt task's own Err becomes -32603 with the message kept.
    let err = prompt_join_outcome(Ok(Err("model exploded".into()))).unwrap_err();
    assert_eq!(err.code, -32603);
    assert_eq!(err.message, "model exploded");

    // An aborted task is a cancelled turn, not an error.
    let cancelled = tokio::spawn(async { std::future::pending::<()>().await });
    cancelled.abort();
    let join_err = cancelled.await.unwrap_err();
    assert!(join_err.is_cancelled());
    let stop = prompt_join_outcome(Err(join_err)).unwrap();
    assert_eq!(stop["stopReason"], "cancelled");

    // Any other join failure (a panic) is an internal error, not a cancel.
    let panicked = tokio::spawn(async move { panic!("prompt task boom") });
    let join_err = panicked.await.unwrap_err();
    assert!(
        !join_err.is_cancelled(),
        "a panic must not be classified as a cancel"
    );
    let err = prompt_join_outcome(Err(join_err)).unwrap_err();
    assert_eq!(err.code, -32603);
    assert!(err.message.contains("prompt task failed"));
}

#[tokio::test]
async fn dispatch_answers_unknown_methods_with_method_not_found_code() {
    let bridge = test_bridge();
    let sink = Arc::new(CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let wire: Arc<dyn WireSink> = Arc::clone(&sink) as Arc<dyn WireSink>;
    let msg: JsonRpcIncoming =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":5,"method":"session/nope"}"#).unwrap();
    let mut in_flight: Option<InFlightPrompt> = None;
    dispatch_stdin_message(&bridge, &wire, msg, &mut in_flight)
        .await
        .expect("routing must succeed even when the method does not exist");
    let captured = sink.lines.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].0, "response");
    assert_eq!(captured[0].1["id"], 5);
    assert_eq!(captured[0].1["error"]["code"], -32601);
    assert!(
        captured[0].1["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with(METHOD_NOT_FOUND_PREFIX)
    );
}

#[tokio::test]
async fn dispatch_maps_handler_failures_to_internal_error_code() {
    let bridge = test_bridge();
    let sink = Arc::new(CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let wire: Arc<dyn WireSink> = Arc::clone(&sink) as Arc<dyn WireSink>;
    let msg: JsonRpcIncoming =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":"r1","method":"session/load","params":{}}"#)
            .unwrap();
    let mut in_flight: Option<InFlightPrompt> = None;
    dispatch_stdin_message(&bridge, &wire, msg, &mut in_flight)
        .await
        .expect("routing succeeds; the handler's failure rides the response");
    let captured = sink.lines.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].1["error"]["code"], -32603);
    assert_eq!(captured[0].1["error"]["message"], "missing sessionId");
}

#[test]
fn new_session_ids_are_prefixed_unique_and_path_safe() {
    let a = new_session_id();
    let b = new_session_id();
    assert!(a.starts_with("lib-"), "{a}");
    assert_ne!(a, b);
    assert!(!a.contains(['/', '\\', ':']), "ids become filenames: {a}");
}

#[tokio::test]
async fn capturing_sink_delegates_responses_not_just_notifications() {
    let inner = Arc::new(CaptureSink::new_test());
    let outer = CapturingSink {
        inner: inner.clone() as Arc<dyn WireSink>,
        captured: std::sync::Mutex::new(String::new()),
    };
    outer
        .write_rpc_response(
            json!("r1"),
            Err(JsonRpcErrorBody {
                code: -32603,
                message: "boom".into(),
            }),
        )
        .expect("delegation must not fail");
    let lines = inner.lines.lock().unwrap();
    assert_eq!(
        lines.len(),
        1,
        "a swallowed response would strand the client"
    );
    assert_eq!(lines[0].0, "response");
}

#[tokio::test]
async fn finishing_a_prompt_clears_the_slot_and_answers() {
    let wire = CaptureSink::new_test();
    let task = tokio::spawn(async { Ok::<Value, String>(json!({ "stopReason": "end_turn" })) });
    let result = task.await.expect("task completes");
    let mut in_flight = Some(InFlightPrompt {
        session_id: "s1".into(),
        request_id: json!(9),
        handle: tokio::spawn(async { Ok::<Value, String>(json!({})) }),
    });
    handle_prompt_join(
        &wire,
        Some(("s1".into(), json!(9), Ok(result))),
        &mut in_flight,
    )
    .expect("join handling must not fail");
    assert!(
        in_flight.is_none(),
        "the slot must free for the next prompt"
    );
    let lines = wire.lines.lock().unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].1["result"]["stopReason"], "end_turn");
}

#[tokio::test]
async fn cancel_notification_aborts_only_the_matching_in_flight_prompt() {
    let bridge = test_bridge();
    let (in_flight, liveness) = session_with_pending_prompt(&bridge, "s1").await;
    let in_flight = Some(in_flight);

    // An unrelated method must not touch the slot.
    dispatch_notification(
        &bridge,
        "session/other",
        json!({ "sessionId": "s1" }),
        &in_flight,
    )
    .await;
    tokio::task::yield_now().await;
    assert!(
        !liveness.is_closed(),
        "non-cancel notifications are ignored"
    );

    // Cancel for a DIFFERENT session must leave this prompt running. The yield
    // gives a wrongly-armed abort time to land, so this pair of asserts kills
    // the ==->!= flip deterministically instead of racing it.
    dispatch_notification(
        &bridge,
        "session/cancel",
        json!({ "sessionId": "other" }),
        &in_flight,
    )
    .await;
    tokio::task::yield_now().await;
    assert!(
        !liveness.is_closed(),
        "another session's cancel must not stop this prompt"
    );

    // Cancel for THIS session hard-stops it and wakes permission waiters.
    let waiter = bridge.permissions.register_waiter("lib-perm-x");
    dispatch_notification(
        &bridge,
        "session/cancel",
        json!({ "sessionId": "s1" }),
        &in_flight,
    )
    .await;
    tokio::task::yield_now().await;
    assert!(
        liveness.is_closed(),
        "the matching in-flight prompt is aborted"
    );
    let reply = waiter.await.expect("waiter woken").expect("ok");
    assert_eq!(
        reply["outcome"]["outcome"], "cancelled",
        "a cancelled command prompt reports cancellation, not silence"
    );
}

#[tokio::test]
async fn cooperative_cancel_flag_is_raised_by_request_session_cancel() {
    let bridge = test_bridge();
    let (cancel_tx, mut cancel_rx) = watch::channel(false);
    let session_rx = cancel_rx.clone();
    bridge.acp_sessions.lock().await.insert(
        "s9".into(),
        AcpSession {
            mode: AgentMode::Chat,
            cwd: ".".into(),
            coding: coding_run::CodingSessionState {
                cwd: ".".into(),
                coding_session_id: "s9".into(),
                prior_feedback: Vec::new(),
                last_summary: None,
                rounds: 0,
            },
            converse: None,
            face_daemon_session: None,
            cancel_tx,
            cancel_rx: session_rx,
        },
    );
    request_session_cancel(&bridge, "s9").await;
    assert!(
        *cancel_rx.borrow_and_update(),
        "the turn loop's watch flag must read true after cancel"
    );
}

#[tokio::test]
async fn a_bare_jsonrpc_frame_with_an_id_is_dispatched_not_swallowed_as_a_reply() {
    // No method, no result, no error: not a client reply, so the dispatcher must answer
    // it (unknown method -> -32601). An over-broad reply check would swallow it silently.
    let bridge = test_bridge();
    let sink = Arc::new(CaptureSink::new_test());
    let wire: Arc<dyn WireSink> = Arc::clone(&sink) as Arc<dyn WireSink>;
    let msg: JsonRpcIncoming = serde_json::from_str(r#"{"jsonrpc":"2.0","id":42}"#).unwrap();
    let mut in_flight: Option<InFlightPrompt> = None;
    dispatch_stdin_message(&bridge, &wire, msg, &mut in_flight)
        .await
        .expect("routing succeeds");
    let captured = sink.lines.lock().unwrap();
    assert_eq!(captured.len(), 1, "the frame deserves an answer");
    assert_eq!(captured[0].1["error"]["code"], -32601);
}

#[test]
fn state_branch_names_the_branch_or_admits_detachment() {
    let repo = tempfile::TempDir::new().unwrap();
    let git = |args: &[&str]| {
        liberado_common::process::std_command("git")
            .args(args)
            .current_dir(repo.path())
            .output()
            .expect("git runs")
    };
    assert!(git(&["init", "-b", "feature-x"]).status.success());
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "Test"]);

    assert_eq!(state_branch(repo.path()), "feature-x");

    let plain = tempfile::TempDir::new().unwrap();
    assert_eq!(
        state_branch(plain.path()),
        "(detached)",
        "outside a repo the helper says so instead of inventing a branch"
    );
}
