//! Remaining main.rs behaviour tests (moved verbatim).

#![allow(unused_imports)]

use super::*;
use crate::provider::catalog_model_ids;
use liberado_provider::MockProvider;
use tempfile::TempDir;

use super::test_support::*;
#[test]
fn catalog_is_full_and_alphabetical() {
    let live = vec![
        "openai/gpt-4o".into(),
        "anthropic/claude-3.5-sonnet".into(),
        "deepseek/deepseek-v4-pro".into(),
        "deepseek/deepseek-chat".into(),
    ];
    let ordered = catalog_model_ids(&live, "deepseek/deepseek-v4-pro");
    assert_eq!(
        ordered,
        vec![
            "anthropic/claude-3.5-sonnet",
            "deepseek/deepseek-chat",
            "deepseek/deepseek-v4-pro",
            "openai/gpt-4o",
        ]
    );
}

#[test]
fn catalog_inserts_current_when_missing_from_live_then_sorts() {
    let live = vec!["openai/gpt-4o".into(), "anthropic/claude-3.5-sonnet".into()];
    let ordered = catalog_model_ids(&live, "deepseek/deepseek-v4-pro");
    assert_eq!(
        ordered,
        vec![
            "anthropic/claude-3.5-sonnet",
            "deepseek/deepseek-v4-pro",
            "openai/gpt-4o",
        ]
    );
}

/// A method the agent does not implement must answer -32601, not -32603.
///
/// A cold review pointed out every error used the same "Internal error" code, so a client
/// routing on it could not tell "you asked for something I do not implement" from "I broke".
#[tokio::test]
async fn an_unknown_method_is_method_not_found() {
    let bridge = test_bridge();
    let sink = CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    };
    let err = handle_request(bridge, "session/does_not_exist", json!({}), &sink)
        .await
        .expect_err("an unimplemented method must be an error");
    assert!(
        err.starts_with(METHOD_NOT_FOUND_PREFIX),
        "must be taggable as -32601 by the wire layer, got: {err}"
    );
}

#[tokio::test]
async fn mock_provider_turn_streams_paired_tool_and_text() {
    use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};

    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "tc1",
                "echo",
                json!({ "msg": "hi" }),
            )]),
            CompletionResponse::text("all done"),
        ],
    ));
    // Chat-path wire test: same SessionHandle / run_prompt_turn stack as mode=chat,
    // with a mock tool so we can assert tool_call id pairing on the ACP wire.
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let session = Arc::new(SessionHandle {
        id: "sess-mock".into(),
        conversation: Mutex::new(Conversation::new("test system")),
        executor: Executor::new(provider, Budget::new(8)),
        tools: Arc::new(EchoTool),
        coding_tools: false,
        pending_ask: std::sync::Mutex::new(None),
        cancel_tx,
        cancel_rx,
    });
    let sink = CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    };

    let stop = run_prompt_turn(session, "please echo".into(), &sink)
        .await
        .expect("turn");
    assert_eq!(stop, "end_turn");

    let lines = sink.lines.lock().unwrap().clone();
    assert!(
        !lines.is_empty(),
        "expected session/update notifications on the wire"
    );

    let tool_starts: Vec<&Value> = lines
        .iter()
        .filter(|(m, p)| m == "session/update" && p["update"]["sessionUpdate"] == "tool_call")
        .map(|(_, p)| p)
        .collect();
    let tool_updates: Vec<&Value> = lines
        .iter()
        .filter(|(m, p)| {
            m == "session/update" && p["update"]["sessionUpdate"] == "tool_call_update"
        })
        .map(|(_, p)| p)
        .collect();
    assert_eq!(tool_starts.len(), 1, "one tool_call: {lines:?}");
    assert_eq!(tool_updates.len(), 1, "one tool_call_update: {lines:?}");
    let start_id = tool_starts[0]["update"]["toolCallId"]
        .as_str()
        .expect("start id");
    let finish_id = tool_updates[0]["update"]["toolCallId"]
        .as_str()
        .expect("finish id");
    assert_eq!(
        start_id, finish_id,
        "MockProvider path must pair toolCallId (mutation target for P0.1)"
    );
    assert_eq!(tool_starts[0]["update"]["title"], "echo");
    assert_eq!(tool_updates[0]["update"]["status"], "completed");

    let text: String = lines
        .iter()
        .filter(|(m, p)| {
            m == "session/update" && p["update"]["sessionUpdate"] == "agent_message_chunk"
        })
        .filter_map(|(_, p)| p["update"]["content"]["text"].as_str())
        .collect();
    assert!(
        text.contains("all done"),
        "expected assistant text chunks, got {text:?} from {lines:?}"
    );
}

#[tokio::test]
async fn load_saved_session_restores_mode_and_model() {
    let dir = TempDir::new().unwrap();
    let _guards = lock_sessions_dir(&dir);

    let record = session_store::SessionRecord {
        id: "lib-load-test".into(),
        mode: "chat".into(),
        cwd: PathBuf::from("/tmp/test-project"),
        model: "gpt-4o".into(),
        messages: vec![],
        updated_at: session_store::new_timestamp(),
    };
    session_store::save(&record).expect("save");

    let bridge = test_bridge();
    let sink = CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    };
    let result = handle_request(
        bridge,
        "session/load",
        json!({"sessionId": "lib-load-test"}),
        &sink,
    )
    .await
    .expect("session/load must succeed");

    assert_eq!(result["sessionId"], "lib-load-test");
    assert_eq!(result["modes"]["currentModeId"], "chat");
    assert_eq!(result["models"]["currentModelId"], "gpt-4o");
}

#[tokio::test]
async fn load_saved_session_registers_in_memory_with_correct_cwd() {
    let dir = TempDir::new().unwrap();
    let _guards = lock_sessions_dir(&dir);

    let cwd = PathBuf::from("/tmp/load-cwd-test");
    let record = session_store::SessionRecord {
        id: "lib-cwd".into(),
        mode: "coding".into(),
        cwd: cwd.clone(),
        model: "mock-model".into(),
        messages: vec![],
        updated_at: session_store::new_timestamp(),
    };
    session_store::save(&record).expect("save");

    let bridge = test_bridge();
    let sink = CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    };
    let result = handle_request(
        Arc::clone(&bridge),
        "session/load",
        json!({"sessionId": "lib-cwd"}),
        &sink,
    )
    .await
    .expect("session/load must succeed");

    assert_eq!(result["sessionId"], "lib-cwd");

    let sessions = bridge.acp_sessions.lock().await;
    let sess = sessions.get("lib-cwd").expect("session must be registered");
    assert_eq!(sess.cwd, cwd, "cwd must match loaded record");
}

#[tokio::test]
async fn load_unsaved_id_is_clear_error_not_empty_session() {
    let dir = TempDir::new().unwrap();
    let _guards = lock_sessions_dir(&dir);

    let bridge = test_bridge();
    let sink = CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    };
    let err = handle_request(
        bridge,
        "session/load",
        json!({"sessionId": "no-such-id"}),
        &sink,
    )
    .await
    .expect_err("loading an unsaved id must be an error");

    assert!(
        err.contains("no saved session found"),
        "error must say no session was found, got: {err}"
    );
}

#[tokio::test]
async fn load_replays_stored_messages_in_stored_order() {
    let dir = TempDir::new().unwrap();
    let _guards = lock_sessions_dir(&dir);

    let record = session_store::SessionRecord {
        id: "lib-replay".into(),
        mode: "coding".into(),
        cwd: PathBuf::from("/tmp/replay"),
        model: "mock-model".into(),
        messages: vec![
            session_store::StoredMessage {
                role: "user".into(),
                content: "hello".into(),
            },
            session_store::StoredMessage {
                role: "assistant".into(),
                content: "hi there".into(),
            },
            session_store::StoredMessage {
                role: "user".into(),
                content: "second".into(),
            },
            session_store::StoredMessage {
                role: "assistant".into(),
                content: "answer".into(),
            },
        ],
        updated_at: session_store::new_timestamp(),
    };
    session_store::save(&record).expect("save");

    let bridge = test_bridge();
    let sink = CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    };
    let result = handle_request(
        bridge,
        "session/load",
        json!({"sessionId": "lib-replay"}),
        &sink,
    )
    .await
    .expect("session/load must succeed");

    assert_eq!(result["sessionId"], "lib-replay");

    let lines = sink.lines.lock().unwrap();
    let updates: Vec<_> = lines
        .iter()
        .filter(|(m, _)| m == "session/update")
        .collect();
    assert_eq!(
        updates.len(),
        4,
        "must emit exactly 4 message chunks, got {:?}",
        updates
    );

    assert_eq!(
        updates[0].1["update"]["sessionUpdate"], "user_message_chunk",
        "first message must be user"
    );
    assert_eq!(updates[0].1["update"]["content"]["text"], "hello");
    assert_eq!(
        updates[1].1["update"]["sessionUpdate"], "agent_message_chunk",
        "second message must be assistant"
    );
    assert_eq!(updates[1].1["update"]["content"]["text"], "hi there");
    assert_eq!(
        updates[2].1["update"]["sessionUpdate"], "user_message_chunk",
        "third message must be user"
    );
    assert_eq!(updates[2].1["update"]["content"]["text"], "second");
    assert_eq!(
        updates[3].1["update"]["sessionUpdate"], "agent_message_chunk",
        "fourth message must be assistant"
    );
    assert_eq!(updates[3].1["update"]["content"]["text"], "answer");
}

/// A resume the *model* can see, not just the editor.
///
/// Replaying the transcript to the client repaints the UI. If the conversation behind it starts
/// empty, the user reads their own history while the agent has none of it — the precise failure
/// `loadSession: false` existed to prevent, and worse once the flag says `true`, because the
/// interface now asserts the memory is there.
///
/// This is the requirement the original implementation skipped, and it skipped the test with
/// it: five tests covered the replay and none covered the restore, so everything looked green.
#[tokio::test]
async fn load_restores_history_into_the_conversation_not_only_the_client() {
    let dir = TempDir::new().unwrap();
    let _guards = lock_sessions_dir(&dir);

    let record = session_store::SessionRecord {
        id: "lib-memory".into(),
        mode: "chat".into(),
        cwd: PathBuf::from("/tmp/memory"),
        model: "mock-model".into(),
        messages: vec![
            session_store::StoredMessage {
                role: "user".into(),
                content: "my name is Ada".into(),
            },
            session_store::StoredMessage {
                role: "assistant".into(),
                content: "noted, Ada".into(),
            },
        ],
        updated_at: session_store::new_timestamp(),
    };
    session_store::save(&record).expect("save");

    let bridge = test_bridge();
    let sink = CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    };
    handle_request(
        bridge.clone(),
        "session/load",
        json!({"sessionId": "lib-memory"}),
        &sink,
    )
    .await
    .expect("session/load must succeed");

    let sessions = bridge.acp_sessions.lock().await;
    let chat = sessions
        .get("lib-memory")
        .and_then(|s| s.converse.clone())
        .expect("chat mode must have a live converse session after load");
    let convo = chat.conversation.lock().await;
    // `transient` is 0 on a freshly built conversation, so this is every message it holds.
    let messages = convo.turn_tail(0);
    let text: String = messages
        .iter()
        .map(|m| format!("{:?}:{}\n", m.role, m.content))
        .collect();

    assert!(
        text.contains("my name is Ada"),
        "the user's prior turn must be in the model's conversation: {text}"
    );
    assert!(
        text.contains("noted, Ada"),
        "the assistant's prior turn must be in the model's conversation: {text}"
    );
    assert!(
        messages
            .iter()
            .any(|m| matches!(m.role, liberado_provider::Role::System)),
        "the system prompt must survive the restore: {text}"
    );
}

#[tokio::test]
async fn two_converse_turns_keep_prior_replies() {
    use liberado_provider::{CompletionResponse, MockProvider};
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::text("first reply"),
            CompletionResponse::text("second reply"),
        ],
    ));
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let session = Arc::new(SessionHandle {
        id: "sess-turns".into(),
        conversation: Mutex::new(Conversation::new("sys")),
        executor: Executor::new(provider, Budget::new(8)),
        tools: Arc::new(NoTools),
        coding_tools: false,
        pending_ask: std::sync::Mutex::new(None),
        cancel_tx,
        cancel_rx,
    });
    let sink = CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    };
    let stop1 = run_prompt_turn(Arc::clone(&session), "one".into(), &sink)
        .await
        .unwrap();
    let stop2 = run_prompt_turn(Arc::clone(&session), "two".into(), &sink)
        .await
        .unwrap();
    assert_eq!(stop1, "end_turn");
    assert_eq!(stop2, "end_turn");
    let convo = session.conversation.lock().await;
    let text: String = convo
        .messages()
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("one") && text.contains("first reply"),
        "first turn must remain: {text}"
    );
    assert!(
        text.contains("two") && text.contains("second reply"),
        "second turn must append, not replace: {text}"
    );
}

#[tokio::test]
async fn ask_human_parks_and_the_next_prompt_is_the_answer() {
    use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "ask-1",
                ask_human::ASK_HUMAN_TOOL,
                json!({ "question": "Which crate?" }),
            )]),
            CompletionResponse::text("using acp-bridge"),
        ],
    ));
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let session = Arc::new(SessionHandle {
        id: "sess-ask".into(),
        conversation: Mutex::new(Conversation::new("sys")),
        executor: Executor::new(provider, Budget::new(8)),
        tools: ask_human::wrap(Arc::new(EchoTool), true),
        coding_tools: true,
        pending_ask: std::sync::Mutex::new(None),
        cancel_tx,
        cancel_rx,
    });
    let sink = CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    };
    let stop1 = run_prompt_turn(Arc::clone(&session), "split this".into(), &sink)
        .await
        .unwrap();
    assert_eq!(stop1, "end_turn");
    let parked = session.pending_ask.lock().unwrap().clone();
    assert_eq!(parked.as_deref(), Some("ask-1"));
    let stop2 = run_prompt_turn(Arc::clone(&session), "acp-bridge".into(), &sink)
        .await
        .unwrap();
    assert_eq!(stop2, "end_turn");
    assert!(session.pending_ask.lock().unwrap().is_none());
    let convo = session.conversation.lock().await;
    let text: String = convo
        .messages()
        .iter()
        .map(|m| format!("{:?}:{}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("acp-bridge"),
        "answer must land as the tool result: {text}"
    );
    assert!(
        text.contains("using acp-bridge"),
        "model must continue after the answer: {text}"
    );
}

#[tokio::test]
async fn coding_prompt_is_a_converse_turn_not_a_pack_run() {
    use liberado_provider::{CompletionResponse, MockProvider};
    let store = TempDir::new().unwrap();
    let _guards = lock_sessions_dir(&store);
    let cwd = TempDir::new().unwrap();
    let bridge = test_bridge_with(Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("hello from converse")],
    )));
    let created = handle_session_new(&bridge, &json!({ "cwd": cwd.path() }))
        .await
        .unwrap();
    let sid = created["sessionId"].as_str().unwrap().to_string();
    let sink: Arc<dyn WireSink> = Arc::new(CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let result = run_session_prompt(
        Arc::clone(&bridge),
        sink,
        json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": "hi" }] }),
    )
    .await
    .unwrap();
    assert_eq!(result["stopReason"], "end_turn");
    // Persistence captures agent_message_chunk text. The pack path announces itself;
    // converse does not.
    let stored = session_store::load(&sid)
        .ok()
        .flatten()
        .expect("prompt persists");
    let assistant: String = stored
        .messages
        .iter()
        .filter(|m| m.role == "assistant")
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        assistant.contains("hello from converse"),
        "the model reply must reach the wire: {assistant:?}"
    );
    assert!(
        !assistant.contains("Starting Liberado coding pack"),
        "coding mode must not fire the one-shot pack: {assistant:?}"
    );
}

#[tokio::test]
async fn coding_mode_attaches_coding_tools_on_a_real_workspace() {
    let store = TempDir::new().unwrap();
    let _guards = lock_sessions_dir(&store);
    let cwd = TempDir::new().unwrap();
    let bridge = test_bridge();
    let created = handle_session_new(&bridge, &json!({ "cwd": cwd.path() }))
        .await
        .unwrap();
    let sid = created["sessionId"].as_str().unwrap().to_string();
    assert_eq!(created["modes"]["currentModeId"], "coding");
    let handle = ensure_converse(&bridge, &sid).await.unwrap();
    assert!(
        handle.coding_tools,
        "interactive coding must attach the pack's tools"
    );
    let names: Vec<String> = handle.tools.catalog().into_iter().map(|t| t.name).collect();
    assert!(names.contains(&"read_file".into()), "{names:?}");
    assert!(
        !names.contains(&"submit_report".into()),
        "converse must not offer the pack terminator: {names:?}"
    );
    assert!(
        !names.contains(&"done".into()),
        "test bridge has no topology, so done must not appear: {names:?}"
    );
}

#[test]
fn snapshot_turns_keeps_user_and_assistant_prose() {
    let convo = Mutex::new(Conversation::from_history(vec![
        liberado_provider::Message::system("sys"),
        liberado_provider::Message::user("hello"),
        liberado_provider::Message::assistant("hi"),
        liberado_provider::Message::tool_result("t1", "ignored"),
        liberado_provider::Message::user(""),
    ]));
    let turns = snapshot_turns(&convo);
    assert_eq!(
        turns
            .iter()
            .map(|m| (m.role.as_str(), m.content.as_str()))
            .collect::<Vec<_>>(),
        vec![("user", "hello"), ("assistant", "hi")]
    );
}

#[test]
fn chat_system_prompt_uses_configured_text_when_present() {
    assert_eq!(
        chat_system_prompt(PathBuf::from("/ws").as_path(), Some("custom chat")),
        "custom chat"
    );
    let fallback = chat_system_prompt(PathBuf::from("/ws").as_path(), Some("  "));
    assert!(
        fallback.contains("no file tools"),
        "whitespace-only config must fall back: {fallback}"
    );
    assert!(fallback.contains("/ws"), "{fallback}");
}

#[tokio::test]
async fn ensure_converse_reapplies_the_project_cache_when_a_handle_is_reused() {
    let store = TempDir::new().unwrap();
    let _sessions = lock_sessions_dir(&store);
    let _env = TARGET_ENV_LOCK.lock().await;
    let saved = std::env::var("CARGO_TARGET_DIR").ok();
    let dir = TempDir::new().unwrap();
    let repo_a = dir.path().join("repo-a");
    let repo_b = dir.path().join("repo-b");
    std::fs::create_dir_all(&repo_a).unwrap();
    std::fs::create_dir_all(&repo_b).unwrap();
    let mut tuning = liberado_coder_core::CoderTuning::default();
    tuning.workspace_build.managed_target_root =
        Some(dir.path().join("managed").to_string_lossy().into_owned());
    let bridge = test_bridge_with_tuning(Arc::new(MockProvider::with_script("mock", [])), tuning);

    let sid_a = handle_session_new(&bridge, &json!({ "cwd": repo_a.display().to_string() }))
        .await
        .unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let sid_b = handle_session_new(&bridge, &json!({ "cwd": repo_b.display().to_string() }))
        .await
        .unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    let handle_a1 = ensure_converse(&bridge, &sid_a).await.unwrap();
    let cache_a = std::env::var("CARGO_TARGET_DIR").expect("session A cache");
    let _handle_b = ensure_converse(&bridge, &sid_b).await.unwrap();
    let cache_b = std::env::var("CARGO_TARGET_DIR").expect("session B cache");
    let handle_a2 = ensure_converse(&bridge, &sid_a).await.unwrap();
    let cache_a_again = std::env::var("CARGO_TARGET_DIR").expect("reused session A cache");

    match saved {
        Some(v) => unsafe { std::env::set_var("CARGO_TARGET_DIR", v) },
        None => unsafe { std::env::remove_var("CARGO_TARGET_DIR") },
    }

    assert!(
        Arc::ptr_eq(&handle_a1, &handle_a2),
        "the second prompt must reuse the existing converse handle"
    );
    assert_ne!(
        cache_a, cache_b,
        "two project roots must not share one ordinary cache"
    );
    assert_eq!(
        cache_a_again, cache_a,
        "a reused handle must reapply its own project cache, not inherit session B"
    );
}

#[tokio::test]
async fn ensure_converse_refuses_goal_mode() {
    let store = TempDir::new().unwrap();
    let _guards = lock_sessions_dir(&store);
    let cwd = TempDir::new().unwrap();
    let bridge = test_bridge();
    let created = handle_session_new(&bridge, &json!({ "cwd": cwd.path() }))
        .await
        .unwrap();
    let sid = created["sessionId"].as_str().unwrap().to_string();
    handle_set_mode(&bridge, &json!({ "sessionId": sid, "modeId": "goal" }))
        .await
        .unwrap();
    let err = ensure_converse(&bridge, &sid)
        .await
        .err()
        .expect("goal is not converse");
    assert!(
        err.contains("not a conversation"),
        "must refuse the pack mode, got: {err}"
    );
}

#[test]
fn help_text_names_the_modes_and_the_exit_free_flags() {
    let text = help_text();
    for mode in ["coding", "goal", "chat", "face"] {
        assert!(text.contains(mode), "help must list mode {mode}: {text}");
    }
    assert!(text.contains("--version"));
    assert!(text.contains("--mode"));
    assert!(text.contains("LIBERADO_ACP_MODE"));
    assert!(text.contains("Usage:"));
}

#[tokio::test]
async fn no_tools_offers_nothing_and_refuses_invocation() {
    assert!(NoTools.catalog().is_empty(), "chat has no file tools");
    let call = liberado_provider::ToolInvocation::new("1", "anything", json!({}));
    let err = NoTools
        .invoke(&call)
        .await
        .expect_err("chat must refuse tool calls, not answer them");
    assert!(err.contains("no coding tools"), "{err}");
}

#[tokio::test]
async fn a_second_prompt_while_busy_is_an_internal_error_response() {
    let h = SpawnHarness::new();
    let mut in_flight: Option<InFlightPrompt> = None;
    spawn_prompt_if_free(
        &h.bridge,
        &(Arc::clone(&h.sink) as Arc<dyn WireSink>),
        &json!({ "sessionId": "s1" }),
        json!(1),
        &mut in_flight,
    )
    .expect("first prompt spawns");
    assert!(in_flight.is_some());

    spawn_prompt_if_free(
        &h.bridge,
        &(Arc::clone(&h.sink) as Arc<dyn WireSink>),
        &json!({ "sessionId": "s2" }),
        json!(2),
        &mut in_flight,
    )
    .expect("the busy refusal itself must not fail");
    {
        let lines = h.sink.lines.lock().unwrap();
        assert_eq!(lines.len(), 1, "busy writes a response, spawns nothing");
        assert_eq!(lines[0].0, "response");
        assert_eq!(
            lines[0].1["error"]["code"], -32603,
            "wire code pinned as a literal so mutating the constant cannot satisfy its own assertion"
        );
    }
    if let Some(inf) = in_flight.as_mut() {
        inf.handle.abort();
        let _ = (&mut inf.handle).await;
    }
}

#[tokio::test]
async fn a_prompt_without_a_session_id_is_invalid_params() {
    let h = SpawnHarness::new();
    let mut in_flight: Option<InFlightPrompt> = None;
    for params in [
        json!({}),
        json!({ "sessionId": "" }),
        json!({ "sessionId": null }),
    ] {
        spawn_prompt_if_free(
            &h.bridge,
            &(Arc::clone(&h.sink) as Arc<dyn WireSink>),
            &params,
            json!("req-1"),
            &mut in_flight,
        )
        .unwrap();
    }
    let lines = h.sink.lines.lock().unwrap();
    assert_eq!(lines.len(), 3);
    for (method, body) in lines.iter() {
        assert_eq!(method, "response");
        assert_eq!(body["error"]["code"], -32602);
        assert_eq!(body["error"]["message"], "missing sessionId");
        assert_eq!(body["id"], "req-1");
    }
    assert!(in_flight.is_none());
}

#[tokio::test]
async fn a_ready_prompt_registers_its_session_and_request_id() {
    let h = SpawnHarness::new();
    let mut in_flight: Option<InFlightPrompt> = None;
    spawn_prompt_if_free(
        &h.bridge,
        &(Arc::clone(&h.sink) as Arc<dyn WireSink>),
        &json!({ "sessionId": "lib-spawn-ready" }),
        json!("req-9"),
        &mut in_flight,
    )
    .expect("ready spawns");
    let inf = in_flight.as_ref().expect("registered");
    assert_eq!(inf.session_id, "lib-spawn-ready");
    assert_eq!(inf.request_id, json!("req-9"));
    if let Some(inf) = in_flight.as_mut() {
        inf.handle.abort();
        let _ = (&mut inf.handle).await;
    }
}

#[tokio::test]
async fn stdin_lines_end_on_eof_and_survive_blanks_and_garbage() {
    let bridge = test_bridge();
    let wire: Arc<dyn WireSink> = Arc::new(CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let mut in_flight: Option<InFlightPrompt> = None;

    assert!(
        !handle_stdin_line(&bridge, &wire, None, &mut in_flight)
            .await
            .unwrap(),
        "a closed channel is EOF"
    );
    assert!(
        !handle_stdin_line(&bridge, &wire, Some(Ok(None)), &mut in_flight)
            .await
            .unwrap(),
        "an explicit EOF line ends the loop"
    );
    assert!(
        handle_stdin_line(&bridge, &wire, Some(Ok(Some("   ".into()))), &mut in_flight)
            .await
            .unwrap(),
        "blank lines are ignored, not fatal"
    );
    assert!(
        handle_stdin_line(
            &bridge,
            &wire,
            Some(Ok(Some("{not json".into()))),
            &mut in_flight
        )
        .await
        .unwrap(),
        "an unparseable line is logged and skipped"
    );
}

#[test]
fn the_reader_stops_on_either_a_dead_receiver_or_a_dead_wire() {
    assert!(!reader_should_stop(false, false));
    assert!(reader_should_stop(true, false), "closed receiver ends it");
    assert!(reader_should_stop(false, true), "EOF/error ends it");
    assert!(reader_should_stop(true, true));
}

/// A notification travelling the real dispatch path must neither get a response
/// nor be mistaken for a request. The is_notification `||`->`&&` flip turns this
/// very message into an unknown-method request and answers it.
#[tokio::test]
async fn a_notification_through_dispatch_gets_no_response_and_still_cancels() {
    let bridge = test_bridge();
    let sink = Arc::new(CaptureSink::new_test());
    let wire: Arc<dyn WireSink> = Arc::clone(&sink) as Arc<dyn WireSink>;
    let (in_flight, liveness) = session_with_pending_prompt(&bridge, "s1").await;
    let mut in_flight = Some(in_flight);

    let msg: JsonRpcIncoming = serde_json::from_str(
        r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}"#,
    )
    .unwrap();
    dispatch_stdin_message(&bridge, &wire, msg, &mut in_flight)
        .await
        .expect("routing a notification must not fail");

    tokio::task::yield_now().await;
    assert!(
        liveness.is_closed(),
        "the notification-shaped cancel must still cancel"
    );
    let captured = sink.lines.lock().unwrap();
    assert!(
        captured.is_empty(),
        "notifications expect no response at all: {captured:?}"
    );
}

/// `session/new` with an empty `cwd` string must fall back to the process
/// cwd: the filter exists so a blank string never becomes the workspace.
#[tokio::test]
async fn session_new_falls_back_when_cwd_is_an_empty_string() {
    let bridge = test_bridge();
    let resp = handle_session_new(&bridge, &json!({ "cwd": "" }))
        .await
        .expect("session/new succeeds");
    let sid = resp["sessionId"]
        .as_str()
        .expect("sessionId in the payload")
        .to_string();
    let sessions = bridge.acp_sessions.lock().await;
    let cwd = &sessions.get(&sid).expect("live session").cwd;
    assert!(
        !cwd.as_os_str().is_empty(),
        "an empty cwd string must fall back, got {cwd:?}"
    );
}
