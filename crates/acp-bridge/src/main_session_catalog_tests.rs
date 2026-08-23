//! Session, catalog and request-handler tests (moved verbatim from main.rs).

#![allow(unused_imports)]

use super::*;
use crate::provider::catalog_model_ids;
use liberado_provider::MockProvider;
use tempfile::TempDir;

use super::test_support::*;
#[test]
fn session_new_payload_has_models_and_modes() {
    let catalog = vec![
        CatalogModel {
            model_id: "deepseek/deepseek-v4-pro".into(),
            name: "deepseek/deepseek-v4-pro".into(),
            description: "OpenRouter · deepseek/deepseek-v4-pro".into(),
        },
        CatalogModel {
            model_id: "deepseek/deepseek-v4-flash".into(),
            name: "deepseek/deepseek-v4-flash".into(),
            description: "OpenRouter · deepseek/deepseek-v4-flash".into(),
        },
    ];
    let v = session_state_payload(
        "sid",
        &catalog,
        "deepseek/deepseek-v4-pro",
        AgentMode::Coding,
        &liberado_common::CapabilitySet::empty(),
    );
    assert_eq!(v["sessionId"], "sid");
    assert_eq!(v["models"]["currentModelId"], "deepseek/deepseek-v4-pro");
    assert_eq!(v["models"]["availableModels"].as_array().unwrap().len(), 2);
    assert_eq!(
        v["models"]["availableModels"][1]["modelId"],
        "deepseek/deepseek-v4-flash"
    );
    assert_eq!(v["modes"]["currentModeId"], "coding");
    assert_eq!(v["modes"]["availableModes"].as_array().unwrap().len(), 4);
    let ids: Vec<&str> = v["modes"]["availableModes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert_eq!(ids, ["coding", "goal", "chat", "face"]);
}

#[tokio::test]
async fn initialize_shape_is_acp_compatible() {
    // Drives the real handler. The previous version built its own JSON literal and asserted on
    // that — it "mirrored the handle_request arm" by its own comment, so deleting the arm, or
    // dropping any field from the real response, left it green.
    let bridge = test_bridge();
    let sink = CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    };
    let result = handle_request(bridge, "initialize", json!({}), &sink)
        .await
        .expect("initialize must succeed");

    assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(result["agentInfo"]["name"], "Liberado");
    assert_eq!(result["agentCapabilities"]["loadSession"], true);
    assert_eq!(
        result["agentCapabilities"]["promptCapabilities"]["embeddedContext"],
        true
    );
}

#[test]
fn load_session_capability_is_honest() {
    const {
        assert!(
            LOAD_SESSION_CAPABILITY,
            "loadSession must be true now that durable resume is implemented; \
                 false would make Paseo think resume is unsupported"
        );
    }
}

#[tokio::test]
async fn chat_turn_stops_with_cancelled_on_cancel_flag() {
    use liberado_provider::{CompletionResponse, MockProvider};
    use std::time::Duration;

    // Slow first completion so cancel can win mid-turn.
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("should not finish")],
    ));
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let session = Arc::new(SessionHandle {
        id: "sess-cancel".into(),
        conversation: Mutex::new(Conversation::new("test system")),
        executor: Executor::new(provider, Budget::new(8)),
        tools: Arc::new(NoTools),
        coding_tools: false,
        pending_ask: std::sync::Mutex::new(None),
        cancel_tx: cancel_tx.clone(),
        cancel_rx,
    });
    let sink = CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    };

    // Cancel BEFORE the turn starts. The previous version slept 5ms and then cancelled, so
    // the fast mock almost always finished first and the assertion accepted `end_turn` —
    // which meant the test passed with the cancel path deleted entirely.
    cancel_tx.send(true).expect("cancel send");

    let turn = tokio::spawn({
        let session = Arc::clone(&session);
        async move { run_prompt_turn(session, "hello".into(), &sink).await }
    });

    let stop = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("turn join timeout")
        .expect("join")
        .expect("turn result");
    assert_eq!(
        stop, "cancelled",
        "a turn whose session was already cancelled must report `cancelled`"
    );
}

#[tokio::test]
async fn load_reloads_already_loaded_session_without_duplicate_emit() {
    let dir = TempDir::new().unwrap();
    let _guards = lock_sessions_dir(&dir);

    let record = session_store::SessionRecord {
        id: "lib-reload".into(),
        mode: "coding".into(),
        cwd: PathBuf::from("/tmp/reload"),
        model: "mock-model".into(),
        messages: vec![session_store::StoredMessage {
            role: "user".into(),
            content: "ping".into(),
        }],
        updated_at: session_store::new_timestamp(),
    };
    session_store::save(&record).expect("save");

    let bridge = test_bridge();
    let sink = CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    };
    // First load emits messages.
    handle_request(
        Arc::clone(&bridge),
        "session/load",
        json!({"sessionId": "lib-reload"}),
        &sink,
    )
    .await
    .expect("first load");

    assert_eq!(
        sink.lines.lock().unwrap().len(),
        1,
        "first load emits 1 message"
    );

    let sink2 = CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    };
    // Second load must succeed without re-emitting history for the already-loaded session.
    let result = handle_request(
        bridge,
        "session/load",
        json!({"sessionId": "lib-reload"}),
        &sink2,
    )
    .await
    .expect("second load");

    assert_eq!(result["sessionId"], "lib-reload");
    assert!(
        sink2.lines.lock().unwrap().is_empty(),
        "re-loading an already-loaded session must not re-emit messages"
    );
}

#[tokio::test]
async fn set_mode_chat_rebuilds_without_tools_and_keeps_turns() {
    use liberado_provider::{CompletionResponse, MockProvider};
    let store = TempDir::new().unwrap();
    let _guards = lock_sessions_dir(&store);
    let cwd = TempDir::new().unwrap();
    let bridge = test_bridge_with(Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("working on it")],
    )));
    let created = handle_session_new(&bridge, &json!({ "cwd": cwd.path() }))
        .await
        .unwrap();
    let sid = created["sessionId"].as_str().unwrap().to_string();
    let sink: Arc<dyn WireSink> = Arc::new(CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    run_session_prompt(
        Arc::clone(&bridge),
        sink,
        json!({
            "sessionId": sid,
            "prompt": [{ "type": "text", "text": "fix the test" }]
        }),
    )
    .await
    .unwrap();

    handle_set_mode(&bridge, &json!({ "sessionId": sid, "modeId": "chat" }))
        .await
        .unwrap();

    let map = bridge.acp_sessions.lock().await;
    let sess = map.get(&sid).unwrap();
    assert_eq!(sess.mode, AgentMode::Chat);
    let handle = sess
        .converse
        .clone()
        .expect("chat mode must keep a converse handle");
    assert!(
        !handle.coding_tools,
        "chat must not keep the coding tool runtime"
    );
    let convo = handle.conversation.lock().await;
    let text: String = convo
        .messages()
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("fix the test"), "{text}");
    assert!(text.contains("working on it"), "{text}");
}

#[tokio::test]
async fn set_mode_goal_drops_converse() {
    let store = TempDir::new().unwrap();
    let _guards = lock_sessions_dir(&store);
    let cwd = TempDir::new().unwrap();
    let bridge = test_bridge();
    let created = handle_session_new(&bridge, &json!({ "cwd": cwd.path() }))
        .await
        .unwrap();
    let sid = created["sessionId"].as_str().unwrap().to_string();
    ensure_converse(&bridge, &sid).await.unwrap();
    handle_set_mode(&bridge, &json!({ "sessionId": sid, "modeId": "goal" }))
        .await
        .unwrap();
    let map = bridge.acp_sessions.lock().await;
    let sess = map.get(&sid).unwrap();
    assert_eq!(sess.mode, AgentMode::Goal);
    assert!(
        sess.converse.is_none(),
        "goal mode must not keep a converse handle"
    );
}

#[tokio::test]
async fn set_mode_rejects_unknown_id_with_expected_list() {
    let store = TempDir::new().unwrap();
    let _guards = lock_sessions_dir(&store);
    let cwd = TempDir::new().unwrap();
    let bridge = test_bridge();
    let created = handle_session_new(&bridge, &json!({ "cwd": cwd.path() }))
        .await
        .unwrap();
    let sid = created["sessionId"].as_str().unwrap();
    let err = handle_set_mode(&bridge, &json!({ "sessionId": sid, "modeId": "banana" }))
        .await
        .expect_err("unknown mode");
    assert!(
        err.contains(AgentMode::EXPECTED),
        "error must list parseable modes, got: {err}"
    );
    assert!(err.contains("goal"), "{err}");
}

#[tokio::test]
async fn load_coding_session_restores_tools_and_history() {
    let store = TempDir::new().unwrap();
    let _guards = lock_sessions_dir(&store);
    let cwd = TempDir::new().unwrap();
    session_store::save(&session_store::SessionRecord {
        id: "lib-coding-memory".into(),
        mode: "coding".into(),
        cwd: cwd.path().to_path_buf(),
        model: "mock-model".into(),
        messages: vec![
            session_store::StoredMessage {
                role: "user".into(),
                content: "fix the test".into(),
            },
            session_store::StoredMessage {
                role: "assistant".into(),
                content: "working on it".into(),
            },
        ],
        updated_at: session_store::new_timestamp(),
    })
    .unwrap();

    let bridge = test_bridge();
    let sink = CaptureSink {
        lines: std::sync::Mutex::new(Vec::new()),
    };
    handle_request(
        Arc::clone(&bridge),
        "session/load",
        json!({ "sessionId": "lib-coding-memory" }),
        &sink,
    )
    .await
    .unwrap();

    let sessions = bridge.acp_sessions.lock().await;
    let handle = sessions
        .get("lib-coding-memory")
        .and_then(|s| s.converse.clone())
        .expect("coding load must restore a converse handle");
    assert!(handle.coding_tools);
    let convo = handle.conversation.lock().await;
    let text: String = convo
        .messages()
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("fix the test"), "{text}");
    assert!(text.contains("working on it"), "{text}");
}

#[tokio::test]
async fn handle_request_serves_the_ack_and_set_model_methods() {
    let bridge = test_bridge();
    let ack_sink = CaptureSink::new_test();
    // Deleting any of these arms would answer -32601; each must acknowledge instead.
    assert_eq!(
        handle_request(
            Arc::clone(&bridge),
            "session/set_config_option",
            json!({}),
            &ack_sink
        )
        .await
        .unwrap(),
        json!({})
    );
    assert_eq!(
        handle_request(Arc::clone(&bridge), "authenticate", json!({}), &ack_sink)
            .await
            .unwrap(),
        json!({})
    );
    assert_eq!(
        handle_request(Arc::clone(&bridge), "logout", json!({}), &ack_sink)
            .await
            .unwrap(),
        json!({})
    );
}

#[tokio::test]
async fn set_model_validates_then_applies_to_provider_and_state() {
    let provider = Arc::new(MockProvider::with_script("start/model", []));
    let bridge = test_bridge_with(provider.clone());
    let ack_sink = CaptureSink::new_test();

    let err = handle_set_model(&bridge, &json!({}))
        .await
        .expect_err("missing modelId is refused");
    assert!(err.contains("missing modelId"), "{err}");

    let err = handle_set_model(&bridge, &json!({ "modelId": "   " }))
        .await
        .expect_err("a whitespace modelId must not blank the model");
    assert!(err.contains("non-empty"), "{err}");
    assert_eq!(
        provider.model(),
        "start/model",
        "refusal leaves state alone"
    );

    handle_request(
        Arc::clone(&bridge),
        "session/set_model",
        json!({ "modelId": "  next/model " }),
        &ack_sink,
    )
    .await
    .expect("valid model accepted");
    assert_eq!(
        provider.model(),
        "next/model",
        "applied trimmed to the provider"
    );
    assert_eq!(*bridge.current_model.lock().await, "next/model");
}

#[tokio::test]
async fn refresh_catalog_keeps_old_on_empty_and_installs_fresh() {
    use crate::provider::CatalogModel;
    let provider = Arc::new(MockProvider::with_script("m", []));
    let bridge = test_bridge_with(provider.clone());
    bridge.catalog.lock().await.push(CatalogModel {
        model_id: "old/model".into(),
        name: "old/model".into(),
        description: String::new(),
    });

    // Live fetch empty (or failed): the static fallback list for the backend installs,
    // with the session's own model appended so the picker still shows it.
    refresh_catalog_from_live(&bridge).await;
    assert_eq!(
        catalog_model_ids_owned(&bridge).await,
        vec![
            "deepseek/deepseek-v4-flash".to_string(),
            "deepseek/deepseek-v4-pro".to_string(),
            "mock-model".to_string(),
        ]
    );

    provider.set_models(["zeta/b", "alpha/a"]);
    refresh_catalog_from_live(&bridge).await;
    let ids = catalog_model_ids_owned(&bridge).await;
    assert_eq!(
        ids,
        vec![
            "alpha/a".to_string(),
            "mock-model".to_string(),
            "zeta/b".to_string()
        ],
        "a non-empty live catalog replaces the fallbacks, sorted, current kept"
    );
}

#[tokio::test]
async fn extend_catalog_appends_unknown_and_never_duplicates() {
    use crate::provider::CatalogModel;
    let provider = Arc::new(MockProvider::with_script("m", []));
    let bridge = test_bridge_with(provider);
    bridge.catalog.lock().await.push(CatalogModel {
        model_id: "beta/2".into(),
        name: "beta/2".into(),
        description: String::new(),
    });

    extend_catalog_with_model(&bridge, "alpha/1").await;
    extend_catalog_with_model(&bridge, "alpha/1").await;
    let ids = catalog_model_ids_owned(&bridge).await;
    assert_eq!(
        ids,
        vec!["alpha/1".to_string(), "beta/2".to_string()],
        "unknown ids are appended and sorted; known ids are never duplicated"
    );
}

#[test]
fn model_state_pins_current_or_falls_back_to_first() {
    use crate::provider::CatalogModel;
    let mk = |id: &str| CatalogModel {
        model_id: id.into(),
        name: id.into(),
        description: String::new(),
    };
    let catalog = vec![mk("a/one"), mk("b/two")];

    assert_eq!(
        model_state(&catalog, "b/two")["currentModelId"],
        "b/two",
        "the session's own model stays selected"
    );
    assert_eq!(
        model_state(&catalog, "ghost")["currentModelId"],
        "a/one",
        "an unknown current falls back to the first pickable entry"
    );
    assert_eq!(
        model_state(&[], "solo")["currentModelId"],
        "solo",
        "an empty catalog cannot lie about the live model"
    );
}

#[test]
fn authority_summary_distinguishes_standalone_from_declared() {
    let standalone = authority_summary(&liberado_common::CapabilitySet::empty());
    assert_eq!(standalone["declared"], false);

    let mut grant = liberado_common::CapabilitySet::empty();
    grant.grant(liberado_common::Capability::AskHuman);
    grant.grant(liberado_common::Capability::Read(
        liberado_common::Zone::named("work"),
    ));
    let declared = authority_summary(&grant);
    assert_eq!(declared["declared"], true);
    assert_eq!(declared["askHuman"], true);
    assert_eq!(declared["capabilities"], 2);
}
