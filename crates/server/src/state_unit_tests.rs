//! Split from `state.rs` for module-health boundaries.

use super::*;

fn reaction(n: usize) -> Reaction {
    Reaction {
        event: Event {
            event_type: format!("TestEvent{n}"),
            timestamp: chrono::Utc::now(),
            source: "test".into(),
            correlation_id: format!("corr-{n}"),
            payload: Default::default(),
            provenance: None,
        },
        outcome: liberado_daemon::ReactionOutcome::Observed,
    }
}

/// The reaction buffer is a bounded ring of the newest 500: overflow trims from the front.
#[tokio::test]
async fn reaction_buffer_keeps_the_newest_five_hundred() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::for_test(
        Arc::new(liberado_session_store::SessionStore::open(dir.path()).await),
        None,
        std::env::temp_dir(),
    );
    let tx = state.reaction_tx();
    for n in 0..505 {
        tx.send(reaction(n)).unwrap();
    }
    // Yield until the mirror task has drained the channel.
    for _ in 0..100 {
        if state.reactions.lock().await.len() >= 500 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let guard = state.reactions.lock().await;
    assert_eq!(guard.len(), 500, "the buffer caps at 500");
    assert_eq!(
        guard[0].event_type, "TestEvent5",
        "the five oldest reactions were trimmed"
    );
    assert_eq!(guard[499].event_type, "TestEvent504");
}

/// The face compaction config is derived from topology: per-declared-model triggers plus an
/// entry for the live face slug — never an empty default table.
#[tokio::test]
async fn compaction_config_carries_per_model_triggers() {
    let config = Config::default();
    let face = "__face_model__";
    let out = compaction_config_for_face(&config, face);
    let compact = &config.topology.main_agent.compaction;
    assert_eq!(out.enabled, compact.enabled);
    assert_eq!(
        out.keep_recent_turns, compact.keep_recent_turns as usize,
        "keep_recent_turns comes from topology"
    );
    assert!(
        out.model_trigger_tokens.contains_key(face),
        "the face slug gets an entry even when undeclared: {:?}",
        out.model_trigger_tokens
    );
    assert_eq!(
        out.model_trigger_tokens[face],
        compact.resolve_trigger_tokens(Some(face), &config.topology.models),
    );
}

/// A face hot-swap retunes only the daemon-default threshold — and it actually runs.
#[tokio::test]
async fn resync_updates_the_default_chat_trigger() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(liberado_session_store::SessionStore::open(dir.path()).await);
    let provider = Arc::new(liberado_provider::MockProvider::new("m"));
    let executor =
        liberado_executor::Executor::new(provider.clone(), liberado_executor::Budget::default());
    let chat = Arc::new(
        ChatSessions::new(store.clone(), executor, Arc::new(NoTools)).with_compaction(
            compaction_config_for_face(&Config::default(), "__face_model__"),
            provider,
        ),
    );
    let state = AppState::for_test(store, Some(chat.clone()), dir.path().to_path_buf());

    chat.set_compaction_trigger_tokens(123_456);
    resync_compaction_trigger_for_face_model(&state, "__face_model__");

    let expected = state
        .config
        .topology
        .main_agent
        .compaction
        .resolve_trigger_tokens(Some("__face_model__"), &state.config.topology.models);
    assert_ne!(
        chat.compaction_trigger_tokens(),
        Some(123_456),
        "the stale boot value must be replaced"
    );
    assert_eq!(
        chat.compaction_trigger_tokens(),
        Some(expected),
        "the new trigger is what topology resolves for the new face model"
    );
}

/// The no-tool runtime answers every call with an explicit refusal, and its catalog is empty.
#[tokio::test]
async fn no_tools_runtime_refuses_invocations() {
    use liberado_executor::ToolRuntime;
    let out = NoTools
        .invoke(&liberado_provider::ToolInvocation {
            id: "t1".into(),
            name: "anything".into(),
            arguments: serde_json::json!({}),
        })
        .await;
    let err = out.expect_err("a no-tool runtime must refuse");
    assert!(err.contains("no tools"), "{err}");
    assert!(NoTools.catalog().is_empty());
}
